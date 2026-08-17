use serde_json::{Value as JsonValue, json};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub(crate) struct LspPosition {
    pub(crate) line: usize,
    pub(crate) character: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct LspLocation {
    pub(crate) path: Option<PathBuf>,
    pub(crate) start_line: usize,
    pub(crate) start_character: usize,
    pub(crate) end_line: usize,
    pub(crate) end_character: usize,
    pub(crate) provider: String,
}

pub(crate) struct RustAnalyzerProvider {
    binary: String,
    sessions: Mutex<BTreeMap<PathBuf, Arc<Mutex<RustAnalyzerSession>>>>,
}

impl fmt::Debug for RustAnalyzerProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RustAnalyzerProvider")
            .field("binary", &self.binary)
            .finish_non_exhaustive()
    }
}

impl RustAnalyzerProvider {
    pub(crate) fn new(binary: String) -> Self {
        Self {
            binary,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn definition(
        &self,
        root: &Path,
        file: &Path,
        text: &str,
        position: LspPosition,
    ) -> Result<Vec<LspLocation>, String> {
        self.with_session(root, |session| {
            session.did_open(file, text)?;
            session.locations_request(
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": file_uri(file)},
                    "position": lsp_position(position),
                }),
            )
        })
    }

    pub(crate) fn references(
        &self,
        root: &Path,
        file: &Path,
        text: &str,
        position: LspPosition,
    ) -> Result<Vec<LspLocation>, String> {
        self.with_session(root, |session| {
            session.did_open(file, text)?;
            session.locations_request(
                "textDocument/references",
                json!({
                    "textDocument": {"uri": file_uri(file)},
                    "position": lsp_position(position),
                    "context": {"includeDeclaration": true},
                }),
            )
        })
    }

    fn with_session<R>(
        &self,
        root: &Path,
        f: impl FnOnce(&mut RustAnalyzerSession) -> Result<R, String>,
    ) -> Result<R, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("invalid rust-analyzer root: {error}"))?;
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| "rust-analyzer session lock poisoned".to_owned())?;
            if let Some(session) = sessions.get(&root) {
                session.clone()
            } else {
                let session = Arc::new(Mutex::new(RustAnalyzerSession::start(
                    self.binary.clone(),
                    root.clone(),
                )?));
                sessions.insert(root.clone(), session.clone());
                session
            }
        };
        let mut session = session
            .lock()
            .map_err(|_| "rust-analyzer session lock poisoned".to_owned())?;
        f(&mut session)
    }
}

struct RustAnalyzerSession {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: Receiver<Result<JsonValue, String>>,
    next_id: u64,
    open_versions: BTreeMap<PathBuf, i32>,
    root_uri: String,
    root_name: String,
    provider: String,
}

impl RustAnalyzerSession {
    fn start(binary: String, root: PathBuf) -> Result<Self, String> {
        let mut child = Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start rust-analyzer: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open rust-analyzer stdin".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open rust-analyzer stdout".to_owned())?;
        let stdout = spawn_stdout_reader(stdout);
        let root_uri = file_uri(&root);
        let mut session = Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout,
            next_id: 1,
            open_versions: BTreeMap::new(),
            root_uri: root_uri.clone(),
            root_name: root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
                .to_owned(),
            provider: "rust-analyzer".to_owned(),
        };
        let result = session.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "rootPath": root.to_string_lossy(),
                "capabilities": {
                    "workspace": {
                        "workspaceFolders": true,
                        "configuration": true,
                    },
                    "textDocument": {
                        "definition": {"dynamicRegistration": false, "linkSupport": true},
                        "references": {"dynamicRegistration": false},
                    },
                },
                "workspaceFolders": [{"uri": session.root_uri.clone(), "name": session.root_name.clone()}],
            }),
        )?;
        if let Some(version) = result
            .pointer("/serverInfo/version")
            .and_then(JsonValue::as_str)
        {
            session.provider = format!("rust-analyzer {version}");
        }
        session.notify("initialized", json!({}))?;
        Ok(session)
    }

    fn did_open(&mut self, file: &Path, text: &str) -> Result<(), String> {
        let file = file
            .canonicalize()
            .map_err(|error| format!("failed to resolve rust source path: {error}"))?;
        let uri = file_uri(&file);
        if let Some(version) = self.open_versions.get_mut(&file) {
            *version += 1;
            let version = *version;
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "version": version,
                    },
                    "contentChanges": [{"text": text}],
                }),
            )?;
        } else {
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": text,
                    }
                }),
            )?;
            self.open_versions.insert(file, 1);
        }
        Ok(())
    }

    fn locations_request(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<Vec<LspLocation>, String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let result = match self.request(method, params.clone()) {
                Ok(result) => result,
                Err(error) if error.contains("\"code\":-32801") && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                Err(error) => return Err(error),
            };
            return Ok(parse_locations(&result, &self.provider));
        }
    }

    fn request(&mut self, method: &str, params: JsonValue) -> Result<JsonValue, String> {
        let id = self.next_id;
        self.next_id += 1;
        let deadline = Instant::now() + Duration::from_secs(5);
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("rust-analyzer {method} timed out"));
            }
            let message = self.read_message()?;
            if message.get("method").is_some() && message.get("id").is_some() {
                self.respond_to_server_request(&message)?;
                continue;
            }
            if message.get("id").and_then(JsonValue::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!("rust-analyzer {method} failed: {error}"));
            }
            return Ok(message.get("result").cloned().unwrap_or(JsonValue::Null));
        }
    }

    fn notify(&mut self, method: &str, params: JsonValue) -> Result<(), String> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn respond_to_server_request(&mut self, message: &JsonValue) -> Result<(), String> {
        let id = message
            .get("id")
            .cloned()
            .ok_or_else(|| "server request omitted id".to_owned())?;
        let method = message
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let result = match method {
            "workspace/configuration" => json!([{}]),
            "workspace/workspaceFolders" => {
                json!([{"uri": self.root_uri, "name": self.root_name}])
            }
            "client/registerCapability" | "window/workDoneProgress/create" => JsonValue::Null,
            _ => JsonValue::Null,
        };
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
    }

    fn write_message(&mut self, message: &JsonValue) -> Result<(), String> {
        let payload = serde_json::to_vec(message)
            .map_err(|error| format!("failed to encode rust-analyzer request: {error}"))?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", payload.len())
            .map_err(|error| format!("failed to write rust-analyzer header: {error}"))?;
        self.stdin
            .write_all(&payload)
            .map_err(|error| format!("failed to write rust-analyzer payload: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("failed to flush rust-analyzer request: {error}"))
    }

    fn read_message(&mut self) -> Result<JsonValue, String> {
        self.stdout
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "timed out waiting for rust-analyzer response".to_owned())?
    }
}

fn spawn_stdout_reader(stdout: ChildStdout) -> Receiver<Result<JsonValue, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        loop {
            let message = read_message_from(&mut stdout);
            let done = message.is_err();
            if tx.send(message).is_err() || done {
                break;
            }
        }
    });
    rx
}

fn read_message_from(stdout: &mut BufReader<ChildStdout>) -> Result<JsonValue, String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = stdout
            .read_line(&mut line)
            .map_err(|error| format!("failed to read rust-analyzer header: {error}"))?;
        if read == 0 {
            return Err("rust-analyzer closed stdout".to_owned());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| format!("invalid rust-analyzer content length: {error}"))?,
            );
        }
    }
    let length =
        content_length.ok_or_else(|| "rust-analyzer response omitted Content-Length".to_owned())?;
    let mut payload = vec![0; length];
    stdout
        .read_exact(&mut payload)
        .map_err(|error| format!("failed to read rust-analyzer payload: {error}"))?;
    serde_json::from_slice(&payload)
        .map_err(|error| format!("failed to decode rust-analyzer response: {error}"))
}

impl Drop for RustAnalyzerSession {
    fn drop(&mut self) {
        let _ = self.request("shutdown", JsonValue::Null);
        let _ = self.notify("exit", JsonValue::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_locations(result: &JsonValue, provider: &str) -> Vec<LspLocation> {
    match result {
        JsonValue::Array(items) => items
            .iter()
            .filter_map(|item| parse_location(item, provider))
            .collect(),
        JsonValue::Object(_) => parse_location(result, provider).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_location(value: &JsonValue, provider: &str) -> Option<LspLocation> {
    if let Some(uri) = value.get("uri").and_then(JsonValue::as_str) {
        let range = value.get("range")?;
        return Some(location_from_parts(uri, range, provider));
    }
    if let Some(uri) = value.get("targetUri").and_then(JsonValue::as_str) {
        let range = value
            .get("targetSelectionRange")
            .or_else(|| value.get("targetRange"))?;
        return Some(location_from_parts(uri, range, provider));
    }
    None
}

fn location_from_parts(uri: &str, range: &JsonValue, provider: &str) -> LspLocation {
    let start = range.get("start").unwrap_or(&JsonValue::Null);
    let end = range.get("end").unwrap_or(&JsonValue::Null);
    LspLocation {
        path: path_from_file_uri(uri),
        start_line: json_usize(start, "line"),
        start_character: json_usize(start, "character"),
        end_line: json_usize(end, "line"),
        end_character: json_usize(end, "character"),
        provider: provider.to_owned(),
    }
}

fn json_usize(value: &JsonValue, key: &str) -> usize {
    value.get(key).and_then(JsonValue::as_u64).unwrap_or(0) as usize
}

fn lsp_position(position: LspPosition) -> JsonValue {
    json!({
        "line": position.line,
        "character": position.character,
    })
}

fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{}", percent_encode_path(&path))
    } else {
        format!("file:///{}", percent_encode_path(&path))
    }
}

fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest.to_owned()
    } else {
        format!("/{rest}")
    };
    Some(PathBuf::from(percent_decode_path(&path)?))
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'-' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = hex(bytes.get(index + 1).copied()?)?;
            let lo = hex(bytes.get(index + 2).copied()?)?;
            out.push((hi << 4) | lo);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
