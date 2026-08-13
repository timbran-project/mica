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

use crate::metrics::{IncomingDatagramKind, RenderOperation, SyncEnvelopeKind, SyncRenderPhase};
use crate::state::{
    ActiveSyncView, InProcessWebTransportHost, PendingSyncTask, RenderedSyncView, SessionState,
    SyncViewKey, format_driver_error,
};
use crate::{
    NEXT_SYNC_CHUNK_ID, SYNC_CHUNK_HEADER_LEN, SYNC_CHUNK_MAGIC, SYNC_CHUNK_PAYLOAD_LEN,
    SYNC_DATAGRAM_MAX_LEN,
};
use bytes::Bytes;
use mica_driver::{
    DriverClient, DriverEvent, DriverSubscriptionRequest, EndpointSession, SubscriptionMailbox,
};
use mica_host_protocol::{
    DomEventPayload, DomNode, SyncEnvelope, SyncMessageKind, SyncViewDependencySubject,
    SyncViewRelation, decode_dom_event_payload, decode_sync_envelope,
    decode_sync_view_dependencies, diff_dom_nodes, dom_patch_payload_json, encoded_sync_envelope,
    snapshot_payload_json, sync_envelope_from_value, sync_payload_signature, sync_u64_value,
};
use mica_runtime::{SubscriptionInitialDelivery, SubscriptionSubject, TaskId, TaskOutcome};
use mica_var::{CapabilityId, Identity, Symbol, Value};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn endpoint_session(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
) -> Result<EndpointSession, String> {
    host.sessions
        .lock()
        .unwrap()
        .get(&endpoint)
        .map(|state| state.endpoint.clone())
        .ok_or_else(|| "WebTransport endpoint is not open".to_owned())
}

pub(crate) async fn route_incoming_datagram(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    datagram: Bytes,
) -> Result<(), String> {
    crate::metrics::metrics()
        .incoming_bytes
        .add(datagram.len() as isize);
    match decode_sync_envelope(&datagram) {
        Ok(envelope) => {
            crate::metrics::metrics()
                .incoming_datagrams
                .inc(IncomingDatagramKind::SyncEnvelope);
            crate::metrics::metrics()
                .sync_envelopes
                .inc(sync_envelope_kind(envelope.kind));
            route_sync_envelope(host, endpoint, envelope).await
        }
        Err(_) => route_plain_datagram(host, endpoint, datagram).await,
    }
}

async fn route_plain_datagram(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    datagram: Bytes,
) -> Result<(), String> {
    if let Some(event) = decode_dom_event_payload(&datagram)? {
        crate::metrics::metrics()
            .incoming_datagrams
            .inc(IncomingDatagramKind::DomEvent);
        return route_dom_event(host, endpoint, event).await;
    }

    crate::metrics::metrics()
        .incoming_datagrams
        .inc(IncomingDatagramKind::Plain);
    endpoint_session(host, endpoint)?
        .input(Value::bytes(datagram))
        .await
        .map(|_| ())
        .map_err(|error| format_driver_error(&host.client, error))
}

async fn route_dom_event(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    event: DomEventPayload,
) -> Result<(), String> {
    let _dom_event_timer = crate::metrics::start_sync_phase(SyncRenderPhase::DomEvent);
    let route_start = Instant::now();
    let trace = SyncTrace::new("dom_event");
    let event_name = event.event.clone();
    let action = event.action.clone();
    let Some(active) = host.active_rendered_sync_view(endpoint, event.session_id, event.view_id)
    else {
        return send_recovery_snapshot(host, endpoint, &event).await;
    };
    trace.mark("active_view");
    if event.refresh
        && (active.server_revision != event.revision || active.server_signature != event.signature)
    {
        let rendered = render_sync_view(host, endpoint, event.view_id).await?;
        trace.mark("stale_render");
        if event.revision > rendered.revision {
            return send_recovery_snapshot_from_rendered(host, endpoint, &event, rendered).await;
        }
        host.store_rendered_sync_view(endpoint, event.session_id, event.view_id, &rendered);
    }

    let sync_event_start = Instant::now();
    let submitted = endpoint_session(host, endpoint)?
        .invoke(
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
        .map_err(|error| format_driver_error(&host.client, error))?;
    let sync_event_us = sync_event_start.elapsed().as_micros();
    trace.mark("sync_event");
    let task_id = submitted.task_id();
    let initial_outcome = submitted.initial_report().outcome.clone();
    let refresh_immediately = match initial_outcome {
        TaskOutcome::Complete { value, .. } => {
            tracing::trace!(
                target: "mica_webtransport_host::sync",
                value = %value,
                "sync event completed"
            );
            value != Value::bool(true)
        }
        TaskOutcome::Suspended { .. } => {
            if let Some(state) = host.sessions.lock().unwrap().get(&endpoint).cloned() {
                state.sync.lock().unwrap().pending_tasks.insert(
                    task_id,
                    PendingSyncTask {
                        session_id: event.session_id,
                        view_id: event.view_id,
                        refresh: event.refresh,
                        action: action.clone(),
                    },
                );
            }
            if let Err(error) = host.events.watch_invocation(submitted) {
                if let Some(state) = host.sessions.lock().unwrap().get(&endpoint).cloned() {
                    state.sync.lock().unwrap().pending_tasks.remove(&task_id);
                }
                return Err(format_driver_error(&host.client, error));
            }
            false
        }
        TaskOutcome::Aborted { error, .. } => {
            return Err(format!("sync_event aborted: {error}"));
        }
    };
    if !event.refresh {
        tracing::debug!(
            target: "mica_webtransport_host::sync",
            endpoint = ?endpoint,
            session_id = event.session_id,
            view_id = event.view_id,
            event = %event_name,
            action = %action,
            sync_event_us,
            total_us = route_start.elapsed().as_micros(),
            "sync DOM event routed without refresh"
        );
        return Ok(());
    }
    if !refresh_immediately {
        tracing::debug!(
            target: "mica_webtransport_host::sync",
            endpoint = ?endpoint,
            session_id = event.session_id,
            view_id = event.view_id,
            action = %action,
            "sync DOM event is awaiting a subscribed view update"
        );
        return Ok(());
    }
    let refresh_start = Instant::now();
    let result = refresh_active_sync_view(
        host,
        endpoint,
        event.session_id,
        event.view_id,
        true,
        Some(action.as_str()),
    )
    .await;
    let refresh_us = refresh_start.elapsed().as_micros();
    tracing::debug!(
        target: "mica_webtransport_host::sync",
        endpoint = ?endpoint,
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
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    event: &DomEventPayload,
) -> Result<(), String> {
    let rendered = render_sync_view(host, endpoint, event.view_id).await?;
    send_recovery_snapshot_from_rendered(host, endpoint, event, rendered).await
}

async fn send_recovery_snapshot_from_rendered(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
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
    crate::metrics::metrics().recovery_snapshots.inc();
    host.send_sync_envelope(endpoint, envelope)?;
    host.store_rendered_sync_view(endpoint, event.session_id, event.view_id, &rendered);
    Ok(())
}

async fn refresh_active_sync_view(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    session_id: u64,
    view_id: u64,
    force_ack: bool,
    action: Option<&str>,
) -> Result<(), String> {
    let Some(view_state) = host.active_rendered_sync_view(endpoint, session_id, view_id) else {
        return Ok(());
    };
    let active = ActiveSyncView {
        endpoint,
        session_id,
        view_id,
        client_revision: view_state.client_revision,
        client_signature: view_state.client_signature,
        server_revision: view_state.server_revision,
        server_signature: view_state.server_signature,
        last_tree: view_state.last_tree,
    };
    refresh_active_sync_view_for(&host.sessions, active, force_ack, action).await
}

async fn refresh_active_sync_view_for(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    active: ActiveSyncView,
    force_ack: bool,
    action: Option<&str>,
) -> Result<(), String> {
    let _refresh_timer = crate::metrics::start_sync_phase(SyncRenderPhase::Refresh);
    let refresh_start = Instant::now();
    let render_start = Instant::now();
    let endpoint = sessions
        .lock()
        .unwrap()
        .get(&active.endpoint)
        .map(|state| state.endpoint.clone())
        .ok_or_else(|| "WebTransport endpoint closed before refresh".to_owned())?;
    let tree = render_sync_tree(&endpoint, active.view_id).await?;
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
            send_sync_envelope_to(
                sessions,
                active.endpoint,
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
            target: "mica_webtransport_host::sync",
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
    let has_queued_view_update =
        has_pending_sync_view(sessions, active.endpoint, active.session_id, active.view_id);
    let envelope = if has_queued_view_update {
        tracing::debug!(
            target: "mica_webtransport_host::sync",
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
        tracing::debug!(
            target: "mica_webtransport_host::sync",
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
        snapshot_envelope(
            active.session_id,
            active.view_id,
            active.client_revision,
            active.client_signature,
            &rendered,
        )
    };
    let send_start = Instant::now();
    send_sync_envelope_to(sessions, active.endpoint, envelope)?;
    tracing::debug!(
        target: "mica_webtransport_host::sync",
        endpoint = ?active.endpoint,
        session_id = active.session_id,
        view_id = active.view_id,
        action = ?action,
        force_ack,
        send_us = send_start.elapsed().as_micros(),
        total_us = refresh_start.elapsed().as_micros(),
        "sync send view"
    );
    store_rendered_sync_view_in(
        sessions,
        active.endpoint,
        active.session_id,
        active.view_id,
        &rendered,
    );
    Ok(())
}

async fn render_sync_view(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    view_id: u64,
) -> Result<RenderedSyncView, String> {
    let start = Instant::now();
    let revision = host
        .sessions
        .lock()
        .unwrap()
        .get(&endpoint)
        .and_then(|state| {
            let sync = state.sync.lock().unwrap();
            sync.sessions
                .values()
                .find_map(|views| views.get(&view_id))
                .map(next_view_revision)
        })
        .unwrap_or(1);
    let endpoint = endpoint_session(host, endpoint)?;
    let tree = render_sync_tree(&endpoint, view_id).await?;
    let rendered = rendered_sync_view(view_id, revision, tree);
    let elapsed = start.elapsed();
    crate::metrics::metrics()
        .sync_render_duration_us
        .record(RenderOperation::View, crate::metrics::duration_us(elapsed));
    crate::metrics::metrics()
        .sync_render_duration
        .record_elapsed(RenderOperation::View, elapsed);
    Ok(rendered)
}

async fn render_sync_tree(endpoint: &EndpointSession, view_id: u64) -> Result<DomNode, String> {
    let trace = SyncTrace::new("render");
    let tree_value = {
        let _tree_timer = crate::metrics::start_sync_phase(SyncRenderPhase::Tree);
        submit_sync_invocation_for(
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
        snapshot_payload_json(view_id, revision, &tree)
    };
    let signature = sync_payload_signature(revision, &payload);
    RenderedSyncView {
        revision,
        signature,
        tree,
        payload,
    }
}

fn next_view_revision(active: &crate::state::ActiveViewState) -> u64 {
    active
        .server_revision
        .max(active.client_revision)
        .saturating_add(1)
        .max(1)
}

pub(crate) struct SyncTrace {
    enabled: bool,
    label: &'static str,
    start: Instant,
    last: Mutex<Instant>,
}

impl SyncTrace {
    pub(crate) fn new(label: &'static str) -> Self {
        let now = Instant::now();
        Self {
            enabled: tracing::enabled!(
                target: "mica_webtransport_host::sync",
                tracing::Level::TRACE
            ),
            label,
            start: now,
            last: Mutex::new(now),
        }
    }

    pub(crate) fn mark(&self, phase: &'static str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let mut last = self.last.lock().unwrap();
        tracing::trace!(
            target: "mica_webtransport_host::sync",
            label = self.label,
            phase,
            elapsed_us = now.duration_since(*last).as_micros(),
            total_us = now.duration_since(self.start).as_micros(),
            "WebTransport sync phase completed"
        );
        *last = now;
    }
}

async fn submit_sync_invocation_for(
    endpoint: &EndpointSession,
    selector: &'static str,
    roles: Vec<(Symbol, Value)>,
) -> Result<Value, String> {
    let submitted = endpoint
        .invoke(Symbol::intern(selector), roles)
        .await
        .map_err(|error| error.to_string())?;
    match &submitted.initial_report().outcome {
        TaskOutcome::Complete { value, .. } => Ok(value.clone()),
        TaskOutcome::Aborted { error, .. } => Err(format!(
            "sync render invocation {selector} aborted: {error}"
        )),
        TaskOutcome::Suspended { .. } => {
            Err(format!("sync render invocation {selector} suspended"))
        }
    }
}

pub(crate) fn store_rendered_sync_view_in(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    endpoint: Identity,
    session_id: u64,
    view_id: u64,
    rendered: &RenderedSyncView,
) {
    if let Some(state) = sessions.lock().unwrap().get(&endpoint).cloned() {
        let _store_timer = crate::metrics::start_sync_phase(SyncRenderPhase::StoreRendered);
        state.sync.lock().unwrap().store_rendered_view(
            session_id,
            view_id,
            rendered.revision,
            rendered.signature,
            rendered.tree.clone(),
        );
    }
}

pub(crate) fn send_sync_envelope_to(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    endpoint: Identity,
    envelope: SyncEnvelope,
) -> Result<(), String> {
    let Some(state) = sessions.lock().unwrap().get(&endpoint).cloned() else {
        return Ok(());
    };
    crate::metrics::metrics()
        .sync_envelopes
        .inc(sync_envelope_kind(envelope.kind));
    crate::metrics::record_sync_payload(sync_envelope_kind(envelope.kind), envelope.payload.len());
    let _send_timer = crate::metrics::start_sync_phase(SyncRenderPhase::SendEnvelope);
    state.output.send_sync_envelope(envelope)?;
    Ok(())
}

fn has_pending_sync_view(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    endpoint: Identity,
    session_id: u64,
    view_id: u64,
) -> bool {
    sessions
        .lock()
        .unwrap()
        .get(&endpoint)
        .is_some_and(|state| state.output.has_pending_view_sync(session_id, view_id))
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

async fn ensure_view_subscriptions(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    session_id: u64,
    view_id: u64,
) -> Result<(), String> {
    let Some(state) = host.sessions.lock().unwrap().get(&endpoint).cloned() else {
        return Ok(());
    };
    let should_initialize = {
        let mut sync = state.sync.lock().unwrap();
        let view = sync
            .sessions
            .entry(session_id)
            .or_default()
            .entry(view_id)
            .or_default();
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
        &host.client,
        &host.subscription_mailbox,
        &host.subscription_views,
        endpoint,
        &state,
        session_id,
        view_id,
    )
    .await;
    if result.is_err() {
        let mut sync = state.sync.lock().unwrap();
        let view = sync
            .sessions
            .entry(session_id)
            .or_default()
            .entry(view_id)
            .or_default();
        view.subscriptions_initialized = false;
        view.subscriptions_initializing = false;
    }
    result
}

async fn register_view_subscriptions(
    client: &DriverClient,
    subscription_mailbox: &SubscriptionMailbox,
    subscription_views: &Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    endpoint: Identity,
    state: &Arc<SessionState>,
    session_id: u64,
    view_id: u64,
) -> Result<(), String> {
    let value = submit_sync_invocation_for(
        &state.endpoint,
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
            SyncViewRelation::Name(name) => client
                .named_relation(name)
                .map_err(|error| client.format_error(&error))?,
        };
        if usize::from(arity) != dependency.bindings.len() {
            cancel_subscriptions(&state.endpoint, subscriptions);
            return Err(format!(
                "sync_view_dependencies binding count for relation {} was {}, expected {arity}",
                client.format_value(&Value::identity(relation)),
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
        match state
            .endpoint
            .subscribe(
                subscription_mailbox,
                DriverSubscriptionRequest {
                    subject,
                    initial_delivery: SubscriptionInitialDelivery::ChangesOnly,
                    cursor: None,
                    queue_budget: NonZeroUsize::new(64),
                },
            )
            .await
        {
            Ok(subscription) => subscriptions.push(subscription),
            Err(error) => {
                cancel_subscriptions(&state.endpoint, subscriptions);
                return Err(client.format_error(&error));
            }
        }
    }

    let key = SyncViewKey {
        endpoint,
        session_id,
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
    let mut sync = state.sync.lock().unwrap();
    let view = sync
        .sessions
        .entry(session_id)
        .or_default()
        .entry(view_id)
        .or_default();
    view.subscriptions = subscriptions;
    view.subscriptions_initialized = true;
    view.subscriptions_initializing = false;
    Ok(())
}

fn cancel_subscriptions(endpoint: &EndpointSession, subscriptions: Vec<Value>) {
    for subscription in subscriptions {
        let _ = endpoint.cancel_subscription(subscription);
    }
}

async fn reinstall_view_subscriptions(
    client: &DriverClient,
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    subscription_mailbox: &SubscriptionMailbox,
    subscription_views: &Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    key: SyncViewKey,
) -> Result<(), String> {
    let Some(state) = sessions.lock().unwrap().get(&key.endpoint).cloned() else {
        return Ok(());
    };
    let subscriptions = {
        let mut sync = state.sync.lock().unwrap();
        let view = sync
            .sessions
            .entry(key.session_id)
            .or_default()
            .entry(key.view_id)
            .or_default();
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
    cancel_subscriptions(&state.endpoint, subscriptions);
    let result = register_view_subscriptions(
        client,
        subscription_mailbox,
        subscription_views,
        key.endpoint,
        &state,
        key.session_id,
        key.view_id,
    )
    .await;
    if result.is_err() {
        let mut sync = state.sync.lock().unwrap();
        let view = sync
            .sessions
            .entry(key.session_id)
            .or_default()
            .entry(key.view_id)
            .or_default();
        view.subscriptions_initializing = false;
        view.subscriptions_initialized = false;
    }
    result
}

async fn route_sync_envelope(
    host: &InProcessWebTransportHost,
    endpoint: Identity,
    envelope: SyncEnvelope,
) -> Result<(), String> {
    if let Some(state) = host.sessions.lock().unwrap().get(&endpoint).cloned() {
        state.sync.lock().unwrap().record_incoming_view(&envelope);
    }
    if matches!(
        envelope.kind,
        SyncMessageKind::NeedView | SyncMessageKind::HaveView
    ) {
        ensure_view_subscriptions(host, endpoint, envelope.session_id, envelope.view_id).await?;
    }
    if let Some(event) = decode_dom_event_payload(&envelope.payload)? {
        return route_dom_event(host, endpoint, event).await;
    }
    match envelope.kind {
        SyncMessageKind::HaveView => {
            if let Some(active) =
                host.active_rendered_sync_view(endpoint, envelope.session_id, envelope.view_id)
                && active.server_revision == envelope.client_revision
                && active.server_signature == envelope.client_signature
                && active.last_tree.is_some()
            {
                return Ok(());
            }
            let endpoint_session = endpoint_session(host, endpoint)?;
            let tree = render_sync_tree(&endpoint_session, envelope.view_id).await?;
            if envelope.client_revision > 0 {
                let client_rendered =
                    rendered_sync_view(envelope.view_id, envelope.client_revision, tree.clone());
                if client_rendered.signature == envelope.client_signature {
                    host.store_rendered_sync_view(
                        endpoint,
                        envelope.session_id,
                        envelope.view_id,
                        &client_rendered,
                    );
                    return Ok(());
                }
            }
            let revision = host
                .active_rendered_sync_view(endpoint, envelope.session_id, envelope.view_id)
                .map_or(1, |active| next_view_revision(&active));
            let rendered = rendered_sync_view(envelope.view_id, revision, tree);
            let response = snapshot_envelope(
                envelope.session_id,
                envelope.view_id,
                envelope.client_revision,
                envelope.client_signature,
                &rendered,
            );
            host.send_sync_envelope(endpoint, response)?;
            host.store_rendered_sync_view(
                endpoint,
                envelope.session_id,
                envelope.view_id,
                &rendered,
            );
            Ok(())
        }
        SyncMessageKind::NeedView => {
            let rendered = render_sync_view(host, endpoint, envelope.view_id).await?;
            let response = snapshot_envelope(
                envelope.session_id,
                envelope.view_id,
                envelope.client_revision,
                envelope.client_signature,
                &rendered,
            );
            host.send_sync_envelope(endpoint, response)?;
            host.store_rendered_sync_view(
                endpoint,
                envelope.session_id,
                envelope.view_id,
                &rendered,
            );
            Ok(())
        }
        SyncMessageKind::ViewSnapshot | SyncMessageKind::ViewDelta => {
            endpoint_session(host, endpoint)?
                .input(Value::bytes(encoded_sync_envelope(envelope.as_ref())))
                .await
                .map(|_| ())
                .map_err(|error| format_driver_error(&host.client, error))
        }
    }
}

#[cfg(test)]
pub(crate) fn active_sync_views(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
) -> Vec<ActiveSyncView> {
    let states = sessions
        .lock()
        .unwrap()
        .iter()
        .map(|(endpoint, state)| (*endpoint, state.clone()))
        .collect::<Vec<_>>();
    let mut active = Vec::new();
    for (endpoint, state) in states {
        let sync = state.sync.lock().unwrap();
        for (session_id, views) in &sync.sessions {
            for (view_id, view_state) in views {
                active.push(ActiveSyncView {
                    endpoint,
                    session_id: *session_id,
                    view_id: *view_id,
                    client_revision: view_state.client_revision,
                    client_signature: view_state.client_signature,
                    server_revision: view_state.server_revision,
                    server_signature: view_state.server_signature,
                    last_tree: view_state.last_tree.clone(),
                });
            }
        }
    }
    crate::metrics::metrics()
        .active_sync_views
        .set(active.len() as i64);
    active
}

fn active_sync_view(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    key: SyncViewKey,
) -> Option<ActiveSyncView> {
    let state = sessions.lock().unwrap().get(&key.endpoint).cloned()?;
    let view = state
        .sync
        .lock()
        .unwrap()
        .sessions
        .get(&key.session_id)?
        .get(&key.view_id)?
        .clone();
    Some(ActiveSyncView {
        endpoint: key.endpoint,
        session_id: key.session_id,
        view_id: key.view_id,
        client_revision: view.client_revision,
        client_signature: view.client_signature,
        server_revision: view.server_revision,
        server_signature: view.server_signature,
        last_tree: view.last_tree,
    })
}

pub(crate) async fn process_driver_event(
    client: &DriverClient,
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    subscription_mailbox: &SubscriptionMailbox,
    subscription_views: &Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    event: DriverEvent,
) -> Result<(), String> {
    let mut refreshes = HashMap::<SyncViewKey, (bool, Option<String>, bool)>::new();
    match event {
        DriverEvent::Effect(effect) => {
            route_driver_effect(sessions, effect);
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
            let messages = subscription_mailbox
                .drain()
                .map_err(|error| client.format_error(&error))?;
            for message in messages {
                let Some((capability, kind)) = subscription_message(&message) else {
                    continue;
                };
                let Some(key) = subscription_views.lock().unwrap().get(&capability).copied() else {
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
    for (key, (force_ack, action, resynchronize)) in refreshes {
        if resynchronize {
            reinstall_view_subscriptions(
                client,
                sessions,
                subscription_mailbox,
                subscription_views,
                key,
            )
            .await?;
        }
        let Some(active) = active_sync_view(sessions, key) else {
            continue;
        };
        refresh_active_sync_view_for(sessions, active, force_ack, action.as_deref()).await?;
    }
    Ok(())
}

pub(crate) fn route_driver_effect(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    effect: mica_runtime::Effect,
) {
    if let Some(state) = sessions.lock().unwrap().get(&effect.target).cloned() {
        crate::metrics::metrics().routed_driver_events.inc();
        for datagram in effect_datagrams(effect.target, &effect.value) {
            let _ = state.output.send_datagram(datagram);
        }
    }
}

pub(crate) fn take_pending_sync_task(
    sessions: &Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    task_id: TaskId,
) -> Option<(SyncViewKey, PendingSyncTask)> {
    let states = sessions
        .lock()
        .unwrap()
        .iter()
        .map(|(endpoint, state)| (*endpoint, state.clone()))
        .collect::<Vec<_>>();
    for (endpoint, state) in states {
        let mut sync = state.sync.lock().unwrap();
        if let Some(pending) = sync.pending_tasks.remove(&task_id) {
            return Some((
                SyncViewKey {
                    endpoint,
                    session_id: pending.session_id,
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

fn effect_datagrams(target: Identity, value: &Value) -> Vec<Bytes> {
    if let Some(envelope) = sync_envelope_from_value(target.raw(), value) {
        crate::metrics::metrics()
            .sync_envelopes
            .inc(sync_envelope_kind(envelope.kind));
        return sync_envelope_datagrams(envelope.as_ref());
    }
    vec![effect_datagram(target, value)]
}

fn sync_envelope_kind(kind: SyncMessageKind) -> SyncEnvelopeKind {
    match kind {
        SyncMessageKind::NeedView => SyncEnvelopeKind::NeedView,
        SyncMessageKind::HaveView => SyncEnvelopeKind::HaveView,
        SyncMessageKind::ViewSnapshot => SyncEnvelopeKind::ViewSnapshot,
        SyncMessageKind::ViewDelta => SyncEnvelopeKind::ViewDelta,
    }
}

pub(crate) fn effect_datagram(target: Identity, value: &Value) -> Bytes {
    if let Some(envelope) = sync_envelope_from_value(target.raw(), value) {
        return Bytes::from(encoded_sync_envelope(envelope.as_ref()));
    }
    if let Some(bytes) = value.with_bytes(Bytes::copy_from_slice) {
        return bytes;
    }
    if let Some(text) = value.with_str(|value| Bytes::copy_from_slice(value.as_bytes())) {
        return text;
    }
    Bytes::from(value.to_string())
}

pub(crate) fn sync_envelope_datagrams(
    envelope: mica_host_protocol::SyncEnvelopeRef<'_>,
) -> Vec<Bytes> {
    let encoded = encoded_sync_envelope(envelope);
    if encoded.len() <= SYNC_DATAGRAM_MAX_LEN {
        crate::metrics::metrics().sync_envelope_datagrams.inc();
        return vec![Bytes::from(encoded)];
    }

    let count = encoded.len().div_ceil(SYNC_CHUNK_PAYLOAD_LEN);
    crate::metrics::metrics()
        .sync_envelope_datagrams
        .add(count as isize);
    crate::metrics::metrics()
        .sync_envelope_chunks
        .add(count as isize);
    let message_id = NEXT_SYNC_CHUNK_ID.fetch_add(1, Ordering::Relaxed);
    encoded
        .chunks(SYNC_CHUNK_PAYLOAD_LEN)
        .enumerate()
        .map(|(index, chunk)| {
            let mut datagram = Vec::with_capacity(SYNC_CHUNK_HEADER_LEN + chunk.len());
            datagram.extend_from_slice(SYNC_CHUNK_MAGIC);
            datagram.extend_from_slice(&message_id.to_le_bytes());
            datagram.extend_from_slice(&(index as u32).to_le_bytes());
            datagram.extend_from_slice(&(count as u32).to_le_bytes());
            datagram.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            datagram.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            datagram.extend_from_slice(chunk);
            Bytes::from(datagram)
        })
        .collect()
}
