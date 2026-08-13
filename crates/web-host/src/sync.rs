// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::codec::{HttpRequest, HttpResponse};
use crate::metrics::{SyncEnvelopeKind, SyncRenderPhase};
use crate::response::internal_error_response;
use crate::{InProcessWebHost, RequestBinding, format_driver_error};
use compio::io::AsyncWriteExt;
use compio::net::TcpStream;
use mica_driver::{
    CompioTaskDriver, DriverEvent, DriverSubscriptionMailbox, DriverSubscriptionRequest,
};
use mica_host_protocol::{
    DomEventPayload, DomNode, SyncEnvelope, SyncMessageKind, SyncViewDependencySubject,
    SyncViewRelation, decode_dom_event_payload, decode_sync_envelope,
    decode_sync_view_dependencies, diff_dom_nodes, dom_patch_payload_json,
    sync_envelope_from_value, sync_payload_signature, sync_u64_value,
};
use mica_runtime::{SubscriptionInitialDelivery, SubscriptionSubject, TaskId, TaskOutcome};
use mica_var::{CapabilityId, Identity, Symbol, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

const ENDPOINT_OUTPUT_HIGH_WATER_MESSAGES: usize = 128;
const ENDPOINT_OUTPUT_DRAIN_MESSAGES: usize = 64;
const SYNC_EVENTS_PATH: &str = "/sync/events";
const SYNC_INPUT_PATH: &str = "/sync/input";

pub(crate) struct InProcessSyncHost {
    driver: Arc<CompioTaskDriver>,
    sessions: Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    subscription_mailbox: Arc<DriverSubscriptionMailbox>,
    subscription_views: Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    stop_events: Arc<AtomicBool>,
}

#[derive(Debug)]
struct SyncSession {
    session_id: u64,
    endpoint: Identity,
    actor: Option<Identity>,
    output: Arc<SessionOutput>,
    sync: Mutex<SessionSyncState>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SessionSyncState {
    views: HashMap<u64, ActiveViewState>,
    pending_tasks: HashMap<TaskId, PendingSyncTask>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ActiveViewState {
    client_revision: u64,
    client_signature: u64,
    server_revision: u64,
    server_signature: u64,
    last_tree: Option<DomNode>,
    subscriptions: Vec<Value>,
    subscriptions_initialized: bool,
    subscriptions_initializing: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingSyncTask {
    view_id: u64,
    refresh: bool,
    action: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SyncViewKey {
    session_id: u64,
    view_id: u64,
}

#[derive(Default, Debug)]
struct SessionOutput {
    state: Mutex<SessionOutputState>,
}

#[derive(Default, Debug)]
struct SessionOutputState {
    messages: VecDeque<SessionOutputMessage>,
    closed: bool,
    writer_generation: u64,
    waker: Option<Waker>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionOutputMessage {
    SyncEnvelope(SyncEnvelope),
}

struct SessionOutputRecv<'a> {
    output: &'a SessionOutput,
    writer_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveSyncView {
    endpoint: Identity,
    session_id: u64,
    view_id: u64,
    client_revision: u64,
    client_signature: u64,
    server_revision: u64,
    server_signature: u64,
    last_tree: Option<DomNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenderedSyncView {
    revision: u64,
    signature: u64,
    tree: DomNode,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionOutputReady {
    Ready { buffered: usize },
    HighWater { buffered: usize },
    Replaced,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SyncRequestKind {
    EventStream,
    Input,
}

impl InProcessSyncHost {
    pub(crate) fn new(driver: Arc<CompioTaskDriver>) -> Self {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let subscription_mailbox = Arc::new(
            driver
                .create_subscription_mailbox()
                .expect("HTTP sync subscription mailbox creation must succeed"),
        );
        let subscription_views = Arc::new(Mutex::new(HashMap::new()));
        let stop_events = Arc::new(AtomicBool::new(false));
        start_event_pump(
            driver.clone(),
            sessions.clone(),
            subscription_mailbox.clone(),
            subscription_views.clone(),
            stop_events.clone(),
        );
        Self {
            driver,
            sessions,
            subscription_mailbox,
            subscription_views,
            stop_events,
        }
    }
}

impl Drop for InProcessSyncHost {
    fn drop(&mut self) {
        self.stop_events.store(true, Ordering::Relaxed);
        for session in self.sessions.lock().unwrap().values() {
            session.output.close();
            let subscriptions = session
                .sync
                .lock()
                .unwrap()
                .views
                .values_mut()
                .flat_map(|view| std::mem::take(&mut view.subscriptions))
                .collect();
            cancel_subscriptions(&self.driver, subscriptions);
        }
    }
}

impl SyncSession {
    fn new(session_id: u64, endpoint: Identity, actor: Option<Identity>) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            endpoint,
            actor,
            output: SessionOutput::new(),
            sync: Mutex::new(SessionSyncState::default()),
        })
    }
}

impl SessionSyncState {
    fn record_incoming_view(&mut self, envelope: &SyncEnvelope) {
        if !matches!(
            envelope.kind,
            SyncMessageKind::NeedView | SyncMessageKind::HaveView
        ) {
            return;
        }
        let view = self.views.entry(envelope.view_id).or_default();
        view.client_revision = envelope.client_revision;
        view.client_signature = envelope.client_signature;
    }

    fn store_rendered_view(&mut self, view_id: u64, revision: u64, signature: u64, tree: DomNode) {
        let view = self.views.entry(view_id).or_default();
        view.client_revision = revision;
        view.client_signature = signature;
        view.server_revision = revision;
        view.server_signature = signature;
        view.last_tree = Some(tree);
    }
}

impl SessionOutput {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn claim_writer(&self) -> u64 {
        let waker = {
            let mut state = self.state.lock().unwrap();
            state.writer_generation = state.writer_generation.saturating_add(1).max(1);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        self.state.lock().unwrap().writer_generation
    }

    fn send_sync_envelope(&self, envelope: SyncEnvelope) -> Result<(), String> {
        let waker = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err("session writer is closed".to_owned());
            }
            if envelope.kind == SyncMessageKind::ViewSnapshot {
                state.messages.retain(|message| match message {
                    SessionOutputMessage::SyncEnvelope(queued) => {
                        queued.session_id != envelope.session_id
                            || queued.view_id != envelope.view_id
                    }
                });
            }
            state
                .messages
                .push_back(SessionOutputMessage::SyncEnvelope(envelope));
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    fn has_pending_view_sync(&self, session_id: u64, view_id: u64) -> bool {
        self.state.lock().unwrap().messages.iter().any(|message| {
            matches!(
                message,
                SessionOutputMessage::SyncEnvelope(envelope)
                    if envelope.session_id == session_id
                        && envelope.view_id == view_id
                        && envelope.kind == SyncMessageKind::ViewSnapshot
            )
        })
    }

    fn close(&self) {
        let waker = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn recv(&self, writer_generation: u64) -> SessionOutputRecv<'_> {
        SessionOutputRecv {
            output: self,
            writer_generation,
        }
    }

    fn drain_batch(&self, max_messages: usize) -> Vec<SessionOutputMessage> {
        let mut state = self.state.lock().unwrap();
        let count = max_messages.min(state.messages.len());
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(message) = state.messages.pop_front() else {
                break;
            };
            messages.push(message);
        }
        messages
    }
}

impl Future for SessionOutputRecv<'_> {
    type Output = SessionOutputReady;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.output.state.lock().unwrap();
        if state.writer_generation != self.writer_generation {
            return Poll::Ready(SessionOutputReady::Replaced);
        }
        if state.messages.len() >= ENDPOINT_OUTPUT_HIGH_WATER_MESSAGES {
            return Poll::Ready(SessionOutputReady::HighWater {
                buffered: state.messages.len(),
            });
        }
        if !state.messages.is_empty() {
            return Poll::Ready(SessionOutputReady::Ready {
                buffered: state.messages.len(),
            });
        }
        if state.closed {
            return Poll::Ready(SessionOutputReady::Closed);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub(crate) fn request_kind(request: &HttpRequest) -> Option<SyncRequestKind> {
    if request.method == "GET" && request_path_without_query(&request.path) == SYNC_EVENTS_PATH {
        return Some(SyncRequestKind::EventStream);
    }
    if request.method == "POST" && request_path_without_query(&request.path) == SYNC_INPUT_PATH {
        return Some(SyncRequestKind::Input);
    }
    None
}

pub(crate) async fn handle_sync_input_request(
    host: &InProcessWebHost,
    binding: &RequestBinding,
    request: &HttpRequest,
    close: bool,
) -> HttpResponse {
    let actor_override = if let Some(auth) = &host.auth {
        let cookie_header = request
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("cookie"))
            .map(|h| std::str::from_utf8(&h.value).unwrap_or(""));

        match auth.resolve_auth_context(cookie_header).await {
            Ok(Some(ctx)) => match host.driver.named_identity(Symbol::intern(&ctx.actor_name)) {
                Ok(actor) => Some(actor),
                Err(error) => {
                    tracing::warn!(
                        actor_name = %ctx.actor_name,
                        error = %error,
                        "failed to resolve authenticated actor for sync"
                    );
                    return internal_error_response("Failed to resolve user identity", close);
                }
            },
            Ok(None) => {
                return HttpResponse::new(401, "Unauthorized", b"Authentication required".to_vec());
            }
            Err(error) => {
                tracing::warn!(error = %error, "sync authentication failed");
                return HttpResponse::new(
                    401,
                    "Unauthorized",
                    b"Invalid or expired session".to_vec(),
                );
            }
        }
    } else {
        binding.actor
    };

    match sync_envelope_from_request(&request.body) {
        Ok(SyncRequestEnvelope::Sync(envelope)) => {
            let session_id = envelope.session_id;
            let view_id = envelope.view_id;
            let kind = envelope.kind;
            let session = match ensure_session(host, binding, envelope.session_id, actor_override) {
                Ok(session) => session,
                Err(error) => {
                    tracing::error!(
                        target: "mica_web_host::sync",
                        session_id,
                        view_id,
                        kind = ?kind,
                        error = %error,
                        "sync input failed"
                    );
                    return internal_error_response(error, close);
                }
            };
            if let Err(error) = route_sync_envelope(host, &session, envelope).await {
                tracing::error!(
                    target: "mica_web_host::sync",
                    session_id,
                    view_id,
                    kind = ?kind,
                    error = %error,
                    "sync input failed"
                );
                return internal_error_response(error, close);
            }
        }
        Ok(SyncRequestEnvelope::DomEvent(event)) => {
            let session_id = event.session_id;
            let view_id = event.view_id;
            let event_name = event.event.clone();
            let action = event.action.clone();
            let target = event.target.clone();
            let session = match ensure_session(host, binding, event.session_id, actor_override) {
                Ok(session) => session,
                Err(error) => {
                    tracing::error!(
                        target: "mica_web_host::sync",
                        session_id,
                        view_id,
                        event = %event_name,
                        action = %action,
                        target = %target,
                        error = %error,
                        "sync DOM input failed"
                    );
                    return internal_error_response(error, close);
                }
            };
            if let Err(error) = route_dom_event(host, &session, event).await {
                tracing::error!(
                    target: "mica_web_host::sync",
                    session_id,
                    view_id,
                    event = %event_name,
                    action = %action,
                    target = %target,
                    error = %error,
                    "sync DOM input failed"
                );
                return internal_error_response(error, close);
            }
        }
        Err(error) => {
            return with_connection_header(
                HttpResponse::text(400, "Bad Request", format!("invalid sync input: {error}\n")),
                close,
            );
        }
    }
    with_connection_header(HttpResponse::new(202, "Accepted", Vec::new()), close)
}

pub(crate) async fn serve_event_stream(
    mut stream: TcpStream,
    host: Arc<InProcessWebHost>,
    binding: RequestBinding,
    request: &HttpRequest,
) -> Result<(), String> {
    let actor_override = if let Some(auth) = &host.auth {
        let cookie_header = request
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("cookie"))
            .map(|h| std::str::from_utf8(&h.value).unwrap_or(""));

        match auth.resolve_auth_context(cookie_header).await {
            Ok(Some(ctx)) => match host.driver.named_identity(Symbol::intern(&ctx.actor_name)) {
                Ok(actor) => Some(actor),
                Err(error) => {
                    tracing::warn!(
                        actor_name = %ctx.actor_name,
                        error = %error,
                        "failed to resolve authenticated actor for event stream"
                    );
                    return Err("Failed to resolve user identity".to_string());
                }
            },
            Ok(None) => {
                write_event_stream_error_response(
                    &mut stream,
                    HttpResponse::new(401, "Unauthorized", b"Authentication required".to_vec()),
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(error = %error, "event stream authentication failed");
                write_event_stream_error_response(
                    &mut stream,
                    HttpResponse::new(401, "Unauthorized", b"Invalid or expired session".to_vec()),
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        binding.actor
    };

    let session_id = session_id_from_stream_request(request)?;
    let session = ensure_session(&host, &binding, session_id, actor_override)?;
    submit_optional_sync_lifecycle(&host, &session, "sync_stream_opened").await?;
    write_event_stream_headers(&mut stream).await?;
    write_event_chunk(&mut stream, b": connected\n\n").await?;
    let result = write_event_stream_loop(&mut stream, session.output.clone()).await;
    submit_optional_sync_lifecycle(&host, &session, "sync_stream_closed").await?;
    result
}

async fn write_event_stream_error_response(
    stream: &mut TcpStream,
    response: HttpResponse,
) -> Result<(), String> {
    let mut bytes = Vec::new();
    crate::codec::encode_response(&response, &mut bytes)
        .map_err(|error| format!("failed to encode sync event stream error response: {error}"))?;
    let (result, _) = stream.write_all(bytes).await.into();
    result.map_err(|error| format!("failed to write sync event stream error response: {error}"))
}

fn session_id_from_stream_request(request: &HttpRequest) -> Result<u64, String> {
    query_u64(&request.path, "session")
        .ok_or_else(|| "sync event stream requires ?session=<u64>".to_owned())
}

fn sync_envelope_from_request(body: &[u8]) -> Result<SyncRequestEnvelope, String> {
    if let Ok(envelope) = decode_sync_envelope(body) {
        return Ok(SyncRequestEnvelope::Sync(envelope));
    }
    if let Some(event) = decode_dom_event_payload(body)? {
        return Ok(SyncRequestEnvelope::DomEvent(event));
    }
    Err("body was neither a sync envelope nor a DOM event payload".to_owned())
}

enum SyncRequestEnvelope {
    Sync(SyncEnvelope),
    DomEvent(DomEventPayload),
}

async fn ensure_view_subscriptions(
    host: &InProcessWebHost,
    session: &Arc<SyncSession>,
    view_id: u64,
) -> Result<(), String> {
    let should_initialize = {
        let mut sync = session.sync.lock().unwrap();
        let view = sync.views.entry(view_id).or_default();
        if view.subscriptions_initializing || view.subscriptions_initialized {
            false
        } else {
            view.subscriptions_initializing = true;
            true
        }
    };
    if !should_initialize {
        return Ok(());
    }

    let result = register_view_subscriptions(
        &host.driver,
        &host.sync.subscription_mailbox,
        &host.sync.subscription_views,
        session,
        view_id,
    )
    .await;
    if result.is_err() {
        session
            .sync
            .lock()
            .unwrap()
            .views
            .entry(view_id)
            .or_default()
            .subscriptions_initialized = false;
        session
            .sync
            .lock()
            .unwrap()
            .views
            .entry(view_id)
            .or_default()
            .subscriptions_initializing = false;
    }
    result
}

async fn register_view_subscriptions(
    driver: &CompioTaskDriver,
    subscription_mailbox: &DriverSubscriptionMailbox,
    subscription_views: &Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    session: &Arc<SyncSession>,
    view_id: u64,
) -> Result<(), String> {
    let value = submit_sync_invocation_for(
        driver,
        session.endpoint,
        "sync_view_dependencies",
        vec![(Symbol::intern("view"), sync_u64_value(view_id))],
    )
    .await?;
    let dependencies = decode_sync_view_dependencies(&value)?;
    let mut subscriptions = Vec::with_capacity(dependencies.len());
    for dependency in dependencies {
        let (relation, arity) = match dependency.relation {
            SyncViewRelation::Identity(relation) => {
                let arity = dependency.bindings.len() as u16;
                (relation, arity)
            }
            SyncViewRelation::Name(name) => driver
                .named_relation(name)
                .map_err(|error| driver.format_error(&error))?,
        };
        if usize::from(arity) != dependency.bindings.len() {
            cancel_subscriptions(driver, subscriptions);
            return Err(format!(
                "sync_view_dependencies binding count for relation {} was {}, expected {arity}",
                driver.format_value(&Value::identity(relation)),
                dependency.bindings.len(),
            ));
        }
        let subject = match dependency.subject {
            SyncViewDependencySubject::Facts => SubscriptionSubject::Facts {
                relation,
                bindings: dependency.bindings,
            },
            SyncViewDependencySubject::Relation => SubscriptionSubject::Relation {
                relation,
                bindings: dependency.bindings,
            },
        };
        match driver
            .register_subscription_for_endpoint(
                session.endpoint,
                subscription_mailbox,
                DriverSubscriptionRequest {
                    subject,
                    initial_delivery: SubscriptionInitialDelivery::ChangesOnly,
                    cursor: None,
                    queue_budget: 64,
                },
            )
            .await
        {
            Ok(subscription) => subscriptions.push(subscription),
            Err(error) => {
                cancel_subscriptions(driver, subscriptions);
                return Err(driver.format_error(&error));
            }
        }
    }

    let key = SyncViewKey {
        session_id: session.session_id,
        view_id,
    };
    {
        let mut subscription_views = subscription_views.lock().unwrap();
        for subscription in &subscriptions {
            if let Some(capability) = subscription.as_capability() {
                subscription_views.insert(capability, key);
            }
        }
    }
    let mut sync = session.sync.lock().unwrap();
    let view = sync.views.entry(view_id).or_default();
    view.subscriptions = subscriptions;
    view.subscriptions_initialized = true;
    view.subscriptions_initializing = false;
    Ok(())
}

fn cancel_subscriptions(driver: &CompioTaskDriver, subscriptions: Vec<Value>) {
    for subscription in subscriptions {
        let _ = driver.cancel_subscription(subscription);
    }
}

async fn reinstall_view_subscriptions(
    driver: &CompioTaskDriver,
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    subscription_mailbox: &DriverSubscriptionMailbox,
    subscription_views: &Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    key: SyncViewKey,
) -> Result<(), String> {
    let Some(session) = sessions.lock().unwrap().get(&key.session_id).cloned() else {
        return Ok(());
    };
    let subscriptions = {
        let mut sync = session.sync.lock().unwrap();
        let view = sync.views.entry(key.view_id).or_default();
        view.subscriptions_initialized = false;
        view.subscriptions_initializing = true;
        std::mem::take(&mut view.subscriptions)
    };
    {
        let mut views = subscription_views.lock().unwrap();
        for subscription in &subscriptions {
            if let Some(capability) = subscription.as_capability() {
                views.remove(&capability);
            }
        }
    }
    cancel_subscriptions(driver, subscriptions);
    let result = register_view_subscriptions(
        driver,
        subscription_mailbox,
        subscription_views,
        &session,
        key.view_id,
    )
    .await;
    if result.is_err() {
        let mut sync = session.sync.lock().unwrap();
        let view = sync.views.entry(key.view_id).or_default();
        view.subscriptions_initializing = false;
        view.subscriptions_initialized = false;
    }
    result
}

fn ensure_session(
    host: &InProcessWebHost,
    binding: &RequestBinding,
    session_id: u64,
    actor_override: Option<Identity>,
) -> Result<Arc<SyncSession>, String> {
    let effective_actor = actor_override.or(binding.actor);
    if let Some(session) = host.sync.sessions.lock().unwrap().get(&session_id).cloned() {
        if effective_actor.is_some() && session.actor != effective_actor {
            return Err("session belongs to a different actor".to_owned());
        }
        return Ok(session);
    }

    let endpoint = host.allocate_endpoint()?;
    host.driver
        .open_endpoint_with_context(
            endpoint,
            Some(binding.principal),
            effective_actor,
            Symbol::intern("http-sync"),
        )
        .map_err(|error| format_driver_error(&host.driver, error))?;

    let session = SyncSession::new(session_id, endpoint, effective_actor);
    let mut sessions = host.sync.sessions.lock().unwrap();
    if let Some(existing) = sessions.get(&session_id).cloned() {
        host.driver.close_endpoint(endpoint);
        if effective_actor.is_some() && existing.actor != effective_actor {
            return Err("session belongs to a different actor".to_owned());
        }
        return Ok(existing);
    }
    sessions.insert(session_id, session.clone());
    Ok(session)
}

async fn submit_optional_sync_lifecycle(
    host: &InProcessWebHost,
    session: &Arc<SyncSession>,
    selector: &str,
) -> Result<(), String> {
    let submitted = match host
        .driver
        .submit_invocation_for_endpoint(
            session.endpoint,
            Symbol::intern(selector),
            vec![(
                Symbol::intern("session"),
                sync_u64_value(session.session_id),
            )],
        )
        .await
    {
        Ok(submitted) => submitted,
        Err(error) => {
            tracing::debug!(
                selector = %selector,
                error = %format_driver_error(&host.driver, error),
                "optional sync lifecycle hook failed to submit"
            );
            return Ok(());
        }
    };
    match submitted.outcome {
        TaskOutcome::Complete { .. } | TaskOutcome::Suspended { .. } => Ok(()),
        TaskOutcome::Aborted { error, .. } => {
            tracing::debug!(selector = %selector, error = %error, "optional sync lifecycle hook aborted");
            Ok(())
        }
    }
}

async fn route_dom_event(
    host: &InProcessWebHost,
    session: &Arc<SyncSession>,
    event: DomEventPayload,
) -> Result<(), String> {
    let _dom_event_timer = crate::metrics::start_sync_phase(SyncRenderPhase::DomEvent);
    let trace = SyncTrace::new("dom_event");
    let Some(active) = active_rendered_sync_view(session, event.view_id) else {
        return send_recovery_snapshot(host, session, &event).await;
    };
    trace.mark("active_view");
    if event.refresh
        && (active.server_revision != event.revision || active.server_signature != event.signature)
    {
        let rendered = render_sync_view(host, session.endpoint, event.view_id).await?;
        trace.mark("stale_render");
        if event.revision > rendered.revision {
            return send_recovery_snapshot_from_rendered(session, &event, rendered).await;
        }
        store_rendered_sync_view(session, event.view_id, &rendered);
    }

    let event_name = event.event.clone();
    let action = event.action.clone();
    let target = event.target.clone();
    let route_start = Instant::now();
    let sync_event_start = Instant::now();
    let submitted = host
        .driver
        .submit_invocation_for_endpoint(
            session.endpoint,
            Symbol::intern("sync_event"),
            vec![
                (Symbol::intern("session"), sync_u64_value(event.session_id)),
                (Symbol::intern("view"), sync_u64_value(event.view_id)),
                (Symbol::intern("event"), Value::string(event.event)),
                (Symbol::intern("target"), Value::string(event.target)),
                (Symbol::intern("action"), Value::string(event.action)),
                (Symbol::intern("fields"), sync_event_fields(event.fields)),
            ],
        )
        .await
        .map_err(|error| host.driver.format_error(&error))?;
    let sync_event_us = sync_event_start.elapsed().as_micros();
    trace.mark("sync_event");
    let refresh_immediately = match submitted.outcome {
        TaskOutcome::Complete { value, .. } => value != Value::bool(true),
        TaskOutcome::Suspended { .. } => {
            session.sync.lock().unwrap().pending_tasks.insert(
                submitted.task_id,
                PendingSyncTask {
                    view_id: event.view_id,
                    refresh: event.refresh,
                    action: action.clone(),
                },
            );
            false
        }
        TaskOutcome::Aborted { error, .. } => {
            let message = format!("sync_event aborted: {}", host.driver.format_value(&error));
            tracing::error!(
                target: "mica_web_host::sync",
                task_id = submitted.task_id,
                session_id = event.session_id,
                view_id = event.view_id,
                event = %event_name,
                action = %action,
                target = %target,
                error = %message,
                "sync event task aborted"
            );
            return Err(message);
        }
    };
    if !event.refresh {
        tracing::debug!(
            target: "mica_web_host::sync",
            endpoint = ?session.endpoint,
            session_id = event.session_id,
            view_id = event.view_id,
            event = %event_name,
            action = %action,
            target = %target,
            sync_event_us,
            total_us = route_start.elapsed().as_micros(),
            "sync DOM event routed without refresh"
        );
        return Ok(());
    }
    if !refresh_immediately {
        tracing::debug!(
            target: "mica_web_host::sync",
            endpoint = ?session.endpoint,
            session_id = event.session_id,
            view_id = event.view_id,
            action = %action,
            "sync DOM event is awaiting a subscribed view update"
        );
        return Ok(());
    }
    let refresh_start = Instant::now();
    let result = refresh_active_sync_views_after_dom_event(
        host,
        session.session_id,
        event.view_id,
        action.as_str(),
    )
    .await;
    let refresh_us = refresh_start.elapsed().as_micros();
    tracing::debug!(
        target: "mica_web_host::sync",
        endpoint = ?session.endpoint,
        session_id = event.session_id,
        view_id = event.view_id,
        event = %event_name,
        action = %action,
        sync_event_us,
        refresh_us,
        total_us = route_start.elapsed().as_micros(),
        "sync DOM event routed"
    );
    trace.mark("refresh");
    result
}

async fn send_recovery_snapshot(
    host: &InProcessWebHost,
    session: &Arc<SyncSession>,
    event: &DomEventPayload,
) -> Result<(), String> {
    let rendered = render_sync_view(host, session.endpoint, event.view_id).await?;
    send_recovery_snapshot_from_rendered(session, event, rendered).await
}

async fn send_recovery_snapshot_from_rendered(
    session: &Arc<SyncSession>,
    event: &DomEventPayload,
    rendered: RenderedSyncView,
) -> Result<(), String> {
    let envelope = snapshot_envelope(
        event.session_id,
        event.view_id,
        event.revision,
        event.signature,
        &rendered,
    );
    crate::metrics::record_sync_envelope(
        SyncEnvelopeKind::RecoverySnapshot,
        rendered.payload.len(),
    );
    session.output.send_sync_envelope(envelope)?;
    store_rendered_sync_view(session, event.view_id, &rendered);
    Ok(())
}

async fn refresh_active_sync_views_after_dom_event(
    host: &InProcessWebHost,
    source_session_id: u64,
    source_view_id: u64,
    action: &str,
) -> Result<(), String> {
    let Some(active) = active_sync_view(&host.sync.sessions, source_session_id, source_view_id)
    else {
        return Ok(());
    };
    refresh_active_sync_view_for(
        &host.driver,
        &host.sync.sessions,
        active,
        true,
        Some(action),
    )
    .await
}

async fn refresh_active_sync_view_for(
    driver: &CompioTaskDriver,
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    active: ActiveSyncView,
    force_ack: bool,
    action: Option<&str>,
) -> Result<(), String> {
    let _refresh_timer = crate::metrics::start_sync_phase(SyncRenderPhase::Refresh);
    let refresh_start = Instant::now();
    let render_start = Instant::now();
    let tree = render_sync_tree(driver, active.endpoint, active.view_id).await?;
    let render_us = render_start.elapsed().as_micros();
    let patches = active.last_tree.as_ref().map(|last_tree| {
        let _diff_timer = crate::metrics::start_sync_phase(SyncRenderPhase::Diff);
        diff_dom_nodes(last_tree, &tree)
    });
    if patches.as_ref().is_some_and(Vec::is_empty) {
        crate::metrics::record_sync_patch_count(0);
        if force_ack {
            let payload = {
                let _payload_timer =
                    crate::metrics::start_sync_phase(SyncRenderPhase::DeltaPayload);
                dom_patch_payload_json(active.view_id, active.server_revision, &[])
            };
            crate::metrics::record_sync_envelope(SyncEnvelopeKind::Ack, payload.len());
            send_sync_envelope_to(
                sessions,
                active.session_id,
                SyncEnvelope {
                    kind: SyncMessageKind::ViewDelta,
                    session_id: active.session_id,
                    view_id: active.view_id,
                    client_revision: active.server_revision,
                    client_signature: active.server_signature,
                    server_revision: active.server_revision,
                    server_signature: active.server_signature,
                    payload,
                },
            )?;
        }
        tracing::debug!(
            target: "mica_web_host::sync",
            endpoint = ?active.endpoint,
            session_id = active.session_id,
            view_id = active.view_id,
            action = ?action,
            force_ack,
            changed = false,
            render_us,
            total_us = refresh_start.elapsed().as_micros(),
            "sync refresh view"
        );
        return Ok(());
    }

    let revision = active
        .server_revision
        .max(active.client_revision)
        .saturating_add(1)
        .max(1);
    let rendered = rendered_sync_view(active.view_id, revision, tree);
    let has_queued_view_update = has_pending_sync_view(sessions, active.session_id, active.view_id);
    let envelope = if has_queued_view_update {
        crate::metrics::record_sync_envelope(SyncEnvelopeKind::Snapshot, rendered.payload.len());
        tracing::debug!(
            target: "mica_web_host::sync",
            endpoint = ?active.endpoint,
            session_id = active.session_id,
            view_id = active.view_id,
            action = ?action,
            force_ack,
            changed = true,
            coalesced = true,
            render_us,
            total_us = refresh_start.elapsed().as_micros(),
            payload_bytes = rendered.payload.len(),
            "sync refresh view"
        );
        snapshot_envelope(
            active.session_id,
            active.view_id,
            active.client_revision,
            active.client_signature,
            &rendered,
        )
    } else if let Some(patches) = patches {
        crate::metrics::record_sync_patch_count(patches.len());
        let patch_count = patches.len();
        let payload_start = Instant::now();
        let payload = {
            let _payload_timer = crate::metrics::start_sync_phase(SyncRenderPhase::DeltaPayload);
            dom_patch_payload_json(active.view_id, rendered.revision, &patches)
        };
        let payload_us = payload_start.elapsed().as_micros();
        crate::metrics::record_sync_envelope(SyncEnvelopeKind::Delta, payload.len());
        tracing::debug!(
            target: "mica_web_host::sync",
            endpoint = ?active.endpoint,
            session_id = active.session_id,
            view_id = active.view_id,
            action = ?action,
            force_ack,
            changed = true,
            render_us,
            payload_us,
            total_us = refresh_start.elapsed().as_micros(),
            payload_bytes = payload.len(),
            patches = patch_count,
            "sync refresh view"
        );
        SyncEnvelope {
            kind: SyncMessageKind::ViewDelta,
            session_id: active.session_id,
            view_id: active.view_id,
            client_revision: active.server_revision,
            client_signature: active.server_signature,
            server_revision: rendered.revision,
            server_signature: rendered.signature,
            payload,
        }
    } else {
        crate::metrics::record_sync_envelope(SyncEnvelopeKind::Snapshot, rendered.payload.len());
        snapshot_envelope(
            active.session_id,
            active.view_id,
            active.client_revision,
            active.client_signature,
            &rendered,
        )
    };
    send_sync_envelope_to(sessions, active.session_id, envelope)?;
    store_rendered_sync_view_in(sessions, active.session_id, active.view_id, &rendered);
    Ok(())
}

async fn render_sync_view(
    host: &InProcessWebHost,
    endpoint: Identity,
    view_id: u64,
) -> Result<RenderedSyncView, String> {
    let revision = host
        .sync
        .sessions
        .lock()
        .unwrap()
        .values()
        .find(|session| session.endpoint == endpoint)
        .and_then(|session| active_rendered_sync_view(session, view_id))
        .map_or(1, |active| next_view_revision(&active));
    let tree = render_sync_tree(&host.driver, endpoint, view_id).await?;
    Ok(rendered_sync_view(view_id, revision, tree))
}

async fn render_sync_tree(
    driver: &CompioTaskDriver,
    endpoint: Identity,
    view_id: u64,
) -> Result<DomNode, String> {
    let trace = SyncTrace::new("render");
    let tree_value = {
        let _tree_timer = crate::metrics::start_sync_phase(SyncRenderPhase::Tree);
        submit_sync_invocation_for(
            driver,
            endpoint,
            "sync_view_tree",
            vec![(Symbol::intern("view"), sync_u64_value(view_id))],
        )
        .await?
    };
    trace.mark("tree");
    let tree = {
        let _decode_timer = crate::metrics::start_sync_phase(SyncRenderPhase::DecodeTree);
        DomNode::from_mica_value(&tree_value)
            .map_err(|error| format!("sync_view_tree returned invalid DOM tree: {error}"))?
    };
    crate::metrics::record_sync_dom_nodes(tree.node_count());
    trace.mark("decode_tree");
    Ok(tree)
}

fn rendered_sync_view(view_id: u64, revision: u64, tree: DomNode) -> RenderedSyncView {
    let payload = {
        let _payload_timer = crate::metrics::start_sync_phase(SyncRenderPhase::SnapshotPayload);
        mica_host_protocol::snapshot_payload_json(view_id, revision, &tree)
    };
    let signature = sync_payload_signature(revision, &payload);
    RenderedSyncView {
        revision,
        signature,
        tree,
        payload,
    }
}

fn next_view_revision(active: &ActiveViewState) -> u64 {
    active
        .server_revision
        .max(active.client_revision)
        .saturating_add(1)
        .max(1)
}

async fn submit_sync_invocation_for(
    driver: &CompioTaskDriver,
    endpoint: Identity,
    selector: &'static str,
    roles: Vec<(Symbol, Value)>,
) -> Result<Value, String> {
    let submitted = driver
        .submit_invocation_for_endpoint(endpoint, Symbol::intern(selector), roles)
        .await
        .map_err(|error| driver.format_error(&error))?;
    match submitted.outcome {
        TaskOutcome::Complete { value, .. } => Ok(value),
        TaskOutcome::Aborted { error, .. } => Err(format!(
            "sync render invocation {selector} aborted: {}",
            driver.format_value(&error)
        )),
        TaskOutcome::Suspended { .. } => {
            Err(format!("sync render invocation {selector} suspended"))
        }
    }
}

fn active_rendered_sync_view(session: &Arc<SyncSession>, view_id: u64) -> Option<ActiveViewState> {
    session.sync.lock().unwrap().views.get(&view_id).cloned()
}

fn store_rendered_sync_view(session: &Arc<SyncSession>, view_id: u64, rendered: &RenderedSyncView) {
    let _store_timer = crate::metrics::start_sync_phase(SyncRenderPhase::StoreRendered);
    session.sync.lock().unwrap().store_rendered_view(
        view_id,
        rendered.revision,
        rendered.signature,
        rendered.tree.clone(),
    );
}

fn store_rendered_sync_view_in(
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    session_id: u64,
    view_id: u64,
    rendered: &RenderedSyncView,
) {
    if let Some(session) = sessions.lock().unwrap().get(&session_id).cloned() {
        store_rendered_sync_view(&session, view_id, rendered);
    }
}

fn send_sync_envelope_to(
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    session_id: u64,
    envelope: SyncEnvelope,
) -> Result<(), String> {
    let Some(session) = sessions.lock().unwrap().get(&session_id).cloned() else {
        return Ok(());
    };
    let _send_timer = crate::metrics::start_sync_phase(SyncRenderPhase::SendEnvelope);
    session.output.send_sync_envelope(envelope)
}

fn has_pending_sync_view(
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    session_id: u64,
    view_id: u64,
) -> bool {
    sessions
        .lock()
        .unwrap()
        .get(&session_id)
        .is_some_and(|session| session.output.has_pending_view_sync(session_id, view_id))
}

fn snapshot_envelope(
    session_id: u64,
    view_id: u64,
    client_revision: u64,
    client_signature: u64,
    rendered: &RenderedSyncView,
) -> SyncEnvelope {
    SyncEnvelope {
        kind: SyncMessageKind::ViewSnapshot,
        session_id,
        view_id,
        client_revision,
        client_signature,
        server_revision: rendered.revision,
        server_signature: rendered.signature,
        payload: rendered.payload.clone(),
    }
}

fn sync_event_fields(fields: BTreeMap<String, String>) -> Value {
    Value::map(
        fields
            .into_iter()
            .map(|(key, value)| (Value::symbol(Symbol::intern(&key)), Value::string(value))),
    )
}

async fn route_sync_envelope(
    host: &InProcessWebHost,
    session: &Arc<SyncSession>,
    envelope: SyncEnvelope,
) -> Result<(), String> {
    session.sync.lock().unwrap().record_incoming_view(&envelope);
    if matches!(
        envelope.kind,
        SyncMessageKind::NeedView | SyncMessageKind::HaveView
    ) {
        ensure_view_subscriptions(host, session, envelope.view_id).await?;
    }
    if let Some(event) = decode_dom_event_payload(&envelope.payload)? {
        return route_dom_event(host, session, event).await;
    }
    match envelope.kind {
        SyncMessageKind::HaveView => {
            if let Some(active) = active_rendered_sync_view(session, envelope.view_id)
                && active.server_revision == envelope.client_revision
                && active.server_signature == envelope.client_signature
                && active.last_tree.is_some()
            {
                return Ok(());
            }
            let tree = render_sync_tree(&host.driver, session.endpoint, envelope.view_id).await?;
            if envelope.client_revision > 0 {
                let client_rendered =
                    rendered_sync_view(envelope.view_id, envelope.client_revision, tree.clone());
                if client_rendered.signature == envelope.client_signature {
                    store_rendered_sync_view(session, envelope.view_id, &client_rendered);
                    return Ok(());
                }
            }
            let revision = active_rendered_sync_view(session, envelope.view_id)
                .map_or(1, |active| next_view_revision(&active));
            let rendered = rendered_sync_view(envelope.view_id, revision, tree);
            let response = snapshot_envelope(
                envelope.session_id,
                envelope.view_id,
                envelope.client_revision,
                envelope.client_signature,
                &rendered,
            );
            crate::metrics::record_sync_envelope(
                SyncEnvelopeKind::Snapshot,
                rendered.payload.len(),
            );
            session.output.send_sync_envelope(response)?;
            store_rendered_sync_view(session, envelope.view_id, &rendered);
            Ok(())
        }
        SyncMessageKind::NeedView => {
            let rendered = render_sync_view(host, session.endpoint, envelope.view_id).await?;
            let response = snapshot_envelope(
                envelope.session_id,
                envelope.view_id,
                envelope.client_revision,
                envelope.client_signature,
                &rendered,
            );
            crate::metrics::record_sync_envelope(
                SyncEnvelopeKind::Snapshot,
                rendered.payload.len(),
            );
            session.output.send_sync_envelope(response)?;
            store_rendered_sync_view(session, envelope.view_id, &rendered);
            Ok(())
        }
        SyncMessageKind::ViewSnapshot | SyncMessageKind::ViewDelta => host
            .driver
            .input(
                session.endpoint,
                Value::bytes(mica_host_protocol::encoded_sync_envelope(envelope.as_ref())),
            )
            .await
            .map(|_| ())
            .map_err(|error| format_driver_error(&host.driver, error)),
    }
}

async fn write_event_stream_headers(stream: &mut TcpStream) -> Result<(), String> {
    let response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/event-stream; charset=utf-8\r\n",
        "Cache-Control: no-store\r\n",
        "Connection: keep-alive\r\n",
        "Transfer-Encoding: chunked\r\n",
        "X-Accel-Buffering: no\r\n",
        "\r\n"
    );
    let (result, _) = stream.write_all(response.as_bytes()).await.into();
    result.map_err(|error| format!("failed to write sync event stream headers: {error}"))
}

async fn write_event_stream_loop(
    stream: &mut TcpStream,
    output: Arc<SessionOutput>,
) -> Result<(), String> {
    let writer_generation = output.claim_writer();
    while let SessionOutputReady::Ready { .. } | SessionOutputReady::HighWater { .. } =
        output.recv(writer_generation).await
    {
        for message in output.drain_batch(ENDPOINT_OUTPUT_DRAIN_MESSAGES) {
            let payload = match message {
                SessionOutputMessage::SyncEnvelope(envelope) => sync_sse_payload(&envelope),
            };
            write_event_chunk(stream, payload.as_bytes()).await?;
        }
    }
    Ok(())
}

async fn write_event_chunk(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    let prefix = format!("{:X}\r\n", payload.len());
    let mut chunk = Vec::with_capacity(prefix.len() + payload.len() + 2);
    chunk.extend_from_slice(prefix.as_bytes());
    chunk.extend_from_slice(payload);
    chunk.extend_from_slice(b"\r\n");
    let (result, _) = stream.write_all(chunk).await.into();
    result.map_err(|error| format!("failed to write sync event chunk: {error}"))
}

fn sync_sse_payload(envelope: &SyncEnvelope) -> String {
    let data = serde_json::json!({
        "kind": sync_kind_name(envelope.kind),
        "session": envelope.session_id.to_string(),
        "view": envelope.view_id.to_string(),
        "clientRevision": envelope.client_revision.to_string(),
        "clientSignature": envelope.client_signature.to_string(),
        "serverRevision": envelope.server_revision.to_string(),
        "serverSignature": envelope.server_signature.to_string(),
        "payload": String::from_utf8_lossy(&envelope.payload),
    });
    format!("event: sync\ndata: {data}\n\n")
}

fn sync_kind_name(kind: SyncMessageKind) -> &'static str {
    match kind {
        SyncMessageKind::HaveView => "HaveView",
        SyncMessageKind::NeedView => "NeedView",
        SyncMessageKind::ViewSnapshot => "ViewSnapshot",
        SyncMessageKind::ViewDelta => "ViewDelta",
    }
}

fn start_event_pump(
    driver: Arc<CompioTaskDriver>,
    sessions: Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    subscription_mailbox: Arc<DriverSubscriptionMailbox>,
    subscription_views: Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    stop_events: Arc<AtomicBool>,
) {
    compio::runtime::spawn(async move {
        while !stop_events.load(Ordering::Relaxed) {
            let events = driver.wait_events().await;
            if let Err(error) = process_driver_events(
                &driver,
                &sessions,
                &subscription_mailbox,
                &subscription_views,
                events,
            )
            .await
            {
                tracing::warn!(error = %error, "failed to process HTTP sync driver events");
            }
        }
    })
    .detach();
}

fn active_sync_view(
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    session_id: u64,
    view_id: u64,
) -> Option<ActiveSyncView> {
    let session = sessions.lock().unwrap().get(&session_id).cloned()?;
    let view = session.sync.lock().unwrap().views.get(&view_id).cloned()?;
    Some(ActiveSyncView {
        endpoint: session.endpoint,
        session_id,
        view_id,
        client_revision: view.client_revision,
        client_signature: view.client_signature,
        server_revision: view.server_revision,
        server_signature: view.server_signature,
        last_tree: view.last_tree,
    })
}

async fn process_driver_events(
    driver: &CompioTaskDriver,
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    subscription_mailbox: &DriverSubscriptionMailbox,
    subscription_views: &Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    events: Vec<DriverEvent>,
) -> Result<(), String> {
    let mut refreshes = HashMap::<SyncViewKey, (bool, Option<String>, bool)>::new();
    for event in events {
        match event {
            DriverEvent::Effect(effect) => {
                if let Some(session) = sessions
                    .lock()
                    .unwrap()
                    .values()
                    .find(|session| session.endpoint == effect.target)
                    .cloned()
                    && let Some(envelope) =
                        sync_envelope_from_value(session.session_id, &effect.value)
                {
                    let _ = session.output.send_sync_envelope(envelope);
                }
            }
            DriverEvent::TaskCompleted { task_id, value } => {
                if let Some((key, pending)) = take_pending_sync_task(sessions, task_id)
                    && pending.refresh
                    && value != Value::bool(true)
                {
                    refreshes.insert(key, (true, Some(pending.action), false));
                }
            }
            DriverEvent::TaskAborted { task_id, .. }
            | DriverEvent::TaskCancelled { task_id, .. }
            | DriverEvent::TaskFailed { task_id, .. } => {
                if let Some((key, pending)) = take_pending_sync_task(sessions, task_id)
                    && pending.refresh
                {
                    refreshes.insert(key, (true, Some(pending.action), false));
                }
            }
            DriverEvent::SubscriptionReady { mailbox } if mailbox == subscription_mailbox.id() => {
                let messages = driver
                    .drain_subscription_mailbox(subscription_mailbox)
                    .map_err(|error| driver.format_error(&error))?;
                for message in messages {
                    let Some((capability, kind)) = subscription_message(&message) else {
                        continue;
                    };
                    let Some(key) = subscription_views.lock().unwrap().get(&capability).copied()
                    else {
                        continue;
                    };
                    let resynchronize = matches!(kind, "resynchronize" | "revoked");
                    refreshes
                        .entry(key)
                        .and_modify(|refresh| refresh.2 |= resynchronize)
                        .or_insert((false, None, resynchronize));
                }
            }
            DriverEvent::SubscriptionReady { .. } | DriverEvent::TaskSuspended { .. } => {}
        }
    }
    for (key, (force_ack, action, resynchronize)) in refreshes {
        if resynchronize {
            reinstall_view_subscriptions(
                driver,
                sessions,
                subscription_mailbox,
                subscription_views,
                key,
            )
            .await?;
        }
        let Some(active) = active_sync_view(sessions, key.session_id, key.view_id) else {
            continue;
        };
        refresh_active_sync_view_for(driver, sessions, active, force_ack, action.as_deref())
            .await?;
    }
    Ok(())
}

fn take_pending_sync_task(
    sessions: &Arc<Mutex<HashMap<u64, Arc<SyncSession>>>>,
    task_id: TaskId,
) -> Option<(SyncViewKey, PendingSyncTask)> {
    let sessions = sessions
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for session in sessions {
        let mut sync = session.sync.lock().unwrap();
        if let Some(pending) = sync.pending_tasks.remove(&task_id) {
            return Some((
                SyncViewKey {
                    session_id: session.session_id,
                    view_id: pending.view_id,
                },
                pending,
            ));
        }
    }
    None
}

fn subscription_message(message: &Value) -> Option<(CapabilityId, &'static str)> {
    message.with_map(|entries| {
        let subscription = map_value(entries, "subscription")?.as_capability()?;
        let kind = map_value(entries, "kind")
            .and_then(Value::as_symbol)
            .and_then(Symbol::name)?;
        let kind = match kind {
            "changes" => "changes",
            "snapshot" => "snapshot",
            "resynchronize" => "resynchronize",
            "revoked" => "revoked",
            _ => return None,
        };
        Some((subscription, kind))
    })?
}

fn map_value<'a>(entries: &'a [(Value, Value)], name: &str) -> Option<&'a Value> {
    let name = Symbol::intern(name);
    entries
        .iter()
        .find_map(|(key, value)| (key.as_symbol() == Some(name)).then_some(value))
}

fn request_path_without_query(path: &str) -> &str {
    path.split_once('?').map(|(path, _)| path).unwrap_or(path)
}

fn query_u64(path: &str, name: &str) -> Option<u64> {
    let (_, query) = path.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key != name {
            return None;
        }
        value.parse::<u64>().ok()
    })
}

fn with_connection_header(response: HttpResponse, close: bool) -> HttpResponse {
    if close {
        response.with_header("Connection", b"close".as_slice())
    } else {
        response.with_header("Connection", b"keep-alive".as_slice())
    }
}

struct SyncTrace {
    enabled: bool,
    label: &'static str,
    start: Instant,
    last: Mutex<Instant>,
}

impl SyncTrace {
    fn new(label: &'static str) -> Self {
        let now = Instant::now();
        Self {
            enabled: tracing::enabled!(target: "mica_web_host::sync", tracing::Level::TRACE),
            label,
            start: now,
            last: Mutex::new(now),
        }
    }

    fn mark(&self, phase: &'static str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let mut last = self.last.lock().unwrap();
        tracing::trace!(
            target: "mica_web_host::sync",
            label = self.label,
            phase,
            elapsed_us = now.duration_since(*last).as_micros(),
            total_us = now.duration_since(self.start).as_micros(),
            "HTTP sync phase completed"
        );
        *last = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::serve_in_process;
    use compio::net::TcpListener;
    use compio::runtime::Runtime;
    use mica_driver::CompioTaskDriver;
    use mica_host_protocol::{dom_event_payload_json, encoded_sync_envelope};
    use mica_runtime::SourceRunner;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream as StdTcpStream};
    use std::num::NonZeroUsize;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn sse_output_snapshot_replaces_queued_updates_for_same_view() {
        let output = SessionOutput::new();

        output
            .send_sync_envelope(test_envelope(SyncMessageKind::ViewDelta, 7, 21, 1))
            .unwrap();
        output
            .send_sync_envelope(test_envelope(SyncMessageKind::ViewDelta, 7, 22, 1))
            .unwrap();
        output
            .send_sync_envelope(test_envelope(SyncMessageKind::ViewSnapshot, 7, 21, 2))
            .unwrap();

        let messages = output.drain_batch(8);
        let envelopes: Vec<_> = messages
            .into_iter()
            .map(|message| match message {
                SessionOutputMessage::SyncEnvelope(envelope) => envelope,
            })
            .collect();

        assert_eq!(
            envelopes
                .iter()
                .map(|envelope| envelope.view_id)
                .collect::<Vec<_>>(),
            vec![22, 21]
        );
        assert_eq!(envelopes[1].kind, SyncMessageKind::ViewSnapshot);
        assert_eq!(envelopes[1].server_revision, 2);
    }

    #[test]
    fn sse_sync_session_persists_mud_actor_and_command() {
        let (addr_tx, addr_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            Runtime::new().unwrap().block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
                    .await
                    .unwrap();
                let addr = listener.local_addr().unwrap();
                let runner = sync_mud_runner();
                let principal = runner.named_identity(Symbol::intern("web")).unwrap();
                let actor = runner.named_identity(Symbol::intern("alice")).unwrap();
                let driver =
                    CompioTaskDriver::spawn_with_workers(runner, NonZeroUsize::new(1)).unwrap();
                let host = InProcessWebHost::new(driver);
                let binding = RequestBinding {
                    principal,
                    actor: Some(actor),
                };
                addr_tx.send(addr).unwrap();
                compio::runtime::spawn(async move {
                    if let Err(error) = serve_in_process(listener, host, binding, None).await {
                        tracing::warn!(error = %error, "test web host stopped");
                    }
                })
                .detach();
                while stop_rx.try_recv().is_err() {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        });

        let addr = addr_rx.recv().unwrap();
        let result = run_mud_sse_client(addr);
        stop_tx.send(()).unwrap();
        server.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn sse_agent_command_streams_through_view_subscriptions() {
        let (addr_tx, addr_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            Runtime::new().unwrap().block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
                    .await
                    .unwrap();
                let addr = listener.local_addr().unwrap();
                let runner = sync_agent_runner();
                let principal = runner.named_identity(Symbol::intern("web")).unwrap();
                let actor = runner
                    .named_identity(Symbol::intern("agent/default"))
                    .unwrap();
                let handler: mica_driver::ExternalRequestHandler = Arc::new(|request| {
                    Box::pin(async move {
                        assert_eq!(request.service, Symbol::intern("openai"));
                        compio::time::sleep(Duration::from_millis(50)).await;
                        Value::map([
                            (
                                Value::symbol(Symbol::intern("text")),
                                Value::string("synthetic subscribed reply"),
                            ),
                            (Value::symbol(Symbol::intern("tool_calls")), Value::list([])),
                            (
                                Value::symbol(Symbol::intern("stop_reason")),
                                Value::string("stop"),
                            ),
                        ])
                    })
                });
                let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
                    runner,
                    NonZeroUsize::new(1),
                    Some(handler),
                )
                .unwrap();
                let host = InProcessWebHost::new(driver);
                let binding = RequestBinding {
                    principal,
                    actor: Some(actor),
                };
                addr_tx.send(addr).unwrap();
                compio::runtime::spawn(async move {
                    if let Err(error) = serve_in_process(listener, host, binding, None).await {
                        tracing::warn!(error = %error, "test web host stopped");
                    }
                })
                .detach();
                while stop_rx.try_recv().is_err() {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        });

        let addr = addr_rx.recv().unwrap();
        let result = run_agent_sse_client(addr);
        stop_tx.send(()).unwrap();
        server.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn sse_noop_dom_event_sends_same_revision_ack() {
        let (addr_tx, addr_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            Runtime::new().unwrap().block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
                    .await
                    .unwrap();
                let addr = listener.local_addr().unwrap();
                let runner = sync_mud_runner();
                let principal = runner.named_identity(Symbol::intern("web")).unwrap();
                let driver =
                    CompioTaskDriver::spawn_with_workers(runner, NonZeroUsize::new(1)).unwrap();
                let host = InProcessWebHost::new(driver);
                let binding = RequestBinding {
                    principal,
                    actor: None,
                };
                addr_tx.send(addr).unwrap();
                compio::runtime::spawn(async move {
                    if let Err(error) = serve_in_process(listener, host, binding, None).await {
                        tracing::warn!(error = %error, "test web host stopped");
                    }
                })
                .detach();
                while stop_rx.try_recv().is_err() {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        });

        let addr = addr_rx.recv().unwrap();
        let result = run_noop_sse_client(addr);
        stop_tx.send(()).unwrap();
        server.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn sse_pending_dom_event_finishes_when_spawn_parent_completes() {
        let endpoint = Identity::new(0x00ee_0000_0000_0100).unwrap();
        let session = SyncSession::new(7, endpoint, None);
        let pending = PendingSyncTask {
            view_id: 21,
            refresh: true,
            action: "test".to_owned(),
        };
        session
            .sync
            .lock()
            .unwrap()
            .pending_tasks
            .insert(11, pending.clone());
        let sessions = Arc::new(Mutex::new(HashMap::from([(7u64, session.clone())])));

        let routed = take_pending_sync_task(&sessions, 11);

        assert_eq!(
            routed,
            Some((
                SyncViewKey {
                    session_id: 7,
                    view_id: 21,
                },
                pending,
            ))
        );
        assert!(session.sync.lock().unwrap().pending_tasks.is_empty());
    }

    #[test]
    fn sse_unrelated_completed_task_does_not_refresh_views() {
        let endpoint = Identity::new(0x00ee_0000_0000_0100).unwrap();
        let session = SyncSession::new(7, endpoint, None);
        let pending = PendingSyncTask {
            view_id: 21,
            refresh: true,
            action: "test".to_owned(),
        };
        session
            .sync
            .lock()
            .unwrap()
            .pending_tasks
            .insert(11, pending.clone());
        let sessions = Arc::new(Mutex::new(HashMap::from([(7u64, session.clone())])));

        let routed = take_pending_sync_task(&sessions, 12);

        assert!(routed.is_none());
        assert_eq!(
            session.sync.lock().unwrap().pending_tasks,
            HashMap::from([(11, pending)])
        );
    }

    fn test_envelope(
        kind: SyncMessageKind,
        session_id: u64,
        view_id: u64,
        revision: u64,
    ) -> SyncEnvelope {
        SyncEnvelope {
            kind,
            session_id,
            view_id,
            client_revision: revision.saturating_sub(1),
            client_signature: revision.saturating_sub(1),
            server_revision: revision,
            server_signature: revision,
            payload: format!("payload-{revision}").into_bytes(),
        }
    }

    fn run_mud_sse_client(addr: SocketAddr) -> Result<(), String> {
        let session_id = 7u64;
        let mut stream = open_event_stream(addr, session_id)?;

        post_sync_input(
            addr,
            encoded_sync_envelope(
                SyncEnvelope {
                    kind: SyncMessageKind::NeedView,
                    session_id,
                    view_id: 21,
                    client_revision: 0,
                    client_signature: 0,
                    server_revision: 0,
                    server_signature: 0,
                    payload: b"need".to_vec(),
                }
                .as_ref(),
            ),
        )?;
        let snapshot = read_next_sync_envelope(&mut stream)?;
        if snapshot.kind != SyncMessageKind::ViewSnapshot {
            return Err(format!("expected snapshot, got {:?}", snapshot.kind));
        }
        let snapshot_payload: serde_json::Value = serde_json::from_slice(&snapshot.payload)
            .map_err(|error| format!("failed to parse snapshot payload: {error}"))?;
        let snapshot_text = serde_json::to_string(&snapshot_payload).unwrap();
        if !snapshot_text.contains("mud-shell") || !snapshot_text.contains("Timbran Hotel") {
            return Err("snapshot did not contain the MUD world view".to_owned());
        }

        let mut current = snapshot;
        let mut command_delta = None;
        for _ in 0..2 {
            post_sync_input(
                addr,
                encoded_sync_envelope(
                    SyncEnvelope {
                        kind: SyncMessageKind::HaveView,
                        session_id,
                        view_id: 21,
                        client_revision: current.server_revision,
                        client_signature: current.server_signature,
                        server_revision: current.server_revision,
                        server_signature: current.server_signature,
                        payload: dom_event_payload_json(&DomEventPayload {
                            session_id,
                            view_id: 21,
                            revision: current.server_revision,
                            signature: current.server_signature,
                            refresh: true,
                            event: "submit".to_owned(),
                            target: "mud-command".to_owned(),
                            action: "mud_command".to_owned(),
                            fields: BTreeMap::from([("text".to_owned(), "look coin".to_owned())]),
                        }),
                    }
                    .as_ref(),
                ),
            )?;
            let envelope = read_next_sync_envelope(&mut stream)?;
            if envelope.kind == SyncMessageKind::ViewSnapshot {
                current = envelope;
            } else {
                command_delta = Some(envelope);
                break;
            }
        }
        let command_delta = command_delta
            .ok_or_else(|| "expected command delta after recovery snapshot".to_owned())?;
        if command_delta.kind != SyncMessageKind::ViewDelta {
            return Err(format!(
                "expected command delta, got {:?}",
                command_delta.kind
            ));
        }
        if command_delta.server_revision <= current.server_revision {
            return Err("command delta did not advance the server revision".to_owned());
        }
        let command_payload: serde_json::Value = serde_json::from_slice(&command_delta.payload)
            .map_err(|error| format!("failed to parse command delta payload: {error}"))?;
        let command_text = serde_json::to_string(&command_payload).unwrap();
        if !command_text.contains("coin") || !command_text.contains("event-line") {
            return Err("command delta did not include the narrative update".to_owned());
        }
        Ok(())
    }

    fn run_agent_sse_client(addr: SocketAddr) -> Result<(), String> {
        let session_id = 8u64;
        let view_id = 31u64;
        let mut stream = open_event_stream(addr, session_id)?;

        post_sync_input(
            addr,
            encoded_sync_envelope(
                SyncEnvelope {
                    kind: SyncMessageKind::NeedView,
                    session_id,
                    view_id,
                    client_revision: 0,
                    client_signature: 0,
                    server_revision: 0,
                    server_signature: 0,
                    payload: b"need".to_vec(),
                }
                .as_ref(),
            ),
        )?;
        let mut current = read_next_sync_envelope(&mut stream)?;
        if current.kind != SyncMessageKind::ViewSnapshot {
            return Err(format!("expected snapshot, got {:?}", current.kind));
        }
        let snapshot: serde_json::Value = serde_json::from_slice(&current.payload)
            .map_err(|error| format!("failed to parse agent snapshot payload: {error}"))?;
        if !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("agent-shell")
        {
            return Err("snapshot did not contain the agent view".to_owned());
        }

        for attempt in 0..2 {
            post_sync_input(
                addr,
                encoded_sync_envelope(
                    SyncEnvelope {
                        kind: SyncMessageKind::HaveView,
                        session_id,
                        view_id,
                        client_revision: current.server_revision,
                        client_signature: current.server_signature,
                        server_revision: current.server_revision,
                        server_signature: current.server_signature,
                        payload: dom_event_payload_json(&DomEventPayload {
                            session_id,
                            view_id,
                            revision: current.server_revision,
                            signature: current.server_signature,
                            refresh: true,
                            event: "submit".to_owned(),
                            target: "agent-command".to_owned(),
                            action: "agent_command".to_owned(),
                            fields: BTreeMap::from([(
                                "text".to_owned(),
                                "hello from subscribed agent test".to_owned(),
                            )]),
                        }),
                    }
                    .as_ref(),
                ),
            )?;

            for _ in 0..4 {
                let envelope = read_next_sync_envelope(&mut stream)?;
                if envelope.kind == SyncMessageKind::ViewSnapshot {
                    current = envelope;
                    break;
                }
                if envelope.kind != SyncMessageKind::ViewDelta {
                    return Err(format!("expected agent delta, got {:?}", envelope.kind));
                }
                if envelope.server_revision <= current.server_revision {
                    return Err("agent delta did not advance the server revision".to_owned());
                }
                let payload: serde_json::Value = serde_json::from_slice(&envelope.payload)
                    .map_err(|error| format!("failed to parse agent delta payload: {error}"))?;
                let text = serde_json::to_string(&payload).unwrap();
                if text.contains("hello from subscribed agent test") {
                    return Ok(());
                }
                current = envelope;
            }
            if attempt == 1 {
                break;
            }
        }

        Err("agent command did not produce a subscription-driven transcript delta".to_owned())
    }

    fn run_noop_sse_client(addr: SocketAddr) -> Result<(), String> {
        let session_id = 7u64;
        let mut stream = open_event_stream(addr, session_id)?;

        post_sync_input(
            addr,
            encoded_sync_envelope(
                SyncEnvelope {
                    kind: SyncMessageKind::NeedView,
                    session_id,
                    view_id: 21,
                    client_revision: 0,
                    client_signature: 0,
                    server_revision: 0,
                    server_signature: 0,
                    payload: b"need".to_vec(),
                }
                .as_ref(),
            ),
        )?;
        let snapshot = read_next_sync_envelope(&mut stream)?;
        if snapshot.kind != SyncMessageKind::ViewSnapshot {
            return Err(format!("expected snapshot, got {:?}", snapshot.kind));
        }

        let mut current = snapshot;
        let mut ack = None;
        for _ in 0..2 {
            post_sync_input(
                addr,
                encoded_sync_envelope(
                    SyncEnvelope {
                        kind: SyncMessageKind::HaveView,
                        session_id,
                        view_id: 21,
                        client_revision: current.server_revision,
                        client_signature: current.server_signature,
                        server_revision: current.server_revision,
                        server_signature: current.server_signature,
                        payload: dom_event_payload_json(&DomEventPayload {
                            session_id,
                            view_id: 21,
                            revision: current.server_revision,
                            signature: current.server_signature,
                            refresh: true,
                            event: "submit".to_owned(),
                            target: "noop".to_owned(),
                            action: "does_not_exist".to_owned(),
                            fields: BTreeMap::new(),
                        }),
                    }
                    .as_ref(),
                ),
            )?;
            let envelope = read_next_sync_envelope(&mut stream)?;
            if envelope.kind == SyncMessageKind::ViewSnapshot {
                current = envelope;
            } else {
                ack = Some(envelope);
                break;
            }
        }
        let ack = ack.ok_or_else(|| "expected no-op ack after recovery snapshot".to_owned())?;
        if ack.kind != SyncMessageKind::ViewDelta {
            return Err(format!("expected no-op delta ack, got {:?}", ack.kind));
        }
        if ack.server_revision != current.server_revision
            || ack.server_signature != current.server_signature
        {
            return Err("no-op delta ack did not preserve rendered revision".to_owned());
        }
        let payload: serde_json::Value = serde_json::from_slice(&ack.payload)
            .map_err(|error| format!("failed to parse no-op delta payload: {error}"))?;
        if payload["patches"] != serde_json::json!([]) {
            return Err(format!(
                "expected empty patches, got {}",
                payload["patches"]
            ));
        }
        Ok(())
    }

    fn open_event_stream(
        addr: SocketAddr,
        session_id: u64,
    ) -> Result<BufReader<StdTcpStream>, String> {
        let mut stream = StdTcpStream::connect(addr)
            .map_err(|error| format!("failed to connect event stream: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| format!("failed to set stream timeout: {error}"))?;
        stream
            .write_all(
                format!("GET /sync/events?session={session_id} HTTP/1.1\r\nHost: {addr}\r\n\r\n")
                    .as_bytes(),
            )
            .map_err(|error| format!("failed to write event stream request: {error}"))?;
        let mut reader = BufReader::new(stream);
        let status = read_http_status(&mut reader)?;
        if !status.contains("200 OK") {
            return Err(format!("unexpected stream status line: {status}"));
        }
        read_http_headers(&mut reader)?;
        Ok(reader)
    }

    fn post_sync_input(addr: SocketAddr, body: Vec<u8>) -> Result<(), String> {
        let mut stream = StdTcpStream::connect(addr)
            .map_err(|error| format!("failed to connect POST: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| format!("failed to set POST timeout: {error}"))?;
        let request = format!(
            "POST /sync/input HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len(),
        );
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(&body))
            .map_err(|error| format!("failed to write sync POST: {error}"))?;
        let mut reader = BufReader::new(stream);
        let status = read_http_status(&mut reader)?;
        if !status.contains("202 Accepted") {
            read_http_headers(&mut reader)?;
            let mut body = String::new();
            reader
                .read_to_string(&mut body)
                .map_err(|error| format!("failed to read sync POST error response: {error}"))?;
            return Err(format!(
                "unexpected sync POST status line: {status}; response: {body}"
            ));
        }
        read_http_headers(&mut reader)?;
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|error| format!("failed to read sync POST response: {error}"))?;
        Ok(())
    }

    fn read_http_status(reader: &mut BufReader<StdTcpStream>) -> Result<String, String> {
        let mut status = String::new();
        reader
            .read_line(&mut status)
            .map_err(|error| format!("failed to read HTTP status line: {error}"))?;
        Ok(status)
    }

    fn read_http_headers(reader: &mut BufReader<StdTcpStream>) -> Result<Vec<String>, String> {
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|error| format!("failed to read HTTP header: {error}"))?;
            if line == "\r\n" {
                break;
            }
            headers.push(line);
        }
        Ok(headers)
    }

    fn read_next_sync_envelope(
        reader: &mut BufReader<StdTcpStream>,
    ) -> Result<SyncEnvelope, String> {
        loop {
            let mut size = String::new();
            reader
                .read_line(&mut size)
                .map_err(|error| format!("failed to read chunk size: {error}"))?;
            let size = size.trim();
            if size.is_empty() {
                continue;
            }
            let size = usize::from_str_radix(size, 16)
                .map_err(|error| format!("invalid chunk size {size:?}: {error}"))?;
            let mut payload = vec![0u8; size];
            reader
                .read_exact(&mut payload)
                .map_err(|error| format!("failed to read chunk payload: {error}"))?;
            let mut suffix = [0u8; 2];
            reader
                .read_exact(&mut suffix)
                .map_err(|error| format!("failed to read chunk suffix: {error}"))?;
            let payload = String::from_utf8(payload)
                .map_err(|error| format!("sync chunk was not UTF-8: {error}"))?;
            if !payload.starts_with("event: sync\n") {
                continue;
            }
            return parse_sync_sse_payload(&payload);
        }
    }

    fn parse_sync_sse_payload(payload: &str) -> Result<SyncEnvelope, String> {
        let data = payload
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or_else(|| format!("missing data line in SSE payload: {payload}"))?;
        let value: serde_json::Value = serde_json::from_str(data)
            .map_err(|error| format!("failed to parse SSE payload JSON: {error}"))?;
        Ok(SyncEnvelope {
            kind: match value["kind"].as_str() {
                Some("HaveView") => SyncMessageKind::HaveView,
                Some("NeedView") => SyncMessageKind::NeedView,
                Some("ViewSnapshot") => SyncMessageKind::ViewSnapshot,
                Some("ViewDelta") => SyncMessageKind::ViewDelta,
                other => return Err(format!("unknown sync kind in SSE payload: {other:?}")),
            },
            session_id: value["session"]
                .as_str()
                .ok_or_else(|| "missing SSE session".to_owned())?
                .parse()
                .map_err(|error| format!("invalid SSE session id: {error}"))?,
            view_id: value["view"]
                .as_str()
                .ok_or_else(|| "missing SSE view".to_owned())?
                .parse()
                .map_err(|error| format!("invalid SSE view id: {error}"))?,
            client_revision: value["clientRevision"]
                .as_str()
                .ok_or_else(|| "missing SSE client revision".to_owned())?
                .parse()
                .map_err(|error| format!("invalid SSE client revision: {error}"))?,
            client_signature: value["clientSignature"]
                .as_str()
                .ok_or_else(|| "missing SSE client signature".to_owned())?
                .parse()
                .map_err(|error| format!("invalid SSE client signature: {error}"))?,
            server_revision: value["serverRevision"]
                .as_str()
                .ok_or_else(|| "missing SSE server revision".to_owned())?
                .parse()
                .map_err(|error| format!("invalid SSE server revision: {error}"))?,
            server_signature: value["serverSignature"]
                .as_str()
                .ok_or_else(|| "missing SSE server signature".to_owned())?
                .parse()
                .map_err(|error| format!("invalid SSE server signature: {error}"))?,
            payload: value["payload"]
                .as_str()
                .ok_or_else(|| "missing SSE payload".to_owned())?
                .as_bytes()
                .to_vec(),
        })
    }

    fn sync_mud_runner() -> SourceRunner {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/sync-host.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/shared/string.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/shared/events.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/core.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/command-parser.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/shared/sync-dom.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/ui-session.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/ui-mica-inspect.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/ui-compose.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/ui-retrieval.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/ui-narrative.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/ui-actions.mica"))
            .unwrap();
        runner
    }

    fn sync_agent_runner() -> SourceRunner {
        let mut runner = SourceRunner::new_empty();
        for filein in [
            include_str!("../../../apps/shared/sync-host.mica"),
            include_str!("../../../apps/shared/string.mica"),
            include_str!("../../../apps/shared/events.mica"),
            include_str!("../../../apps/shared/llm.mica"),
            include_str!("../../../apps/agent/core.mica"),
            include_str!("../../../apps/agent/workspaces.mica"),
            include_str!("../../../apps/agent/tools.mica"),
            include_str!("../../../apps/shared/sync-dom.mica"),
            include_str!("../../../apps/agent/ui-session.mica"),
            include_str!("../../../apps/agent/transcript.mica"),
            include_str!("../../../apps/agent/ui-compose.mica"),
            include_str!("../../../apps/agent/ui-actions.mica"),
        ] {
            runner.run_filein(filein).unwrap();
        }
        runner
    }
}
