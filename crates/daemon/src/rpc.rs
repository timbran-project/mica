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
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use mica_driver::{
    DriverClient, DriverEvent, DriverEventRouter, EndpointConfiguration, EndpointSession,
};
use mica_host_protocol::{HostMessage, PROTOCOL_VERSION};
use mica_host_zmq::{PeerId, ZmqHostSocket, ZmqTransportError};
use mica_var::{Identity, Symbol, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::poll_fn;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

const DEFAULT_DRAIN_LIMIT: u32 = 64;
const MAX_DRAIN_LIMIT: u32 = 1024;
const RPC_EVENT_CAPACITY: usize = 1024;
const RPC_OUTPUT_CAPACITY: usize = 1024;

type RoutedReplies = Vec<(PeerId, HostMessage)>;

pub(crate) struct RpcHandler {
    client: DriverClient,
    router: DriverEventRouter,
    events: Arc<RpcEventQueue>,
    endpoints: BTreeMap<Identity, EndpointState>,
    tasks: BTreeMap<u64, PeerId>,
}

#[derive(Default)]
pub(crate) struct RpcEventQueue {
    state: Mutex<RpcEventQueueState>,
}

#[derive(Default)]
struct RpcEventQueueState {
    events: VecDeque<DriverEvent>,
    targets: HashMap<Identity, usize>,
    tasks: HashSet<u64>,
    saturated: bool,
    waker: Option<Waker>,
}

impl RpcEventQueue {
    pub(crate) fn push(&self, event: DriverEvent) {
        let waker = {
            let mut state = self.state.lock().unwrap();
            match &event {
                DriverEvent::Effect(effect) if state.targets.contains_key(&effect.target) => {
                    state.enqueue(event);
                }
                DriverEvent::TaskCompleted { task_id, .. }
                | DriverEvent::TaskAborted { task_id, .. }
                | DriverEvent::TaskCancelled { task_id, .. }
                | DriverEvent::TaskFailed { task_id, .. }
                    if state.tasks.remove(task_id) =>
                {
                    state.enqueue(event);
                }
                DriverEvent::Effect(_)
                | DriverEvent::TaskCompleted { .. }
                | DriverEvent::TaskAborted { .. }
                | DriverEvent::TaskCancelled { .. }
                | DriverEvent::TaskFailed { .. }
                | DriverEvent::TaskSuspended { .. }
                | DriverEvent::SubscriptionReady { .. } => {}
            }
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn register_endpoint(&self, endpoint: Identity, actor: Option<Identity>) {
        let mut state = self.state.lock().unwrap();
        state.add_target(endpoint);
        if let Some(actor) = actor {
            state.add_target(actor);
        }
    }

    fn unregister_endpoint(&self, endpoint: Identity, actor: Option<Identity>) {
        let mut state = self.state.lock().unwrap();
        state.remove_target(endpoint);
        if let Some(actor) = actor {
            state.remove_target(actor);
        }
    }

    fn register_task(&self, task_id: u64) {
        self.state.lock().unwrap().tasks.insert(task_id);
    }

    async fn wait_for_terminal(&self, task_id: u64) -> Result<(), ()> {
        poll_fn(|context| {
            let mut state = self.state.lock().unwrap();
            if state.saturated {
                return Poll::Ready(Err(()));
            }
            if state.events.iter().any(|event| {
                matches!(
                    event,
                    DriverEvent::TaskCompleted { task_id: current, .. }
                        | DriverEvent::TaskAborted { task_id: current, .. }
                        | DriverEvent::TaskCancelled { task_id: current, .. }
                        | DriverEvent::TaskFailed { task_id: current, .. }
                        if *current == task_id
                )
            }) {
                return Poll::Ready(Ok(()));
            }
            state.waker = Some(context.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn drain(&self) -> RpcEventDrain {
        let mut state = self.state.lock().unwrap();
        let events = state.events.drain(..).collect();
        let saturated = std::mem::take(&mut state.saturated);
        RpcEventDrain { events, saturated }
    }
}

impl RpcEventQueueState {
    fn enqueue(&mut self, event: DriverEvent) {
        if self.events.len() >= RPC_EVENT_CAPACITY {
            self.saturated = true;
            return;
        }
        self.events.push_back(event);
    }

    fn add_target(&mut self, target: Identity) {
        *self.targets.entry(target).or_default() += 1;
    }

    fn remove_target(&mut self, target: Identity) {
        let Some(count) = self.targets.get_mut(&target) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.targets.remove(&target);
        }
    }
}

struct RpcEventDrain {
    events: Vec<DriverEvent>,
    saturated: bool,
}

#[derive(Debug)]
pub(crate) enum RpcServerError {
    Transport(ZmqTransportError),
}

struct EndpointState {
    endpoint: EndpointSession,
    peer: PeerId,
    actor: Option<Identity>,
    output: VecDeque<Value>,
}

impl fmt::Display for RpcServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RpcServerError {}

impl From<ZmqTransportError> for RpcServerError {
    fn from(error: ZmqTransportError) -> Self {
        Self::Transport(error)
    }
}

pub(crate) async fn serve_zmq_rpc_once(
    socket: &ZmqHostSocket,
    handler: &mut RpcHandler,
) -> Result<(), RpcServerError> {
    let request = socket.recv_routed_message().await?;
    let replies = handler
        .dispatch_message(request.peer, request.message)
        .await;
    for (peer, reply) in replies {
        socket.send_routed_message(&peer, &reply).await?;
    }
    Ok(())
}

pub(crate) async fn serve_zmq_rpc_n(
    socket: &ZmqHostSocket,
    handler: &mut RpcHandler,
    requests: usize,
) -> Result<(), RpcServerError> {
    for _ in 0..requests {
        serve_zmq_rpc_once(socket, handler).await?;
    }
    Ok(())
}

pub(crate) async fn serve_zmq_rpc_forever(
    socket: &ZmqHostSocket,
    handler: &mut RpcHandler,
) -> Result<(), RpcServerError> {
    loop {
        serve_zmq_rpc_once(socket, handler).await?;
    }
}

impl RpcHandler {
    pub(crate) fn new(
        client: DriverClient,
        router: DriverEventRouter,
        events: Arc<RpcEventQueue>,
    ) -> Self {
        Self {
            client,
            router,
            events,
            endpoints: BTreeMap::new(),
            tasks: BTreeMap::new(),
        }
    }

    pub(crate) async fn dispatch_message(
        &mut self,
        peer: PeerId,
        message: HostMessage,
    ) -> RoutedReplies {
        let is_request = is_request_message(&message);
        let replies = match message {
            HostMessage::Hello { .. } => vec![HostMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                feature_bits: 0,
            }],
            HostMessage::HelloAck { .. }
            | HostMessage::RequestAccepted { .. }
            | HostMessage::RequestRejected { .. }
            | HostMessage::IdentityResolved { .. }
            | HostMessage::OutputReady { .. }
            | HostMessage::OutputBatch { .. }
            | HostMessage::EndpointClosed { .. }
            | HostMessage::TaskCompleted { .. }
            | HostMessage::TaskFailed { .. } => vec![rejected(
                0,
                "E_UNEXPECTED_MESSAGE",
                "message is not a daemon request",
            )],
            HostMessage::OpenEndpoint {
                request_id,
                endpoint,
                actor,
                protocol,
                grant_token,
            } => self.open_endpoint(&peer, request_id, endpoint, actor, protocol, grant_token),
            HostMessage::CloseEndpoint {
                request_id,
                endpoint,
            } => self.close_endpoint(&peer, request_id, endpoint).await,
            HostMessage::ResolveIdentity { request_id, name } => {
                self.resolve_identity(request_id, name)
            }
            HostMessage::SubmitSource {
                request_id,
                endpoint,
                actor,
                source,
            } => {
                self.submit_source(&peer, request_id, endpoint, actor, source)
                    .await
            }
            HostMessage::SubmitInput {
                request_id,
                endpoint,
                value,
            } => self.submit_input(&peer, request_id, endpoint, value).await,
            HostMessage::DrainOutput {
                request_id,
                endpoint,
                limit,
            } => self.drain_output(&peer, request_id, endpoint, limit),
        };
        if is_request {
            let mut events = self.drain_driver_messages();
            events.append(&mut addressed(&peer, replies));
            return events;
        }
        addressed(&peer, replies)
    }

    fn open_endpoint(
        &mut self,
        peer: &PeerId,
        request_id: u64,
        endpoint: Identity,
        actor: Option<Identity>,
        protocol: String,
        grant_token: Option<String>,
    ) -> Vec<HostMessage> {
        if grant_token.is_some() {
            return vec![rejected(
                request_id,
                "E_GRANT_UNSUPPORTED",
                "grant tokens are not implemented yet",
            )];
        }
        if self.endpoints.contains_key(&endpoint) {
            return vec![rejected(
                request_id,
                "E_ENDPOINT_IN_USE",
                "endpoint is already open",
            )];
        }
        let mut configuration =
            EndpointConfiguration::new(Symbol::intern(&protocol)).endpoint(endpoint);
        if let Some(actor) = actor {
            configuration = configuration.actor(actor);
        }
        match self.client.open_endpoint(configuration) {
            Ok(endpoint_session) => {
                self.events.register_endpoint(endpoint, actor);
                self.endpoints.insert(
                    endpoint,
                    EndpointState {
                        endpoint: endpoint_session,
                        peer: peer.clone(),
                        actor,
                        output: VecDeque::new(),
                    },
                );
                vec![accepted(request_id, None)]
            }
            Err(error) => vec![rejected(request_id, "E_OPEN_ENDPOINT", error.to_string())],
        }
    }

    async fn close_endpoint(
        &mut self,
        peer: &PeerId,
        request_id: u64,
        endpoint: Identity,
    ) -> Vec<HostMessage> {
        if !self.endpoint_belongs_to_peer(endpoint, peer) {
            return vec![rejected(
                request_id,
                "E_NO_ENDPOINT",
                "endpoint is not open",
            )];
        }
        let endpoint_session = self.endpoints.get(&endpoint).unwrap().endpoint.clone();
        let closed = match endpoint_session.close().await {
            Ok(report) => report,
            Err(error) => return vec![rejected(request_id, "E_CLOSE_ENDPOINT", error.to_string())],
        };
        let state = self.endpoints.remove(&endpoint).unwrap();
        self.events.unregister_endpoint(endpoint, state.actor);
        vec![
            HostMessage::EndpointClosed {
                endpoint,
                reason: format!(
                    "closed {} task endpoint bindings and cancelled {} tasks",
                    closed.relation_changes,
                    closed.cancelled_tasks.len()
                ),
            },
            accepted(request_id, None),
        ]
    }

    fn resolve_identity(&self, request_id: u64, name: Symbol) -> Vec<HostMessage> {
        match self.client.named_identity(name) {
            Ok(identity) => vec![HostMessage::IdentityResolved {
                request_id,
                name,
                identity,
            }],
            Err(error) => vec![rejected(
                request_id,
                "E_UNKNOWN_IDENTITY",
                error.to_string(),
            )],
        }
    }

    async fn submit_source(
        &mut self,
        peer: &PeerId,
        request_id: u64,
        endpoint: Identity,
        actor: Identity,
        source: String,
    ) -> Vec<HostMessage> {
        if !self.endpoint_belongs_to_peer(endpoint, peer) {
            return vec![rejected(
                request_id,
                "E_NO_ENDPOINT",
                "endpoint is not open",
            )];
        }
        let state = self.endpoints.get(&endpoint).unwrap();
        if state.actor != Some(actor) {
            return vec![rejected(
                request_id,
                "E_ACTOR_MISMATCH",
                "submitted actor does not match the open endpoint actor",
            )];
        }
        match state.endpoint.evaluate(source).await {
            Ok(invocation) => {
                let task_id = invocation.task_id();
                let completed = invocation.try_outcome().is_some();
                self.tasks.insert(task_id, peer.clone());
                self.events.register_task(task_id);
                if let Err(error) = self.router.watch_invocation(invocation) {
                    self.tasks.remove(&task_id);
                    return vec![rejected(request_id, "E_SUBMIT_SOURCE", error.to_string())];
                }
                if completed {
                    let _ = self.events.wait_for_terminal(task_id).await;
                }
                vec![accepted(request_id, Some(task_id))]
            }
            Err(error) => vec![rejected(request_id, "E_SUBMIT_SOURCE", error.to_string())],
        }
    }

    async fn submit_input(
        &mut self,
        peer: &PeerId,
        request_id: u64,
        endpoint: Identity,
        value: Value,
    ) -> Vec<HostMessage> {
        if !self.endpoint_belongs_to_peer(endpoint, peer) {
            return vec![rejected(
                request_id,
                "E_NO_ENDPOINT",
                "endpoint is not open",
            )];
        }
        match self
            .endpoints
            .get(&endpoint)
            .unwrap()
            .endpoint
            .input(value)
            .await
        {
            Ok(_) => vec![accepted(request_id, None)],
            Err(error) => vec![rejected(request_id, "E_SUBMIT_INPUT", error.to_string())],
        }
    }

    fn drain_output(
        &mut self,
        peer: &PeerId,
        request_id: u64,
        endpoint: Identity,
        limit: u32,
    ) -> Vec<HostMessage> {
        let Some(state) = self.endpoints.get_mut(&endpoint) else {
            return vec![rejected(
                request_id,
                "E_NO_ENDPOINT",
                "endpoint is not open",
            )];
        };
        if state.peer != *peer {
            return vec![rejected(
                request_id,
                "E_NO_ENDPOINT",
                "endpoint is not open",
            )];
        }
        let limit = normalized_drain_limit(limit) as usize;
        let count = limit.min(state.output.len());
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(value) = state.output.pop_front() else {
                break;
            };
            values.push(value);
        }
        vec![
            HostMessage::OutputBatch { endpoint, values },
            accepted(request_id, None),
        ]
    }

    fn drain_driver_messages(&mut self) -> RoutedReplies {
        let drain = self.events.drain();
        let mut messages = drain
            .events
            .into_iter()
            .flat_map(|event| self.route_driver_event(event))
            .collect::<RoutedReplies>();
        if drain.saturated {
            let tasks = std::mem::take(&mut self.tasks);
            let peers = self
                .endpoints
                .values()
                .map(|state| state.peer.clone())
                .chain(tasks.into_values())
                .collect::<Vec<_>>();
            let mut unique_peers = Vec::new();
            for peer in peers {
                if !unique_peers.contains(&peer) {
                    unique_peers.push(peer);
                }
            }
            messages.extend(unique_peers.into_iter().map(|peer| {
                (
                    peer,
                    HostMessage::TaskFailed {
                        task_id: 0,
                        error: Value::error(
                            Symbol::intern("E_RPC_EVENT_SATURATION"),
                            Some("RPC event delivery capacity was exhausted".to_owned()),
                            None,
                        ),
                    },
                )
            }));
        }
        messages
    }

    fn route_driver_event(&mut self, event: DriverEvent) -> RoutedReplies {
        match event {
            DriverEvent::TaskCompleted { task_id, value } => self
                .tasks
                .remove(&task_id)
                .map(|peer| (peer, HostMessage::TaskCompleted { task_id, value }))
                .into_iter()
                .collect(),
            DriverEvent::TaskAborted { task_id, error } => self
                .tasks
                .remove(&task_id)
                .map(|peer| (peer, HostMessage::TaskFailed { task_id, error }))
                .into_iter()
                .collect(),
            DriverEvent::TaskCancelled { task_id, reason } => self
                .tasks
                .remove(&task_id)
                .map(|peer| {
                    (
                        peer,
                        HostMessage::TaskFailed {
                            task_id,
                            error: Value::error(
                                Symbol::intern("TaskCancelled"),
                                Some(format!("task cancelled: {reason:?}")),
                                None,
                            ),
                        },
                    )
                })
                .into_iter()
                .collect(),
            DriverEvent::TaskFailed { task_id, error } => self
                .tasks
                .remove(&task_id)
                .map(|peer| {
                    (
                        peer,
                        HostMessage::TaskFailed {
                            task_id,
                            error: Value::error(Symbol::intern("E_DRIVER"), Some(error), None),
                        },
                    )
                })
                .into_iter()
                .collect(),
            DriverEvent::TaskSuspended { .. } => Vec::new(),
            DriverEvent::SubscriptionReady { .. } => Vec::new(),
            DriverEvent::Effect(effect) => {
                let targets = self.effect_targets(effect.target);
                let mut messages = Vec::with_capacity(targets.len());
                for target in targets {
                    let Some(state) = self.endpoints.get_mut(&target) else {
                        continue;
                    };
                    if state.output.len() >= RPC_OUTPUT_CAPACITY {
                        messages.push((
                            state.peer.clone(),
                            HostMessage::TaskFailed {
                                task_id: 0,
                                error: Value::error(
                                    Symbol::intern("E_RPC_OUTPUT_SATURATION"),
                                    Some(format!(
                                        "RPC endpoint {target:?} output capacity was exhausted"
                                    )),
                                    None,
                                ),
                            },
                        ));
                        continue;
                    }
                    state.output.push_back(effect.value.clone());
                    messages.push((
                        state.peer.clone(),
                        HostMessage::OutputReady {
                            endpoint: target,
                            buffered: state.output.len().try_into().unwrap_or(u32::MAX),
                        },
                    ));
                }
                messages
            }
        }
    }

    fn effect_targets(&self, target: Identity) -> Vec<Identity> {
        let mut targets = Vec::new();
        if self.endpoints.contains_key(&target) {
            targets.push(target);
        }
        for (endpoint, state) in &self.endpoints {
            if state.actor == Some(target) && !targets.contains(endpoint) {
                targets.push(*endpoint);
            }
        }
        targets
    }

    fn endpoint_belongs_to_peer(&self, endpoint: Identity, peer: &PeerId) -> bool {
        self.endpoints
            .get(&endpoint)
            .is_some_and(|state| state.peer == *peer)
    }
}

fn accepted(request_id: u64, task_id: Option<u64>) -> HostMessage {
    HostMessage::RequestAccepted {
        request_id,
        task_id,
    }
}

fn is_request_message(message: &HostMessage) -> bool {
    matches!(
        message,
        HostMessage::OpenEndpoint { .. }
            | HostMessage::CloseEndpoint { .. }
            | HostMessage::ResolveIdentity { .. }
            | HostMessage::SubmitSource { .. }
            | HostMessage::SubmitInput { .. }
            | HostMessage::DrainOutput { .. }
    )
}

fn rejected(request_id: u64, code: &str, message: impl Into<String>) -> HostMessage {
    HostMessage::RequestRejected {
        request_id,
        code: Symbol::intern(code),
        message: message.into(),
    }
}

fn addressed(peer: &PeerId, messages: Vec<HostMessage>) -> RoutedReplies {
    messages
        .into_iter()
        .map(|message| (peer.clone(), message))
        .collect()
}

fn normalized_drain_limit(limit: u32) -> u32 {
    if limit == 0 {
        return DEFAULT_DRAIN_LIMIT;
    }
    limit.min(MAX_DRAIN_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_driver::{
        DriverEventPumpTask, DriverEventRegistration, DriverEventRouter, DriverOwner,
        DriverResources, EndpointConfiguration,
    };
    use mica_host_zmq::ZmqSocketOptions;
    use mica_runtime::{Effect, SourceRunner, TaskOutcome};
    use std::num::NonZeroUsize;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_IPC_ENDPOINT: AtomicU64 = AtomicU64::new(1);

    fn endpoint(raw: u64) -> Identity {
        Identity::new(raw).unwrap()
    }

    #[test]
    fn rpc_event_queue_filters_targets_and_registered_invocations() {
        let queue = RpcEventQueue::default();
        let rpc_endpoint = endpoint(0x00ef_0000_0000_1001);
        let actor = endpoint(0x00ef_0000_0000_1002);
        queue.register_endpoint(rpc_endpoint, Some(actor));

        queue.push(DriverEvent::Effect(Effect {
            task_id: 1,
            target: endpoint(0x00ef_0000_0000_1003),
            value: Value::string("unrelated"),
        }));
        queue.push(DriverEvent::TaskCompleted {
            task_id: 7,
            value: Value::nothing(),
        });
        assert!(queue.drain().events.is_empty());

        queue.register_task(7);
        queue.push(DriverEvent::TaskCompleted {
            task_id: 7,
            value: Value::nothing(),
        });
        queue.push(DriverEvent::Effect(Effect {
            task_id: 7,
            target: actor,
            value: Value::string("owned"),
        }));
        let drain = queue.drain();
        assert!(!drain.saturated);
        assert!(matches!(
            drain.events.as_slice(),
            [DriverEvent::TaskCompleted { task_id: 7, .. }, DriverEvent::Effect(effect)]
                if effect.target == actor
        ));
    }

    #[test]
    fn rpc_event_queue_reports_saturation_at_its_fixed_bound() {
        let queue = RpcEventQueue::default();
        let rpc_endpoint = endpoint(0x00ef_0000_0000_1011);
        queue.register_endpoint(rpc_endpoint, None);
        for task_id in 0..=RPC_EVENT_CAPACITY as u64 {
            queue.register_task(task_id);
            queue.push(DriverEvent::TaskCompleted {
                task_id,
                value: Value::nothing(),
            });
        }
        let drain = queue.drain();
        assert_eq!(drain.events.len(), RPC_EVENT_CAPACITY);
        assert!(drain.saturated);
    }

    struct SeededDriver {
        owner: DriverOwner,
        _event_pump: DriverEventPumpTask,
        _event_registration: DriverEventRegistration,
        events: Arc<RpcEventQueue>,
        router: DriverEventRouter,
        actor: Identity,
    }

    impl SeededDriver {
        fn handler(&self) -> RpcHandler {
            RpcHandler::new(
                self.owner.client(),
                self.router.clone(),
                Arc::clone(&self.events),
            )
        }
    }

    fn seeded_driver() -> SeededDriver {
        let mut runner = SourceRunner::new_empty();
        runner.run_source("make_relation(:GrantEffect, 1)").unwrap();
        runner.run_source("make_identity(:alice)").unwrap();
        runner.run_source("assert GrantEffect(#alice)").unwrap();
        let alice = runner.named_identity(Symbol::intern("alice")).unwrap();
        let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
        let mut owner = DriverOwner::builder(resources)
            .source_runner(runner)
            .build()
            .unwrap();
        let router = owner.event_router();
        let events = Arc::new(RpcEventQueue::default());
        let event_queue = Arc::clone(&events);
        let event_registration = router.register(move |event| {
            let event_queue = Arc::clone(&event_queue);
            async move {
                event_queue.push(event);
            }
        });
        let event_pump = owner
            .take_event_pump()
            .unwrap()
            .spawn_router(router.clone());
        SeededDriver {
            owner,
            _event_pump: event_pump,
            _event_registration: event_registration,
            events,
            router,
            actor: alice,
        }
    }

    fn peer(id: u8) -> PeerId {
        PeerId::new(vec![id])
    }

    async fn dispatch(handler: &mut RpcHandler, message: HostMessage) -> Vec<HostMessage> {
        messages_for_peer(handler.dispatch_message(peer(1), message).await, &peer(1))
    }

    fn messages_for_peer(replies: RoutedReplies, expected_peer: &PeerId) -> Vec<HostMessage> {
        replies
            .into_iter()
            .map(|(peer, message)| {
                assert_eq!(&peer, expected_peer);
                message
            })
            .collect()
    }

    #[test]
    fn rpc_handler_accepts_endpoint_and_submits_source() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let endpoint = endpoint(0x00ef_0000_0000_0001);
            let mut handler = seeded.handler();

            assert_eq!(
                dispatch(
                    &mut handler,
                    HostMessage::OpenEndpoint {
                        request_id: 1,
                        endpoint,
                        actor: Some(actor),
                        protocol: "test".to_owned(),
                        grant_token: None,
                    },
                )
                .await,
                vec![accepted(1, None)]
            );

            let replies = dispatch(
                &mut handler,
                HostMessage::SubmitSource {
                    request_id: 2,
                    endpoint,
                    actor,
                    source: "emit(#endpoint, \"hello\")\nreturn actor()".to_owned(),
                },
            )
            .await;
            assert!(matches!(
                &replies[..],
                [
                    HostMessage::OutputReady { endpoint: target, buffered: 1 },
                    HostMessage::TaskCompleted { value, .. },
                    HostMessage::RequestAccepted {
                        request_id: 2,
                        task_id: Some(_),
                    },
                ] if *target == endpoint && *value == Value::identity(actor)
            ));

            let replies = dispatch(
                &mut handler,
                HostMessage::DrainOutput {
                    request_id: 3,
                    endpoint,
                    limit: 10,
                },
            )
            .await;
            assert_eq!(
                replies,
                vec![
                    HostMessage::OutputBatch {
                        endpoint,
                        values: vec![Value::string("hello")],
                    },
                    accepted(3, None),
                ]
            );
        });
    }

    #[test]
    fn rpc_handler_rejects_grant_tokens_until_validation_exists() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let mut handler = seeded.handler();
            assert_eq!(
                dispatch(
                    &mut handler,
                    HostMessage::OpenEndpoint {
                        request_id: 1,
                        endpoint: endpoint(0x00ef_0000_0000_0002),
                        actor: None,
                        protocol: "test".to_owned(),
                        grant_token: Some("token".to_owned()),
                    },
                )
                .await,
                vec![rejected(
                    1,
                    "E_GRANT_UNSUPPORTED",
                    "grant tokens are not implemented yet"
                )]
            );
        });
    }

    #[test]
    fn rpc_handler_routes_failed_source_as_rejection() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let endpoint = endpoint(0x00ef_0000_0000_0003);
            let mut handler = seeded.handler();
            dispatch(
                &mut handler,
                HostMessage::OpenEndpoint {
                    request_id: 1,
                    endpoint,
                    actor: Some(actor),
                    protocol: "test".to_owned(),
                    grant_token: None,
                },
            )
            .await;

            let replies = dispatch(
                &mut handler,
                HostMessage::SubmitSource {
                    request_id: 2,
                    endpoint,
                    actor,
                    source: "return 1 / 0".to_owned(),
                },
            )
            .await;
            assert!(matches!(
                &replies[..],
                [
                    HostMessage::TaskFailed { error, .. },
                    HostMessage::RequestAccepted {
                        request_id: 2,
                        task_id: Some(_),
                    },
                ] if error.error_code_symbol() == Some(Symbol::intern("E_DIV"))
            ));
        });
    }

    #[test]
    fn rpc_handler_routes_actor_effects_to_actor_endpoint() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let endpoint = endpoint(0x00ef_0000_0000_0007);
            let mut handler = seeded.handler();
            dispatch(
                &mut handler,
                HostMessage::OpenEndpoint {
                    request_id: 1,
                    endpoint,
                    actor: Some(actor),
                    protocol: "test".to_owned(),
                    grant_token: None,
                },
            )
            .await;

            let replies = dispatch(
                &mut handler,
                HostMessage::SubmitSource {
                    request_id: 2,
                    endpoint,
                    actor,
                    source: "emit(#alice, \"hello actor\")".to_owned(),
                },
            )
            .await;
            assert!(matches!(
                &replies[..],
                [
                    HostMessage::OutputReady { endpoint: target, buffered: 1 },
                    HostMessage::TaskCompleted { .. },
                    HostMessage::RequestAccepted { request_id: 2, .. },
                ] if *target == endpoint
            ));
            let replies = dispatch(
                &mut handler,
                HostMessage::DrainOutput {
                    request_id: 3,
                    endpoint,
                    limit: 1,
                },
            )
            .await;
            assert_eq!(
                replies,
                vec![
                    HostMessage::OutputBatch {
                        endpoint,
                        values: vec![Value::string("hello actor")],
                    },
                    accepted(3, None),
                ]
            );
        });
    }

    #[test]
    fn rpc_handler_resolves_named_identity() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let mut handler = seeded.handler();

            assert_eq!(
                dispatch(
                    &mut handler,
                    HostMessage::ResolveIdentity {
                        request_id: 12,
                        name: Symbol::intern("alice"),
                    },
                )
                .await,
                vec![HostMessage::IdentityResolved {
                    request_id: 12,
                    name: Symbol::intern("alice"),
                    identity: actor,
                }]
            );

            assert!(matches!(
                dispatch(
                    &mut handler,
                    HostMessage::ResolveIdentity {
                        request_id: 13,
                        name: Symbol::intern("missing"),
                    },
                )
                .await
                .as_slice(),
                [HostMessage::RequestRejected { request_id: 13, code, .. }]
                    if *code == Symbol::intern("E_UNKNOWN_IDENTITY")
            ));
        });
    }

    #[test]
    fn normalized_drain_limit_defaults_and_clamps() {
        assert_eq!(normalized_drain_limit(0), DEFAULT_DRAIN_LIMIT);
        assert_eq!(normalized_drain_limit(1), 1);
        assert_eq!(normalized_drain_limit(MAX_DRAIN_LIMIT + 1), MAX_DRAIN_LIMIT);
    }

    #[test]
    fn source_runner_identity_request_uses_actor_context() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let endpoint = seeded
                .owner
                .client()
                .open_endpoint(
                    EndpointConfiguration::new(Symbol::intern("test"))
                        .endpoint(endpoint(0x00ef_0000_0000_0004))
                        .actor(actor),
                )
                .unwrap();
            let submitted = endpoint
                .evaluate("return actor()".to_owned())
                .await
                .unwrap();
            assert!(matches!(
                &submitted.initial_report().outcome,
                TaskOutcome::Complete { value, .. } if *value == Value::identity(actor)
            ));
        });
    }

    #[test]
    fn zmq_rpc_service_handles_hello_over_ipc() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let ipc = ipc_endpoint();
            let _cleanup = IpcCleanup::new(ipc.path.clone());
            let context = zmq::Context::new();
            let router =
                ZmqHostSocket::bind(&context, zmq::ROUTER, &ipc.uri, ZmqSocketOptions::default())
                    .unwrap();
            let dealer = ZmqHostSocket::connect(
                &context,
                zmq::DEALER,
                &ipc.uri,
                ZmqSocketOptions::default(),
            )
            .unwrap();
            let seeded = seeded_driver();
            let mut handler = seeded.handler();

            let server = compio::runtime::spawn(async move {
                serve_zmq_rpc_once(&router, &mut handler).await.unwrap();
            });
            dealer
                .send_message(&HostMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    min_protocol_version: PROTOCOL_VERSION,
                    feature_bits: 0,
                    host_name: "client".to_owned(),
                })
                .await
                .unwrap();
            assert_eq!(
                dealer.recv_message().await.unwrap(),
                HostMessage::HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    feature_bits: 0,
                }
            );
            server.await.unwrap();
        });
    }

    #[test]
    fn zmq_rpc_service_handles_multiple_requests_over_ipc() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let ipc = ipc_endpoint();
            let _cleanup = IpcCleanup::new(ipc.path.clone());
            let context = zmq::Context::new();
            let router =
                ZmqHostSocket::bind(&context, zmq::ROUTER, &ipc.uri, ZmqSocketOptions::default())
                    .unwrap();
            let dealer = ZmqHostSocket::connect(
                &context,
                zmq::DEALER,
                &ipc.uri,
                ZmqSocketOptions::default(),
            )
            .unwrap();
            let seeded = seeded_driver();
            let mut handler = seeded.handler();
            let endpoint = endpoint(0x00ef_0000_0000_0005);

            let server = compio::runtime::spawn(async move {
                serve_zmq_rpc_n(&router, &mut handler, 2).await.unwrap();
            });
            dealer
                .send_message(&HostMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    min_protocol_version: PROTOCOL_VERSION,
                    feature_bits: 0,
                    host_name: "client".to_owned(),
                })
                .await
                .unwrap();
            assert!(matches!(
                dealer.recv_message().await.unwrap(),
                HostMessage::HelloAck { .. }
            ));

            dealer
                .send_message(&HostMessage::OpenEndpoint {
                    request_id: 42,
                    endpoint,
                    actor: None,
                    protocol: "test".to_owned(),
                    grant_token: None,
                })
                .await
                .unwrap();
            assert_eq!(
                dealer.recv_message().await.unwrap(),
                HostMessage::RequestAccepted {
                    request_id: 42,
                    task_id: None,
                }
            );
            server.await.unwrap();
        });
    }

    #[test]
    fn rpc_handler_can_drain_delayed_driver_events() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let endpoint = endpoint(0x00ef_0000_0000_0006);
            let mut handler = seeded.handler();
            dispatch(
                &mut handler,
                HostMessage::OpenEndpoint {
                    request_id: 1,
                    endpoint,
                    actor: Some(actor),
                    protocol: "test".to_owned(),
                    grant_token: None,
                },
            )
            .await;
            let replies = dispatch(
                &mut handler,
                HostMessage::SubmitSource {
                    request_id: 2,
                    endpoint,
                    actor,
                    source: "suspend(0.001)\nemit(#endpoint, \"awake\")".to_owned(),
                },
            )
            .await;
            assert!(matches!(
                &replies[..],
                [HostMessage::RequestAccepted {
                    request_id: 2,
                    task_id: Some(_)
                }]
            ));

            compio::time::sleep(std::time::Duration::from_millis(20)).await;
            let events = messages_for_peer(handler.drain_driver_messages(), &peer(1));
            assert!(matches!(
                &events[..],
                [
                    HostMessage::OutputReady { endpoint: target, buffered: 1 },
                    HostMessage::TaskCompleted { .. },
                ] if *target == endpoint
            ));
        });
    }

    #[test]
    fn rpc_handler_routes_delayed_events_to_endpoint_peer() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let seeded = seeded_driver();
            let actor = seeded.actor;
            let endpoint = endpoint(0x00ef_0000_0000_0008);
            let mut handler = seeded.handler();
            let owner = peer(1);
            let other = peer(2);

            let replies = handler
                .dispatch_message(
                    owner.clone(),
                    HostMessage::OpenEndpoint {
                        request_id: 1,
                        endpoint,
                        actor: Some(actor),
                        protocol: "test".to_owned(),
                        grant_token: None,
                    },
                )
                .await;
            assert_eq!(messages_for_peer(replies, &owner), vec![accepted(1, None)]);

            let replies = handler
                .dispatch_message(
                    owner.clone(),
                    HostMessage::SubmitSource {
                        request_id: 2,
                        endpoint,
                        actor,
                        source: "suspend(0.001)\nemit(#endpoint, \"awake\")".to_owned(),
                    },
                )
                .await;
            assert_eq!(messages_for_peer(replies, &owner).len(), 1);

            compio::time::sleep(std::time::Duration::from_millis(20)).await;
            let routed = handler
                .dispatch_message(
                    other.clone(),
                    HostMessage::ResolveIdentity {
                        request_id: 3,
                        name: Symbol::intern("alice"),
                    },
                )
                .await;

            assert!(routed.iter().any(|(peer, message)| {
                peer == &owner
                    && matches!(
                        message,
                        HostMessage::OutputReady {
                            endpoint: target,
                            buffered: 1,
                        } if *target == endpoint
                    )
            }));
            assert!(routed.iter().any(|(peer, message)| {
                peer == &owner && matches!(message, HostMessage::TaskCompleted { .. })
            }));
            assert!(routed.iter().any(|(peer, message)| {
                peer == &other
                    && matches!(message, HostMessage::IdentityResolved { request_id: 3, .. })
            }));
            assert!(!routed.iter().any(|(peer, message)| {
                peer == &other
                    && matches!(
                        message,
                        HostMessage::OutputReady { .. } | HostMessage::TaskCompleted { .. }
                    )
            }));
        });
    }

    struct IpcEndpoint {
        uri: String,
        path: PathBuf,
    }

    struct IpcCleanup {
        path: PathBuf,
    }

    impl IpcCleanup {
        fn new(path: PathBuf) -> Self {
            let _ = std::fs::remove_file(&path);
            Self { path }
        }
    }

    impl Drop for IpcCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn ipc_endpoint() -> IpcEndpoint {
        let index = NEXT_IPC_ENDPOINT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mica-daemon-rpc-{}-{index}.sock",
            std::process::id()
        ));
        IpcEndpoint {
            uri: format!("ipc://{}", path.display()),
            path,
        }
    }
}
