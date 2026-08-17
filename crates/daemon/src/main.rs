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

use clap::{Parser, ValueEnum};
use compio::net::TcpListener;
use compio::runtime::{JoinHandle, Runtime};
use fast_telemetry_export::dogstatsd::DogStatsDConfig;
use mica_auth::{AuthConfig, AuthConfigError, MicaSessionStore};
use mica_driver::{
    DriverClient, DriverEventPump, DriverEventRegistration, DriverEventRouter, DriverOwner,
    DriverResources, InvocationOutcome,
};
use mica_host_zmq::{ZmqHostSocket, ZmqSocketOptions};
use mica_relation_kernel::FjallDurabilityMode;
use mica_runtime::{EmbeddingProviderKind, SourceRunner, TaskOutcome};
use mica_telnet_host::{
    ActorBinding as TelnetActorBinding, InProcessTelnetHost, serve_in_process as serve_telnet,
};
use mica_var::{Symbol, Value};
use mica_web_host::{InProcessWebHost, RequestBinding, serve_in_process as serve_web};
use mica_webtransport_host::{
    InProcessWebTransportHost, SessionBinding, WebTransportTlsConfig,
    bind_server_endpoint as bind_webtransport, serve_in_process as serve_webtransport,
};
use serde_json::Value as JsonValue;
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::{Handle as SignalHandle, Signals},
};
use std::env;
use std::fs;
use std::future;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
};
use std::thread::{self, JoinHandle as ThreadJoinHandle};
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;

mod external_http;
mod metrics;
#[allow(dead_code)]
mod rpc;

#[derive(Parser)]
#[command(
    name = "mica-daemon",
    about = "Run a Mica daemon with optional host endpoints"
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = StorageMode::Memory)]
    storage: StorageMode,
    #[arg(long)]
    store: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = DurabilityMode::Relaxed)]
    durability: DurabilityMode,
    #[arg(long = "filein", value_name = "FILE")]
    fileins: Vec<PathBuf>,
    #[arg(long = "startup-source", value_name = "SOURCE")]
    startup_sources: Vec<String>,
    #[arg(long, value_enum, default_value_t = EmbeddingProviderMode::Deterministic)]
    embedding_provider: EmbeddingProviderMode,
    #[arg(long = "source-root", value_name = "DIR")]
    source_roots: Vec<PathBuf>,
    #[arg(long = "source-index", value_name = "FILE")]
    source_index: Option<PathBuf>,
    #[arg(long = "rust-analyzer", value_name = "BINARY")]
    rust_analyzer: Option<String>,
    #[arg(long, default_value = "alice", value_name = "IDENTITY")]
    actor: String,
    #[arg(long, default_value = "web", value_name = "IDENTITY")]
    web_principal: String,
    #[arg(long, value_name = "THREADS")]
    driver_threads: Option<NonZeroUsize>,
    #[arg(long, value_name = "URI")]
    rpc_bind: Option<String>,
    #[arg(long, value_name = "ADDR")]
    telnet_bind: Option<SocketAddr>,
    #[arg(long, value_name = "ADDR")]
    web_bind: Option<SocketAddr>,
    #[arg(long, value_name = "ADDR")]
    webtransport_bind: Option<SocketAddr>,
    #[arg(long, default_value = "web", value_name = "IDENTITY")]
    webtransport_principal: String,
    #[arg(long, value_name = "FILE")]
    webtransport_cert: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    webtransport_key: Option<PathBuf>,
    #[arg(long, value_name = "ADDR")]
    dogstatsd_endpoint: Option<String>,
    #[arg(long, default_value_t = 10, value_name = "SECONDS")]
    dogstatsd_interval_secs: u64,
    #[arg(long, value_name = "FILTER")]
    log_filter: Option<String>,
    #[arg(long)]
    no_log_ansi: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
enum StorageMode {
    Memory,
    Fjall,
}

#[derive(ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityMode {
    Relaxed,
    Strict,
}

impl From<DurabilityMode> for FjallDurabilityMode {
    fn from(value: DurabilityMode) -> Self {
        match value {
            DurabilityMode::Relaxed => Self::Relaxed,
            DurabilityMode::Strict => Self::Strict,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
enum EmbeddingProviderMode {
    Deterministic,
    Disabled,
    Vllm,
}

impl From<EmbeddingProviderMode> for EmbeddingProviderKind {
    fn from(value: EmbeddingProviderMode) -> Self {
        match value {
            EmbeddingProviderMode::Deterministic => Self::Deterministic,
            EmbeddingProviderMode::Disabled => Self::Disabled,
            EmbeddingProviderMode::Vllm => Self::Vllm,
        }
    }
}

struct ShutdownSignals {
    requested: Arc<AtomicBool>,
    signal: Arc<AtomicI32>,
    handle: SignalHandle,
    thread: Option<ThreadJoinHandle<()>>,
}

impl ShutdownSignals {
    fn install() -> Result<Self, String> {
        let mut signals = Signals::new([SIGINT, SIGTERM])
            .map_err(|error| format!("failed to register shutdown signals: {error}"))?;
        let handle = signals.handle();
        let requested = Arc::new(AtomicBool::new(false));
        let signal = Arc::new(AtomicI32::new(0));
        let thread_requested = requested.clone();
        let thread_signal = signal.clone();
        let thread = thread::Builder::new()
            .name("mica-daemon-signal-listener".to_owned())
            .spawn(move || {
                for signal in signals.forever() {
                    if thread_requested.swap(true, Ordering::AcqRel) {
                        eprintln!(
                            "mica-daemon received {signal} during shutdown; exiting immediately"
                        );
                        std::process::exit(128 + signal);
                    }
                    thread_signal.store(signal, Ordering::Release);
                }
            })
            .map_err(|error| format!("failed to start shutdown signal listener: {error}"))?;

        Ok(Self {
            requested,
            signal,
            handle,
            thread: Some(thread),
        })
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn signal_name(&self) -> &'static str {
        match self.signal.load(Ordering::Acquire) {
            SIGINT => "SIGINT",
            SIGTERM => "SIGTERM",
            _ => "shutdown",
        }
    }
}

impl Drop for ShutdownSignals {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ServerTask {
    name: &'static str,
    handle: JoinHandle<Result<(), String>>,
    _event_registration: Option<DriverEventRegistration>,
}

impl ServerTask {
    fn new(name: &'static str, handle: JoinHandle<Result<(), String>>) -> Self {
        Self {
            name,
            handle,
            _event_registration: None,
        }
    }

    fn with_event_registration(mut self, registration: DriverEventRegistration) -> Self {
        self._event_registration = Some(registration);
        self
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "mica-daemon stopped");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    init_tracing(&cli);
    Runtime::new()
        .map_err(|error| format!("failed to start compio runtime: {error}"))?
        .block_on(run_async(cli))
}

fn init_tracing(cli: &Cli) {
    let filter = cli
        .log_filter
        .clone()
        .or_else(|| env::var("MICA_LOG_FILTER").ok())
        .unwrap_or_else(|| "info".to_owned());
    let filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_log::LogTracer::init();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(!cli.no_log_ansi)
        .try_init();
}

async fn run_async(cli: Cli) -> Result<(), String> {
    if cli.rpc_bind.is_none()
        && cli.telnet_bind.is_none()
        && cli.web_bind.is_none()
        && cli.webtransport_bind.is_none()
    {
        return Err("daemon needs at least one endpoint: use --rpc-bind, --telnet-bind, --web-bind, or --webtransport-bind".to_owned());
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        storage = ?cli.storage,
        store = ?cli.store,
        embedding_provider = ?cli.embedding_provider,
        requested_driver_threads = ?cli.driver_threads.map(NonZeroUsize::get),
        "initializing Mica runtime"
    );
    let configured_endpoints = [
        cli.rpc_bind.is_some(),
        cli.telnet_bind.is_some(),
        cli.web_bind.is_some(),
        cli.webtransport_bind.is_some(),
    ]
    .into_iter()
    .filter(|configured| *configured)
    .count();
    metrics::metrics()
        .endpoints_configured
        .set(configured_endpoints as i64);
    let webtransport_tls =
        if cli.webtransport_bind.is_some() {
            let cert = cli.webtransport_cert.as_ref().ok_or_else(|| {
                "--webtransport-cert is required with --webtransport-bind".to_owned()
            })?;
            let key = cli.webtransport_key.as_ref().ok_or_else(|| {
                "--webtransport-key is required with --webtransport-bind".to_owned()
            })?;
            Some(WebTransportTlsConfig::from_pem_files(cert, key)?)
        } else {
            None
        };
    let shutdown_signals = ShutdownSignals::install()?;
    let dogstatsd_endpoint = cli.dogstatsd_endpoint.clone();
    let dogstatsd_interval = Duration::from_secs(cli.dogstatsd_interval_secs.max(1));
    let mut runner = open_runner(&cli)?;
    for filein in &cli.fileins {
        let source = fs::read_to_string(filein)
            .map_err(|error| format!("failed to read {}: {error}", filein.display()))?;
        let include_base = filein.parent().unwrap_or_else(|| Path::new("."));
        runner
            .run_filein_with_include_loader(&source, |path| read_filein_include(include_base, path))
            .map_err(|error| format_source_error_with_source(error, filein, &source))?;
        metrics::metrics().fileins_loaded.inc();
    }
    let telnet_actor = if cli.telnet_bind.is_some() {
        let actor_name = actor_name(&cli.actor)?;
        let actor = runner
            .named_identity(Symbol::intern(&actor_name))
            .map_err(format_source_error)?;
        Some(TelnetActorBinding {
            name: actor_name,
            identity: actor,
        })
    } else {
        None
    };
    let web_binding = if cli.web_bind.is_some() {
        let principal_name = actor_name(&cli.web_principal)?;
        let principal = runner
            .named_identity(Symbol::intern(&principal_name))
            .map_err(format_source_error)?;
        Some(RequestBinding {
            principal,
            actor: None,
        })
    } else {
        None
    };
    let webtransport_binding = if cli.webtransport_bind.is_some() {
        let principal_name = actor_name(&cli.webtransport_principal)?;
        let principal = runner
            .named_identity(Symbol::intern(&principal_name))
            .map_err(format_source_error)?;
        Some(SessionBinding {
            principal,
            actor: None,
        })
    } else {
        None
    };
    let worker_count = cli.driver_threads.unwrap_or_else(|| {
        std::thread::available_parallelism().unwrap_or(NonZeroUsize::new(1).unwrap())
    });
    let mut resources = DriverResources::new(worker_count);
    resources.relation_acceleration = mica_driver::RelationAcceleration::Automatic;
    let mut owner = DriverOwner::builder(resources)
        .source_runner(runner)
        .external_request_handler(external_http::handler())
        .external_stream_request_handler(external_http::stream_handler())
        .build()
        .map_err(format_driver_error)?;
    metrics::metrics().drivers_started.inc();
    let dogstatsd_task =
        dogstatsd_endpoint.map(|endpoint| start_dogstatsd_export(endpoint, dogstatsd_interval));
    let mut event_pump = owner.take_event_pump().map_err(format_driver_error)?;
    for source in &cli.startup_sources {
        run_startup_source(&owner, &mut event_pump, source).await?;
    }
    let event_router = owner.event_router();
    let event_pump_task = event_pump.spawn_router(event_router.clone());
    let client = owner.client();
    let mut server_tasks = Vec::new();
    if let Some(rpc_bind) = cli.rpc_bind {
        server_tasks.push(start_rpc_server(client.clone(), &event_router, rpc_bind)?);
    }
    let (auth_enabled, auth_config) = match AuthConfig::from_env() {
        Ok(config) => (true, Some(config)),
        Err(AuthConfigError::MissingKey) => (false, None),
        Err(e) => {
            return Err(format!(
                "authentication is configured but invalid: {e}. \
                 Check MICA_AUTH_PASETO_KEY, MICA_AUTH_SESSION_TTL_SECS."
            ));
        }
    };
    if let Some(web_bind) = cli.web_bind {
        let binding = web_binding.expect("web principal should be resolved before driver spawn");
        let listener = TcpListener::bind(web_bind)
            .await
            .map_err(|error| format!("failed to bind web listener {web_bind}: {error}"))?;
        let local_addr = listener.local_addr().unwrap();
        tracing::info!(bind = %web_bind, local_addr = %local_addr, "web listener started");
        metrics::metrics()
            .endpoints_started
            .inc(metrics::DaemonEndpoint::Web);
        let mut host = InProcessWebHost::new(client.clone(), &event_router);

        if auth_enabled {
            let config = auth_config.unwrap();
            let session_store = MicaSessionStore::new(owner.administrator(), config.schema.clone());
            bootstrap_local_users(&session_store).await?;
            let auth_subsystem = mica_web_host::auth::AuthSubsystem::new(config, session_store);
            host = host.with_auth(auth_subsystem);
            tracing::info!("authentication subsystem enabled");
        }

        let handle = compio::runtime::spawn(serve_web(listener, host, binding, None));
        server_tasks.push(ServerTask::new("web", handle));
    }
    if let Some(webtransport_bind) = cli.webtransport_bind {
        if auth_enabled {
            tracing::warn!(
                "authentication is enabled; WebTransport endpoint {} will not be started \
                (WebTransport does not support session authentication)",
                webtransport_bind
            );
        } else {
            let binding = webtransport_binding
                .expect("WebTransport principal should be resolved before driver spawn");
            let tls =
                webtransport_tls.expect("WebTransport TLS should be loaded before driver spawn");
            let endpoint = bind_webtransport(webtransport_bind, tls).await?;
            let local_addr = endpoint.local_addr().unwrap();
            tracing::info!(
                bind = %webtransport_bind,
                local_addr = %local_addr,
                "WebTransport listener started"
            );
            metrics::metrics()
                .endpoints_started
                .inc(metrics::DaemonEndpoint::WebTransport);
            let host = InProcessWebTransportHost::new(client.clone(), &event_router);
            let handle = compio::runtime::spawn(serve_webtransport(endpoint, host, binding, None));
            server_tasks.push(ServerTask::new("webtransport", handle));
        }
    }
    if let Some(telnet_bind) = cli.telnet_bind {
        let actor = telnet_actor.expect("telnet actor should be resolved before driver spawn");
        let listener = TcpListener::bind(telnet_bind)
            .await
            .map_err(|error| format!("failed to bind telnet listener {telnet_bind}: {error}"))?;
        let local_addr = listener.local_addr().unwrap();
        tracing::info!(
            bind = %telnet_bind,
            local_addr = %local_addr,
            "telnet listener started"
        );
        metrics::metrics()
            .endpoints_started
            .inc(metrics::DaemonEndpoint::Telnet);
        let handle = compio::runtime::spawn(serve_telnet(
            listener,
            InProcessTelnetHost::new(client, &event_router),
            actor,
            None,
        ));
        server_tasks.push(ServerTask::new("telnet", handle));
    }
    wait_for_shutdown_signal(&shutdown_signals).await;
    tracing::info!(
        signal = shutdown_signals.signal_name(),
        server_tasks = server_tasks.len(),
        "daemon shutdown requested"
    );
    cancel_server_tasks(server_tasks).await;
    if let Some(dogstatsd_task) = dogstatsd_task {
        tracing::info!("stopping DogStatsD exporter");
        let _ = dogstatsd_task.cancel().await;
    }
    tracing::info!("shutting down task driver and relation store");
    let mut event_pump = event_pump_task
        .stop()
        .await
        .map_err(|error| format!("failed to stop task driver event pump: {error}"))?;
    owner
        .shutdown(&mut event_pump, |_| {})
        .await
        .map_err(|error| format!("failed to shut down task driver: {error}"))?;
    tracing::info!("daemon shutdown complete");
    Ok(())
}

fn open_runner(cli: &Cli) -> Result<SourceRunner, String> {
    let use_fjall = cli.storage == StorageMode::Fjall || cli.store.is_some();
    let source_config = source_config(cli);
    if !use_fjall {
        return Ok(match source_config {
            Some(config) => SourceRunner::new_empty_with_embedding_provider_and_source(
                cli.embedding_provider.into(),
                config,
            ),
            None => SourceRunner::new_empty_with_embedding_provider(cli.embedding_provider.into()),
        });
    }
    let store = cli
        .store
        .as_ref()
        .ok_or_else(|| "--store is required with --storage fjall".to_owned())?;
    tracing::info!(
        store = %store.display(),
        durability = ?cli.durability,
        "opening fjall relation store"
    );
    match source_config {
        Some(config) => SourceRunner::open_fjall_with_embedding_provider_and_source(
            store,
            cli.durability.into(),
            cli.embedding_provider.into(),
            config,
        ),
        None => SourceRunner::open_fjall_with_embedding_provider(
            store,
            cli.durability.into(),
            cli.embedding_provider.into(),
        ),
    }
    .map_err(|error| format!("failed to open fjall store {}: {error}", store.display()))
}

fn source_config(cli: &Cli) -> Option<mica_source_provider::SourceConfig> {
    let roots = if cli.source_roots.is_empty() {
        env::var_os("MICA_SOURCE_ROOTS")
            .map(|roots| env::split_paths(&roots).collect::<Vec<_>>())
            .or_else(|| env::var_os("MICA_SOURCE_ROOT").map(|root| vec![PathBuf::from(root)]))
            .unwrap_or_default()
    } else {
        cli.source_roots.clone()
    };
    let index = cli
        .source_index
        .clone()
        .or_else(|| env::var_os("MICA_SOURCE_INDEX").map(PathBuf::from));
    let rust_analyzer = cli
        .rust_analyzer
        .clone()
        .or_else(|| env::var("MICA_RUST_ANALYZER").ok());
    if roots.is_empty() && index.is_none() && rust_analyzer.is_none() {
        return None;
    }
    let mut config = mica_source_provider::SourceConfig::new(roots);
    if let Some(path) = index {
        config = config.with_semantic_index(path);
    }
    if let Some(binary) = rust_analyzer {
        config = config.with_rust_analyzer(binary);
    }
    Some(config)
}

async fn wait_for_shutdown_signal(signals: &ShutdownSignals) {
    while !signals.requested() {
        compio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn cancel_server_tasks(server_tasks: Vec<ServerTask>) {
    for task in server_tasks {
        tracing::info!(endpoint = task.name, "stopping endpoint server");
        match task.handle.cancel().await {
            Some(Ok(Ok(()))) => tracing::info!(endpoint = task.name, "endpoint server stopped"),
            Some(Ok(Err(error))) => {
                tracing::warn!(endpoint = task.name, error = %error, "endpoint server stopped with error");
            }
            Some(Err(payload)) => std::panic::resume_unwind(payload),
            None => tracing::info!(endpoint = task.name, "endpoint server cancelled"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct LocalUserBootstrap {
    login: String,
    password: String,
    display_name: String,
    roles: Vec<String>,
}

async fn bootstrap_local_users(session_store: &MicaSessionStore) -> Result<(), String> {
    let raw = match env::var("MICA_AUTH_LOCAL_USERS_JSON") {
        Ok(raw) if !raw.trim().is_empty() => raw,
        _ => return Ok(()),
    };

    let users = parse_local_users_json(&raw)?;
    for user in users {
        let local_user = session_store
            .upsert_local_user(&user.login, &user.password, &user.display_name)
            .await
            .map_err(|error| format!("failed to bootstrap local user {}: {error}", user.login))?;
        for role in &user.roles {
            session_store
                .grant_user_role(&local_user.user_id, role)
                .await
                .map_err(|error| {
                    format!(
                        "failed to grant role {role} to local user {}: {error}",
                        user.login
                    )
                })?;
        }
        tracing::info!(login = %user.login, "bootstrapped local auth user");
    }

    Ok(())
}

fn parse_local_users_json(raw: &str) -> Result<Vec<LocalUserBootstrap>, String> {
    let value: JsonValue = serde_json::from_str(raw)
        .map_err(|error| format!("invalid MICA_AUTH_LOCAL_USERS_JSON: {error}"))?;
    let users = value
        .as_array()
        .ok_or_else(|| "MICA_AUTH_LOCAL_USERS_JSON must be a JSON array".to_owned())?;

    users
        .iter()
        .enumerate()
        .map(|(index, value)| parse_local_user_bootstrap(index, value))
        .collect()
}

fn parse_local_user_bootstrap(
    index: usize,
    value: &JsonValue,
) -> Result<LocalUserBootstrap, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("MICA_AUTH_LOCAL_USERS_JSON entry {index} must be a JSON object"))?;
    let login = required_json_string(object, index, "login")?;
    let password = required_json_string(object, index, "password")?;
    let display_name = object
        .get("display_name")
        .or_else(|| object.get("displayName"))
        .and_then(JsonValue::as_str)
        .unwrap_or(&login)
        .trim()
        .to_owned();
    let roles = parse_local_user_roles(object, index)?;

    if display_name.is_empty() {
        return Err(format!(
            "MICA_AUTH_LOCAL_USERS_JSON entry {index} has an empty display_name"
        ));
    }

    Ok(LocalUserBootstrap {
        login,
        password,
        display_name,
        roles,
    })
}

fn parse_local_user_roles(
    object: &serde_json::Map<String, JsonValue>,
    index: usize,
) -> Result<Vec<String>, String> {
    let Some(value) = object.get("roles") else {
        return Ok(Vec::new());
    };
    let roles = value.as_array().ok_or_else(|| {
        format!("MICA_AUTH_LOCAL_USERS_JSON entry {index} field roles must be an array")
    })?;
    let mut parsed = Vec::new();
    for role in roles {
        let role = role.as_str().ok_or_else(|| {
            format!("MICA_AUTH_LOCAL_USERS_JSON entry {index} field roles must contain strings")
        })?;
        let role = role.trim().to_ascii_lowercase();
        if !matches!(role.as_str(), "admin" | "operator" | "viewer") {
            return Err(format!(
                "MICA_AUTH_LOCAL_USERS_JSON entry {index} has unsupported role {role:?}"
            ));
        }
        parsed.push(role);
    }
    Ok(parsed)
}

fn required_json_string(
    object: &serde_json::Map<String, JsonValue>,
    index: usize,
    field: &str,
) -> Result<String, String> {
    let value = object
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            format!("MICA_AUTH_LOCAL_USERS_JSON entry {index} missing string field {field}")
        })?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!(
            "MICA_AUTH_LOCAL_USERS_JSON entry {index} has an empty {field}"
        ));
    }
    Ok(value.to_owned())
}

fn start_rpc_server(
    client: DriverClient,
    event_router: &DriverEventRouter,
    endpoint: String,
) -> Result<ServerTask, String> {
    let context = zmq::Context::new();
    let socket = ZmqHostSocket::bind(
        &context,
        zmq::ROUTER,
        &endpoint,
        ZmqSocketOptions::default(),
    )
    .map_err(|error| format!("failed to bind RPC socket {endpoint}: {error}"))?;
    tracing::info!(endpoint = %endpoint, "RPC listener started");
    metrics::metrics()
        .endpoints_started
        .inc(metrics::DaemonEndpoint::Rpc);
    let events = Arc::new(rpc::RpcEventQueue::default());
    let event_queue = Arc::clone(&events);
    let event_registration = event_router.register(move |event| {
        let event_queue = Arc::clone(&event_queue);
        async move {
            event_queue.push(event);
        }
    });
    let rpc_router = event_router.clone();
    let handle = compio::runtime::spawn(async move {
        let _context = context;
        let mut handler = rpc::RpcHandler::new(client, rpc_router, events);
        rpc::serve_zmq_rpc_forever(&socket, &mut handler)
            .await
            .map_err(|error| error.to_string())
    });
    Ok(ServerTask::new("rpc", handle).with_event_registration(event_registration))
}

fn start_dogstatsd_export(endpoint: String, interval: Duration) -> JoinHandle<()> {
    metrics::metrics().dogstatsd_configured.set(1);
    metrics::metrics().dogstatsd_exporters_started.inc();
    let config = DogStatsDConfig::new(endpoint).with_interval(interval);
    compio::runtime::spawn(async move {
        let mut daemon_state = metrics::DaemonMetricsDogStatsDState::new();
        let mut driver_state = mica_driver::metrics::DriverMetricsDogStatsDState::new();
        let mut relation_kernel_state =
            mica_relation_kernel::metrics::RelationKernelMetricsDogStatsDState::new();
        let mut relation_wgpu_state = mica_relation_wgpu::RelationWgpuMetricsDogStatsDState::new();
        let mut runtime_state = mica_runtime::metrics::RuntimeMetricsDogStatsDState::new();
        let mut web_host_state = mica_web_host::metrics::WebHostMetricsDogStatsDState::new();
        let mut webtransport_host_state =
            mica_webtransport_host::metrics::WebTransportMetricsDogStatsDState::new();
        fast_telemetry_export::dogstatsd::run_compio(
            config,
            future::pending::<()>(),
            move |output| {
                metrics::metrics().export_dogstatsd_delta(output, &[], &mut daemon_state);
                mica_driver::metrics::metrics().export_dogstatsd_delta(
                    output,
                    &[],
                    &mut driver_state,
                );
                mica_relation_kernel::metrics::metrics().export_dogstatsd_delta(
                    output,
                    &[],
                    &mut relation_kernel_state,
                );
                mica_relation_wgpu::metrics().export_dogstatsd_delta(
                    output,
                    &[],
                    &mut relation_wgpu_state,
                );
                mica_runtime::metrics::metrics().export_dogstatsd_delta(
                    output,
                    &[],
                    &mut runtime_state,
                );
                mica_web_host::metrics::metrics().export_dogstatsd_delta(
                    output,
                    &[],
                    &mut web_host_state,
                );
                mica_webtransport_host::metrics::metrics().export_dogstatsd_delta(
                    output,
                    &[],
                    &mut webtransport_host_state,
                );
                metrics::metrics().dogstatsd_export_ticks.inc();
            },
        )
        .await;
    })
}

fn read_filein_include(base: &Path, path: &str) -> Result<String, String> {
    let include_path = base.join(path);
    fs::read_to_string(&include_path)
        .map_err(|error| format!("failed to read {}: {error}", include_path.display()))
}

fn actor_name(actor: &str) -> Result<String, String> {
    let actor = actor.trim().trim_start_matches('#').trim_start_matches(':');
    if actor.is_empty()
        || !actor
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        || actor.chars().next().is_some_and(|ch| ch.is_ascii_digit())
    {
        return Err("actor must be a named identity such as alice or #alice".to_owned());
    }
    Ok(actor.to_owned())
}

fn log_startup_source_begin(source: &str) {
    tracing::info!(
        description = startup_source_description(source),
        "startup source started"
    );
}

fn log_startup_source_end(source: &str, rendered_report: &str) {
    tracing::info!(
        description = startup_source_description(source),
        report = %rendered_report,
        "startup source completed"
    );
}

async fn run_startup_source(
    owner: &DriverOwner,
    event_pump: &mut DriverEventPump,
    source: &str,
) -> Result<(), String> {
    log_startup_source_begin(source);
    let is_source_retrieval_indexing = is_source_retrieval_indexing_source(source);
    let should_follow_spawned_child = startup_source_should_follow_spawned_child(source);
    let track_source_retrieval_indexing =
        is_source_retrieval_indexing && !should_follow_spawned_child;
    let start = Instant::now();
    if track_source_retrieval_indexing {
        metrics::source_retrieval_indexing_started();
    }
    let invocation = owner
        .administrator()
        .evaluate(source.to_owned())
        .await
        .map_err(|error| {
            if track_source_retrieval_indexing {
                metrics::source_retrieval_indexing_failed(start.elapsed());
            }
            format_driver_error(error)
        })?;
    let report = invocation.initial_report();
    if !matches!(report.outcome, TaskOutcome::Suspended { .. }) {
        if track_source_retrieval_indexing {
            record_source_retrieval_indexing_report_outcome(start, &report.outcome);
        }
        log_startup_source_end(source, &report.render());
        return Ok(());
    }
    if let TaskOutcome::Suspended { kind, .. } = &report.outcome
        && !startup_suspend_can_resume(kind)
    {
        return Err(format!(
            "startup source {} suspended without an automatic resume: {:?}",
            startup_source_description(source),
            kind
        ));
    }
    let outcome = event_pump.drive_invocation(&invocation, |_| {}).await;
    match outcome {
        InvocationOutcome::Completed(value) => {
            if should_follow_spawned_child
                && let Some(child_task_id) = spawned_child_task_id(&value)
            {
                tracing::info!(
                    description = startup_source_description(source),
                    parent_task_id = invocation.task_id(),
                    child_task_id,
                    "startup source spawned background task"
                );
                log_startup_source_end(source, &format!("spawned background task {child_task_id}"));
                return Ok(());
            }
            if track_source_retrieval_indexing {
                metrics::source_retrieval_indexing_completed(start.elapsed(), value.as_int());
            }
            log_startup_source_end(source, &format!("completed with {value}"));
            Ok(())
        }
        InvocationOutcome::Aborted(error) => {
            if track_source_retrieval_indexing {
                metrics::source_retrieval_indexing_failed(start.elapsed());
            }
            Err(format!(
                "startup source {} aborted with {error}",
                startup_source_description(source)
            ))
        }
        InvocationOutcome::Failed(error) => Err(format!(
            "startup source {} failed: {error}",
            startup_source_description(source)
        )),
        InvocationOutcome::Cancelled(reason) => Err(format!(
            "startup source {} was cancelled: {reason:?}",
            startup_source_description(source)
        )),
    }
}

fn spawned_child_task_id(value: &Value) -> Option<u64> {
    let task_id = value.as_int()?;
    if task_id <= 0 {
        return None;
    }
    Some(task_id as u64)
}

fn record_source_retrieval_indexing_report_outcome(start: Instant, outcome: &TaskOutcome) {
    match outcome {
        TaskOutcome::Complete { value, .. } => {
            metrics::source_retrieval_indexing_completed(start.elapsed(), value.as_int());
        }
        TaskOutcome::Aborted { .. } | TaskOutcome::Suspended { .. } => {
            metrics::source_retrieval_indexing_failed(start.elapsed());
        }
    }
}

fn startup_suspend_can_resume(kind: &mica_runtime::SuspendKind) -> bool {
    match kind {
        mica_runtime::SuspendKind::Commit
        | mica_runtime::SuspendKind::TimedMillis(_)
        | mica_runtime::SuspendKind::Spawn(_)
        | mica_runtime::SuspendKind::ExternalRequest(_) => true,
        mica_runtime::SuspendKind::MailboxRecv(request) => request.timeout_millis.is_some(),
        mica_runtime::SuspendKind::Never | mica_runtime::SuspendKind::WaitingForInput(_) => false,
    }
}

fn startup_source_description(source: &str) -> &'static str {
    if startup_source_should_follow_spawned_child(source) {
        "spawning source retrieval index prewarm"
    } else if is_source_retrieval_indexing_source(source) {
        "prewarming source retrieval index"
    } else {
        "running startup source"
    }
}

fn startup_source_should_follow_spawned_child(source: &str) -> bool {
    source.contains("spawn") && source.contains("source/run_retrieval_prewarm")
}

fn is_source_retrieval_indexing_source(source: &str) -> bool {
    source.contains("source/prewarm_retrieval_index")
        || source.contains("source/run_retrieval_prewarm")
}

fn format_source_error(error: mica_runtime::SourceTaskError) -> String {
    mica_runtime::format_source_task_error(&error)
}

fn format_source_error_with_source(
    error: mica_runtime::SourceTaskError,
    path: &Path,
    source: &str,
) -> String {
    let path = path.display().to_string();
    mica_runtime::format_source_task_error_with_source(&error, Some(&path), source)
}

fn format_driver_error(error: mica_driver::DriverError) -> String {
    format!("error: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_user_bootstrap_json() {
        let users = parse_local_users_json(
            r#"[
                {"login":"alice","password":"secret","display_name":"Alice"},
                {"login":"bob","password":"also-secret"}
            ]"#,
        )
        .unwrap();

        assert_eq!(
            users,
            vec![
                LocalUserBootstrap {
                    login: "alice".to_owned(),
                    password: "secret".to_owned(),
                    display_name: "Alice".to_owned(),
                    roles: Vec::new(),
                },
                LocalUserBootstrap {
                    login: "bob".to_owned(),
                    password: "also-secret".to_owned(),
                    display_name: "bob".to_owned(),
                    roles: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn rejects_local_user_bootstrap_without_password() {
        let error = parse_local_users_json(r#"[{"login":"alice"}]"#).unwrap_err();
        assert!(error.contains("missing string field password"));
    }

    #[test]
    fn parses_local_user_bootstrap_roles() {
        let users = parse_local_users_json(
            r#"[{"login":"alice","password":"secret","roles":["viewer","operator"]}]"#,
        )
        .unwrap();

        assert_eq!(users[0].roles, vec!["viewer", "operator"]);
    }

    #[test]
    fn rejects_unknown_local_user_bootstrap_role() {
        let error =
            parse_local_users_json(r#"[{"login":"alice","password":"secret","roles":["root"]}]"#)
                .unwrap_err();
        assert!(error.contains("unsupported role"));
    }
}
