// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fixed editor file services rooted in host-configured directory capabilities.

use cap_std::ambient_authority;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, Metadata, OpenOptions};
use mica_var::{Symbol, Value};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_CANDIDATES: usize = 100;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct Root {
    path: PathBuf,
    directory: Dir,
}

pub struct EditorFiles {
    roots: Vec<Root>,
    writes: Mutex<()>,
}

struct Failure {
    status: &'static str,
    message: String,
}

impl Failure {
    fn new(status: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::new(
            if error.kind() == io::ErrorKind::PermissionDenied {
                "denied"
            } else {
                "error"
            },
            error.to_string(),
        )
    }
}

fn symbol(name: &str) -> Value {
    Value::symbol(Symbol::intern(name))
}

fn map(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::map(
        entries
            .into_iter()
            .map(|(name, value)| (symbol(name), value)),
    )
}

fn field(payload: &Value, name: &str) -> Option<Value> {
    payload.map_get(&symbol(name))
}

fn text_field(payload: &Value, name: &str) -> Result<String, Failure> {
    field(payload, name)
        .and_then(|value| value.with_str(str::to_owned))
        .ok_or_else(|| Failure::new("error", format!("file request requires a string {name}")))
}

fn path_text(path: &Path) -> Result<&str, Failure> {
    path.to_str()
        .ok_or_else(|| Failure::new("error", "file path is not valid UTF-8"))
}

impl EditorFiles {
    pub fn new(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self, String> {
        let mut roots = Vec::new();
        for path in paths {
            let path = path
                .canonicalize()
                .map_err(|error| format!("cannot resolve editor root: {error}"))?;
            if path.to_str().is_none() {
                return Err("editor root is not valid UTF-8".to_owned());
            }
            let directory = Dir::open_ambient_dir(&path, ambient_authority())
                .map_err(|error| format!("cannot open editor root {}: {error}", path.display()))?;
            roots.push(Root { path, directory });
        }
        Ok(Self {
            roots,
            writes: Mutex::new(()),
        })
    }

    pub fn handles(service: Symbol) -> bool {
        matches!(
            service.name(),
            Some(
                "editor_file_read"
                    | "editor_file_stat"
                    | "editor_file_list"
                    | "editor_file_write_atomic"
            )
        )
    }

    /// Runs blocking filesystem work. The host must call this outside its I/O executor.
    pub fn handle(&self, service: Symbol, payload: &Value) -> Value {
        let result = if self.roots.is_empty() {
            Err(Failure::new(
                "denied",
                "the editor has no configured file roots",
            ))
        } else {
            match service.name() {
                Some("editor_file_read") => self.read(payload, true),
                Some("editor_file_stat") => self.read(payload, false),
                Some("editor_file_list") => self.list(payload),
                Some("editor_file_write_atomic") => self.write(payload),
                _ => Err(Failure::new("error", "unknown editor file service")),
            }
        };
        result.unwrap_or_else(|failure| {
            map([
                ("status", symbol(failure.status)),
                ("message", Value::string(failure.message)),
            ])
        })
    }

    fn resolve(&self, input: &str, allow_missing: bool) -> Result<(&Root, PathBuf), Failure> {
        if input.is_empty() {
            return Err(Failure::new("error", "file path is empty"));
        }
        let input = Path::new(input);
        let (root, relative) = if input.is_absolute() {
            self.roots
                .iter()
                .find_map(|root| {
                    input
                        .strip_prefix(&root.path)
                        .ok()
                        .map(|relative| (root, relative))
                })
                .ok_or_else(|| {
                    Failure::new("denied", "path is outside the configured editor roots")
                })?
        } else {
            (&self.roots[0], input)
        };
        let relative = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            relative
        };
        match root.directory.canonicalize(relative) {
            Ok(path) => Ok((root, path)),
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => {
                if root.directory.symlink_metadata(relative).is_ok() {
                    return Err(Failure::new(
                        "denied",
                        "file path has an unresolved symbolic link",
                    ));
                }
                let leaf = relative
                    .file_name()
                    .ok_or_else(|| Failure::new("error", "file path needs a leaf name"))?;
                let parent = relative
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let parent: PathBuf = root
                    .directory
                    .canonicalize(parent)?
                    .components()
                    .filter(|part| !matches!(part, Component::CurDir))
                    .collect();
                Ok((root, parent.join(leaf)))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn read(&self, payload: &Value, include_text: bool) -> Result<Value, Failure> {
        let input = text_field(payload, "path")?;
        let (root, path) = self.resolve(&input, true)?;
        let absolute = root.path.join(&path);
        let mut entries = vec![
            ("path", Value::string(path_text(&absolute)?)),
            (
                "name",
                Value::string(path_text(path.file_name().map(Path::new).unwrap_or(&path))?),
            ),
        ];
        let Some((bytes, metadata)) = read_contents(&root.directory, &path)? else {
            entries.push(("status", symbol("missing")));
            return Ok(map(entries));
        };
        entries.push(("status", symbol("ok")));
        entries.push(("stamp", stamp(&bytes, &metadata)?));
        if include_text {
            let text = String::from_utf8(bytes)
                .map_err(|_| Failure::new("error", "file is not valid UTF-8"))?;
            let crlf = text.contains("\r\n");
            entries.push((
                "text",
                Value::string(if crlf {
                    text.replace("\r\n", "\n")
                } else {
                    text
                }),
            ));
            entries.push(("encoding", Value::string("utf-8")));
            entries.push(("line_ending", symbol(if crlf { "crlf" } else { "lf" })));
        }
        Ok(map(entries))
    }

    fn write(&self, payload: &Value) -> Result<Value, Failure> {
        let input = text_field(payload, "path")?;
        let text = text_field(payload, "text")?;
        let expected = field(payload, "expected_stamp")
            .ok_or_else(|| Failure::new("error", "file write requires expected_stamp"))?;
        let line_ending = field(payload, "line_ending").unwrap_or_else(|| symbol("lf"));
        let bytes = match line_ending.as_symbol().and_then(Symbol::name) {
            Some("lf") => text.into_bytes(),
            Some("crlf") => text.replace('\n', "\r\n").into_bytes(),
            _ => return Err(Failure::new("error", "line ending must be :lf or :crlf")),
        };
        if bytes.len() > MAX_BYTES {
            return Err(Failure::new("error", "file exceeds the editor byte limit"));
        }
        // Serialize saves through this host, including their compare-before-rename checks.
        let _write = self.writes.lock().unwrap();
        let (root, path) = self.resolve(&input, true)?;
        let absolute = root.path.join(&path);
        let parent = root.directory.open_dir(
            path.parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        // Directory capabilities can use O_PATH on Linux; open a readable handle for fsync.
        let directory_sync = parent.open(".")?;
        let leaf = path
            .file_name()
            .ok_or_else(|| Failure::new("error", "file path needs a leaf name"))?;
        let current = read_contents(&parent, Path::new(leaf))?;
        let current_stamp = optional_stamp(&current)?;
        if expected != current_stamp {
            return changed(&absolute, current_stamp);
        }
        let (temporary, mut file) = loop {
            let name = format!(
                ".mica-save-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            );
            match parent.open_with(&name, OpenOptions::new().write(true).create_new(true)) {
                Ok(file) => break (name, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let mut cleanup = Temporary {
            directory: &parent,
            name: Some(&temporary),
        };
        if let Some((_, metadata)) = &current {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        let saved_stamp = stamp(&bytes, &file.metadata()?)?;
        drop(file);
        let latest = optional_stamp(&read_contents(&parent, Path::new(leaf))?)?;
        if expected != latest {
            return changed(&absolute, latest);
        }
        parent.rename(&temporary, &parent, leaf)?;
        cleanup.name = None;
        drop(cleanup);
        directory_sync.sync_all().map_err(|error| {
            Failure::new(
                "error",
                format!("file was replaced but its directory could not be flushed: {error}"),
            )
        })?;
        Ok(map([
            ("status", symbol("ok")),
            ("path", Value::string(path_text(&absolute)?)),
            ("stamp", saved_stamp),
        ]))
    }

    fn list(&self, payload: &Value) -> Result<Value, Failure> {
        let query = text_field(payload, "query")?;
        let limit = match field(payload, "limit") {
            None => MAX_CANDIDATES,
            Some(value) => value
                .as_int()
                .ok_or_else(|| Failure::new("error", "file list limit must be an integer"))?
                .clamp(1, MAX_CANDIDATES as i64) as usize,
        };
        let input = Path::new(&query);
        let (directory, prefix) =
            if query.is_empty() || query.ends_with('/') || input.file_name().is_none() {
                (
                    if query.is_empty() {
                        Path::new(".")
                    } else {
                        input
                    },
                    "",
                )
            } else {
                (
                    input
                        .parent()
                        .filter(|path| !path.as_os_str().is_empty())
                        .unwrap_or(Path::new(".")),
                    path_text(input.file_name().map(Path::new).unwrap_or(input))?,
                )
            };
        let (root, relative) = self.resolve(path_text(directory)?, false)?;
        let directory = root.directory.open_dir(&relative)?;
        let mut candidates = Vec::new();
        let mut exact = false;
        for entry in directory.entries()? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(prefix) {
                continue;
            }
            exact |= name == prefix;
            let kind = entry.file_type()?;
            // Do not offer symlinks whose target might leave the configured root.
            if !kind.is_file() && !kind.is_dir() {
                continue;
            }
            let absolute = root.path.join(&relative).join(name);
            let mut path = path_text(&absolute)?.to_owned();
            if kind.is_dir() {
                path.push('/');
            }
            candidates.push((
                !kind.is_dir(),
                path,
                if kind.is_dir() { "directory" } else { "" },
            ));
            // Keep only the best bounded prefix, even for large directories.
            candidates.sort_unstable();
            candidates.truncate(limit);
        }
        if !prefix.is_empty() && !exact {
            let absolute = root.path.join(&relative).join(prefix);
            candidates.push((true, path_text(&absolute)?.to_owned(), "new file"));
            candidates.sort_unstable();
            candidates.truncate(limit);
        }
        Ok(map([
            ("status", symbol("ok")),
            (
                "candidates",
                Value::list(candidates.into_iter().map(|(_, path, annotation)| {
                    map([
                        ("key", Value::string(&path)),
                        ("value", Value::string(&path)),
                        ("label", Value::string(path)),
                        ("annotation", Value::string(annotation)),
                    ])
                })),
            ),
        ]))
    }
}

struct Temporary<'a> {
    directory: &'a Dir,
    name: Option<&'a str>,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if let Some(name) = self.name {
            let _ = self.directory.remove_file(name);
        }
    }
}

fn read_contents(directory: &Dir, path: &Path) -> Result<Option<(Vec<u8>, Metadata)>, Failure> {
    let mut options = OpenOptions::new();
    options.read(true);
    // A FIFO must not block the worker before its file type can be rejected.
    #[cfg(unix)]
    options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    let file = match directory.open_with(path, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if metadata.is_dir() {
        return Err(Failure::new("directory", "path names a directory"));
    }
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err(Failure::new(
            "error",
            "file is not a supported regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > MAX_BYTES
        || after.len() != bytes.len() as u64
        || metadata.len() != after.len()
        || metadata.modified()? != after.modified()?
    {
        return Err(Failure::new(
            "error",
            "file changed while it was being read",
        ));
    }
    Ok(Some((bytes, after)))
}

fn stamp(bytes: &[u8], metadata: &Metadata) -> Result<Value, Failure> {
    let modified = metadata
        .modified()?
        .into_std()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Failure::new("error", "file modification time precedes the Unix epoch"))?;
    Ok(map([
        ("size", Value::int(bytes.len() as i64).unwrap()),
        // Nanosecond timestamps exceed Mica's integer range; stamps are opaque to the app.
        (
            "modified_ns",
            Value::string(modified.as_nanos().to_string()),
        ),
        (
            "content_hash",
            Value::string(format!("{:x}", Sha256::digest(bytes))),
        ),
    ]))
}

fn optional_stamp(contents: &Option<(Vec<u8>, Metadata)>) -> Result<Value, Failure> {
    contents
        .as_ref()
        .map_or(Ok(Value::empty_relation()), |(bytes, metadata)| {
            stamp(bytes, metadata)
        })
}

fn changed(path: &Path, current: Value) -> Result<Value, Failure> {
    Ok(map([
        ("status", symbol("changed")),
        ("path", Value::string(path_text(path)?)),
        ("current_stamp", current),
        ("message", Value::string("file changed on disk")),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{Arc, Barrier};
    use std::thread;

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mica-editor-files-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn files(&self) -> EditorFiles {
            EditorFiles::new([self.0.clone()]).unwrap()
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request(files: &EditorFiles, service: &str, payload: Value, status: &str) -> Value {
        let value = files.handle(Symbol::intern(service), &payload);
        assert_eq!(field(&value, "status"), Some(symbol(status)), "{value:?}");
        value
    }
    fn path_payload(path: impl AsRef<str>) -> Value {
        map([("path", Value::string(path))])
    }
    fn save(path: &str, text: &str, stamp: Value, ending: &str) -> Value {
        map([
            ("path", Value::string(path)),
            ("text", Value::string(text)),
            ("expected_stamp", stamp),
            ("line_ending", symbol(ending)),
        ])
    }

    #[test]
    fn reads_unicode_and_crlf_and_saves_atomically_with_fresh_stamps() {
        let workspace = Workspace::new();
        fs::write(workspace.0.join("notes"), "é🦀\r\nsecond\r\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(workspace.0.join("notes"), fs::Permissions::from_mode(0o640)).unwrap();
        let files = workspace.files();
        let read = request(&files, "editor_file_read", path_payload("notes"), "ok");
        assert_eq!(field(&read, "text"), Some(Value::string("é🦀\nsecond\n")));
        assert_eq!(field(&read, "line_ending"), Some(symbol("crlf")));
        let before = field(&read, "stamp").unwrap();
        assert!(
            field(&before, "modified_ns")
                .unwrap()
                .with_str(|_| ())
                .is_some()
        );
        let saved = request(
            &files,
            "editor_file_write_atomic",
            save("notes", "λ\n🦀\n", before.clone(), "crlf"),
            "ok",
        );
        assert_eq!(
            fs::read(workspace.0.join("notes")).unwrap(),
            "λ\r\n🦀\r\n".as_bytes()
        );
        let stat = request(&files, "editor_file_stat", path_payload("notes"), "ok");
        assert_eq!(field(&stat, "stamp"), field(&saved, "stamp"));
        request(
            &files,
            "editor_file_write_atomic",
            save("notes", "stale", before, "lf"),
            "changed",
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(workspace.0.join("notes"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 1);
    }

    #[test]
    fn missing_files_and_external_changes_require_the_current_stamp() {
        let workspace = Workspace::new();
        let files = workspace.files();
        let missing = request(&files, "editor_file_read", path_payload("fresh"), "missing");
        assert_eq!(
            field(&missing, "path"),
            Some(Value::string(workspace.0.join("fresh").to_str().unwrap()))
        );
        let saved = request(
            &files,
            "editor_file_write_atomic",
            save("fresh", "created", Value::empty_relation(), "lf"),
            "ok",
        );
        let before = field(&saved, "stamp").unwrap();
        fs::write(workspace.0.join("fresh"), "external").unwrap();
        let changed = request(
            &files,
            "editor_file_write_atomic",
            save("fresh", "mine", before, "lf"),
            "changed",
        );
        assert_eq!(
            fs::read_to_string(workspace.0.join("fresh")).unwrap(),
            "external"
        );
        let current = field(&changed, "current_stamp").unwrap();
        fs::remove_file(workspace.0.join("fresh")).unwrap();
        let changed = request(
            &files,
            "editor_file_write_atomic",
            save("fresh", "mine", current, "lf"),
            "changed",
        );
        assert_eq!(
            field(&changed, "current_stamp"),
            Some(Value::empty_relation())
        );
        request(
            &files,
            "editor_file_write_atomic",
            save("fresh", "confirmed", Value::empty_relation(), "lf"),
            "ok",
        );
    }

    #[test]
    fn concurrent_saves_with_the_same_stamp_have_one_winner() {
        let workspace = Workspace::new();
        fs::write(workspace.0.join("notes"), "base").unwrap();
        let files = Arc::new(workspace.files());
        let stamp = field(
            &request(&files, "editor_file_stat", path_payload("notes"), "ok"),
            "stamp",
        )
        .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|text| {
                let files = files.clone();
                let barrier = barrier.clone();
                let stamp = stamp.clone();
                thread::spawn(move || {
                    barrier.wait();
                    files.handle(
                        Symbol::intern("editor_file_write_atomic"),
                        &save("notes", text, stamp, "lf"),
                    )
                })
            })
            .collect();
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| field(&thread.join().unwrap(), "status").unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|value| **value == symbol("ok"))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|value| **value == symbol("changed"))
                .count(),
            1
        );
        assert!(matches!(
            fs::read_to_string(workspace.0.join("notes"))
                .unwrap()
                .as_str(),
            "left" | "right"
        ));
    }

    #[test]
    fn file_services_reject_escape_binary_and_oversized_inputs() {
        let workspace = Workspace::new();
        let outside = Workspace::new();
        fs::write(outside.0.join("secret"), "secret").unwrap();
        fs::write(workspace.0.join("binary"), [0xff, 0xfe]).unwrap();
        let large = fs::File::create(workspace.0.join("large")).unwrap();
        large.set_len((MAX_BYTES + 1) as u64).unwrap();
        let files = workspace.files();
        request(&files, "editor_file_read", path_payload("binary"), "error");
        request(&files, "editor_file_read", path_payload("large"), "error");
        request(&files, "editor_file_read", path_payload("."), "directory");
        request(
            &files,
            "editor_file_read",
            path_payload(outside.0.join("secret").to_str().unwrap()),
            "denied",
        );
        request(
            &files,
            "editor_file_read",
            path_payload("../secret"),
            "denied",
        );
        request(
            &files,
            "editor_file_write_atomic",
            save(
                "large",
                &"a".repeat(MAX_BYTES + 1),
                Value::empty_relation(),
                "lf",
            ),
            "error",
        );
        let disabled = EditorFiles::new([]).unwrap();
        request(
            &disabled,
            "editor_file_read",
            path_payload("binary"),
            "denied",
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_capabilities_reject_symlink_escapes_for_reads_writes_and_lists() {
        let workspace = Workspace::new();
        let outside = Workspace::new();
        fs::write(outside.0.join("secret"), "secret").unwrap();
        symlink(&outside.0, workspace.0.join("escape")).unwrap();
        symlink("../secret", workspace.0.join("relative")).unwrap();
        let files = workspace.files();
        for path in ["escape/secret", "relative"] {
            request(&files, "editor_file_read", path_payload(path), "denied");
            request(
                &files,
                "editor_file_write_atomic",
                save(path, "overwrite", Value::empty_relation(), "lf"),
                "denied",
            );
        }
        request(
            &files,
            "editor_file_list",
            map([("query", Value::string("escape/"))]),
            "denied",
        );
        assert_eq!(
            fs::read_to_string(outside.0.join("secret")).unwrap(),
            "secret"
        );
    }

    #[cfg(unix)]
    #[test]
    fn named_pipes_are_rejected_without_waiting_for_a_writer() {
        let workspace = Workspace::new();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            workspace.0.join("pipe"),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        let files = workspace.files();
        request(&files, "editor_file_read", path_payload("pipe"), "error");
        request(&files, "editor_file_stat", path_payload("pipe"), "error");
        request(
            &files,
            "editor_file_write_atomic",
            save("pipe", "text", Value::empty_relation(), "lf"),
            "error",
        );
    }

    #[test]
    fn completion_is_bounded_sorted_and_includes_new_file_candidates() {
        let workspace = Workspace::new();
        fs::create_dir(workspace.0.join("z-directory")).unwrap();
        for index in (0..150).rev() {
            fs::write(workspace.0.join(format!("file-{index:03}")), "").unwrap();
        }
        let files = workspace.files();
        let listed = request(
            &files,
            "editor_file_list",
            map([
                ("query", Value::string("")),
                ("limit", Value::int(500).unwrap()),
            ]),
            "ok",
        );
        let candidates = field(&listed, "candidates").unwrap();
        candidates
            .with_list(|rows| {
                assert_eq!(rows.len(), MAX_CANDIDATES);
                assert_eq!(
                    field(&rows[0], "annotation"),
                    Some(Value::string("directory"))
                );
                assert!(
                    field(&rows[1], "label")
                        .unwrap()
                        .with_str(|text| text.ends_with("file-000"))
                        .unwrap()
                );
            })
            .unwrap();
        let listed = request(
            &files,
            "editor_file_list",
            map([("query", Value::string("new-note"))]),
            "ok",
        );
        field(&listed, "candidates")
            .unwrap()
            .with_list(|rows| {
                assert_eq!(rows.len(), 1);
                assert_eq!(
                    field(&rows[0], "annotation"),
                    Some(Value::string("new file"))
                );
            })
            .unwrap();
    }
}
