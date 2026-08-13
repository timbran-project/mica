// Copyright (C) 2026 Ryan Daum <ryan@visibletrap.com>
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU Affero General Public License as published by the Free
// Software Foundation, version 3.

use crate::{
    CompioTaskDriver, DriverError, DriverEvent, DriverResourceSnapshot, DriverResources,
    DriverSubscriptionMailbox, DriverSubscriptionRequest, EndpointCloseReport, FileinIncludeLoader,
    FileinMode, FileinReport, ReadOnlySourceQueryOptions, ReadOnlySourceQueryReport, RunReport,
    Symbol, TaskCancellationReason, TaskId, TaskOutcome, Tuple, Value,
};
use futures_util::future::{Either, select};
use mica_var::Identity;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::{Future, poll_fn};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::sync::{
    Weak,
    atomic::{AtomicU64, Ordering},
};
use std::task::{Context, Poll, Waker};

pub type NamedTuple = (Symbol, Tuple);

pub struct SubscriptionMailbox {
    driver: CompioTaskDriver,
    mailbox: Option<DriverSubscriptionMailbox>,
}

impl SubscriptionMailbox {
    pub fn id(&self) -> u64 {
        self.mailbox
            .as_ref()
            .expect("an open subscription mailbox has a value")
            .id()
    }

    pub fn drain(&self) -> Result<Vec<Value>, DriverError> {
        self.driver
            .drain_subscription_mailbox(self.mailbox.as_ref().ok_or_else(|| {
                DriverError::Configuration("subscription mailbox is closed".to_owned())
            })?)
    }

    pub fn close(mut self) -> Result<(), DriverError> {
        let mailbox = self
            .mailbox
            .take()
            .expect("an owned subscription mailbox closes once");
        self.driver.close_subscription_mailbox(&mailbox)
    }
}

impl Drop for SubscriptionMailbox {
    fn drop(&mut self) {
        if let Some(mailbox) = self.mailbox.take() {
            let _ = self.driver.close_subscription_mailbox(&mailbox);
        }
    }
}

pub trait DriverWake: Send + Sync {
    fn wake(&self);
}

type DriverEventHandler = dyn Fn(DriverEvent) -> Pin<Box<dyn Future<Output = ()>>> + Send + Sync;

#[derive(Clone)]
pub struct DriverEventRouter {
    inner: Arc<DriverEventRouterInner>,
}

struct DriverEventRouterInner {
    next_registration: AtomicU64,
    sink_capacity: usize,
    sinks: Mutex<HashMap<u64, Arc<DriverEventSinkQueue>>>,
    invocations: Mutex<HashMap<TaskId, compio::runtime::JoinHandle<()>>>,
}

pub struct DriverEventRegistration {
    router: Weak<DriverEventRouterInner>,
    registration: u64,
    queue: Arc<DriverEventSinkQueue>,
    worker: Option<compio::runtime::JoinHandle<()>>,
}

struct DriverEventSinkQueue {
    capacity: usize,
    state: Mutex<DriverEventSinkQueueState>,
}

#[derive(Default)]
struct DriverEventSinkQueueState {
    events: VecDeque<DriverEvent>,
    closed: bool,
    receiver_waker: Option<Waker>,
}

struct DriverEventSinkWorkerGuard {
    queue: Arc<DriverEventSinkQueue>,
}

impl DriverEventRouter {
    /// Creates an event router with a bounded queue of this size for each registered handler.
    fn new(sink_capacity: NonZeroUsize) -> Self {
        Self {
            inner: Arc::new(DriverEventRouterInner {
                next_registration: AtomicU64::new(1),
                sink_capacity: sink_capacity.get(),
                sinks: Mutex::new(HashMap::new()),
                invocations: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Registers one ordered asynchronous event handler.
    ///
    /// The event pump writes to a bounded per-handler queue. The handler runs in
    /// its own tracked Compio task, so it can call back into Mica without
    /// preventing the pump from draining the driver's event queue.
    pub fn register<F, R>(&self, handle: F) -> DriverEventRegistration
    where
        F: Fn(DriverEvent) -> R + Send + Sync + 'static,
        R: Future<Output = ()> + 'static,
    {
        let registration = self.inner.next_registration.fetch_add(1, Ordering::Relaxed);
        let queue = Arc::new(DriverEventSinkQueue {
            capacity: self.inner.sink_capacity,
            state: Mutex::new(DriverEventSinkQueueState::default()),
        });
        self.inner
            .sinks
            .lock()
            .unwrap()
            .insert(registration, Arc::clone(&queue));
        let worker_queue = Arc::clone(&queue);
        let handler: Arc<DriverEventHandler> = Arc::new(move |event| Box::pin(handle(event)));
        let worker = compio::runtime::spawn(async move {
            let _guard = DriverEventSinkWorkerGuard {
                queue: Arc::clone(&worker_queue),
            };
            while let Some(event) = worker_queue.receive().await {
                handler(event).await;
            }
        });
        DriverEventRegistration {
            router: Arc::downgrade(&self.inner),
            registration,
            queue,
            worker: Some(worker),
        }
    }

    /// Routes one terminal event when an invocation reaches a terminal outcome.
    ///
    /// Watched invocations are retained by the router and bounded by the same
    /// capacity as each event-handler queue. A host therefore does not need a
    /// early-event cache or a detached waiter to bridge invocation completion.
    pub fn watch_invocation(&self, invocation: InvocationHandle) -> Result<(), DriverError> {
        let task_id = invocation.task_id();
        let mut invocations = self.inner.invocations.lock().unwrap();
        invocations.retain(|_, task| !task.is_finished());
        if invocations.len() >= self.inner.sink_capacity {
            return Err(DriverError::Configuration(
                "driver invocation watcher capacity is exhausted".to_owned(),
            ));
        }
        if invocations.contains_key(&task_id) {
            return Err(DriverError::Configuration(format!(
                "task {task_id} already has an invocation watcher"
            )));
        }
        let driver = invocation.driver.clone();
        let task = compio::runtime::spawn(async move {
            driver
                .publish_watched_invocation(task_id, invocation.wait().await)
                .await;
        });
        invocations.insert(task_id, task);
        Ok(())
    }

    fn route(&self, event: DriverEvent) {
        let sinks = self
            .inner
            .sinks
            .lock()
            .unwrap()
            .iter()
            .map(|(registration, queue)| (*registration, Arc::clone(queue)))
            .collect::<Vec<_>>();
        for (registration, queue) in sinks {
            if queue.try_send(event.clone()) {
                continue;
            }
            let mut registered = self.inner.sinks.lock().unwrap();
            if registered
                .get(&registration)
                .is_some_and(|current| Arc::ptr_eq(current, &queue))
            {
                registered.remove(&registration);
                queue.close();
                tracing::error!(
                    registration,
                    capacity = queue.capacity,
                    "driver event handler disconnected after its queue saturated"
                );
            }
        }
    }
}

impl DriverEventSinkQueue {
    fn try_send(&self, event: DriverEvent) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.events.len() >= self.capacity {
            return false;
        }
        state.events.push_back(event);
        if let Some(waker) = state.receiver_waker.take() {
            waker.wake();
        }
        true
    }

    async fn receive(&self) -> Option<DriverEvent> {
        poll_fn(|context| {
            let mut state = self.state.lock().unwrap();
            if let Some(event) = state.events.pop_front() {
                return Poll::Ready(Some(event));
            }
            if state.closed {
                return Poll::Ready(None);
            }
            let waker = context.waker().clone();
            if state
                .receiver_waker
                .as_ref()
                .is_none_or(|current| !current.will_wake(&waker))
            {
                state.receiver_waker = Some(waker);
            }
            Poll::Pending
        })
        .await
    }

    fn close(&self) {
        let receiver = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.events.clear();
            state.receiver_waker.take()
        };
        if let Some(waker) = receiver {
            waker.wake();
        }
    }
}

impl Drop for DriverEventSinkWorkerGuard {
    fn drop(&mut self) {
        self.queue.close();
    }
}

impl Drop for DriverEventRegistration {
    fn drop(&mut self) {
        if let Some(router) = self.router.upgrade() {
            router.sinks.lock().unwrap().remove(&self.registration);
        }
        self.queue.close();
        drop(self.worker.take());
    }
}

impl<F> DriverWake for F
where
    F: Fn() + Send + Sync,
{
    fn wake(&self) {
        self();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationDiagnostics {
    pub task_id: TaskId,
    pub selector: Symbol,
    pub endpoint: Identity,
    pub principal: Option<Identity>,
    pub actor: Option<Identity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationOutcome {
    Completed(Value),
    Aborted(Value),
    Cancelled(TaskCancellationReason),
    Failed(String),
}

pub(crate) struct InvocationState {
    diagnostics: InvocationDiagnostics,
    initial_report: RunReport,
    completion: Mutex<InvocationCompletionState>,
}

#[derive(Default)]
struct InvocationCompletionState {
    outcome: Option<InvocationOutcome>,
    waker: Option<Waker>,
}

impl InvocationState {
    pub(crate) fn new(diagnostics: InvocationDiagnostics, initial_report: RunReport) -> Self {
        Self {
            diagnostics,
            initial_report,
            completion: Mutex::new(InvocationCompletionState::default()),
        }
    }

    pub(crate) fn complete(&self, outcome: InvocationOutcome) {
        let waker = {
            let mut completion = self.completion.lock().unwrap();
            if completion.outcome.is_some() {
                return;
            }
            completion.outcome = Some(outcome);
            completion.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll(&self, context: &mut Context<'_>) -> Poll<InvocationOutcome> {
        let mut completion = self.completion.lock().unwrap();
        if let Some(outcome) = completion.outcome.clone() {
            return Poll::Ready(outcome);
        }
        let waker = context.waker().clone();
        if completion
            .waker
            .as_ref()
            .is_none_or(|current| !current.will_wake(&waker))
        {
            completion.waker = Some(waker);
        }
        Poll::Pending
    }
}

pub struct InvocationHandle {
    driver: CompioTaskDriver,
    state: Arc<InvocationState>,
}

impl InvocationHandle {
    pub(crate) fn new(driver: CompioTaskDriver, state: Arc<InvocationState>) -> Self {
        Self { driver, state }
    }

    pub fn diagnostics(&self) -> &InvocationDiagnostics {
        &self.state.diagnostics
    }

    pub fn task_id(&self) -> TaskId {
        self.state.diagnostics.task_id
    }

    pub fn initial_report(&self) -> &RunReport {
        &self.state.initial_report
    }

    pub async fn wait(&self) -> InvocationOutcome {
        poll_fn(|context| self.state.poll(context)).await
    }

    pub fn try_outcome(&self) -> Option<InvocationOutcome> {
        self.state.completion.lock().unwrap().outcome.clone()
    }

    pub async fn cancel(&self) -> Result<(), DriverError> {
        self.driver.cancel_task(self.task_id()).await.map(|_| ())
    }

    /// Transfers a suspended invocation to the background event stream.
    ///
    /// Detached invocations no longer complete through this handle; their
    /// subsequent suspension and terminal events are delivered by the event
    /// pump. Immediate terminal outcomes cannot be detached.
    pub fn detach(self) -> Result<TaskId, DriverError> {
        let task_id = self.task_id();
        self.driver.detach_invocation(task_id)?;
        Ok(task_id)
    }
}

pub struct DriverEventPump {
    driver: CompioTaskDriver,
}

pub struct DriverEventPumpTask {
    stop: Arc<PumpStop>,
    handle: compio::runtime::JoinHandle<DriverEventPump>,
}

#[derive(Default)]
struct PumpStop {
    state: Mutex<PumpStopState>,
}

#[derive(Default)]
struct PumpStopState {
    requested: bool,
    waker: Option<Waker>,
}

struct PumpStopWait {
    stop: Arc<PumpStop>,
}

impl DriverEventPump {
    fn new(driver: CompioTaskDriver) -> Self {
        Self { driver }
    }

    pub fn drain(&mut self) -> Vec<DriverEvent> {
        self.driver.drain_events()
    }

    pub async fn wait(&mut self) -> Vec<DriverEvent> {
        self.driver.wait_events().await
    }

    pub fn set_wake_handler(&mut self, handler: Arc<dyn DriverWake>) {
        self.driver.set_wake_handler(Some(handler));
    }

    pub fn clear_wake_handler(&mut self) {
        self.driver.set_wake_handler(None);
    }

    pub fn spawn<H>(mut self, mut handle: H) -> DriverEventPumpTask
    where
        H: FnMut(DriverEvent) + 'static,
    {
        let stop = Arc::new(PumpStop::default());
        let task_stop = Arc::clone(&stop);
        let task = compio::runtime::spawn(async move {
            enum PumpTurn {
                Events(Vec<DriverEvent>),
                Stop,
            }
            loop {
                let turn = {
                    let events = Box::pin(self.driver.wait_events());
                    let stopping = Box::pin(PumpStopWait {
                        stop: Arc::clone(&task_stop),
                    });
                    match select(events, stopping).await {
                        Either::Left((events, _)) => PumpTurn::Events(events),
                        Either::Right(((), _)) => PumpTurn::Stop,
                    }
                };
                match turn {
                    PumpTurn::Events(events) => {
                        for event in events {
                            handle(event);
                        }
                    }
                    PumpTurn::Stop => {
                        for event in self.drain() {
                            handle(event);
                        }
                        return self;
                    }
                }
            }
        });
        DriverEventPumpTask { stop, handle: task }
    }

    pub fn spawn_router(mut self, router: DriverEventRouter) -> DriverEventPumpTask {
        let stop = Arc::new(PumpStop::default());
        let task_stop = Arc::clone(&stop);
        let task = compio::runtime::spawn(async move {
            enum PumpTurn {
                Events(Vec<DriverEvent>),
                Stop,
            }
            loop {
                let turn = {
                    let events = Box::pin(self.driver.wait_events());
                    let stopping = Box::pin(PumpStopWait {
                        stop: Arc::clone(&task_stop),
                    });
                    match select(events, stopping).await {
                        Either::Left((events, _)) => PumpTurn::Events(events),
                        Either::Right(((), _)) => PumpTurn::Stop,
                    }
                };
                match turn {
                    PumpTurn::Events(events) => {
                        for event in events {
                            router.route(event);
                        }
                    }
                    PumpTurn::Stop => {
                        for event in self.drain() {
                            router.route(event);
                        }
                        return self;
                    }
                }
            }
        });
        DriverEventPumpTask { stop, handle: task }
    }

    pub async fn drive_until<F, T, H>(&mut self, future: F, mut handle: H) -> T
    where
        F: Future<Output = T>,
        H: FnMut(DriverEvent),
    {
        let mut future = Box::pin(future);
        loop {
            let events = Box::pin(self.driver.wait_events());
            match select(future, events).await {
                Either::Left((result, _)) => {
                    for event in self.driver.drain_events() {
                        handle(event);
                    }
                    return result;
                }
                Either::Right((events, pending)) => {
                    for event in events {
                        handle(event);
                    }
                    future = pending;
                }
            }
        }
    }

    pub async fn drive_invocation<H>(
        &mut self,
        invocation: &InvocationHandle,
        handle: H,
    ) -> InvocationOutcome
    where
        H: FnMut(DriverEvent),
    {
        self.drive_until(invocation.wait(), handle).await
    }
}

impl DriverEventPumpTask {
    pub async fn stop(self) -> Result<DriverEventPump, DriverError> {
        self.stop.request();
        self.handle
            .await
            .map_err(|_| DriverError::Join("driver event pump panicked".to_owned()))
    }
}

impl PumpStop {
    fn request(&self) {
        let waker = {
            let mut state = self.state.lock().unwrap();
            state.requested = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Future for PumpStopWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.stop.state.lock().unwrap();
        if state.requested {
            return Poll::Ready(());
        }
        let waker = context.waker().clone();
        if state
            .waker
            .as_ref()
            .is_none_or(|current| !current.will_wake(&waker))
        {
            state.waker = Some(waker);
        }
        Poll::Pending
    }
}

impl Drop for DriverEventPump {
    fn drop(&mut self) {
        self.driver.set_wake_handler(None);
    }
}

pub struct DriverOwner {
    driver: CompioTaskDriver,
    event_pump_taken: bool,
    shutdown_started: bool,
}

#[derive(Clone)]
pub struct DriverClient {
    driver: CompioTaskDriver,
}

#[derive(Clone)]
pub struct DriverAdministrator {
    driver: CompioTaskDriver,
}

impl DriverAdministrator {
    fn new(driver: CompioTaskDriver) -> Self {
        Self { driver }
    }

    pub async fn evaluate(&self, source: String) -> Result<InvocationHandle, DriverError> {
        self.driver.submit_root_source_handle(source).await
    }

    pub async fn check_filein(
        &self,
        source: String,
        include_loader: Option<FileinIncludeLoader>,
    ) -> Result<Vec<crate::RunReport>, DriverError> {
        self.driver.check_filein(source, include_loader).await
    }

    pub async fn filein_unit(
        &self,
        unit: Symbol,
        source: String,
        mode: FileinMode,
        include_loader: Option<FileinIncludeLoader>,
    ) -> Result<FileinReport, DriverError> {
        self.driver
            .filein_unit(unit, source, mode, include_loader)
            .await
    }

    pub async fn fileout_unit(&self, unit: Symbol) -> Result<String, DriverError> {
        self.driver.fileout_unit(unit).await
    }
}

impl DriverClient {
    fn new(driver: CompioTaskDriver) -> Self {
        Self { driver }
    }

    pub fn open_endpoint(
        &self,
        mut configuration: EndpointConfiguration,
    ) -> Result<EndpointSession, DriverError> {
        if configuration.endpoint.is_none() {
            configuration.endpoint = Some(self.allocate_ephemeral_identity()?);
        }
        EndpointSession::open(self.driver.clone(), configuration)
    }

    pub fn allocate_ephemeral_identity(&self) -> Result<Identity, DriverError> {
        self.driver.allocate_ephemeral_identity()
    }

    pub fn named_identity(&self, name: Symbol) -> Result<Identity, DriverError> {
        self.driver.named_identity(name)
    }

    pub fn named_relation(&self, name: Symbol) -> Result<(Identity, u16), DriverError> {
        self.driver.named_relation(name)
    }

    pub fn format_error(&self, error: &DriverError) -> String {
        self.driver.format_error(error)
    }

    pub fn format_value(&self, value: &Value) -> String {
        self.driver.format_value(value)
    }

    pub fn resource_snapshot(&self) -> DriverResourceSnapshot {
        self.driver.resource_snapshot()
    }

    pub fn create_subscription_mailbox(&self) -> Result<SubscriptionMailbox, DriverError> {
        Ok(SubscriptionMailbox {
            mailbox: Some(self.driver.create_subscription_mailbox()?),
            driver: self.driver.clone(),
        })
    }
}

impl DriverOwner {
    pub fn builder(resources: DriverResources) -> crate::DriverBuilder {
        crate::DriverBuilder::new(resources)
    }

    pub(crate) fn new(driver: CompioTaskDriver) -> Self {
        Self {
            driver,
            event_pump_taken: false,
            shutdown_started: false,
        }
    }

    pub fn take_event_pump(&mut self) -> Result<DriverEventPump, DriverError> {
        if self.event_pump_taken {
            return Err(DriverError::Configuration(
                "the driver event pump has already been taken".to_owned(),
            ));
        }
        self.event_pump_taken = true;
        Ok(DriverEventPump::new(self.driver.clone()))
    }

    pub fn event_router(&self) -> DriverEventRouter {
        DriverEventRouter::new(self.driver.event_queue_capacity())
    }

    pub fn client(&self) -> DriverClient {
        DriverClient::new(self.driver.clone())
    }

    pub fn administrator(&self) -> DriverAdministrator {
        DriverAdministrator::new(self.driver.clone())
    }

    pub async fn shutdown<H>(
        &mut self,
        pump: &mut DriverEventPump,
        handle: H,
    ) -> Result<(), DriverError>
    where
        H: FnMut(DriverEvent),
    {
        if !self.driver.same_driver(&pump.driver) {
            return Err(DriverError::Configuration(
                "event pump belongs to a different driver".to_owned(),
            ));
        }
        self.shutdown_started = true;
        pump.drive_until(self.driver.shutdown(), handle).await
    }
}

impl Drop for DriverOwner {
    fn drop(&mut self) {
        if self.shutdown_started || self.driver.is_shutdown() {
            return;
        }
        self.shutdown_started = true;
        if !self.driver.shutdown_in_background_discarding_events() {
            tracing::error!(
                "DriverOwner dropped outside its Compio runtime; explicit shutdown was not completed"
            );
        }
    }
}

pub struct EndpointConfiguration {
    endpoint: Option<Identity>,
    principal: Option<Identity>,
    actor: Option<Identity>,
    protocol: Symbol,
    volatile_facts: Vec<NamedTuple>,
}

impl EndpointConfiguration {
    pub fn new(protocol: Symbol) -> Self {
        Self {
            endpoint: None,
            principal: None,
            actor: None,
            protocol,
            volatile_facts: Vec::new(),
        }
    }

    pub fn endpoint(mut self, endpoint: Identity) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    pub fn principal(mut self, principal: Identity) -> Self {
        self.principal = Some(principal);
        self
    }

    pub fn actor(mut self, actor: Identity) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn volatile_facts(mut self, facts: Vec<NamedTuple>) -> Self {
        self.volatile_facts = facts;
        self
    }
}

#[derive(Clone)]
pub struct EndpointSession {
    inner: Arc<EndpointSessionInner>,
}

struct EndpointCloseHandle {
    driver: CompioTaskDriver,
    endpoint: Identity,
    pending: bool,
}

impl EndpointCloseHandle {
    pub async fn wait(mut self) -> Result<EndpointCloseReport, DriverError> {
        let result = self.driver.close_endpoint_resources(self.endpoint).await;
        if result.is_ok() {
            self.pending = false;
        }
        result
    }
}

impl Drop for EndpointCloseHandle {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        self.pending = false;
        let _ = self
            .driver
            .close_endpoint_resources_in_background(self.endpoint);
    }
}

impl fmt::Debug for EndpointSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointSession")
            .field("endpoint", &self.inner.endpoint)
            .field("principal", &self.inner.principal)
            .field("actor", &self.inner.actor)
            .field("protocol", &self.inner.protocol)
            .field("closed", &self.inner.state.lock().unwrap().closed)
            .finish()
    }
}

struct EndpointSessionInner {
    driver: CompioTaskDriver,
    endpoint: Identity,
    principal: Option<Identity>,
    actor: Option<Identity>,
    protocol: Symbol,
    state: Mutex<EndpointSessionState>,
}

struct EndpointSessionState {
    closed: bool,
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::{DriverResources, FileinMode};
    use mica_runtime::SourceRunner;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn invocation_handles_and_scoped_endpoint_facts_are_first_class() {
        crate::test_support::run(async {
            let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
            let mut owner = DriverOwner::builder(resources)
                .initial_filein_unit(
                    Symbol::intern("test"),
                    "make_identity(:test_actor)\n\
                     make_relation(:CanInvoke, 2)\n\
                     make_relation(:CanRead, 2)\n\
                     make_relation(:CurrentValue, 2, :volatile)\n\
                     assert CanInvoke(#test_actor, :read_current)\n\
                     assert CanRead(#test_actor, :CurrentValue)\n\
                     verb read_current(subject)\n\
                       return one CurrentValue(subject, ?value)\n\
                     end",
                    FileinMode::Add,
                    None,
                )
                .build()
                .unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            assert!(owner.take_event_pump().is_err());
            let endpoint = Identity::new(0x00ee_0000_0000_5001).unwrap();
            let subject = Identity::new(0x00ee_0000_0000_5002).unwrap();
            let actor = owner
                .driver
                .named_identity(Symbol::intern("test_actor"))
                .unwrap();
            let relation = Symbol::intern("CurrentValue");
            let session = owner
                .client()
                .open_endpoint(
                    EndpointConfiguration::new(Symbol::intern("test"))
                        .endpoint(endpoint)
                        .actor(actor)
                        .volatile_facts(vec![(
                            relation,
                            Tuple::from([Value::identity(subject), Value::int(1).unwrap()]),
                        )]),
                )
                .unwrap();
            assert!(
                owner
                    .client()
                    .open_endpoint(
                        EndpointConfiguration::new(Symbol::intern("test")).endpoint(endpoint)
                    )
                    .is_err()
            );
            session
                .replace_volatile_scope(
                    Symbol::intern("endpoint"),
                    vec![(
                        relation,
                        Tuple::from([Value::identity(subject), Value::int(2).unwrap()]),
                    )],
                )
                .unwrap();
            session
                .replace_volatile_scope(
                    Symbol::intern("shared"),
                    vec![(
                        relation,
                        Tuple::from([Value::identity(subject), Value::int(2).unwrap()]),
                    )],
                )
                .unwrap();
            session
                .replace_volatile_scope(Symbol::intern("endpoint"), Vec::new())
                .unwrap();

            let invocation = session
                .invoke(
                    Symbol::intern("read_current"),
                    vec![(Symbol::intern("subject"), Value::identity(subject))],
                )
                .await
                .unwrap();
            assert_eq!(
                invocation.diagnostics().selector,
                Symbol::intern("read_current")
            );
            assert_eq!(invocation.diagnostics().endpoint, endpoint);
            let outcome = pump.drive_invocation(&invocation, |_| {}).await;
            assert_eq!(
                outcome,
                InvocationOutcome::Completed(Value::int(2).unwrap())
            );

            session.close_with_pump(&mut pump, |_| {}).await.unwrap();
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn readiness_handler_observes_suspended_task_wakeups() {
        crate::test_support::run(async {
            let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
            let mut owner = DriverOwner::builder(resources)
                .initial_filein(
                    "make_identity(:test_actor)\n\
                     make_relation(:CanInvoke, 2)\n\
                     assert CanInvoke(#test_actor, :delayed)\n\
                     verb delayed()\n\
                       suspend(0.01)\n\
                       return 7\n\
                     end",
                    None,
                )
                .build()
                .unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            let wakes = Arc::new(AtomicUsize::new(0));
            let wake_counter = Arc::clone(&wakes);
            pump.set_wake_handler(Arc::new(move || {
                wake_counter.fetch_add(1, Ordering::Relaxed);
            }));
            let endpoint = Identity::new(0x00ee_0000_0000_5011).unwrap();
            let actor = owner
                .driver
                .named_identity(Symbol::intern("test_actor"))
                .unwrap();
            let session = owner
                .client()
                .open_endpoint(
                    EndpointConfiguration::new(Symbol::intern("test"))
                        .endpoint(endpoint)
                        .actor(actor),
                )
                .unwrap();
            let invocation = session
                .invoke(Symbol::intern("delayed"), Vec::new())
                .await
                .unwrap();

            compio::time::sleep(std::time::Duration::from_millis(30)).await;
            assert!(wakes.load(Ordering::Relaxed) > 0);
            assert_eq!(
                pump.drive_invocation(&invocation, |_| {}).await,
                InvocationOutcome::Completed(Value::int(7).unwrap())
            );
            session.close_with_pump(&mut pump, |_| {}).await.unwrap();
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn invocation_completions_do_not_compete_with_the_event_pump() {
        crate::test_support::run(async {
            let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
            let mut owner = DriverOwner::builder(resources)
                .initial_filein(
                    "make_identity(:test_actor)\n\
                     make_relation(:CanInvoke, 2)\n\
                     assert CanInvoke(#test_actor, :delayed)\n\
                     verb delayed(value)\n\
                       suspend(0.01)\n\
                       return value\n\
                     end",
                    None,
                )
                .build()
                .unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            let actor = owner
                .driver
                .named_identity(Symbol::intern("test_actor"))
                .unwrap();
            let session = owner
                .client()
                .open_endpoint(EndpointConfiguration::new(Symbol::intern("test")).actor(actor))
                .unwrap();
            let first = session
                .invoke(
                    Symbol::intern("delayed"),
                    vec![(Symbol::intern("value"), Value::int(1).unwrap())],
                )
                .await
                .unwrap();
            let second = session
                .invoke(
                    Symbol::intern("delayed"),
                    vec![(Symbol::intern("value"), Value::int(2).unwrap())],
                )
                .await
                .unwrap();

            compio::time::sleep(std::time::Duration::from_millis(30)).await;
            pump.drain();
            assert_eq!(
                second.wait().await,
                InvocationOutcome::Completed(Value::int(2).unwrap())
            );
            assert_eq!(
                first.wait().await,
                InvocationOutcome::Completed(Value::int(1).unwrap())
            );

            session.close_with_pump(&mut pump, |_| {}).await.unwrap();
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn asynchronous_event_handlers_can_reenter_mica_with_bounded_queues() {
        crate::test_support::run(async {
            let mut resources = DriverResources::new(NonZeroUsize::MIN);
            resources.event_queue_capacity = NonZeroUsize::MIN;
            let mut owner = DriverOwner::builder(resources)
                .initial_filein("make_identity(:event_target)", None)
                .build()
                .unwrap();
            let router = owner.event_router();
            let administrator = owner.administrator();
            let completed = Arc::new(AtomicUsize::new(0));
            let completed_in_handler = Arc::clone(&completed);
            let registration = router.register(move |event| {
                let administrator = administrator.clone();
                let completed = Arc::clone(&completed_in_handler);
                async move {
                    if let DriverEvent::Effect(_) = event {
                        let invocation = administrator
                            .evaluate("suspend(0.01)\nreturn 9".to_owned())
                            .await
                            .unwrap();
                        assert_eq!(
                            invocation.wait().await,
                            InvocationOutcome::Completed(Value::int(9).unwrap())
                        );
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            let pump_task = owner
                .take_event_pump()
                .unwrap()
                .spawn_router(router.clone());
            owner
                .administrator()
                .evaluate("emit(#event_target, \"redraw\")\nreturn 1".to_owned())
                .await
                .unwrap();

            for _ in 0..100 {
                if completed.load(Ordering::Relaxed) == 1 {
                    break;
                }
                compio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            assert_eq!(completed.load(Ordering::Relaxed), 1);

            drop(registration);
            let mut pump = pump_task.stop().await.unwrap();
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn endpoint_fact_ownership_is_shared_and_preserves_preexisting_facts() {
        crate::test_support::run(async {
            let resources = DriverResources::new(NonZeroUsize::MIN);
            let mut owner = DriverOwner::builder(resources)
                .initial_filein(
                    "make_identity(:subject)\n\
                     make_relation(:Observed, 2, :volatile)\n\
                     assert Observed(#subject, 1)",
                    None,
                )
                .build()
                .unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            let client = owner.client();
            let subject = client.named_identity(Symbol::intern("subject")).unwrap();
            let shared = (
                Symbol::intern("Observed"),
                Tuple::from([Value::identity(subject), Value::int(2).unwrap()]),
            );
            let preexisting = (
                Symbol::intern("Observed"),
                Tuple::from([Value::identity(subject), Value::int(1).unwrap()]),
            );
            let first = client
                .open_endpoint(
                    EndpointConfiguration::new(Symbol::intern("test"))
                        .volatile_facts(vec![shared.clone(), preexisting.clone()]),
                )
                .unwrap();
            let second = client
                .open_endpoint(
                    EndpointConfiguration::new(Symbol::intern("test"))
                        .volatile_facts(vec![shared.clone()]),
                )
                .unwrap();

            first.close_with_pump(&mut pump, |_| {}).await.unwrap();
            assert!(
                owner
                    .driver
                    .inner_runner()
                    .contains_named_tuple(shared.0, &shared.1)
                    .unwrap()
            );
            second.close_with_pump(&mut pump, |_| {}).await.unwrap();
            assert!(
                !owner
                    .driver
                    .inner_runner()
                    .contains_named_tuple(shared.0, &shared.1)
                    .unwrap()
            );
            assert!(
                owner
                    .driver
                    .inner_runner()
                    .contains_named_tuple(preexisting.0, &preexisting.1)
                    .unwrap()
            );
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn mailbox_and_filein_resources_are_released() {
        crate::test_support::run(async {
            let resources = DriverResources::new(NonZeroUsize::MIN);
            let mut owner = DriverOwner::builder(resources).build().unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            let client = owner.client();
            let mailbox = client.create_subscription_mailbox().unwrap();
            assert_eq!(client.resource_snapshot().subscription_mailboxes, 1);
            drop(mailbox);
            assert_eq!(client.resource_snapshot().subscription_mailboxes, 0);

            let administrator = owner.administrator();
            for value in 0..8 {
                administrator
                    .filein_unit(
                        Symbol::intern("reload"),
                        format!("make_identity(:reload_{value})"),
                        if value == 0 {
                            FileinMode::Add
                        } else {
                            FileinMode::Replace
                        },
                        None,
                    )
                    .await
                    .unwrap();
            }
            assert_eq!(client.resource_snapshot().retained_terminal_tasks, 0);
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn endpoint_and_owner_shutdown_complete_invocation_handles_without_task_events() {
        crate::test_support::run(async {
            let mut resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
            resources.event_queue_capacity = NonZeroUsize::new(1).unwrap();
            let mut owner = DriverOwner::builder(resources)
                .initial_filein(
                    "make_identity(:test_actor)\n\
                     make_relation(:CanInvoke, 2)\n\
                     assert CanInvoke(#test_actor, :wait_forever)\n\
                     verb wait_forever()\n\
                       suspend()\n\
                     end",
                    None,
                )
                .build()
                .unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            let actor = owner
                .driver
                .named_identity(Symbol::intern("test_actor"))
                .unwrap();
            let session = owner
                .client()
                .open_endpoint(EndpointConfiguration::new(Symbol::intern("test")).actor(actor))
                .unwrap();
            let invocation = session
                .invoke(Symbol::intern("wait_forever"), Vec::new())
                .await
                .unwrap();
            assert_eq!(owner.client().resource_snapshot().queued_events, 0);

            let mut delivered = 0;
            let report = session
                .close_with_pump(&mut pump, |_| {
                    delivered += 1;
                })
                .await
                .unwrap();
            assert_eq!(report.cancelled_tasks, vec![invocation.task_id()]);
            assert_eq!(
                invocation.wait().await,
                InvocationOutcome::Cancelled(TaskCancellationReason::EndpointClosed)
            );
            assert_eq!(owner.client().resource_snapshot().queued_events, 0);
            assert_eq!(delivered, 0);

            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }

    #[test]
    fn owner_drop_outside_its_compio_runtime_does_not_panic() {
        let runtime = compio::runtime::Runtime::new().unwrap();
        let (owner, endpoint) = runtime.block_on(async {
            let owner = DriverOwner::builder(DriverResources::new(NonZeroUsize::MIN))
                .source_runner(SourceRunner::new_empty())
                .build()
                .unwrap();
            let endpoint = owner
                .client()
                .open_endpoint(EndpointConfiguration::new(Symbol::intern("test")))
                .unwrap();
            (owner, endpoint)
        });
        assert!(endpoint.close_in_background().is_err());
        drop(endpoint);
        drop(runtime);
        drop(owner);
    }
}

impl EndpointSession {
    fn open(
        driver: CompioTaskDriver,
        configuration: EndpointConfiguration,
    ) -> Result<Self, DriverError> {
        let endpoint = configuration
            .endpoint
            .expect("driver owner assigns an endpoint identity before opening");
        driver.open_endpoint_with_context_and_volatile_tuples_named(
            endpoint,
            configuration.principal,
            configuration.actor,
            configuration.protocol,
            configuration.volatile_facts.clone(),
        )?;
        Ok(Self {
            inner: Arc::new(EndpointSessionInner {
                driver,
                endpoint,
                principal: configuration.principal,
                actor: configuration.actor,
                protocol: configuration.protocol,
                state: Mutex::new(EndpointSessionState { closed: false }),
            }),
        })
    }

    pub fn endpoint(&self) -> Identity {
        self.inner.endpoint
    }

    pub fn principal(&self) -> Option<Identity> {
        self.inner.principal
    }

    pub fn actor(&self) -> Option<Identity> {
        self.inner.actor
    }

    pub fn protocol(&self) -> Symbol {
        self.inner.protocol
    }

    pub async fn invoke(
        &self,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<InvocationHandle, DriverError> {
        if self.inner.state.lock().unwrap().closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        self.inner
            .driver
            .submit_invocation_handle(self.inner.endpoint, selector, roles)
            .await
    }

    pub async fn evaluate(&self, source: String) -> Result<InvocationHandle, DriverError> {
        if self.inner.state.lock().unwrap().closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        self.inner
            .driver
            .submit_source_handle(self.inner.endpoint, source)
            .await
    }

    pub async fn input(&self, value: Value) -> Result<Vec<TaskOutcome>, DriverError> {
        if self.inner.state.lock().unwrap().closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        self.inner.driver.input(self.inner.endpoint, value).await
    }

    pub async fn run_read_only_source_query(
        &self,
        source: String,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, DriverError> {
        if self.inner.state.lock().unwrap().closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        self.inner
            .driver
            .run_read_only_source_query(self.inner.endpoint, source, options)
            .await
    }

    pub async fn subscribe(
        &self,
        mailbox: &SubscriptionMailbox,
        request: DriverSubscriptionRequest,
    ) -> Result<Value, DriverError> {
        if !self.inner.driver.same_driver(&mailbox.driver) {
            return Err(DriverError::Configuration(
                "subscription mailbox belongs to a different driver".to_owned(),
            ));
        }
        let Some(mailbox) = mailbox.mailbox.as_ref() else {
            return Err(DriverError::Configuration(
                "subscription mailbox is closed".to_owned(),
            ));
        };
        if self.inner.state.lock().unwrap().closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        let subscription = self
            .inner
            .driver
            .register_subscription_for_endpoint(self.inner.endpoint, mailbox, request)
            .await?;
        let state = self.inner.state.lock().unwrap();
        if state.closed {
            drop(state);
            let _ = self
                .inner
                .driver
                .cancel_subscription_for_endpoint(self.inner.endpoint, subscription.clone());
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        Ok(subscription)
    }

    pub fn cancel_subscription(&self, subscription: Value) -> Result<(), DriverError> {
        self.inner
            .driver
            .cancel_subscription_for_endpoint(self.inner.endpoint, subscription)
    }

    pub fn replace_volatile_scope(
        &self,
        scope: Symbol,
        facts: Vec<NamedTuple>,
    ) -> Result<usize, DriverError> {
        let state = self.inner.state.lock().unwrap();
        if state.closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        self.inner
            .driver
            .replace_endpoint_volatile_scope(self.inner.endpoint, scope, facts)
    }

    pub fn apply_volatile_scope_diff(
        &self,
        scope: Symbol,
        retract: Vec<NamedTuple>,
        assert: Vec<NamedTuple>,
    ) -> Result<usize, DriverError> {
        let state = self.inner.state.lock().unwrap();
        if state.closed {
            return Err(DriverError::EndpointClosed(self.inner.endpoint));
        }
        self.inner.driver.apply_endpoint_volatile_scope_diff(
            self.inner.endpoint,
            scope,
            retract,
            assert,
        )
    }

    pub async fn close_with_pump<H>(
        &self,
        pump: &mut DriverEventPump,
        handle: H,
    ) -> Result<EndpointCloseReport, DriverError>
    where
        H: FnMut(DriverEvent),
    {
        if !self.inner.driver.same_driver(&pump.driver) {
            return Err(DriverError::Configuration(
                "event pump belongs to a different driver".to_owned(),
            ));
        }
        let close = match self.begin_close()? {
            Some(close) => close,
            None => {
                return Ok(EndpointCloseReport {
                    relation_changes: 0,
                    cancelled_tasks: Vec::new(),
                });
            }
        };
        pump.drive_until(close.wait(), handle).await
    }

    /// Closes an endpoint while a separately running event pump continues to
    /// drain driver events.
    pub async fn close(&self) -> Result<EndpointCloseReport, DriverError> {
        match self.begin_close()? {
            Some(close) => close.wait().await,
            None => Ok(EndpointCloseReport {
                relation_changes: 0,
                cancelled_tasks: Vec::new(),
            }),
        }
    }

    fn begin_close(&self) -> Result<Option<EndpointCloseHandle>, DriverError> {
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.closed {
                return Ok(None);
            }
            state.closed = true;
        }
        Ok(Some(EndpointCloseHandle {
            driver: self.inner.driver.clone(),
            endpoint: self.inner.endpoint,
            pending: true,
        }))
    }

    pub fn close_in_background(&self) -> Result<(), DriverError> {
        if compio::runtime::Runtime::try_with_current(|_| ()).is_err() {
            return Err(DriverError::Configuration(
                "background endpoint close requires an active Compio runtime".to_owned(),
            ));
        }
        drop(self.begin_close()?);
        Ok(())
    }
}

impl Drop for EndpointSessionInner {
    fn drop(&mut self) {
        {
            let state = self.state.get_mut().unwrap();
            if state.closed {
                return;
            }
            state.closed = true;
        }
        let _ = self
            .driver
            .close_endpoint_resources_in_background(self.endpoint);
    }
}
