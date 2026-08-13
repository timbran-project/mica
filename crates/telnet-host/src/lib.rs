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

use compio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use compio::net::{TcpListener, TcpStream};
use mica_driver::{CompioTaskDriver, DriverEvent};
use mica_host_protocol::{HostMessage, PROTOCOL_VERSION};
use mica_host_zmq::{ZmqHostSocket, ZmqSocketOptions};
use mica_runtime::{SuspendKind, TaskOutcome};
use mica_var::{Identity, Symbol, Value};
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::io::ErrorKind;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

pub mod codec;

use crate::codec::{TelnetCodec, TelnetCodecError, TelnetItem, encode_telnet_line};

pub const DEFAULT_BIND: &str = "127.0.0.1:7777";
pub const DAEMON_ENDPOINT_ID_START: u64 = 0x00ed_0000_0000_0000;

const ENDPOINT_OUTPUT_HIGH_WATER_LINES: usize = 128;
const ENDPOINT_OUTPUT_DRAIN_LINES: usize = 64;

#[derive(Clone, Debug)]
pub struct ActorBinding {
    pub name: String,
    pub identity: Identity,
}

pub struct InProcessTelnetHost {
    driver: Arc<CompioTaskDriver>,
    endpoints: Arc<Mutex<BTreeMap<Identity, Arc<EndpointOutput>>>>,
    stop_events: Arc<AtomicBool>,
    next_endpoint: AtomicU64,
}

pub struct ZmqTelnetHost {
    context: Arc<zmq::Context>,
    rpc_endpoint: String,
    options: ZmqSocketOptions,
    host_name: String,
    next_endpoint: AtomicU64,
}

#[derive(Default)]
struct EndpointOutput {
    state: Mutex<EndpointOutputState>,
}

#[derive(Default)]
struct EndpointOutputState {
    lines: VecDeque<String>,
    closed: bool,
    waker: Option<Waker>,
}

struct EndpointOutputRecv<'a> {
    output: &'a EndpointOutput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EndpointOutputReady {
    Ready { buffered: usize },
    HighWater { buffered: usize },
    Closed,
}

struct ZmqSession {
    socket: ZmqHostSocket,
    endpoint: Identity,
    actor: Identity,
    next_request: u64,
}

impl InProcessTelnetHost {
    pub fn new(driver: CompioTaskDriver) -> Self {
        let driver = Arc::new(driver);
        let endpoints = Arc::new(Mutex::new(BTreeMap::new()));
        let stop_events = Arc::new(AtomicBool::new(false));
        start_event_pump(driver.clone(), endpoints.clone(), stop_events.clone());
        Self {
            driver,
            endpoints,
            stop_events,
            next_endpoint: AtomicU64::new(DAEMON_ENDPOINT_ID_START),
        }
    }

    #[cfg(test)]
    fn new_without_event_pump(driver: CompioTaskDriver) -> Self {
        Self {
            driver: Arc::new(driver),
            endpoints: Arc::new(Mutex::new(BTreeMap::new())),
            stop_events: Arc::new(AtomicBool::new(false)),
            next_endpoint: AtomicU64::new(DAEMON_ENDPOINT_ID_START),
        }
    }

    fn allocate_endpoint(&self) -> Result<Identity, String> {
        let raw = self.next_endpoint.fetch_add(1, Ordering::Relaxed);
        Identity::new(raw).ok_or_else(|| "endpoint identity space is exhausted".to_owned())
    }
}

impl Drop for InProcessTelnetHost {
    fn drop(&mut self) {
        self.stop_events.store(true, Ordering::Relaxed);
    }
}

impl ZmqTelnetHost {
    pub fn new(rpc_endpoint: impl Into<String>) -> Self {
        Self::with_context(
            Arc::new(zmq::Context::new()),
            rpc_endpoint,
            ZmqSocketOptions::default(),
            "mica-telnet-host",
        )
    }

    pub fn with_context(
        context: Arc<zmq::Context>,
        rpc_endpoint: impl Into<String>,
        options: ZmqSocketOptions,
        host_name: impl Into<String>,
    ) -> Self {
        Self {
            context,
            rpc_endpoint: rpc_endpoint.into(),
            options,
            host_name: host_name.into(),
            next_endpoint: AtomicU64::new(DAEMON_ENDPOINT_ID_START),
        }
    }

    fn allocate_endpoint(&self) -> Result<Identity, String> {
        let raw = self.next_endpoint.fetch_add(1, Ordering::Relaxed);
        Identity::new(raw).ok_or_else(|| "endpoint identity space is exhausted".to_owned())
    }

    async fn connect_session(
        &self,
        endpoint: Identity,
        actor_name: &str,
    ) -> Result<ZmqSession, String> {
        let socket =
            ZmqHostSocket::connect(&self.context, zmq::DEALER, &self.rpc_endpoint, self.options)
                .map_err(|error| format!("failed to connect RPC socket: {error}"))?;
        let mut session = ZmqSession {
            socket,
            endpoint,
            actor: endpoint,
            next_request: 1,
        };
        session.hello(&self.host_name).await?;
        let actor = session.resolve_identity(actor_name).await?;
        session.actor = actor;
        session.open_endpoint().await?;
        Ok(session)
    }
}

pub async fn serve_in_process(
    listener: TcpListener,
    host: InProcessTelnetHost,
    actor: ActorBinding,
    max_connections: Option<usize>,
) -> Result<(), String> {
    let host = Arc::new(host);
    let mut accepted = 0usize;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("failed to accept connection: {error}"))?;
        let host = host.clone();
        let actor = actor.clone();
        compio::runtime::spawn(async move {
            if let Err(error) = handle_connection(stream, host, actor).await {
                tracing::warn!(error = %error, "telnet connection failed");
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

pub async fn serve_zmq_telnet(
    listener: TcpListener,
    host: ZmqTelnetHost,
    actor_name: String,
    max_connections: Option<usize>,
) -> Result<(), String> {
    let host = Arc::new(host);
    let mut accepted = 0usize;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("failed to accept connection: {error}"))?;
        let host = host.clone();
        let actor_name = actor_name.clone();
        compio::runtime::spawn(async move {
            if let Err(error) = handle_zmq_connection(stream, host, actor_name).await {
                tracing::warn!(error = %error, "ZMQ telnet connection failed");
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

impl EndpointOutput {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn send(&self, line: String) -> Result<(), String> {
        let waker = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err("endpoint writer is closed".to_owned());
            }
            state.lines.push_back(line);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
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

    fn recv(&self) -> EndpointOutputRecv<'_> {
        EndpointOutputRecv { output: self }
    }

    fn drain_batch(&self, max_lines: usize) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        let count = max_lines.min(state.lines.len());
        let mut lines = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(line) = state.lines.pop_front() else {
                break;
            };
            lines.push(line);
        }
        lines
    }

    #[cfg(test)]
    fn try_recv(&self) -> Option<String> {
        self.state.lock().unwrap().lines.pop_front()
    }
}

impl Future for EndpointOutputRecv<'_> {
    type Output = EndpointOutputReady;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.output.state.lock().unwrap();
        if state.lines.len() >= ENDPOINT_OUTPUT_HIGH_WATER_LINES {
            return Poll::Ready(EndpointOutputReady::HighWater {
                buffered: state.lines.len(),
            });
        }
        if !state.lines.is_empty() {
            return Poll::Ready(EndpointOutputReady::Ready {
                buffered: state.lines.len(),
            });
        }
        if state.closed {
            return Poll::Ready(EndpointOutputReady::Closed);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

async fn handle_connection(
    stream: TcpStream,
    host: Arc<InProcessTelnetHost>,
    actor: ActorBinding,
) -> Result<(), String> {
    let endpoint = host.allocate_endpoint()?;
    let output = EndpointOutput::new();
    host.endpoints
        .lock()
        .unwrap()
        .insert(endpoint, output.clone());
    open_endpoint(&host, endpoint, actor.identity)?;

    let (read_half, write_half) = stream.into_split();
    let writer = compio::runtime::spawn(write_socket_loop(write_half, output));
    send_line(&host, endpoint, "Connected to Mica.")?;
    send_line(
        &host,
        endpoint,
        "Try: look, get coin, put coin box, north, say hello, quit.",
    )?;

    let result = read_socket_loop(read_half, &host, endpoint, &actor.name).await;
    let _ = host.driver.close_endpoint(endpoint);
    drop_socket_writer(&host, endpoint);
    let _ = match writer.await {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    result
}

async fn handle_zmq_connection(
    stream: TcpStream,
    host: Arc<ZmqTelnetHost>,
    actor_name: String,
) -> Result<(), String> {
    let endpoint = host.allocate_endpoint()?;
    let mut session = host.connect_session(endpoint, &actor_name).await?;
    let output = EndpointOutput::new();

    let (read_half, write_half) = stream.into_split();
    let writer = compio::runtime::spawn(write_socket_loop(write_half, output.clone()));
    output.send("Connected to Mica.".to_owned())?;
    output.send("Try: look, get coin, put coin box, north, say hello, quit.".to_owned())?;

    let result = read_zmq_socket_loop(read_half, &mut session, output.clone(), &actor_name).await;
    let _ = session.close_endpoint().await;
    output.close();
    let _ = match writer.await {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    result
}

async fn read_socket_loop<S: AsyncRead>(
    mut stream: S,
    host: &InProcessTelnetHost,
    endpoint: Identity,
    actor_name: &str,
) -> Result<(), String> {
    let mut codec = TelnetCodec::new();
    let mut pending = VecDeque::new();
    loop {
        start_read_task(host, endpoint).await?;
        let line = read_telnet_line(&mut stream, &mut codec, &mut pending).await?;
        let Some(line) = line else {
            return Ok(());
        };
        let outcomes = host
            .driver
            .input(endpoint, Value::string(line.clone()))
            .await
            .map_err(format_driver_error)?;
        for outcome in outcomes {
            if let TaskOutcome::Complete { value, .. } = outcome {
                let command = value.with_str(str::to_owned).unwrap_or(line.clone());
                if handle_command(host, endpoint, actor_name, &command).await? {
                    return Ok(());
                }
            }
        }
    }
}

async fn read_zmq_socket_loop<S: AsyncRead>(
    mut stream: S,
    session: &mut ZmqSession,
    output: Arc<EndpointOutput>,
    actor_name: &str,
) -> Result<(), String> {
    let mut codec = TelnetCodec::new();
    let mut pending = VecDeque::new();
    loop {
        session.start_read_task(&output).await?;
        let line = read_telnet_line(&mut stream, &mut codec, &mut pending).await?;
        let Some(line) = line else {
            return Ok(());
        };
        for command in session.submit_input(line.clone(), &output).await? {
            if is_quit_command(&command) {
                output.send("Goodbye.".to_owned())?;
                return Ok(());
            }
            let source = command_invocation_source(actor_name, &command);
            session.submit_source(source, &output).await?;
        }
    }
}

async fn read_telnet_line<S: AsyncRead>(
    stream: &mut S,
    codec: &mut TelnetCodec,
    pending: &mut VecDeque<TelnetItem>,
) -> Result<Option<String>, String> {
    loop {
        while let Some(item) = pending.pop_front() {
            match item {
                TelnetItem::Line(line) => return Ok(Some(line)),
                TelnetItem::Bytes(_) | TelnetItem::Command(_) => {}
            }
        }
        let (result, buffer) = stream.read([0u8; 4096]).await.into();
        let bytes = result.map_err(|error| format!("failed to read from connection: {error}"))?;
        if bytes == 0 {
            return Ok(None);
        }
        pending.extend(
            codec
                .decode(&buffer[..bytes])
                .map_err(format_telnet_codec_error)?,
        );
    }
}

async fn start_read_task(host: &InProcessTelnetHost, endpoint: Identity) -> Result<(), String> {
    let report = host
        .driver
        .submit_source_report(endpoint, None, "return read(:line)".to_owned())
        .await
        .map_err(format_driver_error)?;
    match report.outcome {
        TaskOutcome::Suspended {
            kind: SuspendKind::WaitingForInput(_),
            ..
        } => Ok(()),
        other => Err(format!("read task did not wait for input: {other:?}")),
    }
}

impl ZmqSession {
    async fn hello(&mut self, host_name: &str) -> Result<(), String> {
        self.socket
            .send_message(&HostMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                min_protocol_version: PROTOCOL_VERSION,
                feature_bits: 0,
                host_name: host_name.to_owned(),
            })
            .await
            .map_err(format_zmq_error)?;
        match self.socket.recv_message().await.map_err(format_zmq_error)? {
            HostMessage::HelloAck { .. } => Ok(()),
            other => Err(format!("unexpected hello response: {other:?}")),
        }
    }

    async fn resolve_identity(&mut self, actor_name: &str) -> Result<Identity, String> {
        let request_id = self.next_request_id();
        self.socket
            .send_message(&HostMessage::ResolveIdentity {
                request_id,
                name: Symbol::intern(actor_name),
            })
            .await
            .map_err(format_zmq_error)?;
        match self.socket.recv_message().await.map_err(format_zmq_error)? {
            HostMessage::IdentityResolved {
                request_id: actual,
                identity,
                ..
            } if actual == request_id => Ok(identity),
            HostMessage::RequestRejected {
                request_id: actual,
                code,
                message,
            } if actual == request_id => Err(format!(
                "identity resolution rejected with {}: {message}",
                code.name().unwrap_or("<unnamed>")
            )),
            other => Err(format!("unexpected identity response: {other:?}")),
        }
    }

    async fn open_endpoint(&mut self) -> Result<(), String> {
        let request_id = self.next_request_id();
        let messages = self
            .request(HostMessage::OpenEndpoint {
                request_id,
                endpoint: self.endpoint,
                actor: Some(self.actor),
                protocol: "telnet".to_owned(),
                grant_token: None,
            })
            .await?;
        expect_request_accepted(request_id, &messages)
    }

    async fn close_endpoint(&mut self) -> Result<(), String> {
        let request_id = self.next_request_id();
        let messages = self
            .request(HostMessage::CloseEndpoint {
                request_id,
                endpoint: self.endpoint,
            })
            .await?;
        expect_request_accepted(request_id, &messages)
    }

    async fn start_read_task(&mut self, output: &EndpointOutput) -> Result<(), String> {
        self.submit_source("return read(:line)".to_owned(), output)
            .await
            .map(|_| ())
    }

    async fn submit_input(
        &mut self,
        line: String,
        output: &EndpointOutput,
    ) -> Result<Vec<String>, String> {
        let request_id = self.next_request_id();
        let messages = self
            .request(HostMessage::SubmitInput {
                request_id,
                endpoint: self.endpoint,
                value: Value::string(line.clone()),
            })
            .await?;
        expect_request_accepted(request_id, &messages)?;
        let values = self.route_messages(messages, output).await?;
        Ok(values
            .into_iter()
            .map(|value| value.with_str(str::to_owned).unwrap_or(line.clone()))
            .collect())
    }

    async fn submit_source(
        &mut self,
        source: String,
        output: &EndpointOutput,
    ) -> Result<Vec<Value>, String> {
        let request_id = self.next_request_id();
        let messages = self
            .request(HostMessage::SubmitSource {
                request_id,
                endpoint: self.endpoint,
                actor: self.actor,
                source,
            })
            .await?;
        expect_request_accepted(request_id, &messages)?;
        self.route_messages(messages, output).await
    }

    async fn request(&mut self, message: HostMessage) -> Result<Vec<HostMessage>, String> {
        let request_id = request_id_for(&message)
            .ok_or_else(|| format!("message is not a request: {message:?}"))?;
        self.socket
            .send_message(&message)
            .await
            .map_err(format_zmq_error)?;
        let mut messages = Vec::new();
        loop {
            let message = self.socket.recv_message().await.map_err(format_zmq_error)?;
            let terminal = is_terminal_response(request_id, &message);
            messages.push(message);
            if terminal {
                break;
            }
        }
        while let Some(message) = self.socket.try_recv_message().map_err(format_zmq_error)? {
            messages.push(message);
        }
        Ok(messages)
    }

    async fn route_messages(
        &mut self,
        messages: Vec<HostMessage>,
        output: &EndpointOutput,
    ) -> Result<Vec<Value>, String> {
        let mut completed = Vec::new();
        for message in messages {
            match message {
                HostMessage::RequestAccepted { .. } => {}
                HostMessage::RequestRejected { code, message, .. } => {
                    return Err(format!(
                        "request rejected with {}: {message}",
                        code.name().unwrap_or("<unnamed>")
                    ));
                }
                HostMessage::OutputReady { endpoint, .. } if endpoint == self.endpoint => {
                    self.drain_output(output).await?;
                }
                HostMessage::OutputBatch { endpoint, values } if endpoint == self.endpoint => {
                    write_output_values(output, values)?;
                }
                HostMessage::TaskCompleted { value, .. } => completed.push(value),
                HostMessage::TaskFailed { error, .. } => output.send(effect_text(&error))?,
                HostMessage::EndpointClosed { endpoint, .. } if endpoint == self.endpoint => {
                    output.close();
                }
                _ => {}
            }
        }
        Ok(completed)
    }

    async fn drain_output(&mut self, output: &EndpointOutput) -> Result<(), String> {
        let request_id = self.next_request_id();
        let messages = self
            .request(HostMessage::DrainOutput {
                request_id,
                endpoint: self.endpoint,
                limit: ENDPOINT_OUTPUT_DRAIN_LINES as u32,
            })
            .await?;
        expect_request_accepted(request_id, &messages)?;
        for message in messages {
            match message {
                HostMessage::OutputBatch { endpoint, values } if endpoint == self.endpoint => {
                    write_output_values(output, values)?;
                }
                HostMessage::RequestRejected { code, message, .. } => {
                    return Err(format!(
                        "output drain rejected with {}: {message}",
                        code.name().unwrap_or("<unnamed>")
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn next_request_id(&mut self) -> u64 {
        let request_id = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        request_id
    }
}

fn expect_request_accepted(request_id: u64, messages: &[HostMessage]) -> Result<(), String> {
    for message in messages {
        match message {
            HostMessage::RequestAccepted {
                request_id: actual, ..
            } if *actual == request_id => return Ok(()),
            HostMessage::RequestRejected {
                request_id: actual,
                code,
                message,
            } if *actual == request_id => {
                return Err(format!(
                    "request rejected with {}: {message}",
                    code.name().unwrap_or("<unnamed>")
                ));
            }
            _ => {}
        }
    }
    Err(format!(
        "request {request_id} did not receive an accepted reply"
    ))
}

fn request_id_for(message: &HostMessage) -> Option<u64> {
    match message {
        HostMessage::OpenEndpoint { request_id, .. }
        | HostMessage::CloseEndpoint { request_id, .. }
        | HostMessage::ResolveIdentity { request_id, .. }
        | HostMessage::SubmitSource { request_id, .. }
        | HostMessage::SubmitInput { request_id, .. }
        | HostMessage::DrainOutput { request_id, .. } => Some(*request_id),
        _ => None,
    }
}

fn is_terminal_response(request_id: u64, message: &HostMessage) -> bool {
    match message {
        HostMessage::RequestAccepted {
            request_id: actual, ..
        }
        | HostMessage::RequestRejected {
            request_id: actual, ..
        }
        | HostMessage::IdentityResolved {
            request_id: actual, ..
        } => *actual == request_id,
        _ => false,
    }
}

async fn handle_command(
    host: &InProcessTelnetHost,
    endpoint: Identity,
    actor_name: &str,
    command: &str,
) -> Result<bool, String> {
    if is_quit_command(command) {
        send_line(host, endpoint, "Goodbye.")?;
        return Ok(true);
    }
    let source = command_invocation_source(actor_name, command);
    host.driver
        .submit_source_report(endpoint, None, source)
        .await
        .map_err(format_driver_error)?;
    flush_routed_effects(host);
    Ok(false)
}

fn command_invocation_source(actor_name: &str, command: &str) -> String {
    format!(
        ":command(actor: #{actor_name}, endpoint: endpoint(), line: {})",
        mica_string(command)
    )
}

fn is_quit_command(command: &str) -> bool {
    let command = command.trim();
    command.eq_ignore_ascii_case("quit") || command.eq_ignore_ascii_case("exit")
}

fn open_endpoint(
    host: &InProcessTelnetHost,
    endpoint: Identity,
    actor: Identity,
) -> Result<(), String> {
    host.driver
        .open_endpoint(endpoint, Some(actor), Symbol::intern("telnet"))
        .map_err(format_driver_error)
}

fn send_line(host: &InProcessTelnetHost, endpoint: Identity, line: &str) -> Result<(), String> {
    let sender = host
        .endpoints
        .lock()
        .unwrap()
        .get(&endpoint)
        .cloned()
        .ok_or_else(|| "endpoint is not connected".to_owned())?;
    sender.send(line.to_owned())
}

fn drop_socket_writer(host: &InProcessTelnetHost, endpoint: Identity) {
    if let Some(output) = host.endpoints.lock().unwrap().remove(&endpoint) {
        output.close();
    }
}

async fn write_socket_loop<S: AsyncWrite>(
    mut stream: S,
    output: Arc<EndpointOutput>,
) -> Result<(), String> {
    while let EndpointOutputReady::Ready { .. } | EndpointOutputReady::HighWater { .. } =
        output.recv().await
    {
        for line in output.drain_batch(ENDPOINT_OUTPUT_DRAIN_LINES) {
            let mut bytes = Vec::with_capacity(line.len() + 2);
            encode_telnet_line(&line, &mut bytes);
            let (result, _) = stream.write_all(bytes).await.into();
            if result.is_err() {
                return shutdown_socket_writer(stream).await;
            }
        }
    }
    shutdown_socket_writer(stream).await
}

async fn shutdown_socket_writer<S: AsyncWrite>(mut stream: S) -> Result<(), String> {
    match stream.shutdown().await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotConnected => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) if error.kind() == ErrorKind::ConnectionReset => Ok(()),
        Err(error) => Err(format!("failed to shut down connection writer: {error}")),
    }
}

fn start_event_pump(
    driver: Arc<CompioTaskDriver>,
    endpoints: Arc<Mutex<BTreeMap<Identity, Arc<EndpointOutput>>>>,
    stop_events: Arc<AtomicBool>,
) {
    compio::runtime::spawn(async move {
        while !stop_events.load(Ordering::Relaxed) {
            let events = driver.wait_events().await;
            for event in events {
                route_driver_event(&endpoints, event);
            }
        }
    })
    .detach();
}

fn flush_routed_effects(host: &InProcessTelnetHost) {
    let events = host.driver.drain_events();
    for event in events {
        route_driver_event(&host.endpoints, event);
    }
}

fn route_driver_event(
    endpoints: &Arc<Mutex<BTreeMap<Identity, Arc<EndpointOutput>>>>,
    event: DriverEvent,
) {
    if let DriverEvent::Effect(effect) = event {
        let Some(sender) = endpoints.lock().unwrap().get(&effect.target).cloned() else {
            return;
        };
        let _ = sender.send(effect_text(&effect.value));
    }
}

fn effect_text(value: &Value) -> String {
    value
        .with_str(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn write_output_values(output: &EndpointOutput, values: Vec<Value>) -> Result<(), String> {
    for value in values {
        output.send(effect_text(&value))?;
    }
    Ok(())
}

fn mica_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push(' '),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn format_driver_error(error: mica_driver::DriverError) -> String {
    format!("error: {error}")
}

fn format_telnet_codec_error(error: TelnetCodecError) -> String {
    format!("telnet codec error: {error}")
}

fn format_zmq_error(error: mica_host_zmq::ZmqTransportError) -> String {
    format!("RPC transport error: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_runtime::SourceRunner;
    use std::num::NonZeroUsize;

    fn test_driver(runner: SourceRunner) -> CompioTaskDriver {
        CompioTaskDriver::spawn_with_workers(runner, NonZeroUsize::new(1)).unwrap()
    }

    #[test]
    fn builds_command_invocation_source() {
        assert_eq!(
            command_invocation_source("alice", "say hello"),
            ":command(actor: #alice, endpoint: endpoint(), line: \"say hello\")"
        );
        assert!(is_quit_command("quit"));
        assert!(is_quit_command("EXIT"));
        assert!(!is_quit_command("look"));
    }

    #[test]
    fn escapes_mica_string_literals() {
        assert_eq!(mica_string("hello"), "\"hello\"");
        assert_eq!(
            mica_string("a \"quoted\" path"),
            "\"a \\\"quoted\\\" path\""
        );
        assert_eq!(mica_string("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn endpoint_output_wait_reports_buffer_state_without_dequeueing() {
        let output = EndpointOutput::new();
        output.send("first".to_owned()).unwrap();
        output.send("second".to_owned()).unwrap();

        let ready = compio::runtime::Runtime::new()
            .unwrap()
            .block_on(output.recv());

        assert_eq!(ready, EndpointOutputReady::Ready { buffered: 2 });
        assert_eq!(
            output.drain_batch(ENDPOINT_OUTPUT_DRAIN_LINES),
            vec!["first".to_owned(), "second".to_owned()]
        );

        for index in 0..ENDPOINT_OUTPUT_HIGH_WATER_LINES {
            output.send(format!("line {index}")).unwrap();
        }
        let ready = compio::runtime::Runtime::new()
            .unwrap()
            .block_on(output.recv());

        assert_eq!(
            ready,
            EndpointOutputReady::HighWater {
                buffered: ENDPOINT_OUTPUT_HIGH_WATER_LINES
            }
        );
    }

    #[test]
    fn routed_command_effect_reaches_endpoint_sender() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/string.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/shared/events.mica"))
            .unwrap();
        runner
            .run_filein(include_str!("../../../apps/mud/core.mica"))
            .unwrap();
        let alice = runner.named_identity(Symbol::intern("alice")).unwrap();
        let host = InProcessTelnetHost::new_without_event_pump(test_driver(runner));
        let endpoint = host.allocate_endpoint().unwrap();
        let output = EndpointOutput::new();
        host.endpoints
            .lock()
            .unwrap()
            .insert(endpoint, output.clone());
        open_endpoint(&host, endpoint, alice).unwrap();

        let replies = host.driver.submit_source_report(
            endpoint,
            None,
            "emit(#endpoint, \"hello\")".to_owned(),
        );
        compio::runtime::Runtime::new().unwrap().block_on(async {
            replies.await.unwrap();
        });
        flush_routed_effects(&host);

        assert_eq!(output.try_recv().unwrap(), "hello");
        let _ = host.driver.close_endpoint(endpoint);
    }

    #[test]
    fn mud_command_parser_integration_test() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner
                .run_filein(include_str!("../../../apps/shared/string.mica"))
                .unwrap();
            runner
                .run_filein(include_str!("../../../apps/shared/events.mica"))
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
            let alice = runner.named_identity(Symbol::intern("alice")).unwrap();
            let host = InProcessTelnetHost::new_without_event_pump(test_driver(runner));
            let endpoint = host.allocate_endpoint().unwrap();
            let output = EndpointOutput::new();
            host.endpoints
                .lock()
                .unwrap()
                .insert(endpoint, output.clone());
            open_endpoint(&host, endpoint, alice).unwrap();

            assert!(
                !handle_command(&host, endpoint, "alice", "look")
                    .await
                    .unwrap()
            );

            let line = output.try_recv().unwrap();
            assert_eq!(line, "First Room. You are standing in a plain stone room.");
            assert_eq!(
                output.try_recv().unwrap(),
                "A tarnished brass coin catches the light."
            );
            assert_eq!(
                output.try_recv().unwrap(),
                "A small wooden box rests here, open and empty."
            );
            assert_eq!(
                output.try_recv().unwrap(),
                "A red button protrudes from the wall under a brass sign reading DO NOT PRESS."
            );
            assert_eq!(
                output.try_recv().unwrap(),
                "Bob is here, looking faintly puzzled."
            );
            assert!(
                !handle_command(&host, endpoint, "alice", "push button")
                    .await
                    .unwrap()
            );

            let line = output.try_recv().unwrap();
            assert_eq!(
                line,
                "You press the red button. It clicks, then begins to hum."
            );
            compio::time::sleep(std::time::Duration::from_millis(1100)).await;
            flush_routed_effects(&host);

            let line = output.try_recv().unwrap();
            assert_eq!(line, "The red button gives one final cheerful ding.");
            assert!(
                !handle_command(&host, endpoint, "alice", "say hello")
                    .await
                    .unwrap()
            );

            let line = output.try_recv().unwrap();
            assert_eq!(line, "You say, \"hello\"");
            assert!(
                !handle_command(&host, endpoint, "alice", "flailwildly")
                    .await
                    .unwrap()
            );

            let line = output.try_recv().unwrap();
            assert_eq!(line, "I do not understand that.");
            let _ = host.driver.close_endpoint(endpoint);
        });
    }

    #[test]
    fn endpoint_read_task_accepts_driver_input() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let runner = SourceRunner::new_empty();
            let host = InProcessTelnetHost::new_without_event_pump(test_driver(runner));
            let endpoint = host.allocate_endpoint().unwrap();
            host.endpoints
                .lock()
                .unwrap()
                .insert(endpoint, EndpointOutput::new());
            open_endpoint(&host, endpoint, endpoint).unwrap();

            start_read_task(&host, endpoint).await.unwrap();
            let outcomes = host
                .driver
                .input(endpoint, Value::string("look"))
                .await
                .unwrap();

            assert!(matches!(
                outcomes.as_slice(),
                [TaskOutcome::Complete { value, .. }] if *value == Value::string("look")
            ));
            let _ = host.driver.close_endpoint(endpoint);
        });
    }
}
