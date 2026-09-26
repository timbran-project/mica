// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::codec::{HttpRequest, HttpResponse};
use crate::response::query_params;
use crate::sync::{SyncSession, ensure_session};
use crate::{InProcessWebHost, RequestBinding};
use mica_driver::InvocationOutcome;
use mica_var::{Identity, Symbol, Value};
use serde_json::{Value as Json, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const MAX_ITEM_BYTES: usize = 64 * 1024;
const MAX_BATCH_BYTES: usize = 256 * 1024;
const MAX_PENDING_BYTES: usize = 1024 * 1024;
const MAX_PENDING_ITEMS: u64 = 1024;
pub(crate) const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REPLIES: usize = 256;
// Completion tokens are shared across sessions. Keep host tokens in the
// upper half of the JavaScript-safe range, separate from input sequences.
static NEXT_APPLY_TOKEN: AtomicU64 = AtomicU64::new(1 << 52);

#[derive(Debug, Default)]
pub(crate) struct EditorState {
    admitted: u64,
    completed: u64,
    pending_bytes: usize,
    running: bool,
    queue: VecDeque<EditorInput>,
    replies: VecDeque<(u64, Arc<str>)>,
    reply_bytes: usize,
}

#[derive(Debug)]
struct EditorInput {
    sequence: u64,
    frame: u64,
    generation: u64,
    text: String,
    bounds: SnapshotBounds,
}

#[derive(Default)]
struct PendingReply {
    state: Mutex<(Option<Arc<str>>, Option<Waker>)>,
}

impl PendingReply {
    fn complete(&self, body: Arc<str>) {
        let waker = {
            let mut state = self.state.lock().unwrap();
            state.0 = Some(body);
            state.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    async fn wait(&self) -> Arc<str> {
        std::future::poll_fn(|cx| {
            let mut state = self.state.lock().unwrap();
            if let Some(body) = state.0.take() {
                return Poll::Ready(body);
            }
            state.1 = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

#[derive(Clone, Copy, Debug)]
struct SnapshotBounds {
    lines: u64,
    scalars: u64,
}

impl Default for SnapshotBounds {
    fn default() -> Self {
        Self {
            lines: 200,
            scalars: 262_144,
        }
    }
}

#[derive(Debug)]
struct RequestError(u16, &'static str, String);

type Result<T> = std::result::Result<T, RequestError>;

fn bad(message: impl Into<String>) -> RequestError {
    RequestError(400, "Bad Request", message.into())
}

fn conflict(message: &str) -> RequestError {
    RequestError(409, "Conflict", message.to_owned())
}

fn number(text: &str, minimum: u64, maximum: u64) -> Result<u64> {
    text.parse::<u64>()
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| bad(format!("expected integer in {minimum}..={maximum}")))
}

fn json_number(value: Option<&Json>, default: Option<u64>, minimum: u64) -> Result<u64> {
    match value {
        Some(Json::String(value)) => number(value, minimum, MAX_SAFE_INTEGER),
        Some(Json::Number(value)) => value
            .as_u64()
            .filter(|value| (minimum..=MAX_SAFE_INTEGER).contains(value))
            .ok_or_else(|| bad("invalid integer")),
        None => default.ok_or_else(|| bad("missing integer")),
        _ => Err(bad("invalid integer")),
    }
}

fn query_number(
    params: &HashMap<String, String>,
    key: &str,
    default: Option<u64>,
    min: u64,
    max: u64,
) -> Result<u64> {
    match params.get(key) {
        Some(value) => number(value, min, max),
        None => default.ok_or_else(|| bad(format!("missing {key}"))),
    }
}

fn query_bounds(params: &HashMap<String, String>) -> Result<SnapshotBounds> {
    Ok(SnapshotBounds {
        lines: query_number(params, "lines", Some(200), 1, 1000)?,
        scalars: query_number(params, "max", Some(262_144), 1, 1_048_576)?,
    })
}

impl EditorState {
    fn replay(&self, sequence: u64) -> Result<Option<Arc<str>>> {
        if sequence > self.completed {
            return Ok(None);
        }
        self.replies
            .iter()
            .find(|(stored, _)| *stored == sequence)
            .map(|(_, body)| Some(body.clone()))
            .ok_or_else(|| {
                RequestError(
                    410,
                    "Gone",
                    "editor reply expired; start a new session".into(),
                )
            })
    }

    fn complete(&mut self, input: &EditorInput, body: Arc<str>) {
        debug_assert_eq!(self.completed + 1, input.sequence);
        self.completed = input.sequence;
        self.pending_bytes -= input.text.len();
        self.reply_bytes += body.len();
        self.replies.push_back((input.sequence, body));
        while self.replies.len() > MAX_REPLIES || self.reply_bytes > MAX_REPLY_BYTES {
            self.reply_bytes -= self.replies.pop_front().unwrap().1.len();
        }
    }

    fn admit(&mut self, inputs: Vec<EditorInput>) -> Result<Vec<Arc<str>>> {
        // Validate the whole batch before modifying admission state.
        let mut next = self.admitted + 1;
        let mut bytes = self.pending_bytes;
        let mut replays = Vec::new();
        for input in &inputs {
            if let Some(body) = self.replay(input.sequence)? {
                replays.push(body);
                continue;
            }
            if input.sequence <= self.admitted {
                continue;
            }
            if input.sequence != next {
                return Err(conflict("editor sequence gap"));
            }
            next += 1;
            bytes += input.text.len();
        }
        if next - 1 - self.completed > MAX_PENDING_ITEMS || bytes > MAX_PENDING_BYTES {
            return Err(RequestError(
                429,
                "Too Many Requests",
                "editor input queue is full".into(),
            ));
        }
        for input in inputs {
            if input.sequence > self.admitted {
                self.admitted = input.sequence;
                self.queue.push_back(input);
            }
        }
        self.pending_bytes = bytes;
        Ok(replays)
    }
}

pub(crate) async fn handle_request(
    host: &InProcessWebHost,
    binding: &RequestBinding,
    actor_override: Option<Identity>,
    request: &HttpRequest,
    close: bool,
) -> Option<HttpResponse> {
    let path = request.path.split('?').next().unwrap_or(&request.path);
    if !matches!(
        path,
        "/editor/snapshot" | "/editor/input" | "/editor-client.js"
    ) {
        return None;
    }
    let result = route(host, binding, actor_override, request, path).await;
    let response = match result {
        Ok(response) => response,
        Err(RequestError(status, reason, message)) => {
            json_response(status, reason, json!({"error": message}))
        }
    };
    Some(response.with_header(
        "Connection",
        if close {
            b"close".as_slice()
        } else {
            b"keep-alive".as_slice()
        },
    ))
}

async fn route(
    host: &InProcessWebHost,
    binding: &RequestBinding,
    actor_override: Option<Identity>,
    request: &HttpRequest,
    path: &str,
) -> Result<HttpResponse> {
    if path == "/editor-client.js" && request.method == "GET" {
        return Ok(
            HttpResponse::new(200, "OK", include_bytes!("../editor-client.js").to_vec())
                .with_header("Content-Type", b"text/javascript; charset=utf-8")
                .with_header("Cache-Control", b"no-store"),
        );
    }
    let params = query_params(&request.path);
    let actor = actor_override.unwrap_or(binding.principal);
    if path == "/editor/snapshot" && request.method == "GET" {
        let id = query_number(&params, "session", None, 1, MAX_SAFE_INTEGER)?;
        let bounds = query_bounds(&params)?;
        let session = ensure_session(host, binding, id, actor_override)
            .await
            .map_err(session_error)?;
        return Ok(json_response(
            200,
            "OK",
            snapshot(&session, id, actor, bounds)
                .await
                .map_err(internal)?,
        ));
    }
    if path != "/editor/input" || request.method != "POST" {
        return Err(RequestError(
            405,
            "Method Not Allowed",
            "unsupported editor request".into(),
        ));
    }
    let json_content_type = request.headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("content-type")
            && std::str::from_utf8(&header.value).is_ok_and(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            })
    });
    if !json_content_type {
        return Err(RequestError(
            415,
            "Unsupported Media Type",
            "editor input requires application/json".into(),
        ));
    }
    if request.body.len() > MAX_BATCH_BYTES {
        return Err(RequestError(
            413,
            "Content Too Large",
            "editor batch is too large".into(),
        ));
    }
    let body: Json =
        serde_json::from_slice(&request.body).map_err(|error| bad(error.to_string()))?;
    if body.get("type").and_then(Json::as_str) == Some("editor_input") {
        let id = json_number(body.get("session"), None, 1)?;
        let inputs = batch_inputs(&body)?;
        let session = ensure_session(host, binding, id, actor_override)
            .await
            .map_err(session_error)?;
        let (replays, start) = {
            let mut state = session.editor.lock().unwrap();
            let replays = state.admit(inputs)?;
            let start = !state.running && !state.queue.is_empty();
            if start {
                state.running = true;
            }
            (replays, start)
        };
        for body in replays {
            session.send_editor(body);
        }
        if start {
            compio::runtime::spawn(drain_inputs(session, id, actor)).detach();
        }
        return Ok(json_response(202, "Accepted", json!({"accepted": true})));
    }
    let id = query_number(&params, "session", None, 1, MAX_SAFE_INTEGER)?;
    let sequence = query_number(&params, "sequence", None, 1, MAX_SAFE_INTEGER)?;
    if !body.is_object() || request.body.len() > MAX_ITEM_BYTES {
        return Err(bad("editor item must be an object of at most 64 KiB"));
    }
    let input = EditorInput {
        sequence,
        frame: query_number(&params, "frame", Some(1), 1, MAX_SAFE_INTEGER)?,
        generation: query_number(&params, "keymap_generation", Some(0), 0, MAX_SAFE_INTEGER)?,
        text: serde_json::to_string(&body).map_err(|error| bad(error.to_string()))?,
        bounds: query_bounds(&params)?,
    };
    let session = ensure_session(host, binding, id, actor_override)
        .await
        .map_err(session_error)?;
    {
        let mut state = session.editor.lock().unwrap();
        if let Some(body) = state.replay(sequence)? {
            return Ok(body_response(body));
        }
        if state.running || sequence != state.admitted + 1 {
            return Err(conflict("editor input is pending or out of order"));
        }
        state.admitted = sequence;
        state.pending_bytes = input.text.len();
        state.running = true;
    }
    // The detached worker owns completion even if the HTTP request is dropped.
    // Synchronous requests use the same ordered queue and completion cache.
    let pending = Arc::new(PendingReply::default());
    let worker_reply = pending.clone();
    compio::runtime::spawn(async move {
        let body = execute_input(&session, id, actor, &input).await;
        {
            let mut state = session.editor.lock().unwrap();
            state.complete(&input, body.clone());
        }
        worker_reply.complete(body);
        drain_inputs(session, id, actor).await;
    })
    .detach();
    let body = pending.wait().await;
    Ok(body_response(body))
}

fn batch_inputs(body: &Json) -> Result<Vec<EditorInput>> {
    let items = body
        .get("items")
        .and_then(Json::as_array)
        .ok_or_else(|| bad("missing editor items"))?;
    if items.is_empty() || items.len() > 256 {
        return Err(bad("editor batch needs 1..=256 items"));
    }
    let mut inputs = Vec::with_capacity(items.len());
    let mut previous = None;
    for item in items {
        if !item.is_object() {
            return Err(bad("editor item must be an object"));
        }
        let sequence = json_number(item.get("sequence"), None, 1)?;
        if previous.is_some_and(|previous| sequence != previous + 1)
            || json_number(item.get("depends_on"), None, 0)? != sequence - 1
        {
            return Err(conflict("editor items must have contiguous dependencies"));
        }
        let text = serde_json::to_string(item).map_err(|error| bad(error.to_string()))?;
        if text.len() > MAX_ITEM_BYTES {
            return Err(bad("editor item exceeds 64 KiB"));
        }
        inputs.push(EditorInput {
            sequence,
            frame: json_number(item.get("frame"), Some(1), 1)?,
            generation: json_number(item.get("keymap_generation"), Some(0), 0)?,
            text,
            bounds: SnapshotBounds::default(),
        });
        previous = Some(sequence);
    }
    Ok(inputs)
}

async fn drain_inputs(session: Arc<SyncSession>, id: u64, actor: Identity) {
    loop {
        let input = {
            let mut state = session.editor.lock().unwrap();
            let Some(input) = state.queue.pop_front() else {
                state.running = false;
                return;
            };
            input
        };
        let body = execute_input(&session, id, actor, &input).await;
        session
            .editor
            .lock()
            .unwrap()
            .complete(&input, body.clone());
        session.send_editor(body);
    }
}

async fn execute_input(
    session: &SyncSession,
    id: u64,
    actor: Identity,
    input: &EditorInput,
) -> Arc<str> {
    let token = NEXT_APPLY_TOKEN.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
        (token < MAX_SAFE_INTEGER).then_some(token + 1)
    });
    let result = match token {
        Ok(token) => {
            invoke_json(
                session,
                "editor/input_response_json",
                vec![
                    role("endpoint", Value::identity(session.endpoint.endpoint())),
                    role("session", integer(id)),
                    role("actor", Value::identity(actor)),
                    role("frame", integer(input.frame)),
                    role("text", Value::string(&input.text)),
                    role("client_token", integer(token)),
                    role("known_keymap_generation", integer(input.generation)),
                ],
            )
            .await
        }
        Err(_) => Err("editor completion token space exhausted".to_owned()),
    };
    let result = result.unwrap_or_else(
        |error| json!({"status": "resync", "snapshot_required": true, "error": error}),
    );
    let mut body = json!({"through_sequence": input.sequence, "result": result});
    if body["result"]["snapshot_required"] == true || body["result"]["status"] == "resync" {
        match snapshot(session, id, actor, input.bounds).await {
            Ok(snapshot) => body["snapshot"] = snapshot,
            Err(error) => body["snapshot_error"] = Json::String(error),
        }
    }
    let body = body.to_string();
    if body.len() > MAX_REPLY_BYTES {
        return Arc::from(json!({"through_sequence": input.sequence, "result": {"status": "resync", "error": "editor reply exceeds 8 MiB"}}).to_string());
    }
    Arc::from(body)
}

async fn snapshot(
    session: &SyncSession,
    id: u64,
    actor: Identity,
    bounds: SnapshotBounds,
) -> std::result::Result<Json, String> {
    invoke_json(
        session,
        "editor/snapshot_json",
        vec![
            role("session", integer(id)),
            role("actor", Value::identity(actor)),
            role("lines", integer(bounds.lines)),
            role("max_scalars", integer(bounds.scalars)),
        ],
    )
    .await
}

async fn invoke_json(
    session: &SyncSession,
    selector: &str,
    roles: Vec<(Symbol, Value)>,
) -> std::result::Result<Json, String> {
    let invocation = session
        .endpoint
        .invoke(Symbol::intern(selector), roles)
        .await
        .map_err(|error| error.to_string())?;
    match invocation.wait().await {
        InvocationOutcome::Completed(value) => value
            .with_str(|text| {
                if text.len() > MAX_REPLY_BYTES {
                    return Err("editor reply exceeds 8 MiB".into());
                }
                serde_json::from_str(text).map_err(|error| format!("invalid editor reply: {error}"))
            })
            .unwrap_or_else(|| Err("editor verb did not return JSON text".into())),
        InvocationOutcome::Aborted(error) => Err(format!("editor task aborted: {error:?}")),
        InvocationOutcome::Cancelled(reason) => Err(format!("editor task cancelled: {reason:?}")),
        InvocationOutcome::Failed(error) => Err(error),
    }
}

fn role(name: &str, value: Value) -> (Symbol, Value) {
    (Symbol::intern(name), value)
}
fn integer(value: u64) -> Value {
    Value::int(value as i64).expect("validated JavaScript-safe integer")
}
fn internal(message: String) -> RequestError {
    RequestError(500, "Internal Server Error", message)
}
fn session_error(message: String) -> RequestError {
    if message == "session belongs to a different actor" {
        RequestError(403, "Forbidden", message)
    } else {
        internal(message)
    }
}
fn body_response(body: Arc<str>) -> HttpResponse {
    HttpResponse::new(200, "OK", body.as_bytes().to_vec())
        .with_header("Content-Type", b"application/json; charset=utf-8")
        .with_header("Cache-Control", b"no-store")
}
fn json_response(status: u16, reason: &str, value: Json) -> HttpResponse {
    HttpResponse::new(status, reason, value.to_string().into_bytes())
        .with_header("Content-Type", b"application/json; charset=utf-8")
        .with_header("Cache-Control", b"no-store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::HttpHeader;
    use crate::request::handle_in_process_request;
    use compio::runtime::Runtime;
    use mica_driver::{DriverEventPumpTask, DriverOwner, DriverResources};
    use mica_runtime::{SourceRunner, TaskOutcome};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::num::NonZeroUsize;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn test_host(
        interpreter_only: bool,
    ) -> (
        DriverOwner,
        DriverEventPumpTask,
        InProcessWebHost,
        RequestBinding,
    ) {
        let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        for source in [
            include_str!("../../../apps/shared/sync-host.mica"),
            include_str!("../../../apps/shared/buffers.mica"),
            include_str!("../../../apps/editor/schema.mica"),
            include_str!("../../../apps/editor/windows.mica"),
            include_str!("../../../apps/editor/buffers.mica"),
            include_str!("../../../apps/editor/keymaps.mica"),
            include_str!("../../../apps/editor/undo.mica"),
            include_str!("../../../apps/editor/commands.mica"),
            include_str!("../../../apps/editor/session.mica"),
            include_str!("../../../apps/editor/picker.mica"),
            include_str!("../../../apps/editor/minibuffer.mica"),
            include_str!("../../../apps/editor/files.mica"),
            include_str!("../../../apps/editor/ui.mica"),
            include_str!("../../../apps/editor/defaults.mica"),
            include_str!("../../../apps/editor/http.mica"),
            include_str!("../../../apps/editor/host-policy.mica"),
            "make_identity(:alice)\nassert HasRole(#alice, #editor/user)",
        ] {
            for report in runner.run_filein(source).unwrap() {
                assert!(
                    matches!(report.outcome, TaskOutcome::Complete { .. }),
                    "{}",
                    report.render()
                );
            }
        }
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let mut owner = DriverOwner::builder(DriverResources::new(NonZeroUsize::new(1).unwrap()))
            .source_runner(runner)
            .build()
            .unwrap();
        let events = owner.event_router();
        let pump = owner
            .take_event_pump()
            .unwrap()
            .spawn_router(events.clone());
        let host = InProcessWebHost::new(owner.client(), &events);
        (
            owner,
            pump,
            host,
            RequestBinding {
                principal: web,
                actor: None,
            },
        )
    }

    fn request(path: &str, body: Option<Json>) -> HttpRequest {
        HttpRequest {
            method: if body.is_some() { "POST" } else { "GET" }.into(),
            path: path.into(),
            version: 1,
            headers: vec![HttpHeader::new("Content-Type", b"application/json")],
            body: body
                .map(|body| body.to_string().into_bytes())
                .unwrap_or_default(),
        }
    }

    async fn send(
        host: &InProcessWebHost,
        binding: &RequestBinding,
        path: &str,
        body: Option<Json>,
        expected: u16,
    ) -> Json {
        let response = handle_in_process_request(host, binding, &request(path, body), false).await;
        assert_eq!(
            response.status,
            expected,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        serde_json::from_slice(&response.body).unwrap()
    }

    async fn completion(session: &SyncSession, sequence: u64) -> Arc<str> {
        compio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(body) = session.editor.lock().unwrap().replay(sequence).unwrap() {
                    return body;
                }
                compio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("editor input completed")
    }

    #[test]
    fn ordinary_actor_edits_replays_and_keeps_session_ownership() {
        for interpreter_only in [true, false] {
            Runtime::new().unwrap().block_on(async {
                let (_owner, _pump, host, binding) = test_host(interpreter_only);
                let first = send(&host, &binding, "/editor/snapshot?session=7", None, 200).await;
                assert_eq!(first["rows"][0]["text"], "");
                let edit = json!({"kind": "text", "text": "é🦀"});
                let reply = send(
                    &host,
                    &binding,
                    "/editor/input?session=7&sequence=1",
                    Some(edit.clone()),
                    200,
                )
                .await;
                assert_eq!(reply["through_sequence"], 1);
                assert_eq!(reply["result"]["status"], "ok", "{reply}");
                let replay = send(
                    &host,
                    &binding,
                    "/editor/input?session=7&sequence=1",
                    Some(edit.clone()),
                    200,
                )
                .await;
                assert_eq!(reply, replay);
                send(
                    &host,
                    &binding,
                    "/editor/input?session=7&sequence=3",
                    Some(edit),
                    409,
                )
                .await;
                let snapshot = send(
                    &host,
                    &binding,
                    "/editor/snapshot?session=7&lines=1&max=1",
                    None,
                    200,
                )
                .await;
                assert_eq!(snapshot["rows"][0]["text"], "é");
                assert_eq!(snapshot["point"], 2);
                let alice = host.client.named_identity(Symbol::intern("alice")).unwrap();
                let other = RequestBinding {
                    principal: binding.principal,
                    actor: Some(alice),
                };
                send(&host, &other, "/editor/snapshot?session=7", None, 403).await;
                // The ownership check also applies when neither endpoint has an actor override.
                let other_principal = RequestBinding {
                    principal: alice,
                    actor: None,
                };
                send(
                    &host,
                    &other_principal,
                    "/editor/snapshot?session=7",
                    None,
                    403,
                )
                .await;
                send(&host, &other, "/editor/snapshot?session=8", None, 200).await;
                let other_edit = send(
                    &host,
                    &other,
                    "/editor/input?session=8&sequence=1",
                    Some(json!({"kind":"text", "text":"other"})),
                    200,
                )
                .await;
                assert_eq!(other_edit["result"]["status"], "ok", "{other_edit}");
                send(&host, &binding, "/editor/snapshot?session=8", None, 403).await;
                let page =
                    handle_in_process_request(&host, &binding, &request("/editor", None), false)
                        .await;
                assert_eq!(page.status, 200, "{}", String::from_utf8_lossy(&page.body));
                let script = handle_in_process_request(
                    &host,
                    &binding,
                    &request("/editor-client.js", None),
                    false,
                )
                .await;
                assert_eq!(script.status, 200);
                assert_eq!(script.body, include_bytes!("../editor-client.js"));
            });
        }
    }

    #[test]
    fn asynchronous_batches_validate_atomically_and_replay_completed_inputs() {
        Runtime::new().unwrap().block_on(async {
            let (_owner, _pump, host, binding) = test_host(false);
            let batch = json!({"type":"editor_input", "session":"9", "items":[
                {"sequence":"1", "depends_on":"0", "kind":"text", "text":"a"},
                {"sequence":"2", "depends_on":"1", "kind":"text", "text":"🦀"}
            ]});
            let mut invalid = batch.clone();
            invalid["items"][1]["depends_on"] = json!(0);
            send(&host, &binding, "/editor/input", Some(invalid), 409).await;
            send(&host, &binding, "/editor/input", Some(batch.clone()), 202).await;
            let session = ensure_session(&host, &binding, 9, None).await.unwrap();
            let reply = completion(&session, 2).await;
            let parsed: Json = serde_json::from_str(&reply).unwrap();
            assert_eq!(parsed["result"]["status"], "ok", "{parsed}");
            send(&host, &binding, "/editor/input", Some(batch), 202).await;
            assert_eq!(completion(&session, 2).await, reply);
            let snapshot = send(&host, &binding, "/editor/snapshot?session=9", None, 200).await;
            assert_eq!(snapshot["rows"][0]["text"], "a🦀");
            assert_eq!(snapshot["point"], 2);
            let bad_shape = request(
                "/editor/input",
                Some(json!({"type":"editor_input", "session":9, "items":[]})),
            );
            assert_eq!(
                handle_in_process_request(&host, &binding, &bad_shape, false)
                    .await
                    .status,
                400
            );
            let mut wrong_type = request(
                "/editor/input?session=9&sequence=3",
                Some(json!({"kind":"text", "text":"!"})),
            );
            wrong_type.headers.clear();
            assert_eq!(
                handle_in_process_request(&host, &binding, &wrong_type, false)
                    .await
                    .status,
                415
            );
            send(&host, &binding, "/editor/snapshot?session=0", None, 400).await;
            send(
                &host,
                &binding,
                "/editor/snapshot?session=9&max=0",
                None,
                400,
            )
            .await;
        });
    }

    #[test]
    fn admission_limits_and_expired_replays_leave_sequences_unchanged() {
        let input = |sequence, bytes| EditorInput {
            sequence,
            frame: 1,
            generation: 0,
            text: "x".repeat(bytes),
            bounds: SnapshotBounds::default(),
        };
        let mut state = EditorState::default();
        let items = (1..=17)
            .map(|sequence| input(sequence, MAX_ITEM_BYTES))
            .collect();
        assert_eq!(state.admit(items).unwrap_err().0, 429);
        assert_eq!(state.admitted, 0);
        assert_eq!(state.pending_bytes, 0);
        assert!(state.queue.is_empty());
        state.admit(vec![input(1, 1)]).unwrap();
        let item = state.queue.pop_front().unwrap();
        state.complete(&item, Arc::from("first"));
        for sequence in 2..=257 {
            state.admit(vec![input(sequence, 1)]).unwrap();
            let item = state.queue.pop_front().unwrap();
            state.complete(&item, Arc::from("reply"));
        }
        assert_eq!(state.replay(1).unwrap_err().0, 410);
        assert_eq!(state.replies.len(), MAX_REPLIES);
        assert_eq!(
            state.admit(vec![input(1, 1), input(258, 1)]).unwrap_err().0,
            410
        );
        assert_eq!(state.admitted, 257);
        assert!(state.queue.is_empty());
    }
    fn socket(addr: SocketAddr) -> TcpStream {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }

    fn stream_events(addr: SocketAddr) -> BufReader<TcpStream> {
        let mut stream = socket(addr);
        stream
            .write_all(b"GET /sync/events?session=73 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("200"), "{line}");
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        reader
    }

    fn editor_event(reader: &mut BufReader<TcpStream>) -> Json {
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            let length = usize::from_str_radix(line.trim(), 16).unwrap();
            assert!(length > 0);
            let mut chunk = vec![0; length + 2];
            reader.read_exact(&mut chunk).unwrap();
            let text = std::str::from_utf8(&chunk[..length]).unwrap();
            if let Some(body) = text.strip_prefix("event: editor\ndata: ") {
                return serde_json::from_str(body.trim()).unwrap();
            }
        }
    }

    fn post(addr: SocketAddr, body: &Json) {
        let mut stream = socket(addr);
        let body = body.to_string();
        write!(stream, "POST /editor/input HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
    }

    #[test]
    fn sse_reconnection_replays_completed_edits_without_running_them_again() {
        let (address_tx, address_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            Runtime::new().unwrap().block_on(async {
                let (_owner, _pump, host, binding) = test_host(false);
                let listener =
                    compio::net::TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
                        .await
                        .unwrap();
                address_tx.send(listener.local_addr().unwrap()).unwrap();
                compio::runtime::spawn(crate::server::serve_in_process(
                    listener, host, binding, None,
                ))
                .detach();
                while stop_rx.try_recv().is_err() {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        });
        let addr = address_rx.recv().unwrap();
        let result = std::panic::catch_unwind(|| {
            let body = json!({"type":"editor_input", "session":"73", "items":[
                {"sequence":"1", "depends_on":"0", "kind":"text", "text":"é🦀"}
            ]});
            let mut first = stream_events(addr);
            post(addr, &body);
            let reply = editor_event(&mut first);
            assert_eq!(reply["result"]["status"], "ok", "{reply}");
            drop(first);
            let mut reconnected = stream_events(addr);
            post(addr, &body);
            assert_eq!(editor_event(&mut reconnected), reply);
            let next = json!({"type":"editor_input", "session":"73", "items":[
                {"sequence":"2", "depends_on":"1", "kind":"text", "text":"!"}
            ]});
            post(addr, &next);
            let second = editor_event(&mut reconnected);
            assert_eq!(second["through_sequence"], 2);
            assert_eq!(second["result"]["status"], "ok", "{second}");
            let mut snapshot = socket(addr);
            snapshot.write_all(b"GET /editor/snapshot?session=73 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
            let mut response = String::new();
            snapshot.read_to_string(&mut response).unwrap();
            let (_, body) = response.split_once("\r\n\r\n").unwrap();
            let snapshot: Json = serde_json::from_str(body).unwrap();
            assert_eq!(snapshot["rows"][0]["text"], "é🦀!");
        });
        stop_tx.send(()).unwrap();
        server.join().unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
    }
}
