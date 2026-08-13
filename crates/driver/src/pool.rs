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

use crate::embedding::{
    DriverWake, InvocationDiagnostics, InvocationHandle, InvocationOutcome, InvocationState,
};
use crate::execution::{CpuAdmission, ExternalRequestAdmission};
use crate::{
    DEFAULT_ACTIVE_TASK_CAPACITY, DEFAULT_ENDPOINT_CAPACITY, DEFAULT_EPHEMERAL_IDENTITY_CAPACITY,
    DEFAULT_EVENT_QUEUE_CAPACITY, DEFAULT_SUBSCRIPTION_CAPACITY,
    DEFAULT_SUBSCRIPTION_MAILBOX_CAPACITY, DEFAULT_SUSPENDED_TASK_CAPACITY,
    DEFAULT_TERMINAL_TASK_RETENTION, DEFAULT_TIMER_CAPACITY, DispatcherConfig, DriverError,
    DriverEvent, DriverResourceSnapshot, DriverResources, DriverSubscriptionMailbox,
    DriverSubscriptionRequest, EndpointCloseReport, ExternalRequestCancellation,
    ExternalRequestContext, ExternalRequestHandler, ExternalStreamEmitter,
    ExternalStreamRequestHandler, FileinIncludeLoader, RelationAcceleration,
    TaskCancellationReason, TaskContext, configure_dispatcher,
    metrics::{self, AsyncWorkerKind, DispatchOperation, WorkerOutcome},
};
#[cfg(test)]
use crate::{DEFAULT_EXTERNAL_REQUEST_CAPACITY, DEFAULT_SUBSCRIPTION_QUEUE_BUDGET};
use compio::dispatcher::Dispatcher;
use compio::runtime::JoinHandle;
use futures_util::future::{Either, select};
use futures_util::task::{ArcWake, waker};
#[cfg(feature = "wgpu")]
use mica_relation_wgpu::{WgpuAccelerator, WgpuAcceleratorOptions};
use mica_runtime::{
    AuthorityContext, EPHEMERAL_HOST_IDENTITY_END, EPHEMERAL_HOST_IDENTITY_START, ExecutionContext,
    FileinMode, FileinReport, MailboxRecvRequest, ReadOnlySourceQueryOptions,
    ReadOnlySourceQueryReport, ReadOnlySourceQueryStatus, RunReport, RuntimeError, SYSTEM_ENDPOINT,
    SharedSourceRunner, SourceRunner, SourceTaskError, SpawnRequest, SubmittedTask,
    SubscriptionRequest, SuspendKind, TaskError, TaskId, TaskInput, TaskLimits, TaskManagerError,
    TaskOutcome, TaskRequest, Tuple,
};
use mica_var::{Identity, Symbol, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
#[cfg(feature = "wgpu")]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

#[cfg(feature = "wgpu")]
static RELATION_ACCELERATOR: OnceLock<AutomaticRelationAccelerator> = OnceLock::new();

#[cfg(feature = "wgpu")]
enum AutomaticRelationAccelerator {
    Enabled(Arc<WgpuAccelerator>),
    Unavailable(String),
}

#[cfg(feature = "wgpu")]
fn relation_accelerator() -> &'static AutomaticRelationAccelerator {
    RELATION_ACCELERATOR.get_or_init(|| {
        match WgpuAccelerator::new(WgpuAcceleratorOptions::default()) {
            Ok(accelerator) => AutomaticRelationAccelerator::Enabled(Arc::new(accelerator)),
            Err(error) => {
                mica_relation_wgpu::metrics().initialization_failures.inc();
                AutomaticRelationAccelerator::Unavailable(error.to_string())
            }
        }
    })
}

#[derive(Clone)]
pub(crate) struct CompioTaskDriver {
    inner: Arc<PoolInner>,
}

impl CompioTaskDriver {
    #[cfg(test)]
    pub(crate) fn inner_runner(&self) -> Arc<SharedSourceRunner> {
        Arc::clone(&self.inner.runner)
    }

    pub(crate) fn same_driver(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn set_wake_handler(&self, handler: Option<Arc<dyn DriverWake>>) {
        *self.inner.wake_handler.lock().unwrap() = handler.clone();
        if self.inner.state.lock().unwrap().events.is_empty() {
            return;
        }
        if let Some(handler) = handler {
            handler.wake();
        }
    }
}

struct PoolInner {
    runner: Arc<SharedSourceRunner>,
    dispatcher: Mutex<Option<Dispatcher>>,
    cpu_admission: Arc<CpuAdmission>,
    external_request_admission: Arc<ExternalRequestAdmission>,
    external_request_handler: Option<ExternalRequestHandler>,
    external_stream_request_handler: Option<ExternalStreamRequestHandler>,
    subscription_queue_budget: NonZeroUsize,
    next_ephemeral_identity: AtomicU64,
    wake_handler: Mutex<Option<Arc<dyn DriverWake>>>,
    state: Mutex<PoolState>,
}

struct PoolState {
    lifecycle: DriverLifecycle,
    shutdown_error: Option<String>,
    in_flight_dispatches: usize,
    idle_wakers: Vec<Waker>,
    endpoint_activities: HashMap<Identity, usize>,
    endpoint_idle_wakers: HashMap<Identity, Vec<Waker>>,
    shutdown_wakers: Vec<Waker>,
    contexts: BTreeMap<TaskId, TaskContext>,
    invocations: HashMap<TaskId, Arc<InvocationState>>,
    active_tasks: HashSet<TaskId>,
    terminal_tasks: HashMap<TaskId, TerminalTaskKind>,
    terminal_order: VecDeque<TaskId>,
    closed_endpoints: HashSet<Identity>,
    endpoints: HashMap<Identity, EndpointResources>,
    allocated_ephemeral_identities: HashSet<Identity>,
    volatile_fact_owners: BTreeMap<(Symbol, Tuple), VolatileFactOwnership>,
    input_waiters: BTreeMap<Identity, Vec<TaskId>>,
    mailbox_waiters: BTreeMap<u64, VecDeque<MailboxWaiter>>,
    external_subscription_mailboxes: HashMap<u64, Value>,
    subscriptions: HashMap<Value, SubscriptionOwner>,
    events: VecDeque<DriverEvent>,
    discard_events: bool,
    event_wakers: Vec<Waker>,
    event_space_wakers: Vec<Waker>,
    event_capacity: usize,
    active_task_capacity: usize,
    suspended_task_capacity: usize,
    timer_capacity: usize,
    terminal_task_retention: usize,
    endpoint_capacity: usize,
    subscription_capacity: usize,
    subscription_mailbox_capacity: usize,
    ephemeral_identity_capacity: usize,
    next_worker_id: u64,
    workers: HashMap<u64, AsyncWorker>,
}

#[derive(Clone, Default)]
struct EndpointResources {
    scopes: HashMap<Symbol, BTreeSet<(Symbol, Tuple)>>,
    subscriptions: HashSet<Value>,
}

#[derive(Clone, Copy)]
struct VolatileFactOwnership {
    owners: usize,
    asserted_by_driver: bool,
}

struct SubscriptionOwner {
    endpoint: Identity,
    mailbox: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DriverLifecycle {
    Running,
    ShuttingDown,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalTaskKind {
    Completed,
    Aborted,
    Cancelled,
    Failed,
}

struct AsyncWorker {
    task_id: TaskId,
    timer: bool,
    cancellation: Option<ExternalRequestCancellation>,
    handle: JoinHandle<()>,
}

struct DispatchActivity {
    inner: Arc<PoolInner>,
}

struct EndpointActivity {
    inner: Arc<PoolInner>,
    endpoint: Identity,
}

struct DriverIdle<'a> {
    driver: &'a CompioTaskDriver,
}

struct EndpointIdle<'a> {
    driver: &'a CompioTaskDriver,
    endpoint: Identity,
}

struct DriverStopped<'a> {
    driver: &'a CompioTaskDriver,
}

struct EventEnqueue<'a> {
    driver: &'a CompioTaskDriver,
    event: Option<DriverEvent>,
}

struct WakeOnReady<F> {
    future: Pin<Box<F>>,
    inner: Arc<PoolInner>,
}

struct ReadinessWaker {
    runtime_waker: Waker,
    inner: Arc<PoolInner>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            lifecycle: DriverLifecycle::Running,
            shutdown_error: None,
            in_flight_dispatches: 0,
            idle_wakers: Vec::new(),
            endpoint_activities: HashMap::new(),
            endpoint_idle_wakers: HashMap::new(),
            shutdown_wakers: Vec::new(),
            contexts: BTreeMap::new(),
            invocations: HashMap::new(),
            active_tasks: HashSet::new(),
            terminal_tasks: HashMap::new(),
            terminal_order: VecDeque::new(),
            closed_endpoints: HashSet::new(),
            endpoints: HashMap::new(),
            allocated_ephemeral_identities: HashSet::new(),
            volatile_fact_owners: BTreeMap::new(),
            input_waiters: BTreeMap::new(),
            mailbox_waiters: BTreeMap::new(),
            external_subscription_mailboxes: HashMap::new(),
            subscriptions: HashMap::new(),
            events: VecDeque::new(),
            discard_events: false,
            event_wakers: Vec::new(),
            event_space_wakers: Vec::new(),
            event_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            active_task_capacity: DEFAULT_ACTIVE_TASK_CAPACITY,
            suspended_task_capacity: DEFAULT_SUSPENDED_TASK_CAPACITY,
            timer_capacity: DEFAULT_TIMER_CAPACITY,
            terminal_task_retention: DEFAULT_TERMINAL_TASK_RETENTION,
            endpoint_capacity: DEFAULT_ENDPOINT_CAPACITY,
            subscription_capacity: DEFAULT_SUBSCRIPTION_CAPACITY,
            subscription_mailbox_capacity: DEFAULT_SUBSCRIPTION_MAILBOX_CAPACITY,
            ephemeral_identity_capacity: DEFAULT_EPHEMERAL_IDENTITY_CAPACITY,
            next_worker_id: 1,
            workers: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct MailboxWaiter {
    task_id: TaskId,
    receivers: Vec<(u64, Value)>,
}

pub struct DriverEvents<'a> {
    driver: &'a CompioTaskDriver,
}

impl CompioTaskDriver {
    #[cfg(test)]
    pub fn spawn_with_workers(
        runner: SourceRunner,
        workers: Option<NonZeroUsize>,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_workers_and_external_handler(runner, workers, None)
    }

    #[cfg(test)]
    pub fn spawn_with_workers_and_external_handler(
        runner: SourceRunner,
        workers: Option<NonZeroUsize>,
        external_request_handler: Option<ExternalRequestHandler>,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_config_and_external_handlers(
            runner,
            DispatcherConfig {
                workers,
                ..DispatcherConfig::default()
            },
            external_request_handler,
            None,
        )
    }

    #[cfg(test)]
    pub fn spawn_with_workers_and_external_handlers(
        runner: SourceRunner,
        workers: Option<NonZeroUsize>,
        external_request_handler: Option<ExternalRequestHandler>,
        external_stream_request_handler: Option<ExternalStreamRequestHandler>,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_config_and_external_handlers(
            runner,
            DispatcherConfig {
                workers,
                ..DispatcherConfig::default()
            },
            external_request_handler,
            external_stream_request_handler,
        )
    }

    #[cfg(test)]
    pub fn spawn_with_config_and_external_handlers(
        runner: SourceRunner,
        config: DispatcherConfig,
        external_request_handler: Option<ExternalRequestHandler>,
        external_stream_request_handler: Option<ExternalStreamRequestHandler>,
    ) -> Result<Self, DriverError> {
        Self::spawn_configured(
            runner,
            config,
            None,
            TaskLimits::default(),
            NonZeroUsize::new(DEFAULT_EVENT_QUEUE_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_SUBSCRIPTION_QUEUE_BUDGET).unwrap(),
            NonZeroUsize::new(DEFAULT_EXTERNAL_REQUEST_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_ACTIVE_TASK_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_SUSPENDED_TASK_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_TIMER_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_TERMINAL_TASK_RETENTION).unwrap(),
            NonZeroUsize::new(DEFAULT_ENDPOINT_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_SUBSCRIPTION_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_SUBSCRIPTION_MAILBOX_CAPACITY).unwrap(),
            NonZeroUsize::new(DEFAULT_EPHEMERAL_IDENTITY_CAPACITY).unwrap(),
            RelationAcceleration::Automatic,
            external_request_handler,
            external_stream_request_handler,
        )
    }

    #[cfg(test)]
    pub fn spawn_with_resources(
        runner: SourceRunner,
        resources: DriverResources,
    ) -> Result<Self, DriverError> {
        Self::spawn(runner, resources, None, None)
    }

    pub(crate) fn spawn(
        runner: SourceRunner,
        resources: DriverResources,
        external_request_handler: Option<ExternalRequestHandler>,
        external_stream_request_handler: Option<ExternalStreamRequestHandler>,
    ) -> Result<Self, DriverError> {
        resources.validate()?;
        Self::spawn_configured(
            runner,
            resources.dispatcher_config(),
            Some(resources.relation_parallelism),
            resources.task_limits,
            resources.event_queue_capacity,
            resources.subscription_queue_budget,
            resources.external_request_capacity,
            resources.active_task_capacity,
            resources.suspended_task_capacity,
            resources.timer_capacity,
            resources.terminal_task_retention,
            resources.endpoint_capacity,
            resources.subscription_capacity,
            resources.subscription_mailbox_capacity,
            resources.ephemeral_identity_capacity,
            resources.relation_acceleration,
            external_request_handler,
            external_stream_request_handler,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_configured(
        runner: SourceRunner,
        config: DispatcherConfig,
        relation_parallelism: Option<NonZeroUsize>,
        task_limits: TaskLimits,
        event_queue_capacity: NonZeroUsize,
        subscription_queue_budget: NonZeroUsize,
        external_request_capacity: NonZeroUsize,
        active_task_capacity: NonZeroUsize,
        suspended_task_capacity: NonZeroUsize,
        timer_capacity: NonZeroUsize,
        terminal_task_retention: NonZeroUsize,
        endpoint_capacity: NonZeroUsize,
        subscription_capacity: NonZeroUsize,
        subscription_mailbox_capacity: NonZeroUsize,
        ephemeral_identity_capacity: NonZeroUsize,
        relation_acceleration: RelationAcceleration,
        external_request_handler: Option<ExternalRequestHandler>,
        external_stream_request_handler: Option<ExternalStreamRequestHandler>,
    ) -> Result<Self, DriverError> {
        let (builder, placement) = configure_dispatcher(Dispatcher::builder(), config);
        let relation_parallelism = relation_parallelism.unwrap_or(placement.worker_count);
        let cpu_admission = Arc::new(CpuAdmission::new(
            placement.worker_count,
            relation_parallelism,
        ));
        let external_request_admission =
            Arc::new(ExternalRequestAdmission::new(external_request_capacity));
        let dispatcher = builder
            .thread_names(|index| format!("mica-driver-pool-{index}"))
            .build()
            .map_err(|error| DriverError::Join(format!("failed to start dispatcher: {error}")))?;
        metrics::metrics().drivers_started.inc();
        metrics::metrics()
            .dispatcher_workers_configured
            .set(placement.worker_count.get() as i64);
        tracing::info!(
            driver_workers = placement.worker_count.get(),
            relation_parallelism = relation_parallelism.get(),
            affinity = if placement.is_pinned() {
                "performance cores"
            } else {
                "unrestricted"
            },
            pinned_logical_processors = placement.pinned_core_ids.as_ref().map_or(0, Vec::len),
            "runtime task execution configured"
        );
        let mut execution_context = ExecutionContext::parallel(cpu_admission.clone());
        match relation_acceleration {
            RelationAcceleration::Disabled => {
                tracing::info!(
                    enabled = false,
                    backend = "CPU",
                    "relation acceleration configured"
                );
            }
            RelationAcceleration::Automatic => {
                #[cfg(feature = "wgpu")]
                match relation_accelerator() {
                    AutomaticRelationAccelerator::Enabled(accelerator) => {
                        tracing::info!(
                            enabled = true,
                            backend = "wgpu",
                            graphics_api = "Vulkan",
                            adapter = accelerator.adapter_name(),
                            buffer_mode = if accelerator.uses_shared_mappable_buffers() {
                                "shared-mappable"
                            } else {
                                "staged-readback"
                            },
                            "relation GPU backend configured"
                        );
                        execution_context = execution_context.with_accelerator(accelerator.clone());
                    }
                    AutomaticRelationAccelerator::Unavailable(reason) => {
                        tracing::info!(
                            enabled = false,
                            backend = "wgpu",
                            fallback = "CPU",
                            reason,
                            "relation GPU backend configured"
                        );
                    }
                }
                #[cfg(not(feature = "wgpu"))]
                {
                    tracing::info!(
                        enabled = false,
                        backend = "CPU",
                        fallback = "CPU",
                        reason = "mica-driver built without the `wgpu` feature",
                        "relation acceleration configured"
                    );
                }
            }
            RelationAcceleration::HostProvided(accelerator) => {
                tracing::info!(
                    enabled = true,
                    backend = "host-provided",
                    "relation acceleration configured"
                );
                execution_context = execution_context.with_accelerator(accelerator);
            }
        }
        let mut runner = runner
            .with_task_limits(task_limits)
            .with_execution_context(execution_context);
        runner.forget_all_terminal_tasks();
        let state = PoolState {
            event_capacity: event_queue_capacity.get(),
            active_task_capacity: active_task_capacity.get(),
            suspended_task_capacity: suspended_task_capacity.get(),
            timer_capacity: timer_capacity.get(),
            terminal_task_retention: terminal_task_retention.get(),
            endpoint_capacity: endpoint_capacity.get(),
            subscription_capacity: subscription_capacity.get(),
            subscription_mailbox_capacity: subscription_mailbox_capacity.get(),
            ephemeral_identity_capacity: ephemeral_identity_capacity.get(),
            ..PoolState::default()
        };
        Ok(Self {
            inner: Arc::new(PoolInner {
                runner: Arc::new(runner.into_shared()),
                dispatcher: Mutex::new(Some(dispatcher)),
                cpu_admission,
                external_request_admission,
                external_request_handler,
                external_stream_request_handler,
                subscription_queue_budget,
                next_ephemeral_identity: AtomicU64::new(EPHEMERAL_HOST_IDENTITY_START),
                wake_handler: Mutex::new(None),
                state: Mutex::new(state),
            }),
        })
    }

    pub fn named_identity(&self, name: Symbol) -> Result<Identity, DriverError> {
        self.inner
            .runner
            .named_identity(name)
            .map_err(DriverError::Source)
    }

    pub fn named_relation(&self, name: Symbol) -> Result<(Identity, u16), DriverError> {
        self.inner
            .runner
            .named_relation(name)
            .map_err(DriverError::Source)
    }

    /// Allocates a process-local identity that is never written as durable
    /// world identity policy.
    pub fn allocate_ephemeral_identity(&self) -> Result<Identity, DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        if state.allocated_ephemeral_identities.len() >= state.ephemeral_identity_capacity {
            return Err(DriverError::Configuration(
                "driver ephemeral identity capacity is exhausted".to_owned(),
            ));
        }
        let raw = self
            .inner
            .next_ephemeral_identity
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < EPHEMERAL_HOST_IDENTITY_END).then_some(current + 1)
            })
            .map_err(|_| DriverError::EphemeralIdentityExhausted)?;
        let identity = Identity::new(raw).ok_or(DriverError::EphemeralIdentityExhausted)?;
        state.allocated_ephemeral_identities.insert(identity);
        Ok(identity)
    }

    pub fn format_error(&self, error: &DriverError) -> String {
        match error {
            DriverError::Source(error) => self.inner.runner.render_source_task_error(error),
            DriverError::Storage(error) => format!("failed to open driver storage: {error}"),
            DriverError::Configuration(error) => {
                format!("invalid driver configuration: {error}")
            }
            DriverError::Join(error) => format!("driver task failed: {error}"),
            DriverError::MissingTaskContext(task_id) => {
                format!("missing task context for task {task_id}")
            }
            DriverError::EphemeralIdentityExhausted => {
                "ephemeral host identity space is exhausted".to_owned()
            }
            DriverError::TaskCancelled(task_id) => format!("task {task_id} was cancelled"),
            DriverError::EndpointClosed(endpoint) => {
                format!(
                    "endpoint {} is closed",
                    self.inner.runner.render_identity(*endpoint)
                )
            }
            DriverError::DriverStopped => "task driver is stopped".to_owned(),
        }
    }

    pub fn format_value(&self, value: &Value) -> String {
        self.inner.runner.render_task_value(value)
    }

    #[cfg(test)]
    pub async fn submit_source(
        &self,
        endpoint: Identity,
        mut request: TaskRequest,
    ) -> Result<SubmittedTask, DriverError> {
        self.ensure_endpoint_open(endpoint)?;
        request.endpoint = endpoint;
        let context = TaskContext::from_request(&request, endpoint);
        let runner = Arc::clone(&self.inner.runner);
        let submitted = self
            .dispatch(DispatchOperation::Submit, move || async move {
                runner.submit_source(request)
            })
            .await?;
        self.handle_submitted(context, submitted.clone()).await?;
        Ok(submitted)
    }

    pub(crate) async fn submit_source_handle(
        &self,
        endpoint: Identity,
        source: String,
    ) -> Result<InvocationHandle, DriverError> {
        let _activity = self.begin_endpoint_activity(endpoint)?;
        let runner = Arc::clone(&self.inner.runner);
        let (context, submitted) = self
            .dispatch(DispatchOperation::Submit, move || async move {
                let request = runner.source_request_for_endpoint(endpoint, source)?;
                let context = TaskContext::from_request(&request, endpoint);
                let submitted = runner.submit_source(request)?;
                Ok((context, submitted))
            })
            .await?;
        self.install_invocation_handle(Symbol::intern("eval"), context, submitted)
            .await
    }

    pub async fn run_read_only_source_query(
        &self,
        endpoint: Identity,
        source: String,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, DriverError> {
        let _activity = self.begin_endpoint_activity(endpoint)?;
        let runner = Arc::clone(&self.inner.runner);
        self.dispatch(DispatchOperation::Submit, move || async move {
            runner.run_read_only_source_query_for_endpoint(endpoint, source, options)
        })
        .await
    }

    #[cfg(test)]
    pub async fn submit_root_source_report(
        &self,
        source: String,
    ) -> Result<RunReport, DriverError> {
        self.ensure_running()?;
        let context = TaskContext {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
        };
        let runner = Arc::clone(&self.inner.runner);
        let submitted = self
            .dispatch(DispatchOperation::RootSubmit, move || async move {
                runner.submit_root_source(source)
            })
            .await?;
        self.handle_submitted(context, submitted.clone()).await?;
        Ok(self
            .inner
            .runner
            .report_outcome(submitted.task_id, submitted.outcome))
    }

    pub(crate) async fn submit_root_source_handle(
        &self,
        source: String,
    ) -> Result<InvocationHandle, DriverError> {
        self.ensure_running()?;
        let context = TaskContext {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
        };
        let runner = Arc::clone(&self.inner.runner);
        let submitted = self
            .dispatch(DispatchOperation::RootSubmit, move || async move {
                runner.submit_root_source(source)
            })
            .await?;
        self.install_invocation_handle(Symbol::intern("eval"), context, submitted)
            .await
    }

    pub async fn check_filein(
        &self,
        source: String,
        include_loader: Option<FileinIncludeLoader>,
    ) -> Result<Vec<RunReport>, DriverError> {
        let runner = Arc::clone(&self.inner.runner);
        self.dispatch(DispatchOperation::Filein, move || async move {
            match include_loader {
                Some(loader) => {
                    runner.check_filein_with_include_loader(&source, |path| loader(path))
                }
                None => runner.check_filein(&source),
            }
        })
        .await
    }

    pub async fn filein_unit(
        &self,
        unit: Symbol,
        source: String,
        mode: FileinMode,
        include_loader: Option<FileinIncludeLoader>,
    ) -> Result<FileinReport, DriverError> {
        let runner = Arc::clone(&self.inner.runner);
        let report = self
            .dispatch(DispatchOperation::Filein, move || async move {
                match include_loader {
                    Some(loader) => runner.run_filein_with_unit_and_include_loader(
                        unit,
                        &source,
                        mode,
                        |path| loader(path),
                    ),
                    None => runner.run_filein_with_unit(unit, &source, mode),
                }
            })
            .await?;
        for task_report in &report.reports {
            self.inner.runner.forget_terminal_task(task_report.task_id);
        }
        self.enqueue_pending_effects().await;
        Ok(report)
    }

    pub async fn fileout_unit(&self, unit: Symbol) -> Result<String, DriverError> {
        let runner = Arc::clone(&self.inner.runner);
        self.dispatch(DispatchOperation::Fileout, move || async move {
            runner.fileout_unit(unit)
        })
        .await
    }

    #[cfg(test)]
    pub async fn submit_invocation(
        &self,
        endpoint: Identity,
        request: TaskRequest,
    ) -> Result<SubmittedTask, DriverError> {
        self.ensure_endpoint_open(endpoint)?;
        let mut request = request;
        request.endpoint = endpoint;
        let runner = Arc::clone(&self.inner.runner);
        let context = TaskContext::from_request(&request, endpoint);
        let submitted = self
            .dispatch(DispatchOperation::Invoke, move || async move {
                runner.submit_invocation(request)
            })
            .await?;
        self.handle_submitted(context, submitted.clone()).await?;
        Ok(submitted)
    }

    pub(crate) async fn submit_invocation_handle(
        &self,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<InvocationHandle, DriverError> {
        let _activity = self.begin_endpoint_activity(endpoint)?;
        let runner = Arc::clone(&self.inner.runner);
        let (context, submitted) = self
            .dispatch(DispatchOperation::Invoke, move || async move {
                let request = runner.invocation_request_for_endpoint(endpoint, selector, roles)?;
                let context = TaskContext::from_request(&request, endpoint);
                let submitted = runner.submit_invocation(request)?;
                Ok((context, submitted))
            })
            .await?;
        self.install_invocation_handle(selector, context, submitted)
            .await
    }

    async fn install_invocation_handle(
        &self,
        selector: Symbol,
        context: TaskContext,
        submitted: SubmittedTask,
    ) -> Result<InvocationHandle, DriverError> {
        let task_id = submitted.task_id;
        let initial_report = self
            .inner
            .runner
            .report_outcome(task_id, submitted.outcome.clone());
        let state = Arc::new(InvocationState::new(
            InvocationDiagnostics {
                task_id,
                selector,
                endpoint: context.endpoint,
                principal: context.principal,
                actor: context.actor,
            },
            initial_report,
        ));
        self.inner
            .state
            .lock()
            .unwrap()
            .invocations
            .insert(task_id, Arc::clone(&state));
        if let Err(error) = self.handle_submitted(context, submitted).await {
            self.inner
                .state
                .lock()
                .unwrap()
                .invocations
                .remove(&task_id);
            return Err(error);
        }
        Ok(InvocationHandle::new(self.clone(), state))
    }

    pub async fn resume(&self, task_id: TaskId, value: Value) -> Result<TaskOutcome, DriverError> {
        let (context, _activity) = {
            let mut state = self.inner.state.lock().unwrap();
            let context = match state.contexts.get(&task_id) {
                Some(context) => context.clone(),
                None if state.terminal_tasks.get(&task_id)
                    == Some(&TerminalTaskKind::Cancelled) =>
                {
                    return Err(DriverError::TaskCancelled(task_id));
                }
                None => return Err(DriverError::MissingTaskContext(task_id)),
            };
            if state.lifecycle != DriverLifecycle::Running {
                return Err(DriverError::DriverStopped);
            }
            if state.closed_endpoints.contains(&context.endpoint) {
                return Err(DriverError::EndpointClosed(context.endpoint));
            }
            state.contexts.remove(&task_id);
            state.remove_mailbox_waiter(task_id);
            *state
                .endpoint_activities
                .entry(context.endpoint)
                .or_default() += 1;
            state.record_metrics();
            (
                context.clone(),
                EndpointActivity {
                    inner: Arc::clone(&self.inner),
                    endpoint: context.endpoint,
                },
            )
        };
        let runner = Arc::clone(&self.inner.runner);
        let request = TaskRequest {
            principal: context.principal,
            actor: context.actor,
            endpoint: context.endpoint,
            authority: context.authority.clone(),
            input: TaskInput::Continuation { task_id, value },
        };
        let outcome = self
            .dispatch(DispatchOperation::Resume, move || async move {
                runner.resume_task(request)
            })
            .await?;
        self.handle_submitted(
            context,
            SubmittedTask {
                task_id,
                outcome: outcome.clone(),
            },
        )
        .await?;
        Ok(outcome)
    }

    pub async fn input(
        &self,
        endpoint: Identity,
        value: Value,
    ) -> Result<Vec<TaskOutcome>, DriverError> {
        let _activity = self.begin_endpoint_activity(endpoint)?;
        let task_ids = self
            .inner
            .state
            .lock()
            .unwrap()
            .remove_input_waiters(endpoint);
        let mut outcomes = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            outcomes.push(self.resume(task_id, value.clone()).await?);
        }
        Ok(outcomes)
    }

    #[cfg(test)]
    pub fn open_endpoint(
        &self,
        endpoint: Identity,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), DriverError> {
        self.ensure_running()?;
        self.inner
            .runner
            .open_endpoint(endpoint, actor, protocol)
            .map_err(DriverError::Source)?;
        let mut state = self.inner.state.lock().unwrap();
        state.closed_endpoints.remove(&endpoint);
        state
            .endpoints
            .insert(endpoint, EndpointResources::default());
        Ok(())
    }

    pub fn open_endpoint_with_context_and_volatile_tuples_named(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        let tuples = tuples.into_iter().collect::<BTreeSet<_>>();
        let mut state = self.inner.state.lock().unwrap();
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        if state.endpoints.contains_key(&endpoint) {
            return Err(DriverError::Configuration(format!(
                "endpoint {endpoint:?} is already open"
            )));
        }
        if (EPHEMERAL_HOST_IDENTITY_START..EPHEMERAL_HOST_IDENTITY_END).contains(&endpoint.raw())
            && !state.allocated_ephemeral_identities.contains(&endpoint)
        {
            return Err(DriverError::Configuration(format!(
                "endpoint {endpoint:?} is in the driver-reserved identity range but was not allocated by this driver"
            )));
        }
        if state.endpoints.len() >= state.endpoint_capacity {
            return Err(DriverError::Configuration(
                "driver endpoint capacity is exhausted".to_owned(),
            ));
        }
        let mut asserted = Vec::new();
        let mut ownership = Vec::with_capacity(tuples.len());
        for fact @ (relation, tuple) in &tuples {
            if state.volatile_fact_owners.contains_key(fact) {
                ownership.push((fact.clone(), None));
                continue;
            }
            let present = self
                .inner
                .runner
                .contains_named_tuple(*relation, tuple)
                .map_err(DriverError::Source)?;
            ownership.push((fact.clone(), Some(!present)));
            if !present {
                asserted.push(fact.clone());
            }
        }
        let changes = self
            .inner
            .runner
            .open_endpoint_with_context_and_volatile_tuples_named(
                endpoint, principal, actor, protocol, asserted,
            )
            .map_err(DriverError::Source)?;
        for (fact, first_owner_asserted) in ownership {
            match state.volatile_fact_owners.get_mut(&fact) {
                Some(owner) => owner.owners += 1,
                None => {
                    state.volatile_fact_owners.insert(
                        fact,
                        VolatileFactOwnership {
                            owners: 1,
                            asserted_by_driver: first_owner_asserted
                                .expect("a first owner records fact provenance"),
                        },
                    );
                }
            }
        }
        state.endpoints.insert(
            endpoint,
            EndpointResources {
                scopes: HashMap::from([(Symbol::intern("endpoint"), tuples)]),
                subscriptions: HashSet::new(),
            },
        );
        state.closed_endpoints.remove(&endpoint);
        Ok(changes)
    }

    #[cfg(test)]
    pub async fn close_endpoint(&self, endpoint: Identity) -> EndpointCloseReport {
        self.close_endpoint_resources(endpoint)
            .await
            .expect("test endpoint close should succeed")
    }

    pub async fn close_endpoint_resources(
        &self,
        endpoint: Identity,
    ) -> Result<EndpointCloseReport, DriverError> {
        self.mark_endpoint_closed(endpoint);
        self.wait_until_endpoint_idle(endpoint).await;
        let cancelled_tasks = self.cancel_endpoint_tasks(endpoint).await;
        let mut state = self.inner.state.lock().unwrap();
        let Some(resources) = state.endpoints.get(&endpoint).cloned() else {
            return Ok(EndpointCloseReport {
                relation_changes: 0,
                cancelled_tasks,
            });
        };
        let owned_facts = resources
            .scopes
            .into_values()
            .flatten()
            .collect::<BTreeSet<_>>();
        let retract = owned_facts
            .iter()
            .filter(|fact| {
                state
                    .volatile_fact_owners
                    .get(*fact)
                    .is_some_and(|owner| owner.owners == 1 && owner.asserted_by_driver)
            })
            .cloned()
            .collect();
        let relation_changes = self
            .inner
            .runner
            .close_endpoint_and_retract_volatile_tuples_named(endpoint, retract)
            .map_err(DriverError::Source)?;
        state.endpoints.remove(&endpoint);
        for fact in owned_facts {
            let Some(owner) = state.volatile_fact_owners.get_mut(&fact) else {
                continue;
            };
            owner.owners -= 1;
            if owner.owners == 0 {
                state.volatile_fact_owners.remove(&fact);
            }
        }
        for subscription in resources.subscriptions {
            state.subscriptions.remove(&subscription);
        }
        Ok(EndpointCloseReport {
            relation_changes,
            cancelled_tasks,
        })
    }

    pub(crate) fn close_endpoint_resources_in_background(
        &self,
        endpoint: Identity,
    ) -> Result<(), DriverError> {
        self.ensure_running()?;
        self.mark_endpoint_closed(endpoint);
        let driver = self.clone();
        let future = self.wake_on_ready(async move {
            let _ = driver.close_endpoint_resources(endpoint).await;
        });
        let handle =
            compio::runtime::Runtime::try_with_current(move |runtime| runtime.spawn(future))
                .map_err(|_| {
                    DriverError::Configuration(
                        "background endpoint close requires an active Compio runtime".to_owned(),
                    )
                })?;
        let _ = self.track_worker(0, false, handle);
        Ok(())
    }

    pub async fn cancel_task(&self, task_id: TaskId) -> Result<SuspendKind, DriverError> {
        self.cancel_task_with_reason(task_id, TaskCancellationReason::Requested)
            .await
    }

    pub(crate) fn detach_invocation(&self, task_id: TaskId) -> Result<(), DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        if !state.contexts.contains_key(&task_id) {
            return Err(DriverError::Configuration(format!(
                "task {task_id} is not a suspended invocation"
            )));
        }
        if state.invocations.remove(&task_id).is_none() {
            return Err(DriverError::Configuration(format!(
                "task {task_id} does not have an invocation handle"
            )));
        }
        Ok(())
    }

    pub(crate) async fn publish_watched_invocation(
        &self,
        task_id: TaskId,
        outcome: InvocationOutcome,
    ) {
        let event = match outcome {
            InvocationOutcome::Completed(value) => DriverEvent::TaskCompleted { task_id, value },
            InvocationOutcome::Aborted(error) => DriverEvent::TaskAborted { task_id, error },
            InvocationOutcome::Cancelled(reason) => DriverEvent::TaskCancelled { task_id, reason },
            InvocationOutcome::Failed(error) => DriverEvent::TaskFailed { task_id, error },
        };
        self.enqueue_event(event).await;
    }

    pub fn replace_endpoint_volatile_scope(
        &self,
        endpoint: Identity,
        scope: Symbol,
        facts: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        self.ensure_endpoint_resource_update(&state, endpoint)?;
        self.replace_endpoint_volatile_scope_locked(
            &mut state,
            endpoint,
            scope,
            facts.into_iter().collect(),
        )
    }

    pub fn apply_endpoint_volatile_scope_diff(
        &self,
        endpoint: Identity,
        scope: Symbol,
        retract: Vec<(Symbol, Tuple)>,
        assert: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        self.ensure_endpoint_resource_update(&state, endpoint)?;
        let previous = state
            .endpoints
            .get(&endpoint)
            .and_then(|resources| resources.scopes.get(&scope))
            .cloned()
            .unwrap_or_default();
        let retract = retract.into_iter().collect::<BTreeSet<_>>();
        if let Some((relation, tuple)) = retract.difference(&previous).next() {
            return Err(DriverError::Configuration(format!(
                "volatile scope {} cannot retract {}{tuple:?} because it does not own that fact",
                scope.name().unwrap_or("<unnamed>"),
                relation.name().unwrap_or("<unnamed>"),
            )));
        }
        let mut next = previous;
        for fact in retract {
            next.remove(&fact);
        }
        next.extend(assert);
        self.replace_endpoint_volatile_scope_locked(&mut state, endpoint, scope, next)
    }

    fn ensure_endpoint_resource_update(
        &self,
        state: &PoolState,
        endpoint: Identity,
    ) -> Result<(), DriverError> {
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        if state.closed_endpoints.contains(&endpoint) || !state.endpoints.contains_key(&endpoint) {
            return Err(DriverError::EndpointClosed(endpoint));
        }
        Ok(())
    }

    fn replace_endpoint_volatile_scope_locked(
        &self,
        state: &mut PoolState,
        endpoint: Identity,
        scope: Symbol,
        next_scope: BTreeSet<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        let resources = state
            .endpoints
            .get(&endpoint)
            .expect("an open endpoint has driver-owned resources");
        let previous_scope = resources.scopes.get(&scope).cloned().unwrap_or_default();
        let other_facts = resources
            .scopes
            .iter()
            .filter(|(current, _)| **current != scope)
            .flat_map(|(_, facts)| facts.iter().cloned())
            .collect::<BTreeSet<_>>();
        let previous_owned = previous_scope
            .union(&other_facts)
            .cloned()
            .collect::<BTreeSet<_>>();
        let next_owned = next_scope
            .union(&other_facts)
            .cloned()
            .collect::<BTreeSet<_>>();
        let lost = previous_owned
            .difference(&next_owned)
            .cloned()
            .collect::<Vec<_>>();
        let gained = next_owned
            .difference(&previous_owned)
            .cloned()
            .collect::<Vec<_>>();
        let retract = lost
            .iter()
            .filter(|fact| {
                state
                    .volatile_fact_owners
                    .get(*fact)
                    .is_some_and(|owner| owner.owners == 1 && owner.asserted_by_driver)
            })
            .cloned()
            .collect();
        let mut assert = Vec::new();
        let mut first_owner_provenance = Vec::new();
        for fact @ (relation, tuple) in &gained {
            if state.volatile_fact_owners.contains_key(fact) {
                continue;
            }
            let present = self
                .inner
                .runner
                .contains_named_tuple(*relation, tuple)
                .map_err(DriverError::Source)?;
            first_owner_provenance.push((fact.clone(), !present));
            if !present {
                assert.push(fact.clone());
            }
        }
        let changes = self
            .inner
            .runner
            .replace_volatile_tuples_named(retract, assert)
            .map_err(DriverError::Source)?;
        for fact in lost {
            let owner = state
                .volatile_fact_owners
                .get_mut(&fact)
                .expect("an endpoint-owned fact has provenance");
            owner.owners -= 1;
            if owner.owners == 0 {
                state.volatile_fact_owners.remove(&fact);
            }
        }
        for fact in gained {
            if let Some(owner) = state.volatile_fact_owners.get_mut(&fact) {
                owner.owners += 1;
                continue;
            }
            let asserted_by_driver = first_owner_provenance
                .iter()
                .find_map(|(current, asserted)| (current == &fact).then_some(*asserted))
                .expect("a first owner records fact provenance");
            state.volatile_fact_owners.insert(
                fact,
                VolatileFactOwnership {
                    owners: 1,
                    asserted_by_driver,
                },
            );
        }
        state
            .endpoints
            .get_mut(&endpoint)
            .expect("an open endpoint retains resources")
            .scopes
            .insert(scope, next_scope);
        Ok(changes)
    }

    pub fn create_subscription_mailbox(&self) -> Result<DriverSubscriptionMailbox, DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        if state.external_subscription_mailboxes.len() >= state.subscription_mailbox_capacity {
            return Err(DriverError::Configuration(
                "driver subscription mailbox capacity is exhausted".to_owned(),
            ));
        }
        let (receiver, sender) = self
            .inner
            .runner
            .create_mailbox()
            .map_err(runtime_driver_error)?;
        let mailbox = self
            .inner
            .runner
            .mailbox_for_receiver(&receiver)
            .map_err(runtime_driver_error)?;
        state
            .external_subscription_mailboxes
            .insert(mailbox, receiver.clone());
        Ok(DriverSubscriptionMailbox {
            mailbox,
            receiver,
            sender,
        })
    }

    pub async fn register_subscription_for_endpoint(
        &self,
        endpoint: Identity,
        mailbox: &DriverSubscriptionMailbox,
        request: DriverSubscriptionRequest,
    ) -> Result<Value, DriverError> {
        let _activity = self.begin_endpoint_activity(endpoint)?;
        {
            let state = self.inner.state.lock().unwrap();
            if state.subscriptions.len() >= state.subscription_capacity {
                return Err(DriverError::Configuration(
                    "driver subscription capacity is exhausted".to_owned(),
                ));
            }
            if !state
                .external_subscription_mailboxes
                .contains_key(&mailbox.mailbox)
            {
                return Err(DriverError::Configuration(
                    "subscription mailbox is closed".to_owned(),
                ));
            }
        }
        let subscription = self
            .inner
            .runner
            .register_subscription_for_endpoint(
                endpoint,
                SubscriptionRequest {
                    sender: mailbox.sender.clone(),
                    subject: request.subject,
                    initial_delivery: request.initial_delivery,
                    cursor: request.cursor,
                    queue_budget: request
                        .queue_budget
                        .unwrap_or(self.inner.subscription_queue_budget)
                        .get(),
                },
            )
            .map_err(DriverError::Source)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.lifecycle != DriverLifecycle::Running
                || state.closed_endpoints.contains(&endpoint)
            {
                drop(state);
                let _ = self.inner.runner.cancel_subscription(subscription.clone());
                return Err(DriverError::EndpointClosed(endpoint));
            }
            state.subscriptions.insert(
                subscription.clone(),
                SubscriptionOwner {
                    endpoint,
                    mailbox: mailbox.mailbox,
                },
            );
            state
                .endpoints
                .get_mut(&endpoint)
                .expect("an active endpoint retains resources")
                .subscriptions
                .insert(subscription.clone());
        }
        let delivered = self.inner.runner.take_subscription_deliveries();
        let mut queue = VecDeque::new();
        self.route_mailbox_deliveries(delivered, &mut queue).await?;
        self.process_outcome_queue(&mut queue).await?;
        Ok(subscription)
    }

    pub fn cancel_subscription_for_endpoint(
        &self,
        endpoint: Identity,
        subscription: Value,
    ) -> Result<(), DriverError> {
        self.ensure_running()?;
        let mut state = self.inner.state.lock().unwrap();
        let Some(owner) = state.subscriptions.get(&subscription) else {
            return Err(DriverError::Configuration(
                "subscription is not active".to_owned(),
            ));
        };
        if owner.endpoint != endpoint {
            return Err(DriverError::Configuration(
                "endpoint cannot cancel a subscription it does not own".to_owned(),
            ));
        }
        self.inner
            .runner
            .cancel_subscription(subscription.clone())
            .map_err(runtime_driver_error)?;
        state.subscriptions.remove(&subscription);
        if let Some(resources) = state.endpoints.get_mut(&endpoint) {
            resources.subscriptions.remove(&subscription);
        }
        Ok(())
    }

    pub fn close_subscription_mailbox(
        &self,
        mailbox: &DriverSubscriptionMailbox,
    ) -> Result<(), DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        let Some(receiver) = state
            .external_subscription_mailboxes
            .remove(&mailbox.mailbox)
        else {
            return Ok(());
        };
        self.inner
            .runner
            .close_mailbox(&receiver)
            .map_err(runtime_driver_error)?;
        let subscriptions = state
            .subscriptions
            .iter()
            .filter_map(|(subscription, owner)| {
                (owner.mailbox == mailbox.mailbox).then_some((subscription.clone(), owner.endpoint))
            })
            .collect::<Vec<_>>();
        for (subscription, endpoint) in subscriptions {
            state.subscriptions.remove(&subscription);
            if let Some(resources) = state.endpoints.get_mut(&endpoint) {
                resources.subscriptions.remove(&subscription);
            }
        }
        Ok(())
    }

    pub fn drain_subscription_mailbox(
        &self,
        mailbox: &DriverSubscriptionMailbox,
    ) -> Result<Vec<Value>, DriverError> {
        self.ensure_running()?;
        self.inner
            .runner
            .drain_mailbox(mailbox.receiver.clone())
            .map_err(runtime_driver_error)
    }

    /// Drains every currently queued event and releases blocked producers.
    ///
    /// A driver event stream has one logical consumer. Calling this method or
    /// [`Self::wait_events`] from competing consumers divides events between
    /// them rather than broadcasting copies.
    pub fn drain_events(&self) -> Vec<DriverEvent> {
        let mut state = self.inner.state.lock().unwrap();
        state.reap_finished_workers();
        let events = state.events.drain(..).collect();
        let space_wakers = std::mem::take(&mut state.event_space_wakers);
        state.record_metrics();
        drop(state);
        for waker in space_wakers {
            waker.wake();
        }
        events
    }

    /// Waits until at least one event is available, then drains the queue.
    ///
    /// The returned future participates in the same single-consumer stream as
    /// [`Self::drain_events`]. Draining capacity wakes producers applying
    /// backpressure at the configured queue bound.
    pub fn wait_events(&self) -> DriverEvents<'_> {
        DriverEvents { driver: self }
    }

    pub async fn shutdown(&self) -> Result<(), DriverError> {
        enum ShutdownStart {
            Start(Vec<JoinHandle<()>>),
            Wait,
            Complete(Result<(), DriverError>),
        }
        let start = {
            let mut state = self.inner.state.lock().unwrap();
            match state.lifecycle {
                DriverLifecycle::Running => {
                    state.lifecycle = DriverLifecycle::ShuttingDown;
                    state.reap_finished_workers();
                    ShutdownStart::Start(
                        state
                            .workers
                            .drain()
                            .map(|(_, worker)| {
                                if let Some(cancellation) = worker.cancellation {
                                    cancellation.cancel();
                                }
                                worker.handle
                            })
                            .collect(),
                    )
                }
                DriverLifecycle::ShuttingDown => ShutdownStart::Wait,
                DriverLifecycle::Stopped => ShutdownStart::Complete(shutdown_result(&state)),
            }
        };
        let workers = match start {
            ShutdownStart::Start(workers) => workers,
            ShutdownStart::Wait => return self.wait_until_stopped().await,
            ShutdownStart::Complete(result) => return result,
        };

        let mut worker_error = None;
        for worker in workers {
            if matches!(worker.cancel().await, Some(Err(_))) {
                worker_error = Some("driver async worker panicked during shutdown".to_owned());
            }
        }
        self.wait_until_idle().await;

        let task_ids = {
            let state = self.inner.state.lock().unwrap();
            state.contexts.keys().copied().collect::<Vec<_>>()
        };
        for task_id in task_ids {
            let _ = self
                .cancel_task_with_reason(task_id, TaskCancellationReason::DriverShutdown)
                .await;
        }
        for (task_id, _) in self.inner.runner.cancel_all_tasks() {
            self.record_cancelled_task(task_id, TaskCancellationReason::DriverShutdown)
                .await;
        }
        self.inner.runner.cancel_all_subscriptions();

        let (endpoints, volatile_facts, subscription_mailboxes) = {
            let mut state = self.inner.state.lock().unwrap();
            let endpoints = state
                .endpoints
                .drain()
                .map(|(endpoint, _)| endpoint)
                .collect::<Vec<_>>();
            let volatile_facts = std::mem::take(&mut state.volatile_fact_owners)
                .into_iter()
                .filter_map(|(fact, ownership)| ownership.asserted_by_driver.then_some(fact))
                .collect::<Vec<_>>();
            let subscription_mailboxes = state
                .external_subscription_mailboxes
                .drain()
                .map(|(_, receiver)| receiver)
                .collect::<Vec<_>>();
            state.subscriptions.clear();
            (endpoints, volatile_facts, subscription_mailboxes)
        };
        if let Err(error) = self
            .inner
            .runner
            .replace_volatile_tuples_named(volatile_facts, Vec::new())
        {
            worker_error = Some(format!(
                "failed to retract driver-owned volatile facts during shutdown: {}",
                self.inner.runner.render_source_task_error(&error)
            ));
        }
        for endpoint in endpoints {
            self.inner.runner.close_endpoint(endpoint);
        }
        for receiver in subscription_mailboxes {
            if let Err(error) = self.inner.runner.close_mailbox(&receiver) {
                worker_error = Some(format!(
                    "failed to close subscription mailbox during shutdown: {error:?}"
                ));
            }
        }

        let persistence_error = self.inner.runner.flush_persistence().err().map(|error| {
            format!(
                "failed to flush persistent state: {}",
                self.inner.runner.render_source_task_error(&error)
            )
        });

        let dispatcher = self.inner.dispatcher.lock().unwrap().take();
        let join_error = match dispatcher {
            Some(dispatcher) => dispatcher
                .join()
                .await
                .err()
                .map(|error| format!("failed to join dispatcher: {error}")),
            None => None,
        };
        let error = worker_error.or(persistence_error).or(join_error);
        let shutdown_wakers = {
            let mut state = self.inner.state.lock().unwrap();
            state.shutdown_error = error.clone();
            state.lifecycle = DriverLifecycle::Stopped;
            std::mem::take(&mut state.shutdown_wakers)
        };
        for waker in shutdown_wakers {
            waker.wake();
        }
        match error {
            Some(error) => Err(DriverError::Join(error)),
            None => Ok(()),
        }
    }

    pub fn is_shutdown(&self) -> bool {
        self.inner.state.lock().unwrap().lifecycle == DriverLifecycle::Stopped
    }

    pub(crate) fn shutdown_in_background_discarding_events(&self) -> bool {
        if compio::runtime::Runtime::try_with_current(|_| ()).is_err() {
            return false;
        }
        let space_wakers = {
            let mut state = self.inner.state.lock().unwrap();
            if state.lifecycle != DriverLifecycle::Running {
                return true;
            }
            state.discard_events = true;
            state.events.clear();
            std::mem::take(&mut state.event_space_wakers)
        };
        for waker in space_wakers {
            waker.wake();
        }
        let driver = self.clone();
        let shutdown = self.wake_on_ready(async move {
            let _ = driver.shutdown().await;
        });
        compio::runtime::Runtime::try_with_current(move |runtime| {
            runtime.spawn(shutdown).detach();
        })
        .is_ok()
    }

    pub fn resource_snapshot(&self) -> DriverResourceSnapshot {
        let mut state = self.inner.state.lock().unwrap();
        state.reap_finished_workers();
        DriverResourceSnapshot {
            ephemeral_identities: state.allocated_ephemeral_identities.len(),
            endpoints: state.endpoints.len(),
            subscription_mailboxes: state.external_subscription_mailboxes.len(),
            subscriptions: state.subscriptions.len(),
            active_tasks: state.active_tasks.len(),
            suspended_tasks: state.contexts.len(),
            timers: state.workers.values().filter(|worker| worker.timer).count(),
            retained_terminal_tasks: state.terminal_tasks.len() + self.inner.runner.completed_len(),
            queued_events: state.events.len(),
            async_workers: state.workers.len(),
        }
    }

    pub(crate) fn event_queue_capacity(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.inner.state.lock().unwrap().event_capacity)
            .expect("driver event capacity is non-zero")
    }

    fn ensure_running(&self) -> Result<(), DriverError> {
        if self.inner.state.lock().unwrap().lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        Ok(())
    }

    #[cfg(test)]
    fn ensure_endpoint_open(&self, endpoint: Identity) -> Result<(), DriverError> {
        let state = self.inner.state.lock().unwrap();
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        if state.closed_endpoints.contains(&endpoint) {
            return Err(DriverError::EndpointClosed(endpoint));
        }
        Ok(())
    }

    fn mark_endpoint_closed(&self, endpoint: Identity) {
        let mut state = self.inner.state.lock().unwrap();
        state.closed_endpoints.insert(endpoint);
    }

    async fn cancel_endpoint_tasks(&self, endpoint: Identity) -> Vec<TaskId> {
        let task_ids = {
            let state = self.inner.state.lock().unwrap();
            state
                .contexts
                .iter()
                .filter_map(|(task_id, context)| (context.endpoint == endpoint).then_some(*task_id))
                .collect::<Vec<_>>()
        };
        let mut cancelled = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            if self
                .cancel_task_with_reason(task_id, TaskCancellationReason::EndpointClosed)
                .await
                .is_ok()
            {
                cancelled.push(task_id);
            }
        }
        cancelled
    }

    async fn cancel_task_with_reason(
        &self,
        task_id: TaskId,
        reason: TaskCancellationReason,
    ) -> Result<SuspendKind, DriverError> {
        let (kind, workers) = {
            let mut state = self.inner.state.lock().unwrap();
            let Some(_) = state.contexts.remove(&task_id) else {
                if state.terminal_tasks.get(&task_id) == Some(&TerminalTaskKind::Cancelled) {
                    return Err(DriverError::TaskCancelled(task_id));
                }
                return Err(DriverError::MissingTaskContext(task_id));
            };
            let kind = self
                .inner
                .runner
                .cancel_task(task_id)
                .map_err(DriverError::Source)?;
            state.remove_task_waiters(task_id);
            state.record_terminal_task(task_id, TerminalTaskKind::Cancelled);
            let workers = state.take_task_workers(task_id);
            state.record_metrics();
            (kind, workers)
        };
        drop(workers);
        if !self.complete_invocation(task_id, InvocationOutcome::Cancelled(reason)) {
            self.enqueue_event(DriverEvent::TaskCancelled { task_id, reason })
                .await;
        }
        self.inner.runner.forget_terminal_task(task_id);
        Ok(kind)
    }

    async fn record_cancelled_task(&self, task_id: TaskId, reason: TaskCancellationReason) {
        let should_record = {
            let mut state = self.inner.state.lock().unwrap();
            if state.terminal_tasks.contains_key(&task_id) {
                false
            } else {
                state.record_terminal_task(task_id, TerminalTaskKind::Cancelled);
                state.remove_task_waiters(task_id);
                state.contexts.remove(&task_id);
                state.record_metrics();
                true
            }
        };
        if should_record {
            if !self.complete_invocation(task_id, InvocationOutcome::Cancelled(reason)) {
                self.enqueue_event(DriverEvent::TaskCancelled { task_id, reason })
                    .await;
            }
            self.inner.runner.forget_terminal_task(task_id);
        }
    }

    async fn fail_suspended_task_for_capacity(
        &self,
        task_id: TaskId,
        resource: &str,
    ) -> Result<(), DriverError> {
        let error = format!("driver {resource} capacity is exhausted");
        let workers = {
            let mut state = self.inner.state.lock().unwrap();
            if state.contexts.remove(&task_id).is_none() {
                return Ok(());
            }
            self.inner
                .runner
                .cancel_task(task_id)
                .map_err(DriverError::Source)?;
            state.remove_task_waiters(task_id);
            state.record_terminal_task(task_id, TerminalTaskKind::Failed);
            let workers = state.take_task_workers(task_id);
            state.record_metrics();
            workers
        };
        drop(workers);
        if !self.complete_invocation(task_id, InvocationOutcome::Failed(error.clone())) {
            self.enqueue_event(DriverEvent::TaskFailed { task_id, error })
                .await;
        }
        self.inner.runner.forget_terminal_task(task_id);
        Ok(())
    }

    fn begin_dispatch(&self) -> Result<DispatchActivity, DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        state.in_flight_dispatches += 1;
        Ok(DispatchActivity {
            inner: Arc::clone(&self.inner),
        })
    }

    fn begin_endpoint_activity(&self, endpoint: Identity) -> Result<EndpointActivity, DriverError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        if state.closed_endpoints.contains(&endpoint) {
            return Err(DriverError::EndpointClosed(endpoint));
        }
        *state.endpoint_activities.entry(endpoint).or_default() += 1;
        Ok(EndpointActivity {
            inner: Arc::clone(&self.inner),
            endpoint,
        })
    }

    fn wait_until_idle(&self) -> DriverIdle<'_> {
        DriverIdle { driver: self }
    }

    fn wait_until_endpoint_idle(&self, endpoint: Identity) -> EndpointIdle<'_> {
        EndpointIdle {
            driver: self,
            endpoint,
        }
    }

    fn wait_until_stopped(&self) -> DriverStopped<'_> {
        DriverStopped { driver: self }
    }

    fn wake_on_ready<F>(&self, future: F) -> WakeOnReady<F>
    where
        F: Future,
    {
        WakeOnReady {
            future: Box::pin(future),
            inner: Arc::clone(&self.inner),
        }
    }

    fn track_worker(&self, task_id: TaskId, timer: bool, handle: JoinHandle<()>) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        state.reap_finished_workers();
        let timer_saturated = timer
            && state.workers.values().filter(|worker| worker.timer).count() >= state.timer_capacity;
        if state.lifecycle != DriverLifecycle::Running || timer_saturated {
            drop(state);
            drop(handle);
            return false;
        }
        let worker_id = state.next_worker_id;
        state.next_worker_id = state.next_worker_id.wrapping_add(1).max(1);
        state.workers.insert(
            worker_id,
            AsyncWorker {
                task_id,
                timer,
                cancellation: None,
                handle,
            },
        );
        true
    }

    fn track_external_worker(
        &self,
        task_id: TaskId,
        cancellation: ExternalRequestCancellation,
        handle: JoinHandle<()>,
    ) {
        let mut state = self.inner.state.lock().unwrap();
        state.reap_finished_workers();
        if state.lifecycle != DriverLifecycle::Running {
            cancellation.cancel();
            drop(state);
            drop(handle);
            return;
        }
        let worker_id = state.next_worker_id;
        state.next_worker_id = state.next_worker_id.wrapping_add(1).max(1);
        state.workers.insert(
            worker_id,
            AsyncWorker {
                task_id,
                timer: false,
                cancellation: Some(cancellation),
                handle,
            },
        );
    }

    async fn enqueue_event(&self, event: DriverEvent) {
        EventEnqueue {
            driver: self,
            event: Some(event),
        }
        .await;
    }

    async fn enqueue_pending_effects(&self) {
        for effect in self.inner.runner.drain_routed_emissions() {
            self.enqueue_event(DriverEvent::Effect(effect)).await;
        }
    }

    async fn dispatch<F, Fut, T>(
        &self,
        operation: DispatchOperation,
        f: F,
    ) -> Result<T, DriverError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, mica_runtime::SourceTaskError>> + 'static,
        T: Send + 'static,
    {
        let _activity = self.begin_dispatch()?;
        let start = Instant::now();
        metrics::dispatch_started(operation);
        let dispatch_permit = self.inner.cpu_admission.acquire_dispatch().await;
        let receiver = match self
            .inner
            .dispatcher
            .lock()
            .unwrap()
            .as_ref()
            .ok_or(DriverError::DriverStopped)?
            .dispatch(move || async move {
                let _dispatch_permit = dispatch_permit;
                f().await
            }) {
            Ok(receiver) => receiver,
            Err(_) => {
                let result = Err(DriverError::Join("dispatcher is stopped".to_owned()));
                metrics::record_dispatch_result(operation, start.elapsed(), &result);
                return result;
            }
        };
        let result = match receiver.await {
            Ok(result) => result.map_err(DriverError::Source),
            Err(_) => Err(DriverError::Join(
                "dispatched task was cancelled".to_owned(),
            )),
        };
        metrics::record_dispatch_result(operation, start.elapsed(), &result);
        result
    }

    async fn handle_submitted(
        &self,
        context: TaskContext,
        submitted: SubmittedTask,
    ) -> Result<(), DriverError> {
        tracing::debug!(
            target: "mica_driver::pool",
            task_id = submitted.task_id,
            principal = ?context.principal,
            actor = ?context.actor,
            endpoint = ?context.endpoint,
            "driver task submitted"
        );
        let mut queue = VecDeque::new();
        queue.push_back((submitted.task_id, context, submitted.outcome));
        self.process_outcome_queue(&mut queue).await
    }

    async fn process_outcome_queue(
        &self,
        queue: &mut VecDeque<(TaskId, TaskContext, TaskOutcome)>,
    ) -> Result<(), DriverError> {
        while let Some((task_id, context, outcome)) = queue.pop_front() {
            let delivered_mailboxes = self.delivered_mailboxes(&outcome);
            let admitted = self.inner.state.lock().unwrap().admit_active_task(task_id);
            if !admitted {
                if matches!(&outcome, TaskOutcome::Suspended { .. }) {
                    self.inner
                        .runner
                        .cancel_task(task_id)
                        .map_err(DriverError::Source)?;
                }
                self.enqueue_pending_effects().await;
                let handled = self.complete_invocation(
                    task_id,
                    InvocationOutcome::Failed(
                        "driver active task capacity is exhausted".to_owned(),
                    ),
                );
                if !handled {
                    self.enqueue_event(DriverEvent::TaskFailed {
                        task_id,
                        error: "driver active task capacity is exhausted".to_owned(),
                    })
                    .await;
                }
                self.route_mailbox_deliveries(delivered_mailboxes, queue)
                    .await?;
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .record_terminal_task(task_id, TerminalTaskKind::Failed);
                self.inner.runner.forget_terminal_task(task_id);
                continue;
            }
            let mut timer = None;
            let mut spawn = None;
            let mut mailbox_recv = None;
            let mut external_request = None;
            let mut task_event = None;
            let mut cancellation = None;
            let mut capacity_failure = None;
            let mut terminal_kind = None;
            let mut invocation_outcome = None;
            {
                let mut state = self.inner.state.lock().unwrap();
                match outcome {
                    TaskOutcome::Complete { value, .. } => {
                        terminal_kind = Some(TerminalTaskKind::Completed);
                        invocation_outcome = Some(InvocationOutcome::Completed(value.clone()));
                        tracing::debug!(
                            target: "mica_driver::pool",
                            task_id,
                            principal = %context.principal.map_or("none".to_owned(), |id| self.inner.runner.render_identity(id)),
                            actor = %context.actor.map_or("none".to_owned(), |id| self.inner.runner.render_identity(id)),
                            endpoint = %self.inner.runner.render_identity(context.endpoint),
                            "driver task completed"
                        );
                        task_event = Some(DriverEvent::TaskCompleted { task_id, value });
                    }
                    TaskOutcome::Aborted { error, .. } => {
                        terminal_kind = Some(TerminalTaskKind::Aborted);
                        invocation_outcome = Some(InvocationOutcome::Aborted(error.clone()));
                        tracing::error!(
                            target: "mica_driver::pool",
                            task_id,
                            principal = %context.principal.map_or("none".to_owned(), |id| self.inner.runner.render_identity(id)),
                            actor = %context.actor.map_or("none".to_owned(), |id| self.inner.runner.render_identity(id)),
                            endpoint = %self.inner.runner.render_identity(context.endpoint),
                            error = %self.inner.runner.render_task_value(&error),
                            "driver task aborted"
                        );
                        task_event = Some(DriverEvent::TaskAborted { task_id, error });
                    }
                    TaskOutcome::Suspended { kind, .. } => {
                        let cancellation_reason = match state.lifecycle {
                            DriverLifecycle::Running
                                if state.closed_endpoints.contains(&context.endpoint) =>
                            {
                                Some(TaskCancellationReason::EndpointClosed)
                            }
                            DriverLifecycle::Running => None,
                            DriverLifecycle::ShuttingDown | DriverLifecycle::Stopped => {
                                Some(TaskCancellationReason::DriverShutdown)
                            }
                        };
                        if let Some(reason) = cancellation_reason {
                            cancellation = Some(reason);
                        } else if state.contexts.len() >= state.suspended_task_capacity {
                            capacity_failure = Some("suspended task");
                        } else {
                            tracing::debug!(
                                target: "mica_driver::pool",
                                task_id,
                                principal = %context.principal.map_or("none".to_owned(), |id| self.inner.runner.render_identity(id)),
                                actor = %context.actor.map_or("none".to_owned(), |id| self.inner.runner.render_identity(id)),
                                endpoint = %self.inner.runner.render_identity(context.endpoint),
                                kind = ?kind,
                                "driver task suspended"
                            );
                            metrics::record_suspend(&kind);
                            state.contexts.insert(task_id, context.clone());
                            task_event = Some(DriverEvent::TaskSuspended {
                                task_id,
                                kind: kind.clone(),
                            });
                            match kind {
                                SuspendKind::Commit => timer = Some(Duration::ZERO),
                                SuspendKind::Never => {}
                                SuspendKind::TimedMillis(millis) => {
                                    timer = Some(Duration::from_millis(millis));
                                }
                                SuspendKind::WaitingForInput(_) => {
                                    state
                                        .input_waiters
                                        .entry(context.endpoint)
                                        .or_default()
                                        .push(task_id);
                                }
                                SuspendKind::MailboxRecv(request) => {
                                    mailbox_recv = Some(request);
                                }
                                SuspendKind::Spawn(request) => {
                                    spawn = Some(request);
                                }
                                SuspendKind::ExternalRequest(request) => {
                                    external_request = Some(request);
                                }
                            }
                        }
                    }
                }
                state.record_metrics();
            }
            if let Some(reason) = cancellation {
                self.inner
                    .runner
                    .cancel_task(task_id)
                    .map_err(DriverError::Source)?;
                self.record_cancelled_task(task_id, reason).await;
                continue;
            }
            if let Some(resource) = capacity_failure {
                let error = format!("driver {resource} capacity is exhausted");
                self.inner
                    .runner
                    .cancel_task(task_id)
                    .map_err(DriverError::Source)?;
                self.enqueue_pending_effects().await;
                if !self.complete_invocation(task_id, InvocationOutcome::Failed(error.clone())) {
                    self.enqueue_event(DriverEvent::TaskFailed { task_id, error })
                        .await;
                }
                self.route_mailbox_deliveries(delivered_mailboxes, queue)
                    .await?;
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .record_terminal_task(task_id, TerminalTaskKind::Failed);
                self.inner.runner.forget_terminal_task(task_id);
                continue;
            }
            self.enqueue_pending_effects().await;
            let invocation_suspended = invocation_outcome.is_none()
                && self
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .invocations
                    .contains_key(&task_id);
            let invocation_terminal = invocation_outcome
                .map(|outcome| self.complete_invocation(task_id, outcome))
                .unwrap_or(false);
            if !invocation_terminal && !invocation_suspended {
                self.enqueue_event(task_event.expect("non-cancelled outcome has an event"))
                    .await;
            }
            if let Some(kind) = terminal_kind {
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .record_terminal_task(task_id, kind);
                self.inner.runner.forget_terminal_task(task_id);
            }
            if let Some(duration) = timer
                && !self.spawn_timer_resume(task_id, duration)
            {
                self.fail_suspended_task_for_capacity(task_id, "timer")
                    .await?;
                continue;
            }
            if let Some(request) = mailbox_recv {
                self.handle_mailbox_recv(task_id, request, queue).await?;
            }
            self.route_mailbox_deliveries(delivered_mailboxes, queue)
                .await?;
            if let Some(request) = spawn {
                self.spawn_child_and_resume(task_id, context, request, queue)
                    .await?;
            }
            if let Some(request) = external_request {
                self.spawn_external_request_resume(task_id, request);
            }
        }
        Ok(())
    }

    fn delivered_mailboxes(&self, outcome: &TaskOutcome) -> Vec<u64> {
        let mailbox_sends = match outcome {
            TaskOutcome::Complete { mailbox_sends, .. }
            | TaskOutcome::Suspended { mailbox_sends, .. }
            | TaskOutcome::Aborted { mailbox_sends, .. } => mailbox_sends,
        };
        let mut mailboxes = self.inner.runner.take_subscription_deliveries();
        for mailbox in mailbox_sends
            .iter()
            .filter_map(|send| self.inner.runner.mailbox_for_sender(&send.sender).ok())
        {
            if !mailboxes.contains(&mailbox) {
                mailboxes.push(mailbox);
            }
        }
        mailboxes
    }

    async fn handle_mailbox_recv(
        &self,
        task_id: TaskId,
        request: MailboxRecvRequest,
        queue: &mut VecDeque<(TaskId, TaskContext, TaskOutcome)>,
    ) -> Result<(), DriverError> {
        metrics::async_worker_started(AsyncWorkerKind::MailboxRecv);
        let start = Instant::now();
        let result = async {
            let mut receivers = Vec::with_capacity(request.receivers.len());
            for receiver in request.receivers {
                let mailbox = self
                    .inner
                    .runner
                    .mailbox_for_receiver(&receiver)
                    .map_err(runtime_driver_error)?;
                if receivers
                    .iter()
                    .any(|(existing_mailbox, _)| *existing_mailbox == mailbox)
                {
                    continue;
                }
                receivers.push((mailbox, receiver));
            }
            let mut timeout = None;
            let ready = {
                let mut state = self.inner.state.lock().unwrap();
                for (mailbox, _) in &receivers {
                    state
                        .mailbox_waiters
                        .entry(*mailbox)
                        .or_default()
                        .push_back(MailboxWaiter {
                            task_id,
                            receivers: receivers.clone(),
                        });
                }
                let ready = self.drain_ready_mailbox_groups(&receivers)?;
                let should_wait = ready.is_empty() && request.timeout_millis != Some(0);
                if should_wait {
                    timeout = request
                        .timeout_millis
                        .map(|millis| Duration::from_millis(millis).max(Duration::from_millis(1)));
                    state.record_metrics();
                    Vec::new()
                } else {
                    state.remove_mailbox_waiter(task_id);
                    state.record_metrics();
                    ready
                }
            };
            if !ready.is_empty() || request.timeout_millis == Some(0) {
                let (ctx, submitted) = self.resume_raw(task_id, Value::list(ready)).await?;
                queue.push_back((submitted.task_id, ctx, submitted.outcome));
                return Ok(());
            }
            if let Some(timeout) = timeout
                && !self.spawn_mailbox_timeout(task_id, timeout)
            {
                self.fail_suspended_task_for_capacity(task_id, "timer")
                    .await?;
            }
            Ok(())
        }
        .await;
        metrics::async_worker_finished(
            AsyncWorkerKind::MailboxRecv,
            if result.is_ok() {
                WorkerOutcome::Complete
            } else {
                WorkerOutcome::Error
            },
            start.elapsed(),
        );
        result
    }

    fn drain_ready_mailbox_groups(
        &self,
        receivers: &[(u64, Value)],
    ) -> Result<Vec<Value>, DriverError> {
        let mut ready = Vec::new();
        for (_, receiver) in receivers {
            let messages = self
                .inner
                .runner
                .drain_mailbox(receiver.clone())
                .map_err(runtime_driver_error)?;
            if messages.is_empty() {
                continue;
            }
            ready.push(Value::list([receiver.clone(), Value::list(messages)]));
        }
        Ok(ready)
    }

    fn spawn_mailbox_timeout(&self, task_id: TaskId, duration: Duration) -> bool {
        let driver = self.clone();
        let future = self.wake_on_ready(async move {
            metrics::async_worker_started(AsyncWorkerKind::MailboxTimeout);
            let start = Instant::now();
            compio::time::sleep(duration).await;
            let still_waiting = {
                let state = driver.inner.state.lock().unwrap();
                state
                    .mailbox_waiters
                    .values()
                    .any(|waiters| waiters.iter().any(|waiter| waiter.task_id == task_id))
            };
            if !still_waiting {
                metrics::async_worker_finished(
                    AsyncWorkerKind::MailboxTimeout,
                    WorkerOutcome::Cancelled,
                    start.elapsed(),
                );
                return;
            }
            let mut outcome = WorkerOutcome::Complete;
            if let Err(error) = driver.resume(task_id, Value::list([])).await {
                outcome = driver.record_worker_resume_error(task_id, error).await;
            }
            metrics::async_worker_finished(
                AsyncWorkerKind::MailboxTimeout,
                outcome,
                start.elapsed(),
            );
        });
        let handle = compio::runtime::spawn(future);
        self.track_worker(task_id, true, handle)
    }

    async fn wake_mailbox_waiters(
        &self,
        mailboxes: Vec<u64>,
        queue: &mut VecDeque<(TaskId, TaskContext, TaskOutcome)>,
    ) -> Result<(), DriverError> {
        if mailboxes.is_empty() {
            return Ok(());
        }
        metrics::async_worker_started(AsyncWorkerKind::MailboxWake);
        let start = Instant::now();
        let result = async {
            for mailbox in mailboxes {
                let waiter = {
                    let mut state = self.inner.state.lock().unwrap();
                    let waiter = state
                        .mailbox_waiters
                        .get_mut(&mailbox)
                        .and_then(VecDeque::pop_front);
                    if let Some(waiter) = &waiter {
                        state.remove_mailbox_waiter(waiter.task_id);
                    }
                    state.record_metrics();
                    waiter
                };
                let Some(waiter) = waiter else {
                    continue;
                };
                let ready = self.drain_ready_mailbox_groups(&waiter.receivers)?;
                if ready.is_empty() {
                    continue;
                }
                let (ctx, submitted) = self.resume_raw(waiter.task_id, Value::list(ready)).await?;
                queue.push_back((submitted.task_id, ctx, submitted.outcome));
            }
            Ok(())
        }
        .await;
        metrics::async_worker_finished(
            AsyncWorkerKind::MailboxWake,
            if result.is_ok() {
                WorkerOutcome::Complete
            } else {
                WorkerOutcome::Error
            },
            start.elapsed(),
        );
        result
    }

    async fn route_mailbox_deliveries(
        &self,
        mailboxes: Vec<u64>,
        queue: &mut VecDeque<(TaskId, TaskContext, TaskOutcome)>,
    ) -> Result<(), DriverError> {
        if mailboxes.is_empty() {
            return Ok(());
        }
        let (task_mailboxes, external_mailboxes) = {
            let state = self.inner.state.lock().unwrap();
            let mut task_mailboxes = Vec::new();
            let mut external_mailboxes = Vec::new();
            for mailbox in mailboxes {
                if state.external_subscription_mailboxes.contains_key(&mailbox) {
                    external_mailboxes.push(mailbox);
                } else {
                    task_mailboxes.push(mailbox);
                }
            }
            state.record_metrics();
            (task_mailboxes, external_mailboxes)
        };
        for mailbox in external_mailboxes {
            self.enqueue_event(DriverEvent::SubscriptionReady { mailbox })
                .await;
        }
        self.wake_mailbox_waiters(task_mailboxes, queue).await
    }

    async fn spawn_child_and_resume(
        &self,
        parent_task_id: TaskId,
        context: TaskContext,
        request: SpawnRequest,
        queue: &mut VecDeque<(TaskId, TaskContext, TaskOutcome)>,
    ) -> Result<(), DriverError> {
        metrics::async_worker_started(AsyncWorkerKind::SpawnChild);
        let start = Instant::now();
        let result = async {
            let (child_ctx, child_submitted) = self.submit_spawn_raw(context, request).await?;
            tracing::debug!(
                target: "mica_driver::pool",
                parent_task_id,
                child_task_id = child_submitted.task_id,
                "driver child task spawned"
            );
            queue.push_back((child_submitted.task_id, child_ctx, child_submitted.outcome));

            let child_id = Value::int(child_submitted.task_id as i64)
                .expect("allocated task id fits in Value");
            let (parent_ctx, parent_submitted) = self.resume_raw(parent_task_id, child_id).await?;
            tracing::debug!(
                target: "mica_driver::pool",
                parent_task_id,
                child_task_id = child_submitted.task_id,
                "driver parent task resumed after spawn"
            );
            queue.push_back((
                parent_submitted.task_id,
                parent_ctx,
                parent_submitted.outcome,
            ));
            Ok(())
        }
        .await;
        metrics::async_worker_finished(
            AsyncWorkerKind::SpawnChild,
            if result.is_ok() {
                WorkerOutcome::Complete
            } else {
                WorkerOutcome::Error
            },
            start.elapsed(),
        );
        result
    }

    async fn submit_spawn_raw(
        &self,
        context: TaskContext,
        request: SpawnRequest,
    ) -> Result<(TaskContext, SubmittedTask), DriverError> {
        let runner = Arc::clone(&self.inner.runner);
        let context_authority = context.authority.clone();
        let submit_context = context.clone();
        tracing::debug!(
            target: "mica_driver::pool",
            selector = request.selector.name().unwrap_or("<unnamed>"),
            target = ?request.target,
            delay_millis = ?request.delay_millis,
            principal = ?context.principal,
            actor = ?context.actor,
            endpoint = ?context.endpoint,
            "driver spawn requested"
        );
        let submitted = self
            .dispatch(DispatchOperation::Spawn, move || async move {
                runner.submit_spawn(
                    context.principal,
                    context.actor,
                    context.endpoint,
                    context_authority,
                    request,
                )
            })
            .await?;
        Ok((submit_context, submitted))
    }

    async fn resume_raw(
        &self,
        task_id: TaskId,
        value: Value,
    ) -> Result<(TaskContext, SubmittedTask), DriverError> {
        let context = {
            let mut state = self.inner.state.lock().unwrap();
            let context = state
                .contexts
                .remove(&task_id)
                .ok_or(DriverError::MissingTaskContext(task_id))?;
            state.record_metrics();
            context
        };
        tracing::debug!(
            target: "mica_driver::pool",
            task_id,
            principal = ?context.principal,
            actor = ?context.actor,
            endpoint = ?context.endpoint,
            value = ?value,
            "driver task resume requested"
        );
        let request = TaskRequest {
            principal: context.principal,
            actor: context.actor,
            endpoint: context.endpoint,
            authority: context.authority.clone(),
            input: TaskInput::Continuation { task_id, value },
        };
        let runner = Arc::clone(&self.inner.runner);
        let submitted = self
            .dispatch(DispatchOperation::Resume, move || async move {
                runner.resume_task(request)
            })
            .await
            .map(|outcome| SubmittedTask { task_id, outcome })?;
        tracing::debug!(
            target: "mica_driver::pool",
            task_id,
            principal = ?context.principal,
            actor = ?context.actor,
            endpoint = ?context.endpoint,
            "driver task resume returned"
        );
        Ok((context, submitted))
    }

    fn spawn_timer_resume(&self, task_id: TaskId, duration: Duration) -> bool {
        let driver = self.clone();
        let future = self.wake_on_ready(async move {
            metrics::async_worker_started(AsyncWorkerKind::TimerResume);
            let start = Instant::now();
            compio::time::sleep(duration).await;
            let mut outcome = WorkerOutcome::Complete;
            if let Err(error) = driver.resume(task_id, Value::nothing()).await {
                outcome = driver.record_worker_resume_error(task_id, error).await;
            }
            metrics::async_worker_finished(AsyncWorkerKind::TimerResume, outcome, start.elapsed());
        });
        let handle = compio::runtime::spawn(future);
        self.track_worker(task_id, true, handle)
    }

    fn spawn_external_request_resume(
        &self,
        task_id: TaskId,
        request: mica_runtime::ExternalRequest,
    ) {
        let driver = self.clone();
        let handler = self.inner.external_request_handler.clone();
        let stream_handler = self.inner.external_stream_request_handler.clone();
        let admission = Arc::clone(&self.inner.external_request_admission);
        let stream_sender = request
            .payload
            .map_get(&Value::symbol(Symbol::intern("stream_to")));
        let timeout = request.timeout_millis.map(Duration::from_millis);
        let service = request.service;
        let task_context = self
            .inner
            .state
            .lock()
            .unwrap()
            .contexts
            .get(&task_id)
            .cloned()
            .expect("external request task context is registered");
        let cancellation = ExternalRequestCancellation::new();
        let request_context = ExternalRequestContext {
            task_id,
            principal: task_context.principal,
            actor: task_context.actor,
            endpoint: task_context.endpoint,
            cancellation: cancellation.clone(),
        };
        tracing::debug!(
            target: "mica_driver::pool",
            task_id,
            service = service.name().unwrap_or("<unnamed>"),
            timeout_millis = ?request.timeout_millis,
            "driver external request scheduled"
        );
        let tracked_cancellation = cancellation.clone();
        let future = self.wake_on_ready(async move {
            metrics::async_worker_started(AsyncWorkerKind::ExternalRequest);
            let start = Instant::now();
            tracing::debug!(
                target: "mica_driver::pool",
                task_id,
                service = service.name().unwrap_or("<unnamed>"),
                "driver external request started"
            );
            let operation = Box::pin(async {
                let _permit = admission.acquire().await;
                driver
                    .perform_external_request(
                        request_context,
                        request,
                        handler,
                        stream_handler,
                        stream_sender,
                    )
                    .await
            });
            let (value, mut outcome) = if let Some(timeout) = timeout {
                match select(operation, Box::pin(compio::time::sleep(timeout))).await {
                    Either::Left((value, _)) => (value, WorkerOutcome::Complete),
                    Either::Right(((), _)) => {
                        cancellation.cancel();
                        tracing::warn!(
                            target: "mica_driver::pool",
                            task_id,
                            service = service.name().unwrap_or("<unnamed>"),
                            "external request timed out"
                        );
                        (
                            Value::error(
                                Symbol::intern("ExternalTimeout"),
                                Some("external request timed out"),
                                None,
                            ),
                            WorkerOutcome::Timeout,
                        )
                    }
                }
            } else {
                (operation.await, WorkerOutcome::Complete)
            };
            if let Err(error) = driver.resume(task_id, value).await {
                outcome = driver.record_worker_resume_error(task_id, error).await;
            } else {
                tracing::debug!(
                    target: "mica_driver::pool",
                    task_id,
                    service = service.name().unwrap_or("<unnamed>"),
                    elapsed_us = start.elapsed().as_micros(),
                    "driver external request resumed task"
                );
            }
            metrics::async_worker_finished(
                AsyncWorkerKind::ExternalRequest,
                outcome,
                start.elapsed(),
            );
        });
        let handle = compio::runtime::spawn(future);
        self.track_external_worker(task_id, tracked_cancellation, handle);
    }

    async fn perform_external_request(
        &self,
        context: ExternalRequestContext,
        request: mica_runtime::ExternalRequest,
        handler: Option<ExternalRequestHandler>,
        stream_handler: Option<ExternalStreamRequestHandler>,
        stream_sender: Option<Value>,
    ) -> Value {
        if let Some(stream_sender) = stream_sender {
            let Some(handler) = stream_handler else {
                let message = "no external stream request handler is configured";
                let _ = self
                    .deliver_external_mailbox(
                        stream_sender,
                        Value::map([
                            (
                                Value::symbol(Symbol::intern("type")),
                                Value::symbol(Symbol::intern("error")),
                            ),
                            (
                                Value::symbol(Symbol::intern("message")),
                                Value::string(message),
                            ),
                        ]),
                    )
                    .await;
                return Value::error(Symbol::intern("ExternalUnavailable"), Some(message), None);
            };
            let emit_driver = self.clone();
            let emit_cancellation = context.cancellation.clone();
            let emitter = ExternalStreamEmitter::new(Arc::new(move |value| {
                let emit_driver = emit_driver.clone();
                let stream_sender = stream_sender.clone();
                let cancellation = emit_cancellation.clone();
                Box::pin(async move {
                    if cancellation.is_cancelled() {
                        return Err("external request was cancelled".to_owned());
                    }
                    emit_driver
                        .deliver_external_mailbox(stream_sender, value)
                        .await
                        .map_err(|error| emit_driver.format_error(&error))
                })
            }));
            return handler(context, request, emitter).await;
        }
        if request.service == Symbol::intern("mica_query") {
            return self
                .perform_mica_query_request(context.task_id, request)
                .await;
        }
        match handler {
            Some(handler) => handler(context, request).await,
            None => {
                tracing::warn!(
                    target: "mica_driver::pool",
                    task_id = context.task_id,
                    service = request.service.name().unwrap_or("<unnamed>"),
                    "external request has no configured handler"
                );
                Value::error(
                    Symbol::intern("ExternalUnavailable"),
                    Some("no external request handler is configured"),
                    None,
                )
            }
        }
    }

    async fn deliver_external_mailbox(
        &self,
        sender: Value,
        value: Value,
    ) -> Result<(), DriverError> {
        let mailbox = self
            .inner
            .runner
            .deliver_mailbox(sender, value)
            .map_err(runtime_driver_error)?;
        let mut queue = VecDeque::new();
        self.wake_mailbox_waiters(vec![mailbox], &mut queue).await?;
        self.process_outcome_queue(&mut queue).await
    }

    async fn perform_mica_query_request(
        &self,
        task_id: TaskId,
        request: mica_runtime::ExternalRequest,
    ) -> Value {
        let context = {
            let state = self.inner.state.lock().unwrap();
            state.contexts.get(&task_id).cloned()
        };
        let Some(context) = context else {
            return mica_query_error_value(format!("missing task context for task {task_id}"));
        };
        let query = match request
            .payload
            .map_get(&Value::symbol(Symbol::intern("query")))
            .and_then(|value| value.with_str(str::to_owned))
        {
            Some(query) => query,
            None => return mica_query_error_value("mica_query request missing query string"),
        };
        let options = match read_only_query_options_from_payload(&request.payload) {
            Ok(options) => options,
            Err(error) => return mica_query_error_value(error),
        };
        tracing::debug!(
            target: "mica_driver::pool",
            task_id,
            endpoint = ?context.endpoint,
            "driver mica_query request started"
        );
        match self
            .run_read_only_source_query(context.endpoint, query, options)
            .await
        {
            Ok(report) => report.as_value(),
            Err(error) => mica_query_error_value(self.format_error(&error)),
        }
    }

    async fn record_task_failure(&self, task_id: TaskId, error: DriverError) {
        let rendered = self.format_error(&error);
        tracing::error!(task_id, error = %rendered, "driver task failed");
        if !self.complete_invocation(task_id, InvocationOutcome::Failed(rendered.clone())) {
            self.enqueue_event(DriverEvent::TaskFailed {
                task_id,
                error: rendered,
            })
            .await;
        }
        self.inner
            .state
            .lock()
            .unwrap()
            .record_terminal_task(task_id, TerminalTaskKind::Failed);
        self.inner.runner.forget_terminal_task(task_id);
    }

    fn complete_invocation(&self, task_id: TaskId, outcome: InvocationOutcome) -> bool {
        let state = self
            .inner
            .state
            .lock()
            .unwrap()
            .invocations
            .remove(&task_id);
        if let Some(state) = state {
            state.complete(outcome);
            true
        } else {
            false
        }
    }

    async fn record_worker_resume_error(
        &self,
        task_id: TaskId,
        error: DriverError,
    ) -> WorkerOutcome {
        if matches!(
            error,
            DriverError::TaskCancelled(_)
                | DriverError::EndpointClosed(_)
                | DriverError::DriverStopped
        ) {
            return WorkerOutcome::Cancelled;
        }
        self.record_task_failure(task_id, error).await;
        WorkerOutcome::Error
    }
}

impl Future for DriverEvents<'_> {
    type Output = Vec<DriverEvent>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        state.reap_finished_workers();
        if !state.events.is_empty() {
            let events = state.events.drain(..).collect();
            let space_wakers = std::mem::take(&mut state.event_space_wakers);
            state.record_metrics();
            drop(state);
            for waker in space_wakers {
                waker.wake();
            }
            return Poll::Ready(events);
        }
        let waker = cx.waker().clone();
        if !state.event_wakers.iter().any(|w| w.will_wake(&waker)) {
            state.event_wakers.push(waker);
        }
        state.record_metrics();
        Poll::Pending
    }
}

impl<F> Future for WakeOnReady<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let relay = waker(Arc::new(ReadinessWaker {
            runtime_waker: context.waker().clone(),
            inner: Arc::clone(&self.inner),
        }));
        self.future.as_mut().poll(&mut Context::from_waker(&relay))
    }
}

impl ArcWake for ReadinessWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.runtime_waker.wake_by_ref();
        let handler = arc_self.inner.wake_handler.lock().unwrap().clone();
        if let Some(handler) = handler {
            handler.wake();
        }
    }
}

impl Future for EventEnqueue<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        if state.discard_events {
            drop(state);
            self.event.take();
            return Poll::Ready(());
        }
        let event = self.event.as_ref().expect("event is present while pending");
        if state.coalesces(event) {
            drop(state);
            self.event.take();
            return Poll::Ready(());
        }
        if state.events.len() >= state.event_capacity {
            let waker = cx.waker().clone();
            if !state
                .event_space_wakers
                .iter()
                .any(|entry| entry.will_wake(&waker))
            {
                state.event_space_wakers.push(waker);
            }
            return Poll::Pending;
        }
        let event = self.event.take().unwrap();
        let was_empty = state.events.is_empty();
        state.events.push_back(event);
        let event_wakers = std::mem::take(&mut state.event_wakers);
        state.record_metrics();
        drop(state);
        for waker in event_wakers {
            waker.wake();
        }
        if was_empty {
            let handler = self.driver.inner.wake_handler.lock().unwrap().clone();
            if let Some(handler) = handler {
                handler.wake();
            }
        }
        Poll::Ready(())
    }
}

impl Future for DriverIdle<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        if state.in_flight_dispatches == 0 && state.endpoint_activities.is_empty() {
            return Poll::Ready(());
        }
        let waker = cx.waker().clone();
        if !state
            .idle_wakers
            .iter()
            .any(|entry| entry.will_wake(&waker))
        {
            state.idle_wakers.push(waker);
        }
        Poll::Pending
    }
}

impl Future for EndpointIdle<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        if !state.endpoint_activities.contains_key(&self.endpoint) {
            return Poll::Ready(());
        }
        let waker = cx.waker().clone();
        let wakers = state.endpoint_idle_wakers.entry(self.endpoint).or_default();
        if !wakers.iter().any(|entry| entry.will_wake(&waker)) {
            wakers.push(waker);
        }
        Poll::Pending
    }
}

impl Future for DriverStopped<'_> {
    type Output = Result<(), DriverError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        if state.lifecycle == DriverLifecycle::Stopped {
            return Poll::Ready(shutdown_result(&state));
        }
        let waker = cx.waker().clone();
        if !state
            .shutdown_wakers
            .iter()
            .any(|entry| entry.will_wake(&waker))
        {
            state.shutdown_wakers.push(waker);
        }
        Poll::Pending
    }
}

impl Drop for DispatchActivity {
    fn drop(&mut self) {
        let idle_wakers = {
            let mut state = self.inner.state.lock().unwrap();
            state.in_flight_dispatches = state.in_flight_dispatches.saturating_sub(1);
            if state.in_flight_dispatches == 0 {
                std::mem::take(&mut state.idle_wakers)
            } else {
                Vec::new()
            }
        };
        for waker in idle_wakers {
            waker.wake();
        }
    }
}

impl Drop for EndpointActivity {
    fn drop(&mut self) {
        let (endpoint_wakers, idle_wakers) = {
            let mut state = self.inner.state.lock().unwrap();
            let Some(activities) = state.endpoint_activities.get_mut(&self.endpoint) else {
                return;
            };
            *activities = activities.saturating_sub(1);
            if *activities > 0 {
                return;
            }
            state.endpoint_activities.remove(&self.endpoint);
            let endpoint_wakers = state
                .endpoint_idle_wakers
                .remove(&self.endpoint)
                .unwrap_or_default();
            let idle_wakers =
                if state.endpoint_activities.is_empty() && state.in_flight_dispatches == 0 {
                    std::mem::take(&mut state.idle_wakers)
                } else {
                    Vec::new()
                };
            (endpoint_wakers, idle_wakers)
        };
        for waker in endpoint_wakers.into_iter().chain(idle_wakers) {
            waker.wake();
        }
    }
}

impl PoolState {
    fn admit_active_task(&mut self, task_id: TaskId) -> bool {
        if self.active_tasks.contains(&task_id) {
            return true;
        }
        if self.active_tasks.len() >= self.active_task_capacity {
            return false;
        }
        self.active_tasks.insert(task_id);
        true
    }

    fn record_terminal_task(&mut self, task_id: TaskId, kind: TerminalTaskKind) {
        self.active_tasks.remove(&task_id);
        self.contexts.remove(&task_id);
        if self.terminal_tasks.insert(task_id, kind).is_none() {
            self.terminal_order.push_back(task_id);
        }
        while self.terminal_order.len() > self.terminal_task_retention {
            let Some(expired) = self.terminal_order.pop_front() else {
                break;
            };
            self.terminal_tasks.remove(&expired);
        }
    }

    fn reap_finished_workers(&mut self) {
        self.workers
            .retain(|_, worker| !worker.handle.is_finished());
    }

    fn take_task_workers(&mut self, task_id: TaskId) -> Vec<JoinHandle<()>> {
        let worker_ids = self
            .workers
            .iter()
            .filter_map(|(worker_id, worker)| (worker.task_id == task_id).then_some(*worker_id))
            .collect::<Vec<_>>();
        worker_ids
            .into_iter()
            .filter_map(|worker_id| self.workers.remove(&worker_id))
            .map(|worker| {
                if let Some(cancellation) = worker.cancellation {
                    cancellation.cancel();
                }
                worker.handle
            })
            .collect()
    }

    fn coalesces(&self, event: &DriverEvent) -> bool {
        self.events.iter().any(|queued| match (queued, event) {
            (
                DriverEvent::TaskSuspended {
                    task_id: queued_task,
                    kind: queued_kind,
                },
                DriverEvent::TaskSuspended { task_id, kind },
            ) => queued_task == task_id && queued_kind == kind,
            (
                DriverEvent::SubscriptionReady {
                    mailbox: queued_mailbox,
                },
                DriverEvent::SubscriptionReady { mailbox },
            ) => queued_mailbox == mailbox,
            _ => false,
        })
    }

    fn remove_mailbox_waiter(&mut self, task_id: TaskId) {
        for waiters in self.mailbox_waiters.values_mut() {
            waiters.retain(|waiter| waiter.task_id != task_id);
        }
        self.mailbox_waiters
            .retain(|_, waiters| !waiters.is_empty());
    }

    fn remove_task_waiters(&mut self, task_id: TaskId) {
        self.remove_mailbox_waiter(task_id);
        for task_ids in self.input_waiters.values_mut() {
            task_ids.retain(|waiting| *waiting != task_id);
        }
        self.input_waiters
            .retain(|_, task_ids| !task_ids.is_empty());
    }

    fn remove_input_waiters(&mut self, endpoint: Identity) -> Vec<TaskId> {
        let task_ids = self.input_waiters.remove(&endpoint).unwrap_or_default();
        self.record_metrics();
        task_ids
    }

    fn record_metrics(&self) {
        let input_waiters = self.input_waiters.values().map(Vec::len).sum::<usize>();
        let mailbox_waiters = self
            .mailbox_waiters
            .values()
            .map(VecDeque::len)
            .sum::<usize>();
        metrics::record_waiting_state(input_waiters, mailbox_waiters, self.events.len());
    }
}

fn shutdown_result(state: &PoolState) -> Result<(), DriverError> {
    match &state.shutdown_error {
        Some(error) => Err(DriverError::Join(error.clone())),
        None => Ok(()),
    }
}

fn read_only_query_options_from_payload(
    payload: &Value,
) -> Result<ReadOnlySourceQueryOptions, String> {
    let mut options = ReadOnlySourceQueryOptions::default();
    let Some(value) = payload.map_get(&Value::symbol(Symbol::intern("options"))) else {
        return Ok(options);
    };
    if value == Value::nothing() {
        return Ok(options);
    }
    let Some(entries) = value.with_map(<[(Value, Value)]>::to_vec) else {
        return Err("mica_query options must be a map".to_owned());
    };
    for (key, value) in entries {
        let Some(name) = key.as_symbol().and_then(Symbol::name) else {
            return Err("mica_query option keys must be symbols".to_owned());
        };
        match name {
            "max_output_chars" => {
                options.max_output_chars = usize_option(name, &value)?;
            }
            "instruction_budget" => {
                options.instruction_budget = usize_option(name, &value)?;
            }
            "max_call_depth" => {
                options.max_call_depth = usize_option(name, &value)?;
            }
            _ => return Err(format!("unknown mica_query option `{name}`")),
        }
    }
    Ok(options)
}

fn usize_option(name: &str, value: &Value) -> Result<usize, String> {
    let Some(value) = value.as_int() else {
        return Err(format!("mica_query option `{name}` must be an integer"));
    };
    usize::try_from(value).map_err(|_| format!("mica_query option `{name}` must be non-negative"))
}

fn mica_query_error_value(message: impl Into<String>) -> Value {
    Value::map([
        (Value::symbol(Symbol::intern("task_id")), Value::nothing()),
        (
            Value::symbol(Symbol::intern("status")),
            Value::string(ReadOnlySourceQueryStatus::Error.as_str()),
        ),
        (Value::symbol(Symbol::intern("value")), Value::nothing()),
        (Value::symbol(Symbol::intern("error")), Value::nothing()),
        (
            Value::symbol(Symbol::intern("diagnostics")),
            Value::list([Value::string(message.into())]),
        ),
        (Value::symbol(Symbol::intern("rendered")), Value::string("")),
        (
            Value::symbol(Symbol::intern("rendered_truncated")),
            Value::bool(false),
        ),
    ])
}

fn runtime_driver_error(error: RuntimeError) -> DriverError {
    DriverError::Source(SourceTaskError::TaskManager(TaskManagerError::Task(
        TaskError::Runtime(error),
    )))
}
