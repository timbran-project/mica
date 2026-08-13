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

use crate::execution::CpuAdmission;
use crate::{
    DispatcherConfig, DriverError, DriverEvent, DriverSubscriptionMailbox,
    DriverSubscriptionRequest, EndpointCloseReport, ExternalRequestHandler, ExternalStreamEmitter,
    ExternalStreamRequestHandler, TaskCancellationReason, TaskContext, configure_dispatcher,
    metrics::{self, AsyncWorkerKind, DispatchOperation, WorkerOutcome},
};
use compio::dispatcher::Dispatcher;
use mica_relation_wgpu::{WgpuAccelerator, WgpuAcceleratorOptions};
use mica_runtime::{
    AuthorityContext, ExecutionContext, MailboxRecvRequest, ReadOnlySourceQueryOptions,
    ReadOnlySourceQueryReport, ReadOnlySourceQueryStatus, RunReport, RuntimeError, SYSTEM_ENDPOINT,
    SharedSourceRunner, SourceRunner, SourceTaskError, SpawnRequest, SubmittedTask,
    SubscriptionRequest, SuspendKind, TaskError, TaskId, TaskInput, TaskManagerError, TaskOutcome,
    TaskRequest, Tuple,
};
use mica_var::{Identity, Symbol, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

static RELATION_ACCELERATOR: OnceLock<RelationAccelerator> = OnceLock::new();

enum RelationAccelerator {
    Enabled(Arc<WgpuAccelerator>),
    Unavailable(String),
}

fn relation_accelerator() -> &'static RelationAccelerator {
    RELATION_ACCELERATOR.get_or_init(|| {
        match WgpuAccelerator::new(WgpuAcceleratorOptions::default()) {
            Ok(accelerator) => RelationAccelerator::Enabled(Arc::new(accelerator)),
            Err(error) => {
                mica_relation_wgpu::metrics().initialization_failures.inc();
                RelationAccelerator::Unavailable(error.to_string())
            }
        }
    })
}

#[derive(Clone)]
pub struct CompioTaskDriver {
    inner: Arc<PoolInner>,
}

impl CompioTaskDriver {
    pub fn inner_runner(&self) -> Arc<SharedSourceRunner> {
        Arc::clone(&self.inner.runner)
    }
}

struct PoolInner {
    runner: Arc<SharedSourceRunner>,
    dispatcher: Dispatcher,
    cpu_admission: Arc<CpuAdmission>,
    external_request_handler: Option<ExternalRequestHandler>,
    external_stream_request_handler: Option<ExternalStreamRequestHandler>,
    state: Mutex<PoolState>,
}

#[derive(Default)]
struct PoolState {
    contexts: BTreeMap<TaskId, TaskContext>,
    cancelled_tasks: HashSet<TaskId>,
    closed_endpoints: HashSet<Identity>,
    input_waiters: BTreeMap<Identity, Vec<TaskId>>,
    mailbox_waiters: BTreeMap<u64, VecDeque<MailboxWaiter>>,
    external_subscription_mailboxes: HashSet<u64>,
    events: Vec<DriverEvent>,
    event_wakers: Vec<Waker>,
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
        let (builder, placement) = configure_dispatcher(Dispatcher::builder(), config);
        let cpu_admission = Arc::new(CpuAdmission::new(placement.worker_count));
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
            relation_parallel_workers = placement.worker_count.get().saturating_sub(1),
            affinity = if placement.is_pinned() {
                "performance cores"
            } else {
                "unrestricted"
            },
            pinned_logical_processors = placement.pinned_core_ids.as_ref().map_or(0, Vec::len),
            "runtime task execution configured"
        );
        let mut execution_context = ExecutionContext::parallel(cpu_admission.clone());
        match relation_accelerator() {
            RelationAccelerator::Enabled(accelerator) => {
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
            RelationAccelerator::Unavailable(reason) => {
                tracing::info!(
                    enabled = false,
                    backend = "wgpu",
                    fallback = "CPU",
                    reason,
                    "relation GPU backend configured"
                );
            }
        }
        let runner = runner.with_execution_context(execution_context);
        Ok(Self {
            inner: Arc::new(PoolInner {
                runner: Arc::new(runner.into_shared()),
                dispatcher,
                cpu_admission,
                external_request_handler,
                external_stream_request_handler,
                state: Mutex::new(PoolState::default()),
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

    pub fn format_error(&self, error: &DriverError) -> String {
        match error {
            DriverError::Source(error) => self.inner.runner.render_source_task_error(error),
            DriverError::Join(error) => format!("driver task failed: {error}"),
            DriverError::MissingTaskContext(task_id) => {
                format!("missing task context for task {task_id}")
            }
            DriverError::TaskCancelled(task_id) => format!("task {task_id} was cancelled"),
            DriverError::EndpointClosed(endpoint) => {
                format!(
                    "endpoint {} is closed",
                    self.inner.runner.render_identity(*endpoint)
                )
            }
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
        self.inner
            .runner
            .open_endpoint(endpoint, actor, protocol)
            .map_err(DriverError::Source)?;
        self.inner
            .state
            .lock()
            .unwrap()
            .closed_endpoints
            .remove(&endpoint);
        Ok(())
    }

    pub fn open_endpoint_with_context(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), DriverError> {
        self.inner
            .runner
            .open_endpoint_with_context(endpoint, principal, actor, protocol)
            .map_err(DriverError::Source)?;
        self.inner
            .state
            .lock()
            .unwrap()
            .closed_endpoints
            .remove(&endpoint);
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
        let changes = self
            .inner
            .runner
            .open_endpoint_with_context_and_volatile_tuples_named(
                endpoint, principal, actor, protocol, tuples,
            )
            .map_err(DriverError::Source)?;
        self.inner
            .state
            .lock()
            .unwrap()
            .closed_endpoints
            .remove(&endpoint);
        Ok(changes)
    }

    pub fn close_endpoint(&self, endpoint: Identity) -> EndpointCloseReport {
        self.mark_endpoint_closed(endpoint);
        let cancelled_tasks = self.cancel_endpoint_tasks(endpoint);
        EndpointCloseReport {
            relation_changes: self.inner.runner.close_endpoint(endpoint),
            cancelled_tasks,
        }
    }

    pub fn close_endpoint_and_retract_volatile_tuples_named(
        &self,
        endpoint: Identity,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<EndpointCloseReport, DriverError> {
        self.mark_endpoint_closed(endpoint);
        let cancelled_tasks = self.cancel_endpoint_tasks(endpoint);
        let relation_changes = self
            .inner
            .runner
            .close_endpoint_and_retract_volatile_tuples_named(endpoint, tuples)
            .map_err(DriverError::Source)?;
        Ok(EndpointCloseReport {
            relation_changes,
            cancelled_tasks,
        })
    }

    pub fn cancel_task(&self, task_id: TaskId) -> Result<SuspendKind, DriverError> {
        self.cancel_task_with_reason(task_id, TaskCancellationReason::Requested)
    }

    pub fn assert_volatile_tuples_named(
        &self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        self.inner
            .runner
            .assert_volatile_tuples_named(tuples)
            .map_err(DriverError::Source)
    }

    pub fn retract_volatile_tuples_named(
        &self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, DriverError> {
        self.inner
            .runner
            .retract_volatile_tuples_named(tuples)
            .map_err(DriverError::Source)
    }

    pub fn create_subscription_mailbox(&self) -> Result<DriverSubscriptionMailbox, DriverError> {
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
                    queue_budget: request.queue_budget,
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
        self.inner
            .runner
            .cancel_subscription(subscription)
            .map_err(runtime_driver_error)
    }

    pub fn drain_subscription_mailbox(
        &self,
        mailbox: &DriverSubscriptionMailbox,
    ) -> Result<Vec<Value>, DriverError> {
        self.inner
            .runner
            .drain_mailbox(mailbox.receiver.clone())
            .map_err(runtime_driver_error)
    }

    pub fn drain_events(&self) -> Vec<DriverEvent> {
        let mut state = self.inner.state.lock().unwrap();
        state.drain_effects_into_events(&self.inner.runner);
        let events = std::mem::take(&mut state.events);
        state.record_metrics();
        events
    }

    pub fn wait_events(&self) -> DriverEvents<'_> {
        DriverEvents { driver: self }
    }

    fn ensure_endpoint_open(&self, endpoint: Identity) -> Result<(), DriverError> {
        if self
            .inner
            .state
            .lock()
            .unwrap()
            .closed_endpoints
            .contains(&endpoint)
        {
            return Err(DriverError::EndpointClosed(endpoint));
        }
        Ok(())
    }

    fn mark_endpoint_closed(&self, endpoint: Identity) {
        self.inner
            .state
            .lock()
            .unwrap()
            .closed_endpoints
            .insert(endpoint);
    }

    fn cancel_endpoint_tasks(&self, endpoint: Identity) -> Vec<TaskId> {
        let task_ids = {
            let state = self.inner.state.lock().unwrap();
            state
                .contexts
                .iter()
                .filter_map(|(task_id, context)| (context.endpoint == endpoint).then_some(*task_id))
                .collect::<Vec<_>>()
        };
        task_ids
            .into_iter()
            .filter_map(|task_id| {
                self.cancel_task_with_reason(task_id, TaskCancellationReason::EndpointClosed)
                    .ok()
                    .map(|_| task_id)
            })
            .collect()
    }

    fn cancel_task_with_reason(
        &self,
        task_id: TaskId,
        reason: TaskCancellationReason,
    ) -> Result<SuspendKind, DriverError> {
        let (kind, event_wakers) = {
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
            state
                .events
                .push(DriverEvent::TaskCancelled { task_id, reason });
            state.record_metrics();
            (kind, std::mem::take(&mut state.event_wakers))
        };
        for waker in event_wakers {
            waker.wake();
        }
        Ok(kind)
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
        let start = Instant::now();
        metrics::dispatch_started(operation);
        let dispatch_permit = self.inner.cpu_admission.acquire_dispatch().await;
        let receiver = match self.inner.dispatcher.dispatch(move || async move {
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
            let event_wakers;
            {
                let mut state = self.inner.state.lock().unwrap();
                state.drain_effects_into_events(&self.inner.runner);
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
                        state
                            .events
                            .push(DriverEvent::TaskCompleted { task_id, value });
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
                        state
                            .events
                            .push(DriverEvent::TaskAborted { task_id, error });
                    }
                    TaskOutcome::Suspended { kind, .. } => {
                        if state.closed_endpoints.contains(&context.endpoint) {
                            drop(state);
                            self.inner
                                .runner
                                .cancel_task(task_id)
                                .map_err(DriverError::Source)?;
                            let mut state = self.inner.state.lock().unwrap();
                            state.cancelled_tasks.insert(task_id);
                            state.events.push(DriverEvent::TaskCancelled {
                                task_id,
                                reason: TaskCancellationReason::EndpointClosed,
                            });
                            state.record_metrics();
                            event_wakers = std::mem::take(&mut state.event_wakers);
                            drop(state);
                            for waker in event_wakers {
                                waker.wake();
                            }
                            continue;
                        }
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
                        state.events.push(DriverEvent::TaskSuspended {
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
                state.record_metrics();
                event_wakers = std::mem::take(&mut state.event_wakers);
            }
            for waker in event_wakers {
                waker.wake();
            }
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
        compio::runtime::spawn(async move {
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
                outcome = driver.record_worker_resume_error(task_id, error);
            }
            metrics::async_worker_finished(
                AsyncWorkerKind::MailboxTimeout,
                outcome,
                start.elapsed(),
            );
        })
        .detach();
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
        let (task_mailboxes, event_wakers) = {
            let mut state = self.inner.state.lock().unwrap();
            let mut task_mailboxes = Vec::new();
            let mut external_ready = false;
            for mailbox in mailboxes {
                if state.external_subscription_mailboxes.contains(&mailbox) {
                    state
                        .events
                        .push(DriverEvent::SubscriptionReady { mailbox });
                    external_ready = true;
                } else {
                    task_mailboxes.push(mailbox);
                }
            }
            let event_wakers = if external_ready {
                std::mem::take(&mut state.event_wakers)
            } else {
                Vec::new()
            };
            state.record_metrics();
            (task_mailboxes, event_wakers)
        };
        for waker in event_wakers {
            waker.wake();
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
        compio::runtime::spawn(async move {
            metrics::async_worker_started(AsyncWorkerKind::TimerResume);
            let start = Instant::now();
            compio::time::sleep(duration).await;
            let mut outcome = WorkerOutcome::Complete;
            if let Err(error) = driver.resume(task_id, Value::nothing()).await {
                outcome = driver.record_worker_resume_error(task_id, error);
            }
            metrics::async_worker_finished(AsyncWorkerKind::TimerResume, outcome, start.elapsed());
        })
        .detach();
    }

    fn spawn_external_request_resume(
        &self,
        task_id: TaskId,
        request: mica_runtime::ExternalRequest,
    ) {
        let driver = self.clone();
        let handler = self.inner.external_request_handler.clone();
        let stream_handler = self.inner.external_stream_request_handler.clone();
        let stream_sender = request
            .payload
            .map_get(&Value::symbol(Symbol::intern("stream_to")));
        let timeout = request.timeout_millis.map(Duration::from_millis);
        let service = request.service;
        tracing::debug!(
            target: "mica_driver::pool",
            task_id,
            service = service.name().unwrap_or("<unnamed>"),
            timeout_millis = ?request.timeout_millis,
            "driver external request scheduled"
        );
        let completed = Arc::new(AtomicBool::new(false));
        if let Some(timeout) = timeout {
            let timeout_driver = self.clone();
            let timeout_completed = Arc::clone(&completed);
            let timeout_service = service;
            compio::runtime::spawn(async move {
                metrics::async_worker_started(AsyncWorkerKind::ExternalRequestTimeout);
                let start = Instant::now();
                compio::time::sleep(timeout).await;
                if timeout_completed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    metrics::async_worker_finished(
                        AsyncWorkerKind::ExternalRequestTimeout,
                        WorkerOutcome::Cancelled,
                        start.elapsed(),
                    );
                    return;
                }
                let value = Value::error(
                    Symbol::intern("ExternalTimeout"),
                    Some("external request timed out"),
                    None,
                );
                tracing::warn!(
                    target: "mica_driver::pool",
                    task_id,
                    service = timeout_service.name().unwrap_or("<unnamed>"),
                    "external request timed out"
                );
                let mut outcome = WorkerOutcome::Timeout;
                if let Err(error) = timeout_driver.resume(task_id, value).await {
                    outcome = timeout_driver.record_worker_resume_error(task_id, error);
                }
                metrics::async_worker_finished(
                    AsyncWorkerKind::ExternalRequestTimeout,
                    outcome,
                    start.elapsed(),
                );
            })
            .detach();
        }
        compio::runtime::spawn(async move {
            metrics::async_worker_started(AsyncWorkerKind::ExternalRequest);
            let start = Instant::now();
            tracing::debug!(
                target: "mica_driver::pool",
                task_id,
                service = service.name().unwrap_or("<unnamed>"),
                "driver external request started"
            );
            let value = if let Some(stream_sender) = stream_sender {
                match stream_handler {
                    Some(handler) => {
                        let emit_driver = driver.clone();
                        let emitter = ExternalStreamEmitter::new(Arc::new(move |value| {
                            let emit_driver = emit_driver.clone();
                            let stream_sender = stream_sender.clone();
                            Box::pin(async move {
                                emit_driver
                                    .deliver_external_mailbox(stream_sender, value)
                                    .await
                                    .map_err(|error| emit_driver.format_error(&error))
                            })
                        }));
                        handler(request, emitter).await
                    }
                    None => {
                        let message = "no external stream request handler is configured";
                        let _ = driver
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
                        Value::error(Symbol::intern("ExternalUnavailable"), Some(message), None)
                    }
                }
            } else if service == Symbol::intern("mica_query") {
                driver.perform_mica_query_request(task_id, request).await
            } else {
                match handler {
                    Some(handler) => handler(request).await,
                    None => {
                        tracing::warn!(
                            target: "mica_driver::pool",
                            task_id,
                            service = service.name().unwrap_or("<unnamed>"),
                            "external request has no configured handler"
                        );
                        Value::error(
                            Symbol::intern("ExternalUnavailable"),
                            Some("no external request handler is configured"),
                            None,
                        )
                    }
                }
            };
            if completed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                metrics::async_worker_finished(
                    AsyncWorkerKind::ExternalRequest,
                    WorkerOutcome::Cancelled,
                    start.elapsed(),
                );
                return;
            }
            let mut outcome = WorkerOutcome::Complete;
            if let Err(error) = driver.resume(task_id, value).await {
                outcome = driver.record_worker_resume_error(task_id, error);
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
        })
        .detach();
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

    fn record_task_failure(&self, task_id: TaskId, error: DriverError) {
        let rendered = self.format_error(&error);
        tracing::error!(task_id, error = %rendered, "driver task failed");
        let event_wakers = {
            let mut state = self.inner.state.lock().unwrap();
            state.events.push(DriverEvent::TaskFailed {
                task_id,
                error: rendered,
            });
            state.record_metrics();
            std::mem::take(&mut state.event_wakers)
        };
        for waker in event_wakers {
            waker.wake();
        }
    }

    fn record_worker_resume_error(&self, task_id: TaskId, error: DriverError) -> WorkerOutcome {
        if matches!(error, DriverError::TaskCancelled(_)) {
            return WorkerOutcome::Cancelled;
        }
        self.record_task_failure(task_id, error);
        WorkerOutcome::Error
    }
}

impl Future for DriverEvents<'_> {
    type Output = Vec<DriverEvent>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.driver.inner.state.lock().unwrap();
        state.drain_effects_into_events(&self.driver.inner.runner);
        if !state.events.is_empty() {
            let events = std::mem::take(&mut state.events);
            state.record_metrics();
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

impl PoolState {
    fn drain_effects_into_events(&mut self, runner: &SharedSourceRunner) {
        self.events.extend(
            runner
                .drain_routed_emissions()
                .into_iter()
                .map(DriverEvent::Effect),
        );
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
