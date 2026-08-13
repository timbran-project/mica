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

use crate::ENDPOINT_OUTPUT_HIGH_WATER_DATAGRAMS;
use crate::sync::{process_driver_event, send_sync_envelope_to, store_rendered_sync_view_in};
use bytes::Bytes;
use mica_driver::{
    DriverClient, DriverEventRegistration, DriverEventRouter, EndpointSession, SubscriptionMailbox,
};
use mica_host_protocol::{DomNode, SyncEnvelope, SyncMessageKind};
use mica_runtime::TaskId;
use mica_var::{CapabilityId, Identity, Value};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

#[derive(Clone, Debug)]
pub struct SessionBinding {
    pub principal: Identity,
    pub actor: Option<Identity>,
}

pub struct WebTransportTlsConfig {
    pub(crate) cert_chain: Vec<CertificateDer<'static>>,
    pub(crate) key_der: PrivateKeyDer<'static>,
}

pub struct InProcessWebTransportHost {
    pub(crate) client: DriverClient,
    pub(crate) events: DriverEventRouter,
    pub(crate) sessions: Arc<Mutex<HashMap<Identity, Arc<SessionState>>>>,
    pub(crate) subscription_mailbox: Arc<SubscriptionMailbox>,
    pub(crate) subscription_views: Arc<Mutex<HashMap<CapabilityId, SyncViewKey>>>,
    pub(crate) _event_registration: DriverEventRegistration,
}

pub(crate) struct SessionState {
    pub(crate) endpoint: EndpointSession,
    pub(crate) output: Arc<SessionOutput>,
    pub(crate) sync: Mutex<SessionSyncState>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SessionSyncState {
    pub(crate) sessions: HashMap<u64, HashMap<u64, ActiveViewState>>,
    pub(crate) pending_tasks: HashMap<TaskId, PendingSyncTask>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ActiveViewState {
    pub(crate) client_revision: u64,
    pub(crate) client_signature: u64,
    pub(crate) server_revision: u64,
    pub(crate) server_signature: u64,
    pub(crate) last_tree: Option<DomNode>,
    pub(crate) subscriptions: Vec<Value>,
    pub(crate) subscriptions_initialized: bool,
    pub(crate) subscriptions_initializing: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingSyncTask {
    pub(crate) session_id: u64,
    pub(crate) view_id: u64,
    pub(crate) refresh: bool,
    pub(crate) action: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SyncViewKey {
    pub(crate) endpoint: Identity,
    pub(crate) session_id: u64,
    pub(crate) view_id: u64,
}

#[derive(Default)]
pub(crate) struct SessionOutput {
    pub(crate) state: Mutex<SessionOutputState>,
}

#[derive(Default)]
pub(crate) struct SessionOutputState {
    pub(crate) messages: VecDeque<SessionOutputMessage>,
    pub(crate) closed: bool,
    pub(crate) waker: Option<Waker>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionOutputMessage {
    Datagram(Bytes),
    SyncEnvelope(SyncEnvelope),
}

pub(crate) struct SessionOutputRecv<'a> {
    pub(crate) output: &'a SessionOutput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActiveSyncView {
    pub(crate) endpoint: Identity,
    pub(crate) session_id: u64,
    pub(crate) view_id: u64,
    pub(crate) client_revision: u64,
    pub(crate) client_signature: u64,
    pub(crate) server_revision: u64,
    pub(crate) server_signature: u64,
    pub(crate) last_tree: Option<DomNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RenderedSyncView {
    pub(crate) revision: u64,
    pub(crate) signature: u64,
    pub(crate) tree: DomNode,
    pub(crate) payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionOutputReady {
    Ready { buffered: usize },
    HighWater { buffered: usize },
    Closed,
}

impl SessionState {
    pub(crate) fn new(endpoint: EndpointSession) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            output: SessionOutput::new(),
            sync: Mutex::new(SessionSyncState::default()),
        })
    }
}

impl SessionSyncState {
    pub(crate) fn record_incoming_view(&mut self, envelope: &SyncEnvelope) {
        if !matches!(
            envelope.kind,
            SyncMessageKind::NeedView | SyncMessageKind::HaveView
        ) {
            return;
        }
        let view = self
            .sessions
            .entry(envelope.session_id)
            .or_default()
            .entry(envelope.view_id)
            .or_default();
        view.client_revision = envelope.client_revision;
        view.client_signature = envelope.client_signature;
    }

    pub(crate) fn store_rendered_view(
        &mut self,
        session_id: u64,
        view_id: u64,
        revision: u64,
        signature: u64,
        tree: DomNode,
    ) {
        let view = self
            .sessions
            .entry(session_id)
            .or_default()
            .entry(view_id)
            .or_default();
        view.client_revision = revision;
        view.client_signature = signature;
        view.server_revision = revision;
        view.server_signature = signature;
        view.last_tree = Some(tree);
    }

    #[cfg(test)]
    pub(crate) fn has_active_view(&self, session_id: u64, view_id: u64) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|views| views.contains_key(&view_id))
    }
}

impl WebTransportTlsConfig {
    pub fn from_pem_files(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, String> {
        let cert_path = cert_path.as_ref();
        let key_path = key_path.as_ref();
        let cert_file = File::open(cert_path).map_err(|error| {
            format!(
                "failed to open certificate {}: {error}",
                cert_path.display()
            )
        })?;
        let mut cert_reader = BufReader::new(cert_file);
        let cert_chain = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                format!(
                    "failed to read certificate {}: {error}",
                    cert_path.display()
                )
            })?;
        if cert_chain.is_empty() {
            return Err(format!(
                "certificate file {} did not contain any certificates",
                cert_path.display()
            ));
        }

        let key_file = File::open(key_path).map_err(|error| {
            format!("failed to open private key {}: {error}", key_path.display())
        })?;
        let mut key_reader = BufReader::new(key_file);
        let key_der = rustls_pemfile::private_key(&mut key_reader)
            .map_err(|error| format!("failed to read private key {}: {error}", key_path.display()))?
            .ok_or_else(|| {
                format!(
                    "private key file {} did not contain a supported key",
                    key_path.display()
                )
            })?;

        Ok(Self {
            cert_chain,
            key_der,
        })
    }
}

impl InProcessWebTransportHost {
    pub fn new(client: DriverClient, events: &DriverEventRouter) -> Self {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let subscription_mailbox = Arc::new(
            client
                .create_subscription_mailbox()
                .expect("WebTransport sync subscription mailbox creation must succeed"),
        );
        let subscription_views = Arc::new(Mutex::new(HashMap::new()));
        let event_client = client.clone();
        let event_sessions = Arc::clone(&sessions);
        let event_mailbox = Arc::clone(&subscription_mailbox);
        let event_views = Arc::clone(&subscription_views);
        let event_registration = events.register(move |event| {
            let client = event_client.clone();
            let sessions = Arc::clone(&event_sessions);
            let mailbox = Arc::clone(&event_mailbox);
            let views = Arc::clone(&event_views);
            async move {
                if let Err(error) =
                    process_driver_event(&client, &sessions, &mailbox, &views, event).await
                {
                    tracing::warn!(
                        error = %error,
                        "failed to process WebTransport sync driver event"
                    );
                }
            }
        });
        Self {
            client,
            events: events.clone(),
            sessions,
            subscription_mailbox,
            subscription_views,
            _event_registration: event_registration,
        }
    }

    pub(crate) fn allocate_endpoint(&self) -> Result<Identity, String> {
        self.client
            .allocate_ephemeral_identity()
            .map_err(|error| self.client.format_error(&error))
    }

    #[cfg(test)]
    pub(crate) fn active_sync_views(&self) -> Vec<ActiveSyncView> {
        crate::sync::active_sync_views(&self.sessions)
    }

    pub(crate) fn store_rendered_sync_view(
        &self,
        endpoint: Identity,
        session_id: u64,
        view_id: u64,
        rendered: &RenderedSyncView,
    ) {
        store_rendered_sync_view_in(&self.sessions, endpoint, session_id, view_id, rendered);
    }

    pub(crate) fn send_sync_envelope(
        &self,
        endpoint: Identity,
        envelope: SyncEnvelope,
    ) -> Result<(), String> {
        send_sync_envelope_to(&self.sessions, endpoint, envelope)
    }

    pub(crate) fn active_rendered_sync_view(
        &self,
        endpoint: Identity,
        session_id: u64,
        view_id: u64,
    ) -> Option<ActiveViewState> {
        let state = self.sessions.lock().unwrap().get(&endpoint).cloned()?;
        state
            .sync
            .lock()
            .unwrap()
            .sessions
            .get(&session_id)?
            .get(&view_id)
            .cloned()
    }

    pub(crate) fn cancel_session_subscriptions(&self, state: &Arc<SessionState>) {
        let subscriptions = state
            .sync
            .lock()
            .unwrap()
            .sessions
            .values_mut()
            .flat_map(HashMap::values_mut)
            .flat_map(|view| std::mem::take(&mut view.subscriptions))
            .collect::<Vec<_>>();
        let mut subscription_views = self.subscription_views.lock().unwrap();
        for subscription in subscriptions {
            if let Some(capability) = subscription.as_capability() {
                subscription_views.remove(&capability);
            }
            let _ = state.endpoint.cancel_subscription(subscription);
        }
    }
}

impl Drop for InProcessWebTransportHost {
    fn drop(&mut self) {
        for state in self.sessions.lock().unwrap().values() {
            self.cancel_session_subscriptions(state);
            let _ = state.endpoint.close_in_background();
        }
    }
}

impl SessionOutput {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn send_datagram(&self, datagram: Bytes) -> Result<(), String> {
        self.send_message(SessionOutputMessage::Datagram(datagram))
    }

    pub(crate) fn send_sync_envelope(&self, envelope: SyncEnvelope) -> Result<(), String> {
        if envelope.kind == SyncMessageKind::ViewSnapshot {
            self.send_message_replacing_view_sync(envelope)
        } else {
            self.send_message(SessionOutputMessage::SyncEnvelope(envelope))
        }
    }

    fn send_message_replacing_view_sync(&self, envelope: SyncEnvelope) -> Result<(), String> {
        let waker = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                crate::metrics::metrics().output_send_after_close.inc();
                return Err("session writer is closed".to_owned());
            }
            state.messages.retain(|message| match message {
                SessionOutputMessage::SyncEnvelope(queued) => {
                    queued.session_id != envelope.session_id || queued.view_id != envelope.view_id
                }
                SessionOutputMessage::Datagram(_) => true,
            });
            state
                .messages
                .push_back(SessionOutputMessage::SyncEnvelope(envelope));
            crate::metrics::metrics()
                .queued_outgoing_datagrams
                .set(state.messages.len() as i64);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    pub(crate) fn send_message(&self, message: SessionOutputMessage) -> Result<(), String> {
        let waker = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                crate::metrics::metrics().output_send_after_close.inc();
                return Err("session writer is closed".to_owned());
            }
            state.messages.push_back(message);
            crate::metrics::metrics()
                .queued_outgoing_datagrams
                .set(state.messages.len() as i64);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    pub(crate) fn close(&self) {
        let waker = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(crate) fn recv(&self) -> SessionOutputRecv<'_> {
        SessionOutputRecv { output: self }
    }

    pub(crate) fn drain_batch(&self, max_messages: usize) -> Vec<SessionOutputMessage> {
        let mut state = self.state.lock().unwrap();
        let count = max_messages.min(state.messages.len());
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(message) = state.messages.pop_front() else {
                break;
            };
            messages.push(message);
        }
        crate::metrics::metrics()
            .queued_outgoing_datagrams
            .set(state.messages.len() as i64);
        messages
    }

    pub(crate) fn has_pending_view_sync(&self, session_id: u64, view_id: u64) -> bool {
        self.state.lock().unwrap().messages.iter().any(|message| {
            matches!(
                message,
                SessionOutputMessage::SyncEnvelope(envelope)
                    if envelope.session_id == session_id && envelope.view_id == view_id
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn try_recv(&self) -> Option<Bytes> {
        match self.state.lock().unwrap().messages.pop_front()? {
            SessionOutputMessage::Datagram(datagram) => Some(datagram),
            SessionOutputMessage::SyncEnvelope(envelope) => Some(Bytes::from(
                mica_host_protocol::encoded_sync_envelope(envelope.as_ref()),
            )),
        }
    }
}

impl Future for SessionOutputRecv<'_> {
    type Output = SessionOutputReady;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.output.state.lock().unwrap();
        if state.messages.len() >= ENDPOINT_OUTPUT_HIGH_WATER_DATAGRAMS {
            crate::metrics::metrics().output_high_water_events.inc();
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

pub(crate) fn format_driver_error(
    client: &DriverClient,
    error: mica_driver::DriverError,
) -> String {
    format!("error: {}", client.format_error(&error))
}
