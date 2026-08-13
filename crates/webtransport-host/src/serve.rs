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

use crate::ENDPOINT_OUTPUT_DRAIN_DATAGRAMS;
use crate::metrics::ConnectionErrorKind;
use crate::state::{
    InProcessWebTransportHost, SessionBinding, SessionOutput, SessionOutputMessage,
    SessionOutputReady, SessionState, WebTransportTlsConfig, format_driver_error,
};
use crate::sync::route_incoming_datagram;
use bytes::{Buf, Bytes};
use compio_quic::h3::quic::RecvStream as H3RecvStream;
use compio_quic::{Endpoint, ServerBuilder};
use h3_webtransport::server::WebTransportSession;
use mica_driver::EndpointConfiguration;
use mica_var::{Identity, Symbol};
use std::future::poll_fn;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

type H3RequestStream =
    compio_quic::h3::server::RequestStream<compio_quic::h3::BidiStream<Bytes>, Bytes>;
type WtSession = WebTransportSession<compio_quic::Connection, Bytes>;

pub async fn bind_server_endpoint(
    bind: SocketAddr,
    tls: WebTransportTlsConfig,
) -> Result<Endpoint, String> {
    ServerBuilder::new_with_single_cert(tls.cert_chain, tls.key_der)
        .map_err(|error| format!("failed to configure WebTransport TLS: {error}"))?
        .with_alpn_protocols(&["h3"])
        .bind(bind)
        .await
        .map_err(|error| format!("failed to bind WebTransport listener {bind}: {error}"))
}

pub async fn serve_in_process(
    endpoint: Endpoint,
    host: InProcessWebTransportHost,
    binding: SessionBinding,
    max_connections: Option<usize>,
) -> Result<(), String> {
    let host = Arc::new(host);
    let mut accepted = 0usize;
    while let Some(incoming) = endpoint.wait_incoming().await {
        let host = host.clone();
        let binding = binding.clone();
        compio::runtime::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    crate::metrics::metrics().connections_accepted.inc();
                    if let Err(error) = handle_quic_connection(connection, host, binding).await {
                        tracing::warn!(error = %error, "WebTransport connection failed");
                    }
                }
                Err(error) => {
                    crate::metrics::metrics()
                        .connection_errors
                        .inc(ConnectionErrorKind::Handshake);
                    tracing::warn!(error = %error, "WebTransport handshake failed");
                }
            }
        })
        .detach();
        accepted += 1;
        if max_connections.is_some_and(|max| accepted >= max) {
            break;
        }
    }
    Ok(())
}

async fn handle_quic_connection(
    connection: compio_quic::Connection,
    host: Arc<InProcessWebTransportHost>,
    binding: SessionBinding,
) -> Result<(), String> {
    let mut builder = compio_quic::h3::server::builder();
    builder
        .enable_extended_connect(true)
        .enable_datagram(true)
        .enable_webtransport(true)
        .max_webtransport_sessions(1);
    let mut connection = builder
        .build::<_, Bytes>(connection)
        .await
        .map_err(|error| {
            crate::metrics::metrics()
                .connection_errors
                .inc(ConnectionErrorKind::Http3);
            format!("failed to start HTTP/3 connection: {error}")
        })?;

    loop {
        let Some(resolver) = connection.accept().await.map_err(|error| {
            crate::metrics::metrics()
                .connection_errors
                .inc(ConnectionErrorKind::Request);
            format!("failed to accept HTTP/3 request: {error}")
        })?
        else {
            return Ok(());
        };
        let (request, stream) = resolver.resolve_request().await.map_err(|error| {
            crate::metrics::metrics()
                .connection_errors
                .inc(ConnectionErrorKind::Request);
            format!("failed to resolve HTTP/3 request: {error}")
        })?;
        if is_webtransport_connect(&request) {
            let session = WebTransportSession::accept(request, stream, connection)
                .await
                .map_err(|error| {
                    crate::metrics::metrics()
                        .connection_errors
                        .inc(ConnectionErrorKind::Session);
                    format!("failed to accept WebTransport session: {error}")
                })?;
            return handle_session(Rc::new(session), host, binding).await;
        }
        reject_non_webtransport_request(stream).await?;
    }
}

fn is_webtransport_connect(request: &http::Request<()>) -> bool {
    let protocol = request.extensions().get::<compio_quic::h3::ext::Protocol>();
    matches!(
        (request.method(), protocol),
        (&http::Method::CONNECT, Some(protocol))
            if protocol == &compio_quic::h3::ext::Protocol::WEB_TRANSPORT
    )
}

async fn reject_non_webtransport_request(mut stream: H3RequestStream) -> Result<(), String> {
    let response = http::Response::builder()
        .status(http::StatusCode::NOT_FOUND)
        .body(())
        .map_err(|error| format!("failed to build HTTP/3 response: {error}"))?;
    stream
        .send_response(response)
        .await
        .map_err(|error| format!("failed to reject HTTP/3 request: {error}"))
}

async fn handle_session(
    session: Rc<WtSession>,
    host: Arc<InProcessWebTransportHost>,
    binding: SessionBinding,
) -> Result<(), String> {
    let endpoint = host.allocate_endpoint()?;
    let mut configuration = EndpointConfiguration::new(Symbol::intern("webtransport"))
        .endpoint(endpoint)
        .principal(binding.principal);
    if let Some(actor) = binding.actor {
        configuration = configuration.actor(actor);
    }
    let endpoint_session = host
        .client
        .open_endpoint(configuration)
        .map_err(|error| format_driver_error(&host.client, error))?;
    let state = SessionState::new(endpoint_session.clone());
    let output = state.output.clone();
    {
        let mut sessions = host.sessions.lock().unwrap();
        sessions.insert(endpoint, state);
        crate::metrics::metrics()
            .active_sessions
            .set(sessions.len() as i64);
    }
    crate::metrics::metrics().sessions_accepted.inc();
    let writer = compio::runtime::spawn(write_datagram_loop(session.clone(), output));
    let stream_reader = compio::runtime::spawn(read_uni_stream_loop(
        session.clone(),
        host.clone(),
        endpoint,
    ));
    let result = read_datagram_loop(session, &host, endpoint).await;
    let _ = endpoint_session.close_in_background();
    drop_session_writer(&host, endpoint);
    let _ = match writer.await {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    let _ = match stream_reader.await {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    result
}

async fn read_datagram_loop(
    session: Rc<WtSession>,
    host: &InProcessWebTransportHost,
    endpoint: Identity,
) -> Result<(), String> {
    let mut reader = session.datagram_reader();
    loop {
        let datagram = match reader.read_datagram().await {
            Ok(datagram) => datagram,
            Err(error) => {
                let message = error.to_string();
                if message.contains("closed") {
                    return Ok(());
                }
                crate::metrics::metrics()
                    .connection_errors
                    .inc(ConnectionErrorKind::DatagramRead);
                return Err(format!("failed to read WebTransport datagram: {message}"));
            }
        };
        route_incoming_datagram(host, endpoint, datagram.into_payload()).await?;
    }
}

async fn read_uni_stream_loop(
    session: Rc<WtSession>,
    host: Arc<InProcessWebTransportHost>,
    endpoint: Identity,
) -> Result<(), String> {
    loop {
        let stream = match session.accept_uni().await {
            Ok(Some((_session_id, stream))) => stream,
            Ok(None) => return Ok(()),
            Err(error) => {
                let message = error.to_string();
                if message.contains("closed") {
                    return Ok(());
                }
                crate::metrics::metrics()
                    .connection_errors
                    .inc(ConnectionErrorKind::UniStreamRead);
                return Err(format!("failed to accept WebTransport stream: {message}"));
            }
        };
        let payload = read_uni_stream_payload(stream).await?;
        crate::metrics::metrics().incoming_uni_streams.inc();
        crate::metrics::metrics()
            .incoming_uni_stream_bytes
            .add(payload.len() as isize);
        if let Err(error) = route_incoming_datagram(&host, endpoint, payload).await {
            tracing::warn!(
                error = %error,
                "failed to route WebTransport stream payload"
            );
        }
    }
}

async fn read_uni_stream_payload<S>(mut stream: S) -> Result<Bytes, String>
where
    S: H3RecvStream<Buf = Bytes>,
{
    let mut payload = Vec::new();
    loop {
        let chunk = poll_fn(|cx| stream.poll_data(cx)).await.map_err(|error| {
            crate::metrics::metrics()
                .connection_errors
                .inc(ConnectionErrorKind::UniStreamRead);
            format!("failed to read WebTransport stream: {error}")
        })?;
        let Some(mut chunk) = chunk else {
            return Ok(Bytes::from(payload));
        };
        while chunk.has_remaining() {
            let bytes = chunk.copy_to_bytes(chunk.remaining());
            payload.extend_from_slice(&bytes);
        }
    }
}

async fn write_datagram_loop(
    session: Rc<WtSession>,
    output: Arc<SessionOutput>,
) -> Result<(), String> {
    let mut sender = session.datagram_sender();
    while let SessionOutputReady::Ready { .. } | SessionOutputReady::HighWater { .. } =
        output.recv().await
    {
        for message in output.drain_batch(ENDPOINT_OUTPUT_DRAIN_DATAGRAMS) {
            match message {
                SessionOutputMessage::Datagram(datagram) => {
                    crate::metrics::metrics().outgoing_datagrams.inc();
                    crate::metrics::metrics()
                        .outgoing_bytes
                        .add(datagram.len() as isize);
                    sender.send_datagram(datagram).map_err(|error| {
                        crate::metrics::metrics()
                            .connection_errors
                            .inc(ConnectionErrorKind::DatagramWrite);
                        format!("failed to send WebTransport datagram: {error}")
                    })?;
                    compio::time::sleep(Duration::from_millis(2)).await;
                }
                SessionOutputMessage::SyncEnvelope(envelope) => {
                    let datagrams = crate::sync::sync_envelope_datagrams(envelope.as_ref());
                    for _ in 0..crate::SYNC_ENVELOPE_SEND_ATTEMPTS {
                        for datagram in &datagrams {
                            crate::metrics::metrics().outgoing_datagrams.inc();
                            crate::metrics::metrics()
                                .outgoing_bytes
                                .add(datagram.len() as isize);
                            sender.send_datagram(datagram.clone()).map_err(|error| {
                                crate::metrics::metrics()
                                    .connection_errors
                                    .inc(ConnectionErrorKind::DatagramWrite);
                                format!("failed to send WebTransport datagram: {error}")
                            })?;
                            compio::time::sleep(Duration::from_millis(2)).await;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn drop_session_writer(host: &InProcessWebTransportHost, endpoint: Identity) {
    if let Some(state) = host.sessions.lock().unwrap().remove(&endpoint) {
        host.cancel_session_subscriptions(&state);
        state.output.close();
    }
    crate::metrics::metrics()
        .active_sessions
        .set(host.sessions.lock().unwrap().len() as i64);
}
