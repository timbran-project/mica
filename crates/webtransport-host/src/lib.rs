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

pub mod metrics;
mod serve;
mod state;
mod sync;

pub use serve::{bind_server_endpoint, serve_in_process};
pub use state::{InProcessWebTransportHost, SessionBinding, WebTransportTlsConfig};

use std::sync::atomic::AtomicU32;

pub const DEFAULT_BIND: &str = "127.0.0.1:4433";

const ENDPOINT_OUTPUT_HIGH_WATER_DATAGRAMS: usize = 128;
const ENDPOINT_OUTPUT_DRAIN_DATAGRAMS: usize = 64;
const SYNC_DATAGRAM_MAX_LEN: usize = 1024;
const SYNC_CHUNK_HEADER_LEN: usize = 24;
const SYNC_CHUNK_PAYLOAD_LEN: usize = SYNC_DATAGRAM_MAX_LEN - SYNC_CHUNK_HEADER_LEN;
const SYNC_CHUNK_MAGIC: &[u8; 4] = b"MSC1";
const SYNC_ENVELOPE_SEND_ATTEMPTS: usize = 3;
static NEXT_SYNC_CHUNK_ID: AtomicU32 = AtomicU32::new(1);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::*;
    use crate::state::*;
    use crate::sync::*;
    use bytes::Bytes;
    use mica_driver::{CompioTaskDriver, EPHEMERAL_HOST_IDENTITY_START};
    use mica_host_protocol::dom_event_payload_json;
    use mica_host_protocol::{
        DomEventPayload, SUPPORTED_DOM_ATTRIBUTES, SUPPORTED_DOM_TAGS, SyncEnvelope,
        SyncMessageKind, decode_sync_envelope, encoded_sync_envelope, sync_payload_signature,
    };
    use mica_runtime::SourceRunner;
    use mica_var::{Identity, Symbol, Value};
    use std::collections::{BTreeMap, HashMap};
    use std::net::SocketAddr;
    use std::num::NonZeroUsize;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};
    use tokio::io::AsyncReadExt;

    type TestChunkMap = HashMap<u32, (u32, u32, Vec<Option<Vec<u8>>>)>;

    fn test_driver(runner: SourceRunner) -> CompioTaskDriver {
        CompioTaskDriver::spawn_with_workers(runner, NonZeroUsize::new(1)).unwrap()
    }

    #[test]
    fn webtransport_output_snapshot_replaces_queued_updates_for_same_view() {
        let output = SessionOutput::new();

        output
            .send_sync_envelope(test_envelope(SyncMessageKind::ViewDelta, 7, 21, 1))
            .unwrap();
        output.send_datagram(Bytes::from_static(b"plain")).unwrap();
        output
            .send_sync_envelope(test_envelope(SyncMessageKind::ViewDelta, 7, 22, 1))
            .unwrap();
        output
            .send_sync_envelope(test_envelope(SyncMessageKind::ViewSnapshot, 7, 21, 2))
            .unwrap();

        let messages = output.drain_batch(8);
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0], SessionOutputMessage::Datagram(_)));
        assert!(matches!(
            &messages[1],
            SessionOutputMessage::SyncEnvelope(envelope)
                if envelope.view_id == 22 && envelope.kind == SyncMessageKind::ViewDelta
        ));
        assert!(matches!(
            &messages[2],
            SessionOutputMessage::SyncEnvelope(envelope)
                if envelope.view_id == 21
                    && envelope.kind == SyncMessageKind::ViewSnapshot
                    && envelope.server_revision == 2
        ));
    }

    #[test]
    fn sync_client_accepts_protocol_tags() {
        let client = include_str!("../sync-client.js");

        for tag in SUPPORTED_DOM_TAGS {
            assert!(
                client.contains(&format!("\"{tag}\"")),
                "sync-client.js is missing supported DOM tag {tag}"
            );
        }
    }

    #[test]
    fn sync_client_accepts_protocol_attributes() {
        let client = include_str!("../sync-client.js");

        for attr in SUPPORTED_DOM_ATTRIBUTES {
            assert!(
                client.contains(&format!("\"{attr}\"")),
                "sync-client.js is missing supported DOM attribute {attr}"
            );
        }
    }

    #[test]
    fn effect_datagrams_preserve_bytes_and_strings() {
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();
        assert_eq!(
            effect_datagram(endpoint, &Value::bytes([0xde, 0xad, 0xbe, 0xef])).as_ref(),
            &[0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            effect_datagram(endpoint, &Value::string("hello")).as_ref(),
            b"hello"
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

    #[test]
    fn sync_effect_values_encode_view_datagrams() {
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();
        let datagram = effect_datagram(
            endpoint,
            &Value::list([
                Value::symbol(Symbol::intern("view_delta")),
                Value::identity(endpoint),
                Value::int(9).unwrap(),
                Value::int(1).unwrap(),
                Value::int(2).unwrap(),
                Value::int(3).unwrap(),
                Value::int(4).unwrap(),
                Value::string("patch"),
            ]),
        );
        let envelope = decode_sync_envelope(&datagram).unwrap();

        assert_eq!(envelope.kind, SyncMessageKind::ViewDelta);
        assert_eq!(envelope.session_id, endpoint.raw());
        assert_eq!(envelope.view_id, 9);
        assert_eq!(envelope.client_revision, 1);
        assert_eq!(envelope.client_signature, 2);
        assert_eq!(envelope.server_revision, 3);
        assert_eq!(envelope.server_signature, 4);
        assert_eq!(envelope.payload, b"patch");
    }

    #[test]
    fn session_sync_state_tracks_client_active_views() {
        let mut state = SessionSyncState::default();
        state.record_incoming_view(&SyncEnvelope {
            kind: SyncMessageKind::NeedView,
            session_id: 7,
            view_id: 11,
            client_revision: 0,
            client_signature: 0,
            server_revision: 0,
            server_signature: 0,
            payload: Vec::new(),
        });
        state.record_incoming_view(&SyncEnvelope {
            kind: SyncMessageKind::ViewSnapshot,
            session_id: 7,
            view_id: 12,
            client_revision: 0,
            client_signature: 0,
            server_revision: 0,
            server_signature: 0,
            payload: Vec::new(),
        });

        assert!(state.has_active_view(7, 11));
        assert!(!state.has_active_view(7, 12));
    }

    #[test]
    fn active_sync_views_snapshot_does_not_hold_session_lock() {
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let state = SessionState::new();
        state
            .sync
            .lock()
            .unwrap()
            .record_incoming_view(&SyncEnvelope {
                kind: SyncMessageKind::NeedView,
                session_id: 7,
                view_id: 11,
                client_revision: 0,
                client_signature: 0,
                server_revision: 0,
                server_signature: 0,
                payload: Vec::new(),
            });
        sessions.lock().unwrap().insert(endpoint, state.clone());

        assert_eq!(
            active_sync_views(&sessions),
            vec![ActiveSyncView {
                endpoint,
                session_id: 7,
                view_id: 11,
                client_revision: 0,
                client_signature: 0,
                server_revision: 0,
                server_signature: 0,
                last_tree: None,
            }]
        );
        assert!(state.sync.try_lock().is_ok());
    }

    #[test]
    fn drop_session_writer_removes_active_views() {
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();
        let host = InProcessWebTransportHost::new_without_event_pump(test_driver(
            SourceRunner::new_empty(),
        ));
        let state = SessionState::new();
        state
            .sync
            .lock()
            .unwrap()
            .record_incoming_view(&SyncEnvelope {
                kind: SyncMessageKind::HaveView,
                session_id: 7,
                view_id: 11,
                client_revision: 1,
                client_signature: 305,
                server_revision: 1,
                server_signature: 305,
                payload: Vec::new(),
            });
        host.sessions.lock().unwrap().insert(endpoint, state);

        assert_eq!(host.active_sync_views().len(), 1);
        drop_session_writer(&host, endpoint);
        assert!(host.active_sync_views().is_empty());
    }

    #[test]
    fn endpoint_allocation_uses_webtransport_identity_space() {
        let host = InProcessWebTransportHost::new_without_event_pump(test_driver(
            SourceRunner::new_empty(),
        ));
        assert_eq!(
            host.allocate_endpoint().unwrap(),
            Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap()
        );
    }

    #[test]
    fn routed_effect_reaches_session_output() {
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();
        let output = SessionOutput::new();
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert(
            endpoint,
            Arc::new(SessionState {
                output: output.clone(),
                sync: Mutex::new(SessionSyncState::default()),
            }),
        );

        route_driver_effect(
            &sessions,
            mica_runtime::Effect {
                task_id: 1,
                target: endpoint,
                value: Value::string("hello"),
            },
        );

        assert_eq!(output.try_recv().unwrap().as_ref(), b"hello");
    }

    #[test]
    fn unrelated_completed_task_does_not_refresh_views() {
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();
        let output = SessionOutput::new();
        let sync = Mutex::new(SessionSyncState::default());
        sync.lock().unwrap().pending_tasks.insert(
            11,
            PendingSyncTask {
                session_id: 7,
                view_id: 9,
                refresh: true,
                action: "look".to_owned(),
            },
        );
        let sessions = Arc::new(Mutex::new(HashMap::from([(
            endpoint,
            Arc::new(SessionState { output, sync }),
        )])));

        let routed = take_pending_sync_task(&sessions, 12);

        assert!(routed.is_none());
        assert_eq!(
            sessions
                .lock()
                .unwrap()
                .get(&endpoint)
                .unwrap()
                .sync
                .lock()
                .unwrap()
                .pending_tasks,
            HashMap::from([(
                11,
                PendingSyncTask {
                    session_id: 7,
                    view_id: 9,
                    refresh: true,
                    action: "look".to_owned(),
                },
            )])
        );
    }

    #[test]
    fn webtransport_sync_need_view_renders_snapshot() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let tls = test_tls_config();
            let endpoint = bind_server_endpoint("127.0.0.1:0".parse().unwrap(), tls)
                .await
                .unwrap();
            let server_addr = endpoint.local_addr().unwrap();
            let server_endpoint = endpoint.clone();

            let runner = sync_chat_runner();
            let principal = runner.named_identity(Symbol::intern("web")).unwrap();
            let driver = test_driver(runner);
            let host = InProcessWebTransportHost::new(driver.clone());
            let binding = SessionBinding {
                principal,
                actor: None,
            };
            compio::runtime::spawn(serve_in_process(endpoint, host, binding, Some(1))).detach();

            let (connected_tx, connected_rx) = mpsc::channel();
            let (send_tx, send_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let request = encoded_sync_envelope(
                SyncEnvelope {
                    kind: SyncMessageKind::NeedView,
                    session_id: 7,
                    view_id: 11,
                    client_revision: 13,
                    client_signature: 17,
                    server_revision: 19,
                    server_signature: 23,
                    payload: b"need".to_vec(),
                }
                .as_ref(),
            );
            let client = spawn_wtransport_smoke_client(
                server_addr,
                request,
                connected_tx,
                send_rx,
                result_tx,
            );

            wait_for_client_connected(&connected_rx).await;
            send_tx.send(()).unwrap();
            let received = wait_for_client_result(&result_rx).await.unwrap();
            let envelope = decode_sync_envelope(&received).unwrap();

            server_endpoint.close(0u32.into(), b"test complete");
            client.join().unwrap();
            assert_eq!(envelope.kind, SyncMessageKind::ViewSnapshot);
            assert_eq!(envelope.session_id, 7);
            assert_eq!(envelope.view_id, 11);
            assert_eq!(envelope.client_revision, 13);
            assert_eq!(envelope.client_signature, 17);
            assert_eq!(envelope.server_revision, 14);
            assert_eq!(
                envelope.server_signature,
                sync_payload_signature(envelope.server_revision, &envelope.payload)
            );
            let payload: serde_json::Value = serde_json::from_slice(&envelope.payload).unwrap();
            assert_eq!(payload["view"], 11);
            assert_eq!(payload["revision"], 14);
            assert_eq!(payload["root"]["tag"], "main");
            assert_eq!(payload["root"]["attrs"]["id"], "chat-root");
            assert_eq!(
                payload["root"]["children"][0]["children"][0]["children"][0]["children"][0]["text"],
                "alice"
            );
            assert_eq!(payload["root"]["children"][1]["tag"], "form");
            assert_eq!(
                payload["root"]["children"][1]["attrs"]["data-sync-action"],
                "chat_post"
            );
        });
    }

    #[test]
    fn webtransport_sync_event_pushes_chat_delta() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let tls = test_tls_config();
            let endpoint = bind_server_endpoint("127.0.0.1:0".parse().unwrap(), tls)
                .await
                .unwrap();
            let server_addr = endpoint.local_addr().unwrap();
            let server_endpoint = endpoint.clone();

            let runner = sync_chat_runner();
            let principal = runner.named_identity(Symbol::intern("web")).unwrap();
            let driver = test_driver(runner);
            let host = InProcessWebTransportHost::new(driver.clone());
            let binding = SessionBinding {
                principal,
                actor: None,
            };
            compio::runtime::spawn(serve_in_process(endpoint, host, binding, Some(1))).detach();

            let (connected_tx, connected_rx) = mpsc::channel();
            let (send_tx, send_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let client =
                spawn_wtransport_dom_event_client(server_addr, connected_tx, send_rx, result_tx);

            wait_for_client_connected(&connected_rx).await;
            send_tx.send(()).unwrap();
            let (snapshot, delta) = wait_for_ack_client_result(&result_rx).await.unwrap();

            server_endpoint.close(0u32.into(), b"test complete");
            client.join().unwrap();
            assert_eq!(snapshot.kind, SyncMessageKind::ViewSnapshot);
            assert_eq!(delta.kind, SyncMessageKind::ViewDelta);
            assert_eq!(delta.session_id, 7);
            assert_eq!(delta.view_id, 11);
            assert_eq!(delta.client_revision, 1);
            assert_eq!(delta.server_revision, 2);
            assert_ne!(delta.server_signature, 0);
            let payload: serde_json::Value = serde_json::from_slice(&delta.payload).unwrap();
            assert_eq!(payload["type"], "dom_patch");
            assert_eq!(payload["view"], 11);
            assert_eq!(payload["revision"], 2);
            assert_eq!(payload["patches"][0]["op"], "append_child");
            assert_eq!(payload["patches"][0]["path"], serde_json::json!([0]));
            assert_eq!(payload["patches"][0]["node"]["tag"], "li");
            assert_eq!(
                payload["patches"][0]["node"]["children"][0]["children"][0]["text"],
                "bob"
            );
            assert_eq!(
                payload["patches"][0]["node"]["children"][2]["text"],
                "hello from sync event"
            );
        });
    }

    #[test]
    fn webtransport_noop_dom_event_sends_same_revision_ack() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let tls = test_tls_config();
            let endpoint = bind_server_endpoint("127.0.0.1:0".parse().unwrap(), tls)
                .await
                .unwrap();
            let server_addr = endpoint.local_addr().unwrap();
            let server_endpoint = endpoint.clone();

            let runner = sync_chat_runner();
            let principal = runner.named_identity(Symbol::intern("web")).unwrap();
            let driver = test_driver(runner);
            let host = InProcessWebTransportHost::new(driver.clone());
            let binding = SessionBinding {
                principal,
                actor: None,
            };
            compio::runtime::spawn(serve_in_process(endpoint, host, binding, Some(1))).detach();

            let (connected_tx, connected_rx) = mpsc::channel();
            let (send_tx, send_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let client =
                spawn_wtransport_noop_event_client(server_addr, connected_tx, send_rx, result_tx);

            wait_for_client_connected(&connected_rx).await;
            send_tx.send(()).unwrap();
            let (snapshot, ack) = wait_for_ack_client_result(&result_rx).await.unwrap();

            server_endpoint.close(0u32.into(), b"test complete");
            client.join().unwrap();
            assert_eq!(snapshot.kind, SyncMessageKind::ViewSnapshot);
            assert_eq!(ack.kind, SyncMessageKind::ViewDelta);
            assert_eq!(ack.client_revision, snapshot.server_revision);
            assert_eq!(ack.server_revision, snapshot.server_revision);
            assert_eq!(ack.client_signature, snapshot.server_signature);
            assert_eq!(ack.server_signature, snapshot.server_signature);
            let payload: serde_json::Value = serde_json::from_slice(&ack.payload).unwrap();
            assert_eq!(payload["type"], "dom_patch");
            assert_eq!(payload["patches"], serde_json::json!([]));
        });
    }

    #[test]
    fn webtransport_stale_dom_event_is_still_processed() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let tls = test_tls_config();
            let endpoint = bind_server_endpoint("127.0.0.1:0".parse().unwrap(), tls)
                .await
                .unwrap();
            let server_addr = endpoint.local_addr().unwrap();
            let server_endpoint = endpoint.clone();

            let runner = sync_chat_runner();
            let principal = runner.named_identity(Symbol::intern("web")).unwrap();
            let driver = test_driver(runner);
            let host = InProcessWebTransportHost::new(driver.clone());
            let binding = SessionBinding {
                principal,
                actor: None,
            };
            compio::runtime::spawn(serve_in_process(endpoint, host, binding, Some(1))).detach();

            let (connected_tx, connected_rx) = mpsc::channel();
            let (send_tx, send_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let client =
                spawn_wtransport_stale_event_client(server_addr, connected_tx, send_rx, result_tx);

            wait_for_client_connected(&connected_rx).await;
            send_tx.send(()).unwrap();
            let delta = wait_for_snapshot_client_result(&result_rx).await.unwrap();

            server_endpoint.close(0u32.into(), b"test complete");
            client.join().unwrap();
            assert_eq!(delta.kind, SyncMessageKind::ViewDelta);
            assert!(delta.client_revision < delta.server_revision);
            assert!(delta.server_revision > 1);
            let payload: serde_json::Value = serde_json::from_slice(&delta.payload).unwrap();
            let payload_text = serde_json::to_string(&payload).unwrap();
            assert!(payload_text.contains("bob"));
            assert!(payload_text.contains("stale"));
        });
    }

    #[test]
    fn webtransport_have_view_ack_does_not_snapshot_current_state() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let tls = test_tls_config();
            let endpoint = bind_server_endpoint("127.0.0.1:0".parse().unwrap(), tls)
                .await
                .unwrap();
            let server_addr = endpoint.local_addr().unwrap();
            let server_endpoint = endpoint.clone();

            let runner = sync_chat_runner();
            let principal = runner.named_identity(Symbol::intern("web")).unwrap();
            let driver = test_driver(runner);
            let host = InProcessWebTransportHost::new(driver.clone());
            let binding = SessionBinding {
                principal,
                actor: None,
            };
            compio::runtime::spawn(serve_in_process(endpoint, host, binding, Some(1))).detach();

            let (connected_tx, connected_rx) = mpsc::channel();
            let (send_tx, send_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let client = spawn_wtransport_ack_client(server_addr, connected_tx, send_rx, result_tx);

            wait_for_client_connected(&connected_rx).await;
            send_tx.send(()).unwrap();
            let (snapshot, delta) = wait_for_ack_client_result(&result_rx).await.unwrap();

            server_endpoint.close(0u32.into(), b"test complete");
            client.join().unwrap();
            assert_eq!(snapshot.kind, SyncMessageKind::ViewSnapshot);
            assert_eq!(delta.kind, SyncMessageKind::ViewDelta);
            assert_eq!(delta.server_revision, 2);
            assert_ne!(delta.server_signature, 0);
        });
    }

    fn test_tls_config() -> WebTransportTlsConfig {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        WebTransportTlsConfig {
            cert_chain: vec![cert.der().clone()],
            key_der: signing_key.serialize_der().try_into().unwrap(),
        }
    }

    fn sync_chat_runner() -> SourceRunner {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/sync-host.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/chat/sync.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/shared/sync-dom.mica"))
            .unwrap();
        runner
            .run_filein_with_include_loader(
                include_str!("../../../apps/chat/http.mica"),
                chat_http_include,
            )
            .unwrap();
        runner
    }

    fn chat_http_include(path: &str) -> Result<String, String> {
        match path {
            "style.css" => Ok(include_str!("../../../apps/chat/style.css").to_owned()),
            "bootstrap.js" => Ok(include_str!("../../../apps/chat/bootstrap.js").to_owned()),
            other => Err(format!("unknown chat HTTP include {other}")),
        }
    }

    fn spawn_wtransport_smoke_client(
        server_addr: SocketAddr,
        request: Vec<u8>,
        connected_tx: mpsc::Sender<()>,
        send_rx: mpsc::Receiver<()>,
        result_tx: mpsc::Sender<Result<Vec<u8>, String>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async move {
                        let config = wtransport::ClientConfig::builder()
                            .with_bind_default()
                            .with_no_cert_validation()
                            .build();
                        let url = format!("https://127.0.0.1:{}/view", server_addr.port());
                        let connection = wtransport::Endpoint::client(config)
                            .map_err(|error| error.to_string())?
                            .connect(&url)
                            .await
                            .map_err(|error| error.to_string())?;
                        connected_tx.send(()).map_err(|error| error.to_string())?;
                        send_rx.recv().map_err(|error| error.to_string())?;
                        connection
                            .send_datagram(request)
                            .map_err(|error| error.to_string())?;
                        let datagram = tokio::time::timeout(
                            Duration::from_secs(3),
                            connection.receive_datagram(),
                        )
                        .await
                        .map_err(|_| "timed out waiting for WebTransport datagram".to_owned())?
                        .map_err(|error| error.to_string())?;
                        Ok(datagram.payload().to_vec())
                    })
                });
            let _ = result_tx.send(result);
        })
    }

    fn spawn_wtransport_dom_event_client(
        server_addr: SocketAddr,
        connected_tx: mpsc::Sender<()>,
        send_rx: mpsc::Receiver<()>,
        result_tx: mpsc::Sender<Result<(SyncEnvelope, SyncEnvelope), String>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async move {
                        let config = wtransport::ClientConfig::builder()
                            .with_bind_default()
                            .with_no_cert_validation()
                            .build();
                        let url = format!("https://127.0.0.1:{}/view", server_addr.port());
                        let connection = wtransport::Endpoint::client(config)
                            .map_err(|error| error.to_string())?
                            .connect(&url)
                            .await
                            .map_err(|error| error.to_string())?;
                        connected_tx.send(()).map_err(|error| error.to_string())?;
                        send_rx.recv().map_err(|error| error.to_string())?;

                        let need_view = SyncEnvelope {
                            kind: SyncMessageKind::NeedView,
                            session_id: 7,
                            view_id: 11,
                            client_revision: 0,
                            client_signature: 0,
                            server_revision: 0,
                            server_signature: 0,
                            payload: b"need".to_vec(),
                        };
                        connection
                            .send_datagram(encoded_sync_envelope(need_view.as_ref()))
                            .map_err(|error| error.to_string())?;
                        let snapshot = receive_sync_envelope(&connection)
                            .await
                            .map_err(|error| format!("initial MUD snapshot: {error}"))?;

                        connection
                            .send_datagram(dom_event_payload_json(&DomEventPayload {
                                session_id: 7,
                                view_id: 11,
                                revision: snapshot.server_revision,
                                signature: snapshot.server_signature,
                                refresh: true,
                                event: "submit".to_owned(),
                                target: "chat-composer".to_owned(),
                                action: "chat_post".to_owned(),
                                fields: BTreeMap::from([
                                    ("actor".to_owned(), "bob".to_owned()),
                                    ("text".to_owned(), "hello from sync event".to_owned()),
                                ]),
                            }))
                            .map_err(|error| error.to_string())?;
                        let delta =
                            receive_newer_sync_envelope(&connection, snapshot.server_revision)
                                .await
                                .map_err(|error| format!("MUD login delta: {error}"))?;

                        Ok((snapshot, delta))
                    })
                });
            let _ = result_tx.send(result);
        })
    }

    fn spawn_wtransport_noop_event_client(
        server_addr: SocketAddr,
        connected_tx: mpsc::Sender<()>,
        send_rx: mpsc::Receiver<()>,
        result_tx: mpsc::Sender<Result<(SyncEnvelope, SyncEnvelope), String>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async move {
                        let config = wtransport::ClientConfig::builder()
                            .with_bind_default()
                            .with_no_cert_validation()
                            .build();
                        let url = format!("https://127.0.0.1:{}/view", server_addr.port());
                        let connection = wtransport::Endpoint::client(config)
                            .map_err(|error| error.to_string())?
                            .connect(&url)
                            .await
                            .map_err(|error| error.to_string())?;
                        connected_tx.send(()).map_err(|error| error.to_string())?;
                        send_rx.recv().map_err(|error| error.to_string())?;

                        let need_view = SyncEnvelope {
                            kind: SyncMessageKind::NeedView,
                            session_id: 7,
                            view_id: 11,
                            client_revision: 0,
                            client_signature: 0,
                            server_revision: 0,
                            server_signature: 0,
                            payload: b"need".to_vec(),
                        };
                        connection
                            .send_datagram(encoded_sync_envelope(need_view.as_ref()))
                            .map_err(|error| error.to_string())?;
                        let snapshot = receive_sync_envelope(&connection).await?;

                        connection
                            .send_datagram(dom_event_payload_json(&DomEventPayload {
                                session_id: 7,
                                view_id: 11,
                                revision: snapshot.server_revision,
                                signature: snapshot.server_signature,
                                refresh: true,
                                event: "submit".to_owned(),
                                target: "chat-composer".to_owned(),
                                action: "does_not_exist".to_owned(),
                                fields: BTreeMap::new(),
                            }))
                            .map_err(|error| error.to_string())?;
                        let ack = loop {
                            let envelope = receive_sync_envelope(&connection).await?;
                            if envelope.kind == SyncMessageKind::ViewDelta {
                                break envelope;
                            }
                        };

                        Ok((snapshot, ack))
                    })
                });
            let _ = result_tx.send(result);
        })
    }

    fn spawn_wtransport_stale_event_client(
        server_addr: SocketAddr,
        connected_tx: mpsc::Sender<()>,
        send_rx: mpsc::Receiver<()>,
        result_tx: mpsc::Sender<Result<SyncEnvelope, String>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async move {
                        let config = wtransport::ClientConfig::builder()
                            .with_bind_default()
                            .with_no_cert_validation()
                            .build();
                        let url = format!("https://127.0.0.1:{}/view", server_addr.port());
                        let connection = wtransport::Endpoint::client(config)
                            .map_err(|error| error.to_string())?
                            .connect(&url)
                            .await
                            .map_err(|error| error.to_string())?;
                        connected_tx.send(()).map_err(|error| error.to_string())?;
                        send_rx.recv().map_err(|error| error.to_string())?;

                        let need_view = SyncEnvelope {
                            kind: SyncMessageKind::NeedView,
                            session_id: 7,
                            view_id: 11,
                            client_revision: 0,
                            client_signature: 0,
                            server_revision: 0,
                            server_signature: 0,
                            payload: b"need".to_vec(),
                        };
                        connection
                            .send_datagram(encoded_sync_envelope(need_view.as_ref()))
                            .map_err(|error| error.to_string())?;
                        let snapshot = receive_sync_envelope(&connection).await?;
                        connection
                            .send_datagram(dom_event_payload_json(&DomEventPayload {
                                session_id: 7,
                                view_id: 11,
                                revision: snapshot.server_revision,
                                signature: 999,
                                refresh: true,
                                event: "submit".to_owned(),
                                target: "chat-composer".to_owned(),
                                action: "chat_post".to_owned(),
                                fields: BTreeMap::from([
                                    ("actor".to_owned(), "bob".to_owned()),
                                    ("text".to_owned(), "stale".to_owned()),
                                ]),
                            }))
                            .map_err(|error| error.to_string())?;
                        receive_newer_sync_envelope(&connection, snapshot.server_revision).await
                    })
                });
            let _ = result_tx.send(result);
        })
    }

    fn spawn_wtransport_ack_client(
        server_addr: SocketAddr,
        connected_tx: mpsc::Sender<()>,
        send_rx: mpsc::Receiver<()>,
        result_tx: mpsc::Sender<Result<(SyncEnvelope, SyncEnvelope), String>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async move {
                        let config = wtransport::ClientConfig::builder()
                            .with_bind_default()
                            .with_no_cert_validation()
                            .build();
                        let url = format!("https://127.0.0.1:{}/view", server_addr.port());
                        let connection = wtransport::Endpoint::client(config)
                            .map_err(|error| error.to_string())?
                            .connect(&url)
                            .await
                            .map_err(|error| error.to_string())?;
                        connected_tx.send(()).map_err(|error| error.to_string())?;
                        send_rx.recv().map_err(|error| error.to_string())?;

                        let need_view = SyncEnvelope {
                            kind: SyncMessageKind::NeedView,
                            session_id: 7,
                            view_id: 11,
                            client_revision: 0,
                            client_signature: 0,
                            server_revision: 0,
                            server_signature: 0,
                            payload: b"need".to_vec(),
                        };
                        connection
                            .send_datagram(encoded_sync_envelope(need_view.as_ref()))
                            .map_err(|error| error.to_string())?;
                        let snapshot = receive_sync_envelope(&connection).await?;

                        connection
                            .send_datagram(dom_event_payload_json(&DomEventPayload {
                                session_id: 7,
                                view_id: 11,
                                revision: snapshot.server_revision,
                                signature: snapshot.server_signature,
                                refresh: true,
                                event: "submit".to_owned(),
                                target: "chat-composer".to_owned(),
                                action: "chat_post".to_owned(),
                                fields: BTreeMap::from([
                                    ("actor".to_owned(), "bob".to_owned()),
                                    ("text".to_owned(), "ack check".to_owned()),
                                ]),
                            }))
                            .map_err(|error| error.to_string())?;
                        let delta =
                            receive_newer_sync_envelope(&connection, snapshot.server_revision)
                                .await?;

                        let have_view = SyncEnvelope {
                            kind: SyncMessageKind::HaveView,
                            session_id: 7,
                            view_id: 11,
                            client_revision: delta.server_revision,
                            client_signature: delta.server_signature,
                            server_revision: delta.server_revision,
                            server_signature: delta.server_signature,
                            payload: b"have".to_vec(),
                        };
                        connection
                            .send_datagram(encoded_sync_envelope(have_view.as_ref()))
                            .map_err(|error| error.to_string())?;
                        expect_no_newer_sync_envelope(
                            &connection,
                            delta.server_revision,
                            Duration::from_millis(200),
                        )
                        .await?;
                        Ok((snapshot, delta))
                    })
                });
            let _ = result_tx.send(result);
        })
    }

    async fn receive_sync_envelope(
        connection: &wtransport::Connection,
    ) -> Result<SyncEnvelope, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        receive_sync_envelope_until(connection, deadline).await
    }

    async fn receive_newer_sync_envelope(
        connection: &wtransport::Connection,
        current_revision: u64,
    ) -> Result<SyncEnvelope, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let envelope = receive_sync_envelope_until(connection, deadline).await?;
            if envelope.server_revision > current_revision {
                return Ok(envelope);
            }
        }
    }

    async fn expect_no_newer_sync_envelope(
        connection: &wtransport::Connection,
        current_revision: u64,
        duration: Duration,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + duration;
        loop {
            match receive_sync_envelope_until(connection, deadline).await {
                Ok(envelope) if envelope.server_revision > current_revision => {
                    return Err(format!("unexpected newer sync envelope: {envelope:?}"));
                }
                Ok(_) => {}
                Err(error) if error.starts_with("timed out waiting") => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    async fn receive_sync_envelope_until(
        connection: &wtransport::Connection,
        deadline: tokio::time::Instant,
    ) -> Result<SyncEnvelope, String> {
        let mut chunks: TestChunkMap = HashMap::new();
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err("timed out waiting for WebTransport datagram".to_owned());
            }
            let payload = tokio::select! {
                datagram = tokio::time::timeout_at(deadline, connection.receive_datagram()) => {
                    datagram
                        .map_err(|_| "timed out waiting for WebTransport datagram".to_owned())?
                        .map_err(|error| error.to_string())?
                        .payload()
                }
                stream = tokio::time::timeout_at(deadline, connection.accept_uni()) => {
                    let mut stream = stream
                        .map_err(|_| "timed out waiting for WebTransport stream".to_owned())?
                        .map_err(|error| error.to_string())?;
                    let mut payload = Vec::new();
                    stream
                        .read_to_end(&mut payload)
                        .await
                        .map_err(|error| error.to_string())?;
                    Bytes::from(payload)
                }
            };
            if !payload.starts_with(SYNC_CHUNK_MAGIC) {
                match decode_sync_envelope(&payload) {
                    Ok(envelope) => return Ok(envelope),
                    Err(_) => continue,
                }
            }
            if payload.len() < SYNC_CHUNK_HEADER_LEN {
                return Err("short sync chunk datagram".to_owned());
            }
            let message_id =
                u32::from_le_bytes(payload[4..8].try_into().map_err(|_| "bad chunk id")?);
            let index =
                u32::from_le_bytes(payload[8..12].try_into().map_err(|_| "bad chunk index")?);
            let count =
                u32::from_le_bytes(payload[12..16].try_into().map_err(|_| "bad chunk count")?);
            let total_len =
                u32::from_le_bytes(payload[16..20].try_into().map_err(|_| "bad chunk len")?);
            let chunk_len =
                u32::from_le_bytes(payload[20..24].try_into().map_err(|_| "bad chunk size")?);
            if count == 0
                || index >= count
                || chunk_len as usize > payload.len() - SYNC_CHUNK_HEADER_LEN
            {
                return Err("invalid sync chunk datagram".to_owned());
            }
            let entry = chunks.entry(message_id).or_insert_with(|| {
                (
                    count,
                    total_len,
                    vec![None; usize::try_from(count).unwrap_or(0)],
                )
            });
            if entry.0 != count || entry.1 != total_len {
                return Err("inconsistent sync chunk datagram".to_owned());
            }
            entry.2[index as usize] = Some(
                payload[SYNC_CHUNK_HEADER_LEN..SYNC_CHUNK_HEADER_LEN + chunk_len as usize].to_vec(),
            );
            if entry.2.iter().all(Option::is_some) {
                let mut encoded = Vec::with_capacity(total_len as usize);
                for part in &entry.2 {
                    encoded.extend_from_slice(part.as_ref().unwrap());
                }
                if encoded.len() != total_len as usize {
                    return Err("sync chunk length mismatch".to_owned());
                }
                return decode_sync_envelope(&encoded).map_err(|error| error.to_string());
            }
        }
    }

    async fn wait_for_client_connected(receiver: &mpsc::Receiver<()>) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match receiver.try_recv() {
                Ok(()) => return,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(mpsc::TryRecvError::Empty) => panic!("timed out waiting for client connect"),
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("client disconnected before connect")
                }
            }
        }
    }

    async fn wait_for_client_result(
        receiver: &mpsc::Receiver<Result<Vec<u8>, String>>,
    ) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match receiver.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    return Err("timed out waiting for client result".to_owned());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("client result channel disconnected".to_owned());
                }
            }
        }
    }

    async fn wait_for_snapshot_client_result(
        receiver: &mpsc::Receiver<Result<SyncEnvelope, String>>,
    ) -> Result<SyncEnvelope, String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match receiver.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    return Err("timed out waiting for client result".to_owned());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("client result channel disconnected".to_owned());
                }
            }
        }
    }

    async fn wait_for_ack_client_result(
        receiver: &mpsc::Receiver<Result<(SyncEnvelope, SyncEnvelope), String>>,
    ) -> Result<(SyncEnvelope, SyncEnvelope), String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match receiver.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    compio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    return Err("timed out waiting for client result".to_owned());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("client result channel disconnected".to_owned());
                }
            }
        }
    }
}
