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

use crate::execution::{CpuAdmission, ExternalRequestAdmission};
use crate::{
    DEFAULT_EVENT_QUEUE_CAPACITY, DEFAULT_EXTERNAL_REQUEST_CAPACITY,
    DEFAULT_SUBSCRIPTION_QUEUE_BUDGET, DispatcherConfig, DriverError, DriverEvent, DriverResources,
    DriverSubscriptionMailbox, DriverSubscriptionRequest, EndpointCloseReport,
    ExternalRequestCancellation, ExternalRequestContext, ExternalRequestHandler,
    ExternalStreamEmitter, ExternalStreamRequestHandler, FileinIncludeLoader, RelationAcceleration,
    TaskCancellationReason, TaskContext, configure_dispatcher,
    metrics::{self, AsyncWorkerKind, DispatchOperation, WorkerOutcome},
};
use compio::dispatcher::Dispatcher;
use compio::runtime::JoinHandle;
use futures_util::future::{Either, select};
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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
pub struct CompioTaskDriver {
    inner: Arc<PoolInner>,
}

impl CompioTaskDriver {
    pub fn builder(resources: DriverResources) -> crate::CompioTaskDriverBuilder {
        crate::CompioTaskDriverBuilder::new(resources)
    }

    #[cfg(test)]
    pub(crate) fn inner_runner(&self) -> Arc<SharedSourceRunner> {
        Arc::clone(&self.inner.runner)
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
    state: Mutex<PoolState>,
}

struct PoolState {
    lifecycle: DriverLifecycle,
    shutdown_error: Option<String>,
    in_flight_dispatches: usize,
    idle_wakers: Vec<Waker>,
    shutdown_wakers: Vec<Waker>,
    contexts: BTreeMap<TaskId, TaskContext>,
    cancelled_tasks: HashSet<TaskId>,
    closed_endpoints: HashSet<Identity>,
    open_endpoints: HashSet<Identity>,
    input_waiters: BTreeMap<Identity, Vec<TaskId>>,
    mailbox_waiters: BTreeMap<u64, VecDeque<MailboxWaiter>>,
    external_subscription_mailboxes: HashSet<u64>,
    events: VecDeque<DriverEvent>,
    event_wakers: Vec<Waker>,
    event_space_wakers: Vec<Waker>,
    event_capacity: usize,
    next_worker_id: u64,
    workers: HashMap<u64, AsyncWorker>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DriverLifecycle {
    Running,
    ShuttingDown,
    Stopped,
}

struct AsyncWorker {
    task_id: TaskId,
    cancellation: Option<ExternalRequestCancellation>,
    handle: JoinHandle<()>,
}

struct DispatchActivity {
    inner: Arc<PoolInner>,
}

struct DriverIdle<'a> {
    driver: &'a CompioTaskDriver,
}

struct DriverStopped<'a> {
    driver: &'a CompioTaskDriver,
}

struct EventEnqueue<'a> {
    driver: &'a CompioTaskDriver,
    event: Option<DriverEvent>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            lifecycle: DriverLifecycle::Running,
            shutdown_error: None,
            in_flight_dispatches: 0,
            idle_wakers: Vec::new(),
            shutdown_wakers: Vec::new(),
            contexts: BTreeMap::new(),
            cancelled_tasks: HashSet::new(),
            closed_endpoints: HashSet::new(),
            open_endpoints: HashSet::new(),
            input_waiters: BTreeMap::new(),
            mailbox_waiters: BTreeMap::new(),
            external_subscription_mailboxes: HashSet::new(),
            events: VecDeque::new(),
            event_wakers: Vec::new(),
            event_space_wakers: Vec::new(),
            event_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
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
    pub fn spawn(runner: SourceRunner) -> Result<Self, DriverError> {
        Self::spawn_with_workers(runner, None)
    }

    pub fn spawn_empty() -> Result<Self, DriverError> {
        Self::spawn(SourceRunner::new_empty())
    }

    pub fn spawn_with_workers(
        runner: SourceRunner,
        workers: Option<NonZeroUsize>,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_workers_and_external_handler(runner, workers, None)
    }

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

    pub fn spawn_with_config(
        runner: SourceRunner,
        config: DispatcherConfig,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_config_and_external_handler(runner, config, None)
    }

    pub fn spawn_with_external_handler(
        runner: SourceRunner,
        handler: ExternalRequestHandler,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_config_and_external_handler(
            runner,
            DispatcherConfig::default(),
            Some(handler),
        )
    }

    pub fn spawn_with_config_and_external_handler(
        runner: SourceRunner,
        config: DispatcherConfig,
        external_request_handler: Option<ExternalRequestHandler>,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_config_and_external_handlers(
            runner,
            config,
            external_request_handler,
            None,
        )
    }

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
            RelationAcceleration::Automatic,
            external_request_handler,
            external_stream_request_handler,
        )
    }

    pub fn spawn_with_resources(
        runner: SourceRunner,
        resources: DriverResources,
    ) -> Result<Self, DriverError> {
        Self::spawn_with_resources_and_external_handlers(runner, resources, None, None)
    }

    pub fn spawn_with_resources_and_external_handlers(
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
        let runner = runner
            .with_task_limits(task_limits)
            .with_execution_context(execution_context);
        let state = PoolState {
            event_capacity: event_queue_capacity.get(),
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
        let raw = self
            .inner
            .next_ephemeral_identity
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < EPHEMERAL_HOST_IDENTITY_END).then_some(current + 1)
            })
            .map_err(|_| DriverError::EphemeralIdentityExhausted)?;
        Identity::new(raw).ok_or(DriverError::EphemeralIdentityExhausted)
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

    pub async fn submit_source_report(
        &self,
        endpoint: Identity,
        actor: Option<Symbol>,
        source: String,
    ) -> Result<RunReport, DriverError> {
        self.ensure_endpoint_open(endpoint)?;
        let mut request = match actor {
            Some(actor) => self
                .inner
                .runner
                .source_request_as(actor, source)
                .map_err(DriverError::Source)?,
            None => self
                .inner
                .runner
                .source_request_for_endpoint(endpoint, source)
                .map_err(DriverError::Source)?,
        };
        request.endpoint = endpoint;
        let context = TaskContext::from_request(&request, endpoint);
        let runner = Arc::clone(&self.inner.runner);
        let submitted = self
            .dispatch(DispatchOperation::Submit, move || async move {
                runner.submit_source(request)
            })
            .await?;
        self.handle_submitted(context, submitted.clone()).await?;
        Ok(self
            .inner
            .runner
            .report_outcome(submitted.task_id, submitted.outcome))
    }

    pub async fn run_read_only_source_query(
        &self,
        endpoint: Identity,
        source: String,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, DriverError> {
        self.ensure_endpoint_open(endpoint)?;
        let runner = Arc::clone(&self.inner.runner);
        self.dispatch(DispatchOperation::Submit, move || async move {
            runner.run_read_only_source_query_for_endpoint(endpoint, source, options)
        })
        .await
    }

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

    pub async fn submit_source_as_actor(
        &self,
        endpoint: Identity,
        actor: Identity,
        source: String,
    ) -> Result<SubmittedTask, DriverError> {
        self.ensure_endpoint_open(endpoint)?;
        let mut request = self
            .inner
            .runner
            .source_request_as_identity(actor, source)
            .map_err(DriverError::Source)?;
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

    pub async fn submit_invocation_for_endpoint(
        &self,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<SubmittedTask, DriverError> {
        self.ensure_endpoint_open(endpoint)?;
        let trace_selector = selector;
        let dispatch_start = Instant::now();
        let runner = Arc::clone(&self.inner.runner);
        let (context, submitted) = self
            .dispatch(DispatchOperation::Invoke, move || async move {
                let request = runner.invocation_request_for_endpoint(endpoint, selector, roles)?;
                let context = TaskContext::from_request(&request, endpoint);
                let submitted = runner.submit_invocation(request)?;
                Ok((context, submitted))
            })
            .await?;
        tracing::debug!(
            selector = trace_selector.name().unwrap_or("<unnamed>"),
            task_id = submitted.task_id,
            elapsed_us = dispatch_start.elapsed().as_micros(),
            "driver invocation dispatched"
        );
        let handle_start = Instant::now();
        self.handle_submitted(context, submitted.clone()).await?;
        tracing::debug!(
            selector = trace_selector.name().unwrap_or("<unnamed>"),
            task_id = submitted.task_id,
            elapsed_us = handle_start.elapsed().as_micros(),
            "driver invocation outcome processed"
        );
        Ok(submitted)
    }

    pub async fn resume(&self, task_id: TaskId, value: Value) -> Result<TaskOutcome, DriverError> {
        let context = {
            let mut state = self.inner.state.lock().unwrap();
            state.remove_mailbox_waiter(task_id);
            let context = match state.contexts.remove(&task_id) {
                Some(context) => context,
                None if state.cancelled_tasks.contains(&task_id) => {
                    return Err(DriverError::TaskCancelled(task_id));
                }
                None => return Err(DriverError::MissingTaskContext(task_id)),
            };
            state.record_metrics();
            context
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
        self.ensure_endpoint_open(endpoint)?;
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
        state.open_endpoints.insert(endpoint);
        Ok(())
    }

    pub fn open_endpoint_with_context(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), DriverError> {
        self.ensure_running()?;
        self.inner
            .runner
            .open_endpoint_with_context(endpoint, principal, actor, protocol)
            .map_err(DriverError::Source)?;
        let mut state = self.inner.state.lock().unwrap();
        state.closed_endpoints.remove(&endpoint);
        state.open_endpoints.insert(endpoint);
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
        self.ensure_running()?;
        let changes = self
            .inner
            .runner
            .open_endpoint_with_context_and_volatile_tuples_named(
                endpoint, principal, actor, protocol, tuples,
            )
            .map_err(DriverError::Source)?;
        let mut state = self.inner.state.lock().unwrap();
        state.closed_endpoints.remove(&endpoint);
        state.open_endpoints.insert(endpoint);
        Ok(changes)
    }

    pub async fn close_endpoint(&self, endpoint: Identity) -> EndpointCloseReport {
        self.mark_endpoint_closed(endpoint);
        let cancelled_tasks = self.cancel_endpoint_tasks(endpoint).await;
        let report = EndpointCloseReport {
            relation_changes: self.inner.runner.close_endpoint(endpoint),
            cancelled_tasks,
        };
        self.finish_endpoint_close(endpoint);
        report
    }

    /// Starts endpoint closure for use by a host's synchronous drop guard.
    ///
    /// New submissions are rejected before this method returns. The actual
    /// close is tracked as driver-owned work and is joined or cancelled by
    /// [`Self::shutdown`]. Hosts with an asynchronous control path should await
    /// [`Self::close_endpoint`] directly.
    pub fn close_endpoint_in_background(&self, endpoint: Identity) -> Result<(), DriverError> {
        self.ensure_running()?;
        self.mark_endpoint_closed(endpoint);
        let driver = self.clone();
        let handle = compio::runtime::spawn(async move {
            driver.close_endpoint(endpoint).await;
        });
        self.track_worker(0, handle);
        Ok(())
    }

    pub async fn close_endpoint_and_retract_volatile_tuples_named(
        &self,
        endpoint: Identity,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<EndpointCloseReport, DriverError> {
        self.mark_endpoint_closed(endpoint);
        let cancelled_tasks = self.cancel_endpoint_tasks(endpoint).await;
        let relation_changes = self
            .inner
            .runner
            .close_endpoint_and_retract_volatile_tuples_named(endpoint, tuples)
            .map_err(DriverError::Source)?;
        self.finish_endpoint_close(endpoint);
        Ok(EndpointCloseReport {
            relation_changes,
            cancelled_tasks,
        })
    }

    pub async fn cancel_task(&self, task_id: TaskId) -> Result<SuspendKind, DriverError> {
        self.cancel_task_with_reason(task_id, TaskCancellationReason::Requested)
            .await
    }

    pub fn assert_volatile_tuples_named(
        &self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        self.ensure_running()?;
        self.inner
            .runner
            .assert_volatile_tuples_named(tuples)
            .map_err(DriverError::Source)
    }

    pub fn retract_volatile_tuples_named(
        &self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        self.ensure_running()?;
        self.inner
            .runner
            .retract_volatile_tuples_named(tuples)
            .map_err(DriverError::Source)
    }

    pub fn create_subscription_mailbox(&self) -> Result<DriverSubscriptionMailbox, DriverError> {
        self.ensure_running()?;
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
        self.inner
            .state
            .lock()
            .unwrap()
            .external_subscription_mailboxes
            .insert(mailbox);
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
        self.ensure_endpoint_open(endpoint)?;
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
        let delivered = self.inner.runner.take_subscription_deliveries();
        let mut queue = VecDeque::new();
        self.route_mailbox_deliveries(delivered, &mut queue).await?;
        self.process_outcome_queue(&mut queue).await?;
        Ok(subscription)
    }

    pub fn cancel_subscription(&self, subscription: Value) -> Result<(), DriverError> {
        self.ensure_running()?;
        self.inner
            .runner
            .cancel_subscription(subscription)
            .map_err(runtime_driver_error)
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

        let endpoints = {
            let mut state = self.inner.state.lock().unwrap();
            state.open_endpoints.drain().collect::<Vec<_>>()
        };
        for endpoint in endpoints {
            self.inner.runner.close_endpoint(endpoint);
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

    fn ensure_running(&self) -> Result<(), DriverError> {
        if self.inner.state.lock().unwrap().lifecycle != DriverLifecycle::Running {
            return Err(DriverError::DriverStopped);
        }
        Ok(())
    }

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

    fn finish_endpoint_close(&self, endpoint: Identity) {
        self.inner
            .state
            .lock()
            .unwrap()
            .open_endpoints
            .remove(&endpoint);
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
                if state.cancelled_tasks.contains(&task_id) {
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
            state.cancelled_tasks.insert(task_id);
            let workers = state.take_task_workers(task_id);
            state.record_metrics();
            (kind, workers)
        };
        drop(workers);
        self.enqueue_event(DriverEvent::TaskCancelled { task_id, reason })
            .await;
        Ok(kind)
    }

    async fn record_cancelled_task(&self, task_id: TaskId, reason: TaskCancellationReason) {
        let should_record = {
            let mut state = self.inner.state.lock().unwrap();
            if !state.cancelled_tasks.insert(task_id) {
                false
            } else {
                state.remove_task_waiters(task_id);
                state.contexts.remove(&task_id);
                state.record_metrics();
                true
            }
        };
        if should_record {
            self.enqueue_event(DriverEvent::TaskCancelled { task_id, reason })
                .await;
        }
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

    fn wait_until_idle(&self) -> DriverIdle<'_> {
        DriverIdle { driver: self }
    }

    fn wait_until_stopped(&self) -> DriverStopped<'_> {
        DriverStopped { driver: self }
    }

    fn track_worker(&self, task_id: TaskId, handle: JoinHandle<()>) {
        let mut state = self.inner.state.lock().unwrap();
        state.reap_finished_workers();
        if state.lifecycle != DriverLifecycle::Running {
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
                cancellation: None,
                handle,
            },
        );
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
            let mut timer = None;
            let mut spawn = None;
            let mut mailbox_recv = None;
            let mut external_request = None;
            let mut task_event = None;
            let mut cancellation = None;
            {
                let mut state = self.inner.state.lock().unwrap();
                match outcome {
                    TaskOutcome::Complete { value, .. } => {
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
            self.enqueue_pending_effects().await;
            self.enqueue_event(task_event.expect("non-cancelled outcome has an event"))
                .await;
            if let Some(duration) = timer {
                self.spawn_timer_resume(task_id, duration);
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
            if let Some(timeout) = timeout {
                self.spawn_mailbox_timeout(task_id, timeout);
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

    fn spawn_mailbox_timeout(&self, task_id: TaskId, duration: Duration) {
        let driver = self.clone();
        let handle = compio::runtime::spawn(async move {
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
        self.track_worker(task_id, handle);
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
                if state.external_subscription_mailboxes.contains(&mailbox) {
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

    fn spawn_timer_resume(&self, task_id: TaskId, duration: Duration) {
        let driver = self.clone();
        let handle = compio::runtime::spawn(async move {
            metrics::async_worker_started(AsyncWorkerKind::TimerResume);
            let start = Instant::now();
            compio::time::sleep(duration).await;
            let mut outcome = WorkerOutcome::Complete;
            if let Err(error) = driver.resume(task_id, Value::nothing()).await {
                outcome = driver.record_worker_resume_error(task_id, error).await;
            }
            metrics::async_worker_finished(AsyncWorkerKind::TimerResume, outcome, start.elapsed());
        });
        self.track_worker(task_id, handle);
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
        let handle = compio::runtime::spawn(async move {
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
        self.enqueue_event(DriverEvent::TaskFailed {
            task_id,
            error: rendered,
        })
        .await;
    }

    async fn record_worker_resume_error(
        &self,
        task_id: TaskId,
        error: DriverError,
    ) -> WorkerOutcome {
        if matches!(error, DriverError::TaskCancelled(_)) {
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

impl Future for EventEnqueue<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
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
        state.events.push_back(event);
        let event_wakers = std::mem::take(&mut state.event_wakers);
        state.record_metrics();
        drop(state);
        for waker in event_wakers {
            waker.wake();
        }
        Poll::Ready(())
    }
}

impl Future for DriverIdle<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        if state.in_flight_dispatches == 0 {
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

impl PoolState {
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
