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

use crate::subscription::SubscriptionRuntimeHandle;
use crate::{
    Task, TaskError, TaskId, TaskLimits, TaskOutcome, endpoint_actor_relation,
    endpoint_open_relation, endpoint_principal_relation, endpoint_protocol_relation,
    endpoint_relation,
};
use mica_relation_kernel::{
    ExecutionContext, KernelError, RelationId, RelationKernel, RelationRead, RelationSource,
    Transaction, Tuple,
};
use mica_var::{CapabilityId, Identity, Symbol, Value};
use mica_vm::{
    AuthorityContext, BuiltinRegistry, Emission, MailboxRuntime, MailboxSend, Program,
    ProgramResolver, RuntimeContext, RuntimeError, SubscriptionInitialDelivery,
    SubscriptionOperation, SubscriptionRequest, SubscriptionSubject, SuspendKind,
};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskManagerError {
    UnknownTask(TaskId),
    TaskAlreadyCompleted(TaskId),
    TaskCancelled(TaskId),
    Task(TaskError),
}

const MAILBOX_CAP_BASE: u64 = 0x00f0_0000_0000_0000;

#[derive(Clone, Debug)]
pub struct MailboxRuntimeHandle {
    store: Arc<Mutex<MailboxStore>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MailboxCapKind {
    Receiver,
    Sender,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeCap {
    Mailbox { mailbox: u64, kind: MailboxCapKind },
    Subscription { mailbox: u64 },
}

#[derive(Debug)]
struct MailboxEntry {
    value: Value,
    subscription: Option<CapabilityId>,
}

#[derive(Debug)]
struct MailboxStore {
    next_mailbox_id: u64,
    next_cap_id: u64,
    caps: HashMap<CapabilityId, RuntimeCap>,
    queues: HashMap<u64, VecDeque<MailboxEntry>>,
    subscription_deliveries: HashSet<u64>,
}

impl MailboxRuntimeHandle {
    fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(MailboxStore::new())),
        }
    }

    pub fn drain_receiver(&self, receiver: Value) -> Result<Vec<Value>, RuntimeError> {
        self.store.lock().unwrap().drain_receiver(receiver)
    }

    pub fn mailbox_for_receiver(&self, receiver: &Value) -> Result<u64, RuntimeError> {
        self.store.lock().unwrap().mailbox_for_receiver(receiver)
    }

    pub fn mailbox_for_sender(&self, sender: &Value) -> Result<u64, RuntimeError> {
        self.store.lock().unwrap().mailbox_for_sender(sender)
    }

    fn deliver(&self, sends: &[MailboxSend]) -> Vec<u64> {
        self.store.lock().unwrap().deliver(sends)
    }

    pub(crate) fn take_subscription_deliveries(&self) -> Vec<u64> {
        let mut store = self.store.lock().unwrap();
        let mut mailboxes = store.subscription_deliveries.drain().collect::<Vec<_>>();
        mailboxes.sort_unstable();
        mailboxes
    }

    pub(crate) fn discard_pending_subscriptions(&self, operations: &[SubscriptionOperation]) {
        let mut store = self.store.lock().unwrap();
        for operation in operations {
            let SubscriptionOperation::Register { subscription, .. } = operation else {
                continue;
            };
            store.release_subscription(subscription);
        }
    }

    pub(crate) fn release_subscription(&self, subscription: &Value) {
        self.store
            .lock()
            .unwrap()
            .release_subscription(subscription);
    }

    pub(crate) fn deliver_subscription(
        &self,
        subscription: &Value,
        value: Value,
        resynchronization: Value,
        queue_budget: usize,
    ) -> Result<(u64, bool), RuntimeError> {
        self.store.lock().unwrap().deliver_subscription(
            subscription,
            value,
            resynchronization,
            queue_budget,
        )
    }

    pub(crate) fn replace_subscription_message(
        &self,
        subscription: &Value,
        value: Value,
    ) -> Result<u64, RuntimeError> {
        self.store
            .lock()
            .unwrap()
            .replace_subscription_message(subscription, value)
    }
}

impl MailboxRuntime for MailboxRuntimeHandle {
    fn create_mailbox(&self) -> Result<(Value, Value), RuntimeError> {
        self.store.lock().unwrap().create_mailbox()
    }

    fn close_mailbox(&self, receiver: &Value) -> Result<(), RuntimeError> {
        self.store.lock().unwrap().close_mailbox(receiver)
    }

    fn validate_mailbox_sender(&self, sender: &Value) -> Result<(), RuntimeError> {
        self.store
            .lock()
            .unwrap()
            .mailbox_for_sender(sender)
            .map(|_| ())
    }

    fn validate_mailbox_receiver(&self, receiver: &Value) -> Result<(), RuntimeError> {
        self.store
            .lock()
            .unwrap()
            .mailbox_for_receiver(receiver)
            .map(|_| ())
    }

    fn allocate_subscription(&self, sender: &Value) -> Result<Value, RuntimeError> {
        self.store.lock().unwrap().allocate_subscription(sender)
    }

    fn validate_subscription(&self, subscription: &Value) -> Result<(), RuntimeError> {
        self.store
            .lock()
            .unwrap()
            .subscription_mailbox(subscription)
            .map(|_| ())
    }
}

impl MailboxStore {
    fn new() -> Self {
        Self {
            next_mailbox_id: 1,
            next_cap_id: MAILBOX_CAP_BASE,
            caps: HashMap::new(),
            queues: HashMap::new(),
            subscription_deliveries: HashSet::new(),
        }
    }

    fn create_mailbox(&mut self) -> Result<(Value, Value), RuntimeError> {
        let mailbox = self.next_mailbox_id;
        self.next_mailbox_id += 1;
        self.queues.entry(mailbox).or_default();
        let receiver = self.allocate_cap(mailbox, MailboxCapKind::Receiver)?;
        let sender = self.allocate_cap(mailbox, MailboxCapKind::Sender)?;
        crate::metrics::metrics().mailboxes_created.inc();
        self.record_queue_metrics();
        Ok((receiver, sender))
    }

    fn close_mailbox(&mut self, receiver: &Value) -> Result<(), RuntimeError> {
        let mailbox = self.mailbox_for_receiver(receiver)?;
        self.caps.retain(|_, cap| match cap {
            RuntimeCap::Mailbox {
                mailbox: cap_mailbox,
                ..
            }
            | RuntimeCap::Subscription {
                mailbox: cap_mailbox,
            } => *cap_mailbox != mailbox,
        });
        self.queues.remove(&mailbox);
        self.subscription_deliveries.remove(&mailbox);
        self.record_queue_metrics();
        Ok(())
    }

    fn allocate_cap(&mut self, mailbox: u64, kind: MailboxCapKind) -> Result<Value, RuntimeError> {
        loop {
            let raw = self.next_cap_id;
            self.next_cap_id += 1;
            let Some(id) = CapabilityId::new(raw) else {
                return Err(RuntimeError::InvalidBuiltinCall {
                    name: Symbol::intern("mailbox"),
                    message: "mailbox capability id space exhausted".to_owned(),
                });
            };
            if self.caps.contains_key(&id) {
                continue;
            }
            self.caps.insert(id, RuntimeCap::Mailbox { mailbox, kind });
            return Ok(Value::capability(id));
        }
    }

    fn allocate_subscription(&mut self, sender: &Value) -> Result<Value, RuntimeError> {
        let mailbox = self.mailbox_for_sender(sender)?;
        loop {
            let raw = self.next_cap_id;
            self.next_cap_id += 1;
            let Some(id) = CapabilityId::new(raw) else {
                return Err(RuntimeError::InvalidBuiltinCall {
                    name: Symbol::intern("subscribe_changes"),
                    message: "subscription capability id space exhausted".to_owned(),
                });
            };
            if self.caps.contains_key(&id) {
                continue;
            }
            self.caps.insert(id, RuntimeCap::Subscription { mailbox });
            return Ok(Value::capability(id));
        }
    }

    fn mailbox_for_sender(&self, sender: &Value) -> Result<u64, RuntimeError> {
        self.mailbox_for(sender, MailboxCapKind::Sender, "send")
    }

    fn mailbox_for_receiver(&self, receiver: &Value) -> Result<u64, RuntimeError> {
        self.mailbox_for(receiver, MailboxCapKind::Receiver, "recv")
    }

    fn mailbox_for(
        &self,
        value: &Value,
        expected_kind: MailboxCapKind,
        operation: &'static str,
    ) -> Result<u64, RuntimeError> {
        let Some(id) = value.as_capability() else {
            return Err(RuntimeError::InvalidMailboxCapability {
                operation,
                capability: value.clone(),
            });
        };
        let Some(RuntimeCap::Mailbox { mailbox, kind }) = self.caps.get(&id) else {
            return Err(RuntimeError::InvalidMailboxCapability {
                operation,
                capability: value.clone(),
            });
        };
        if *kind != expected_kind {
            return Err(RuntimeError::InvalidMailboxCapability {
                operation,
                capability: value.clone(),
            });
        }
        Ok(*mailbox)
    }

    fn subscription_mailbox(&self, subscription: &Value) -> Result<u64, RuntimeError> {
        let Some(id) = subscription.as_capability() else {
            return Err(RuntimeError::InvalidMailboxCapability {
                operation: "subscription",
                capability: subscription.clone(),
            });
        };
        let Some(RuntimeCap::Subscription { mailbox }) = self.caps.get(&id) else {
            return Err(RuntimeError::InvalidMailboxCapability {
                operation: "subscription",
                capability: subscription.clone(),
            });
        };
        Ok(*mailbox)
    }

    fn release_subscription(&mut self, subscription: &Value) {
        let Some(id) = subscription.as_capability() else {
            return;
        };
        if matches!(self.caps.get(&id), Some(RuntimeCap::Subscription { .. })) {
            self.caps.remove(&id);
        }
    }

    fn drain_receiver(&mut self, receiver: Value) -> Result<Vec<Value>, RuntimeError> {
        let mailbox = self.mailbox_for_receiver(&receiver)?;
        let drained = self
            .queues
            .entry(mailbox)
            .or_default()
            .drain(..)
            .map(|entry| entry.value)
            .collect::<Vec<_>>();
        crate::metrics::metrics().mailbox_drains.inc();
        crate::metrics::metrics()
            .mailbox_messages_drained
            .add(drained.len() as isize);
        self.record_queue_metrics();
        Ok(drained)
    }

    fn deliver(&mut self, sends: &[MailboxSend]) -> Vec<u64> {
        let mut delivered = Vec::new();
        for send in sends {
            let Ok(mailbox) = self.mailbox_for_sender(&send.sender) else {
                continue;
            };
            self.queues
                .entry(mailbox)
                .or_default()
                .push_back(MailboxEntry {
                    value: send.value.clone(),
                    subscription: None,
                });
            delivered.push(mailbox);
        }
        crate::metrics::metrics()
            .mailbox_messages_delivered
            .add(delivered.len() as isize);
        self.record_queue_metrics();
        delivered
    }

    fn deliver_subscription(
        &mut self,
        subscription: &Value,
        value: Value,
        resynchronization: Value,
        queue_budget: usize,
    ) -> Result<(u64, bool), RuntimeError> {
        let mailbox = self.subscription_mailbox(subscription)?;
        let subscription_id = subscription
            .as_capability()
            .expect("validated subscription is a capability");
        let queue = self.queues.entry(mailbox).or_default();
        let queued = queue
            .iter()
            .filter(|entry| entry.subscription == Some(subscription_id))
            .count();
        let overflow = queued >= queue_budget;
        if overflow {
            queue.retain(|entry| entry.subscription != Some(subscription_id));
        }
        queue.push_back(MailboxEntry {
            value: if overflow { resynchronization } else { value },
            subscription: Some(subscription_id),
        });
        crate::metrics::metrics().mailbox_messages_delivered.inc();
        self.subscription_deliveries.insert(mailbox);
        self.record_queue_metrics();
        Ok((mailbox, overflow))
    }

    fn replace_subscription_message(
        &mut self,
        subscription: &Value,
        value: Value,
    ) -> Result<u64, RuntimeError> {
        let mailbox = self.subscription_mailbox(subscription)?;
        let subscription_id = subscription
            .as_capability()
            .expect("validated subscription is a capability");
        let queue = self.queues.entry(mailbox).or_default();
        queue.retain(|entry| entry.subscription != Some(subscription_id));
        queue.push_back(MailboxEntry {
            value,
            subscription: Some(subscription_id),
        });
        self.subscription_deliveries.insert(mailbox);
        self.record_queue_metrics();
        Ok(mailbox)
    }

    fn record_queue_metrics(&self) {
        crate::metrics::metrics()
            .mailboxes
            .set(self.queues.len() as i64);
        crate::metrics::metrics()
            .queued_mailbox_messages
            .set(self.queues.values().map(VecDeque::len).sum::<usize>() as i64);
    }
}

impl From<TaskError> for TaskManagerError {
    fn from(value: TaskError) -> Self {
        Self::Task(value)
    }
}

impl From<KernelError> for TaskManagerError {
    fn from(value: KernelError) -> Self {
        Self::Task(TaskError::from(value))
    }
}

fn validate_subscription_request(
    kernel: &RelationKernel,
    request: &SubscriptionRequest,
    authority: &AuthorityContext,
) -> Result<(), RuntimeError> {
    if request.queue_budget == 0 {
        return Err(RuntimeError::InvalidBuiltinCall {
            name: Symbol::intern("subscribe_changes"),
            message: "queue budget must be positive".to_owned(),
        });
    }
    if request.initial_delivery == SubscriptionInitialDelivery::SnapshotThenChanges
        && request.cursor.is_some()
    {
        return Err(RuntimeError::InvalidBuiltinCall {
            name: Symbol::intern("subscribe_changes"),
            message: "snapshot delivery cannot be combined with a resume cursor".to_owned(),
        });
    }
    match &request.subject {
        SubscriptionSubject::Catalogue if !authority.is_root() => {
            Err(RuntimeError::PermissionDenied {
                operation: "subscribe",
                target: Value::symbol(Symbol::intern("catalogue")),
            })
        }
        SubscriptionSubject::Catalogue => Ok(()),
        SubscriptionSubject::Facts { relation, bindings }
        | SubscriptionSubject::Relation { relation, bindings } => {
            if !authority.can_read_relation(*relation) {
                return Err(RuntimeError::PermissionDenied {
                    operation: "subscribe",
                    target: Value::identity(*relation),
                });
            }
            let arity = kernel
                .snapshot()
                .relation_metadata()
                .find(|metadata| metadata.id() == *relation)
                .map(|metadata| metadata.arity() as usize)
                .ok_or(RuntimeError::Kernel(KernelError::UnknownRelation(
                    *relation,
                )))?;
            if bindings.len() != arity {
                return Err(RuntimeError::InvalidBuiltinCall {
                    name: Symbol::intern("subscribe_changes"),
                    message: format!("bindings must contain exactly {arity} entries"),
                });
            }
            if matches!(request.subject, SubscriptionSubject::Facts { .. })
                && kernel.snapshot().relation_capabilities(*relation)?.source
                    == RelationSource::Computed
            {
                return Err(RuntimeError::InvalidBuiltinCall {
                    name: Symbol::intern("subscribe_changes"),
                    message: "fact subscriptions require a stored relation".to_owned(),
                });
            }
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Effect {
    pub task_id: TaskId,
    pub target: Identity,
    pub value: Value,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectLog {
    effects: Vec<Effect>,
}

impl EffectLog {
    pub fn emit(&mut self, task_id: TaskId, effects: Vec<Emission>) {
        self.effects
            .extend(effects.into_iter().map(|effect| Effect {
                task_id,
                target: effect.target(),
                value: effect.value().clone(),
            }));
    }

    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    pub fn len(&self) -> usize {
        self.effects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub fn drain(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }
}

pub struct TaskManager {
    kernel: RelationKernel,
    next_task_id: TaskId,
    suspended: HashMap<TaskId, SuspendedTask>,
    completed: HashMap<TaskId, TaskOutcome>,
    cancelled: HashSet<TaskId>,
    effects: EffectLog,
    mailboxes: MailboxRuntimeHandle,
    subscriptions: SubscriptionRuntimeHandle,
    limits: TaskLimits,
    resolver: Arc<ProgramResolver>,
    builtins: Arc<BuiltinRegistry>,
}

pub struct SharedTaskManager {
    kernel: Arc<RelationKernel>,
    next_task_id: AtomicU64,
    state: Mutex<SharedTaskState>,
    mailboxes: MailboxRuntimeHandle,
    subscriptions: SubscriptionRuntimeHandle,
    limits: TaskLimits,
    resolver: Arc<ProgramResolver>,
    builtins: Arc<BuiltinRegistry>,
}

#[derive(Default)]
struct SharedTaskState {
    suspended: HashMap<TaskId, SuspendedTask>,
    completed: HashSet<TaskId>,
    cancelled: HashSet<TaskId>,
    effects: EffectLog,
}

impl TaskManager {
    pub fn new(kernel: RelationKernel) -> Self {
        let mailboxes = MailboxRuntimeHandle::new();
        Self {
            kernel,
            next_task_id: 1,
            suspended: HashMap::new(),
            completed: HashMap::new(),
            cancelled: HashSet::new(),
            effects: EffectLog::default(),
            subscriptions: SubscriptionRuntimeHandle::new(mailboxes.clone()),
            mailboxes,
            limits: TaskLimits::default(),
            resolver: Arc::new(ProgramResolver::new()),
            builtins: Arc::new(BuiltinRegistry::new()),
        }
    }

    pub fn with_limits(mut self, limits: TaskLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) fn with_execution_context(mut self, execution_context: ExecutionContext) -> Self {
        self.kernel = self.kernel.with_execution_context(execution_context);
        self
    }

    pub fn with_resolver(mut self, resolver: Arc<ProgramResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    pub fn with_builtins(mut self, builtins: Arc<BuiltinRegistry>) -> Self {
        self.builtins = builtins;
        self
    }

    pub fn kernel(&self) -> &RelationKernel {
        &self.kernel
    }

    pub(crate) fn fork_with_kernel(&self, kernel: RelationKernel) -> Self {
        Self::new(kernel)
            .with_limits(self.limits)
            .with_resolver(self.resolver.clone())
            .with_builtins(self.builtins.clone())
    }

    pub(crate) fn record_completed_outcome(&mut self, outcome: TaskOutcome) -> TaskId {
        debug_assert!(matches!(outcome, TaskOutcome::Complete { .. }));
        let task_id = self.allocate_task_id();
        self.record_outcome(task_id, outcome, None);
        task_id
    }

    #[cfg(test)]
    pub(crate) fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    pub fn effects(&self) -> &EffectLog {
        &self.effects
    }

    pub fn effects_mut(&mut self) -> &mut EffectLog {
        &mut self.effects
    }

    pub fn drain_mailbox(&self, receiver: Value) -> Result<Vec<Value>, RuntimeError> {
        self.mailboxes.drain_receiver(receiver)
    }

    pub fn create_mailbox(&self) -> Result<(Value, Value), RuntimeError> {
        self.mailboxes.create_mailbox()
    }

    pub fn mailbox_for_receiver(&self, receiver: &Value) -> Result<u64, RuntimeError> {
        self.mailboxes.mailbox_for_receiver(receiver)
    }

    pub fn mailbox_for_sender(&self, sender: &Value) -> Result<u64, RuntimeError> {
        self.mailboxes.mailbox_for_sender(sender)
    }

    pub fn deliver_mailbox(&self, sender: Value, value: Value) -> Result<u64, RuntimeError> {
        self.mailboxes
            .deliver(&[MailboxSend { sender, value }])
            .into_iter()
            .next()
            .ok_or_else(|| RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("mailbox_send"),
                message: "invalid external mailbox sender".to_owned(),
            })
    }

    pub fn take_subscription_deliveries(&self) -> Vec<u64> {
        self.mailboxes.take_subscription_deliveries()
    }

    pub fn dispatch_subscriptions(&self) -> Result<Vec<u64>, RuntimeError> {
        let mut operations = Vec::new();
        self.kernel.at_publication_boundary(|snapshot| {
            self.subscriptions
                .apply_boundary(&self.kernel, snapshot, &mut operations)
        })
    }

    pub fn register_subscription(
        &self,
        request: SubscriptionRequest,
        runtime_context: RuntimeContext,
        authority: &AuthorityContext,
    ) -> Result<Value, RuntimeError> {
        if !authority.is_root()
            && runtime_context.actor().is_none()
            && runtime_context.principal().is_none()
        {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("subscribe_changes"),
                message: "non-root subscriptions require a refreshable actor or principal context"
                    .to_owned(),
            });
        }
        validate_subscription_request(&self.kernel, &request, authority)?;
        let subscription = self.mailboxes.allocate_subscription(&request.sender)?;
        let mut operations = vec![SubscriptionOperation::Register {
            subscription: subscription.clone(),
            request,
            runtime_context,
            root_authority: authority.is_root(),
        }];
        self.kernel.at_publication_boundary(|snapshot| {
            self.subscriptions
                .apply_boundary(&self.kernel, snapshot, &mut operations)
        })?;
        Ok(subscription)
    }

    pub fn cancel_subscription(&self, subscription: Value) -> Result<(), RuntimeError> {
        self.mailboxes.validate_subscription(&subscription)?;
        let mut operations = vec![SubscriptionOperation::Cancel { subscription }];
        self.kernel.at_publication_boundary(|snapshot| {
            self.subscriptions
                .apply_boundary(&self.kernel, snapshot, &mut operations)
        })?;
        Ok(())
    }

    pub fn drain_emissions(&mut self) -> Vec<Effect> {
        let effects = self.effects.drain();
        crate::metrics::metrics().queued_effects.set(0);
        effects
    }

    pub fn drain_routed_emissions(&mut self) -> Vec<Effect> {
        let effects = self.effects.drain();
        crate::metrics::metrics().queued_effects.set(0);
        effects
            .into_iter()
            .flat_map(|effect| {
                self.route_effect_targets(effect.target)
                    .into_iter()
                    .map(move |target| Effect {
                        task_id: effect.task_id,
                        target,
                        value: effect.value.clone(),
                    })
            })
            .collect()
    }

    pub fn open_endpoint(
        &mut self,
        endpoint: Identity,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), TaskManagerError> {
        self.open_endpoint_with_context(endpoint, None, actor, protocol)
    }

    pub fn open_endpoint_with_context(
        &mut self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), TaskManagerError> {
        open_endpoint_in(&self.kernel, endpoint, principal, actor, protocol)?;
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Open);
        crate::metrics::endpoint_opened();
        Ok(())
    }

    pub fn open_endpoint_with_context_and_rows(
        &mut self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
        rows: Vec<(RelationId, Tuple)>,
    ) -> Result<usize, TaskManagerError> {
        let changes =
            open_endpoint_with_rows_in(&self.kernel, endpoint, principal, actor, protocol, &rows)?;
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Open);
        crate::metrics::endpoint_opened();
        Ok(changes)
    }

    pub fn endpoint_runtime_context(
        &self,
        endpoint: Identity,
    ) -> Result<RuntimeContext, TaskManagerError> {
        let snapshot = self.kernel.snapshot();
        let principal =
            endpoint_binding_in(snapshot.as_ref(), endpoint, endpoint_principal_relation())?;
        let actor = endpoint_binding_in(snapshot.as_ref(), endpoint, endpoint_actor_relation())?;
        Ok(RuntimeContext::new(principal, actor, endpoint))
    }

    pub fn close_endpoint(&mut self, endpoint: Identity) -> usize {
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Close);
        let removed = close_endpoint_in(&self.kernel, endpoint);
        if removed > 0 {
            crate::metrics::endpoint_closed();
        }
        removed
    }

    pub fn close_endpoint_with_rows(
        &mut self,
        endpoint: Identity,
        rows: Vec<(RelationId, Tuple)>,
    ) -> Result<usize, TaskManagerError> {
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Close);
        let (changes, endpoint_rows) = close_endpoint_with_rows_in(&self.kernel, endpoint, &rows)?;
        if endpoint_rows > 0 {
            crate::metrics::endpoint_closed();
        }
        Ok(changes)
    }

    pub fn route_effect_targets(&self, target: Identity) -> Vec<Identity> {
        self.route_effect_targets_result(target)
            .unwrap_or_else(|_| vec![target])
    }

    pub fn resolver(&self) -> &Arc<ProgramResolver> {
        &self.resolver
    }

    pub fn builtins(&self) -> &Arc<BuiltinRegistry> {
        &self.builtins
    }

    pub fn into_shared(self) -> SharedTaskManager {
        SharedTaskManager {
            kernel: Arc::new(self.kernel),
            next_task_id: AtomicU64::new(self.next_task_id),
            state: Mutex::new(SharedTaskState {
                suspended: self.suspended,
                completed: self.completed.into_keys().collect(),
                cancelled: self.cancelled,
                effects: self.effects,
            }),
            mailboxes: self.mailboxes,
            subscriptions: self.subscriptions,
            limits: self.limits,
            resolver: self.resolver,
            builtins: self.builtins,
        }
    }

    pub fn submit(
        &mut self,
        program: Arc<Program>,
    ) -> Result<(TaskId, TaskOutcome), TaskManagerError> {
        self.submit_with_authority(program, AuthorityContext::root())
    }

    pub fn submit_with_authority(
        &mut self,
        program: Arc<Program>,
        authority: AuthorityContext,
    ) -> Result<(TaskId, TaskOutcome), TaskManagerError> {
        self.submit_with_context(program, authority, RuntimeContext::default())
    }

    pub fn submit_with_context(
        &mut self,
        program: Arc<Program>,
        authority: AuthorityContext,
        runtime_context: RuntimeContext,
    ) -> Result<(TaskId, TaskOutcome), TaskManagerError> {
        self.submit_with_context_and_limits(program, authority, runtime_context, self.limits)
    }

    pub fn submit_with_context_and_limits(
        &mut self,
        program: Arc<Program>,
        authority: AuthorityContext,
        runtime_context: RuntimeContext,
        limits: TaskLimits,
    ) -> Result<(TaskId, TaskOutcome), TaskManagerError> {
        let task_id = self.allocate_task_id();
        let task_snapshot = self.task_snapshot_values(Some(task_id));
        let mut task = Task::new_with_authority(
            task_id,
            &self.kernel,
            program,
            self.resolver.clone(),
            self.builtins.clone(),
            authority,
            limits,
        );
        task.set_task_snapshot(task_snapshot);
        task.set_runtime_context(runtime_context);
        task.set_mailbox_runtime(self.mailboxes.clone());
        task.set_subscription_runtime(self.subscriptions.clone());
        let start = Instant::now();
        let result = task.run();
        crate::metrics::record_task_result(
            crate::metrics::TaskOperation::Submit,
            start.elapsed(),
            &result,
        );
        let outcome = result?;
        let suspended_state = suspended_state(&outcome, &task);
        drop(task);
        self.record_outcome(task_id, outcome.clone(), suspended_state);
        Ok((task_id, outcome))
    }

    pub fn complete_immediate(&mut self, value: Value) -> (TaskId, TaskOutcome) {
        if let Err(error) = self.dispatch_subscriptions() {
            tracing::error!(?error, "failed to dispatch settled subscription batches");
        }
        let task_id = self.allocate_task_id();
        let outcome = TaskOutcome::Complete {
            value,
            effects: Vec::new(),
            mailbox_sends: Vec::new(),
            retries: 0,
        };
        self.record_outcome(task_id, outcome.clone(), None);
        crate::metrics::metrics()
            .task_operations
            .inc(crate::metrics::TaskOperation::Immediate);
        crate::metrics::metrics()
            .task_outcomes
            .inc(crate::metrics::RuntimeTaskOutcome::Complete);
        (task_id, outcome)
    }

    pub fn resume_with_authority(
        &mut self,
        task_id: TaskId,
        authority: AuthorityContext,
    ) -> Result<TaskOutcome, TaskManagerError> {
        self.resume_with_value(task_id, authority, Value::nothing())
    }

    pub fn resume_with_value(
        &mut self,
        task_id: TaskId,
        authority: AuthorityContext,
        value: Value,
    ) -> Result<TaskOutcome, TaskManagerError> {
        self.resume_with_context(task_id, authority, value, RuntimeContext::default())
    }

    pub fn resume_with_context(
        &mut self,
        task_id: TaskId,
        authority: AuthorityContext,
        value: Value,
        runtime_context: RuntimeContext,
    ) -> Result<TaskOutcome, TaskManagerError> {
        if self.cancelled.contains(&task_id) {
            return Err(TaskManagerError::TaskCancelled(task_id));
        }
        if self.completed.contains_key(&task_id) {
            return Err(TaskManagerError::TaskAlreadyCompleted(task_id));
        }
        let suspended = self
            .suspended
            .remove(&task_id)
            .ok_or(TaskManagerError::UnknownTask(task_id))?;
        let task_snapshot = self.task_snapshot_values(Some(task_id));
        let mut task = Task::from_state_with_authority(
            task_id,
            &self.kernel,
            self.resolver.clone(),
            self.builtins.clone(),
            suspended.state,
            authority,
        );
        task.set_task_snapshot(task_snapshot);
        task.set_runtime_context(runtime_context);
        task.set_mailbox_runtime(self.mailboxes.clone());
        task.set_subscription_runtime(self.subscriptions.clone());
        task.resume_with(value)?;
        let start = Instant::now();
        let result = task.run();
        crate::metrics::record_task_result(
            crate::metrics::TaskOperation::Resume,
            start.elapsed(),
            &result,
        );
        let outcome = result?;
        let suspended_state = suspended_state(&outcome, &task);
        drop(task);
        self.record_outcome(task_id, outcome.clone(), suspended_state);
        Ok(outcome)
    }

    pub fn suspended(&self, task_id: TaskId) -> Option<&SuspendedTask> {
        self.suspended.get(&task_id)
    }

    pub fn cancel_task(&mut self, task_id: TaskId) -> Result<SuspendKind, TaskManagerError> {
        if self.cancelled.contains(&task_id) {
            return Err(TaskManagerError::TaskCancelled(task_id));
        }
        if self.completed.contains_key(&task_id) {
            return Err(TaskManagerError::TaskAlreadyCompleted(task_id));
        }
        let suspended = self
            .suspended
            .remove(&task_id)
            .ok_or(TaskManagerError::UnknownTask(task_id))?;
        self.cancelled.insert(task_id);
        crate::metrics::metrics()
            .suspended_tasks
            .set(self.suspended.len() as i64);
        crate::metrics::metrics()
            .cancelled_tasks
            .set(self.cancelled.len() as i64);
        Ok(suspended.kind)
    }

    pub fn completed(&self, task_id: TaskId) -> Option<&TaskOutcome> {
        self.completed.get(&task_id)
    }

    pub fn suspended_len(&self) -> usize {
        self.suspended.len()
    }

    pub fn completed_len(&self) -> usize {
        self.completed.len()
    }

    pub fn cancelled_len(&self) -> usize {
        self.cancelled.len()
    }

    fn allocate_task_id(&mut self) -> TaskId {
        let task_id = self.next_task_id;
        self.next_task_id += 1;
        task_id
    }

    fn task_snapshot_values(&self, running: Option<TaskId>) -> Vec<Value> {
        let mut tasks = self
            .suspended
            .values()
            .map(|task| task_status_value(task.task_id, Symbol::intern("suspended")))
            .collect::<Vec<_>>();
        if let Some(task_id) = running {
            tasks.push(task_status_value(task_id, Symbol::intern("running")));
        }
        tasks.sort();
        tasks
    }

    fn record_outcome(
        &mut self,
        task_id: TaskId,
        outcome: TaskOutcome,
        suspended_state: Option<crate::task::TaskState>,
    ) {
        crate::metrics::record_outcome_side_effects(&outcome);
        match &outcome {
            TaskOutcome::Complete {
                effects,
                mailbox_sends,
                ..
            }
            | TaskOutcome::Aborted {
                effects,
                mailbox_sends,
                ..
            } => {
                self.effects.emit(task_id, effects.clone());
                crate::metrics::metrics()
                    .queued_effects
                    .set(self.effects.len() as i64);
                self.mailboxes.deliver(mailbox_sends);
                self.completed.insert(task_id, outcome);
            }
            TaskOutcome::Suspended {
                kind,
                effects,
                mailbox_sends,
                ..
            } => {
                self.effects.emit(task_id, effects.clone());
                crate::metrics::metrics()
                    .queued_effects
                    .set(self.effects.len() as i64);
                self.mailboxes.deliver(mailbox_sends);
                self.suspended.insert(
                    task_id,
                    SuspendedTask {
                        task_id,
                        kind: kind.clone(),
                        state: suspended_state.expect("suspended task state is present"),
                    },
                );
            }
        }
        crate::metrics::metrics()
            .suspended_tasks
            .set(self.suspended.len() as i64);
        crate::metrics::metrics()
            .completed_tasks
            .set(self.completed.len() as i64);
    }

    fn route_effect_targets_result(&self, target: Identity) -> Result<Vec<Identity>, KernelError> {
        let snapshot = self.kernel.snapshot();
        route_effect_targets_in(snapshot.as_ref(), target)
    }
}

impl SharedTaskManager {
    pub fn kernel(&self) -> &RelationKernel {
        &self.kernel
    }

    pub fn builtins(&self) -> &Arc<BuiltinRegistry> {
        &self.builtins
    }

    pub fn submit_with_context(
        &self,
        program: Arc<Program>,
        authority: AuthorityContext,
        runtime_context: RuntimeContext,
    ) -> Result<(TaskId, TaskOutcome), TaskManagerError> {
        self.submit_with_context_and_limits(program, authority, runtime_context, self.limits)
    }

    pub fn submit_with_context_and_limits(
        &self,
        program: Arc<Program>,
        authority: AuthorityContext,
        runtime_context: RuntimeContext,
        limits: TaskLimits,
    ) -> Result<(TaskId, TaskOutcome), TaskManagerError> {
        let task_id = self.allocate_task_id();
        let task_snapshot = self.task_snapshot_values(Some(task_id));
        let mut task = Task::new_with_authority(
            task_id,
            &self.kernel,
            program,
            self.resolver.clone(),
            self.builtins.clone(),
            authority,
            limits,
        );
        task.set_task_snapshot(task_snapshot);
        task.set_runtime_context(runtime_context);
        task.set_mailbox_runtime(self.mailboxes.clone());
        task.set_subscription_runtime(self.subscriptions.clone());
        let start = Instant::now();
        let result = task.run();
        crate::metrics::record_task_result(
            crate::metrics::TaskOperation::Submit,
            start.elapsed(),
            &result,
        );
        let outcome = result?;
        let suspended_state = suspended_state(&outcome, &task);
        drop(task);
        self.record_outcome(task_id, outcome.clone(), suspended_state);
        Ok((task_id, outcome))
    }

    pub fn resume_with_context(
        &self,
        task_id: TaskId,
        authority: AuthorityContext,
        value: Value,
        runtime_context: RuntimeContext,
    ) -> Result<TaskOutcome, TaskManagerError> {
        let (suspended, task_snapshot) = {
            let mut state = self.state.lock().unwrap();
            if state.cancelled.contains(&task_id) {
                return Err(TaskManagerError::TaskCancelled(task_id));
            }
            if state.completed.contains(&task_id) {
                return Err(TaskManagerError::TaskAlreadyCompleted(task_id));
            }
            let suspended = state
                .suspended
                .remove(&task_id)
                .ok_or(TaskManagerError::UnknownTask(task_id))?;
            let mut tasks = state
                .suspended
                .values()
                .map(|task| task_status_value(task.task_id, Symbol::intern("suspended")))
                .collect::<Vec<_>>();
            tasks.push(task_status_value(task_id, Symbol::intern("running")));
            tasks.sort();
            (suspended, tasks)
        };

        let mut task = Task::from_state_with_authority(
            task_id,
            self.kernel.as_ref(),
            self.resolver.clone(),
            self.builtins.clone(),
            suspended.state,
            authority,
        );
        task.set_task_snapshot(task_snapshot);
        task.set_runtime_context(runtime_context);
        task.set_mailbox_runtime(self.mailboxes.clone());
        task.set_subscription_runtime(self.subscriptions.clone());
        task.resume_with(value)?;
        let start = Instant::now();
        let result = task.run();
        crate::metrics::record_task_result(
            crate::metrics::TaskOperation::Resume,
            start.elapsed(),
            &result,
        );
        let outcome = result?;
        let suspended_state = suspended_state(&outcome, &task);
        drop(task);
        self.record_outcome(task_id, outcome.clone(), suspended_state);
        Ok(outcome)
    }

    pub fn drain_emissions(&self) -> Vec<Effect> {
        let effects = self.state.lock().unwrap().effects.drain();
        crate::metrics::metrics().queued_effects.set(0);
        effects
    }

    pub fn drain_mailbox(&self, receiver: Value) -> Result<Vec<Value>, RuntimeError> {
        self.mailboxes.drain_receiver(receiver)
    }

    pub fn create_mailbox(&self) -> Result<(Value, Value), RuntimeError> {
        self.mailboxes.create_mailbox()
    }

    pub fn mailbox_for_receiver(&self, receiver: &Value) -> Result<u64, RuntimeError> {
        self.mailboxes.mailbox_for_receiver(receiver)
    }

    pub fn mailbox_for_sender(&self, sender: &Value) -> Result<u64, RuntimeError> {
        self.mailboxes.mailbox_for_sender(sender)
    }

    pub fn deliver_mailbox(&self, sender: Value, value: Value) -> Result<u64, RuntimeError> {
        self.mailboxes
            .deliver(&[MailboxSend { sender, value }])
            .into_iter()
            .next()
            .ok_or_else(|| RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("mailbox_send"),
                message: "invalid external mailbox sender".to_owned(),
            })
    }

    pub fn take_subscription_deliveries(&self) -> Vec<u64> {
        self.mailboxes.take_subscription_deliveries()
    }

    pub fn register_subscription(
        &self,
        request: SubscriptionRequest,
        runtime_context: RuntimeContext,
        authority: &AuthorityContext,
    ) -> Result<Value, RuntimeError> {
        if !authority.is_root()
            && runtime_context.actor().is_none()
            && runtime_context.principal().is_none()
        {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("subscribe_changes"),
                message: "non-root subscriptions require a refreshable actor or principal context"
                    .to_owned(),
            });
        }
        validate_subscription_request(&self.kernel, &request, authority)?;
        let subscription = self.mailboxes.allocate_subscription(&request.sender)?;
        let mut operations = vec![SubscriptionOperation::Register {
            subscription: subscription.clone(),
            request,
            runtime_context,
            root_authority: authority.is_root(),
        }];
        self.kernel.at_publication_boundary(|snapshot| {
            self.subscriptions
                .apply_boundary(&self.kernel, snapshot, &mut operations)
        })?;
        Ok(subscription)
    }

    pub fn cancel_subscription(&self, subscription: Value) -> Result<(), RuntimeError> {
        self.mailboxes.validate_subscription(&subscription)?;
        let mut operations = vec![SubscriptionOperation::Cancel { subscription }];
        self.kernel.at_publication_boundary(|snapshot| {
            self.subscriptions
                .apply_boundary(&self.kernel, snapshot, &mut operations)
        })?;
        Ok(())
    }

    pub fn drain_routed_emissions(&self) -> Vec<Effect> {
        let effects = self.state.lock().unwrap().effects.drain();
        crate::metrics::metrics().queued_effects.set(0);
        effects
            .into_iter()
            .flat_map(|effect| {
                self.route_effect_targets(effect.target)
                    .into_iter()
                    .map(move |target| Effect {
                        task_id: effect.task_id,
                        target,
                        value: effect.value.clone(),
                    })
            })
            .collect()
    }

    pub fn open_endpoint(
        &self,
        endpoint: Identity,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), TaskManagerError> {
        self.open_endpoint_with_context(endpoint, None, actor, protocol)
    }

    pub fn open_endpoint_with_context(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), TaskManagerError> {
        open_endpoint_in(&self.kernel, endpoint, principal, actor, protocol)?;
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Open);
        crate::metrics::endpoint_opened();
        Ok(())
    }

    pub fn open_endpoint_with_context_and_rows(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
        rows: Vec<(RelationId, Tuple)>,
    ) -> Result<usize, TaskManagerError> {
        let changes =
            open_endpoint_with_rows_in(&self.kernel, endpoint, principal, actor, protocol, &rows)?;
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Open);
        crate::metrics::endpoint_opened();
        Ok(changes)
    }

    pub fn endpoint_runtime_context(
        &self,
        endpoint: Identity,
    ) -> Result<RuntimeContext, TaskManagerError> {
        let snapshot = self.kernel.snapshot();
        let principal =
            endpoint_binding_in(snapshot.as_ref(), endpoint, endpoint_principal_relation())?;
        let actor = endpoint_binding_in(snapshot.as_ref(), endpoint, endpoint_actor_relation())?;
        Ok(RuntimeContext::new(principal, actor, endpoint))
    }

    pub fn close_endpoint(&self, endpoint: Identity) -> usize {
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Close);
        let removed = close_endpoint_in(&self.kernel, endpoint);
        if removed > 0 {
            crate::metrics::endpoint_closed();
        }
        removed
    }

    pub fn close_endpoint_with_rows(
        &self,
        endpoint: Identity,
        rows: Vec<(RelationId, Tuple)>,
    ) -> Result<usize, TaskManagerError> {
        crate::metrics::metrics()
            .endpoint_operations
            .inc(crate::metrics::EndpointOperation::Close);
        let (changes, endpoint_rows) = close_endpoint_with_rows_in(&self.kernel, endpoint, &rows)?;
        if endpoint_rows > 0 {
            crate::metrics::endpoint_closed();
        }
        Ok(changes)
    }

    pub fn route_effect_targets(&self, target: Identity) -> Vec<Identity> {
        self.route_effect_targets_result(target)
            .unwrap_or_else(|_| vec![target])
    }

    pub fn completed_len(&self) -> usize {
        self.state.lock().unwrap().completed.len()
    }

    pub fn cancel_task(&self, task_id: TaskId) -> Result<SuspendKind, TaskManagerError> {
        let mut state = self.state.lock().unwrap();
        if state.cancelled.contains(&task_id) {
            return Err(TaskManagerError::TaskCancelled(task_id));
        }
        if state.completed.contains(&task_id) {
            return Err(TaskManagerError::TaskAlreadyCompleted(task_id));
        }
        let suspended = state
            .suspended
            .remove(&task_id)
            .ok_or(TaskManagerError::UnknownTask(task_id))?;
        state.cancelled.insert(task_id);
        crate::metrics::metrics()
            .suspended_tasks
            .set(state.suspended.len() as i64);
        crate::metrics::metrics()
            .cancelled_tasks
            .set(state.cancelled.len() as i64);
        Ok(suspended.kind)
    }

    pub fn suspended_len(&self) -> usize {
        self.state.lock().unwrap().suspended.len()
    }

    pub fn cancelled_len(&self) -> usize {
        self.state.lock().unwrap().cancelled.len()
    }

    fn allocate_task_id(&self) -> TaskId {
        self.next_task_id.fetch_add(1, Ordering::Relaxed)
    }

    fn task_snapshot_values(&self, running: Option<TaskId>) -> Vec<Value> {
        let mut tasks = {
            let state = self.state.lock().unwrap();
            state
                .suspended
                .values()
                .map(|task| task_status_value(task.task_id, Symbol::intern("suspended")))
                .collect::<Vec<_>>()
        };
        if let Some(task_id) = running {
            tasks.push(task_status_value(task_id, Symbol::intern("running")));
        }
        tasks.sort();
        tasks
    }

    fn record_outcome(
        &self,
        task_id: TaskId,
        outcome: TaskOutcome,
        suspended_state: Option<crate::task::TaskState>,
    ) {
        crate::metrics::record_outcome_side_effects(&outcome);
        let mut state = self.state.lock().unwrap();
        match &outcome {
            TaskOutcome::Complete {
                effects,
                mailbox_sends,
                ..
            }
            | TaskOutcome::Aborted {
                effects,
                mailbox_sends,
                ..
            } => {
                state.effects.emit(task_id, effects.clone());
                crate::metrics::metrics()
                    .queued_effects
                    .set(state.effects.len() as i64);
                self.mailboxes.deliver(mailbox_sends);
                state.completed.insert(task_id);
            }
            TaskOutcome::Suspended {
                kind,
                effects,
                mailbox_sends,
                ..
            } => {
                state.effects.emit(task_id, effects.clone());
                crate::metrics::metrics()
                    .queued_effects
                    .set(state.effects.len() as i64);
                self.mailboxes.deliver(mailbox_sends);
                state.suspended.insert(
                    task_id,
                    SuspendedTask {
                        task_id,
                        kind: kind.clone(),
                        state: suspended_state.expect("suspended task state is present"),
                    },
                );
            }
        }
        crate::metrics::metrics()
            .suspended_tasks
            .set(state.suspended.len() as i64);
        crate::metrics::metrics()
            .completed_tasks
            .set(state.completed.len() as i64);
    }

    fn route_effect_targets_result(&self, target: Identity) -> Result<Vec<Identity>, KernelError> {
        let snapshot = self.kernel.snapshot();
        route_effect_targets_in(snapshot.as_ref(), target)
    }
}

fn suspended_state(outcome: &TaskOutcome, task: &Task<'_>) -> Option<crate::task::TaskState> {
    if matches!(outcome, TaskOutcome::Suspended { .. }) {
        Some(task.checkpoint())
    } else {
        None
    }
}

fn open_endpoint_in(
    kernel: &RelationKernel,
    endpoint: Identity,
    principal: Option<Identity>,
    actor: Option<Identity>,
    protocol: Symbol,
) -> Result<(), TaskManagerError> {
    open_endpoint_with_rows_in(kernel, endpoint, principal, actor, protocol, &[]).map(|_| ())
}

fn open_endpoint_with_rows_in(
    kernel: &RelationKernel,
    endpoint: Identity,
    principal: Option<Identity>,
    actor: Option<Identity>,
    protocol: Symbol,
    rows: &[(RelationId, Tuple)],
) -> Result<usize, TaskManagerError> {
    let mut transaction = kernel.begin();
    if !transaction
        .scan(endpoint_open_relation(), &[Some(Value::identity(endpoint))])?
        .is_empty()
    {
        return Err(TaskManagerError::Task(TaskError::Runtime(
            RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("open_endpoint"),
                message: "endpoint is already open".to_owned(),
            },
        )));
    }
    transaction.assert(
        endpoint_relation(),
        Tuple::from([Value::identity(endpoint)]),
    )?;
    if let Some(principal) = principal {
        transaction.assert(
            endpoint_principal_relation(),
            Tuple::from([Value::identity(endpoint), Value::identity(principal)]),
        )?;
    }
    if let Some(actor) = actor {
        transaction.assert(
            endpoint_actor_relation(),
            Tuple::from([Value::identity(endpoint), Value::identity(actor)]),
        )?;
    }
    transaction.assert(
        endpoint_protocol_relation(),
        Tuple::from([Value::identity(endpoint), Value::symbol(protocol)]),
    )?;
    transaction.assert(
        endpoint_open_relation(),
        Tuple::from([Value::identity(endpoint)]),
    )?;
    for (relation, tuple) in rows {
        transaction.assert(*relation, tuple.clone())?;
    }
    let result = transaction.commit()?;
    Ok(result.commit().changes().len())
}

fn close_endpoint_in(kernel: &RelationKernel, endpoint: Identity) -> usize {
    loop {
        let mut transaction = kernel.begin();
        let removed = retract_endpoint_rows(&mut transaction, endpoint)
            .expect("runtime endpoint relations must remain readable");
        if removed == 0 {
            return 0;
        }
        match transaction.commit() {
            Ok(_) => return removed,
            Err(KernelError::Conflict(_)) => continue,
            Err(error) => panic!("endpoint close transaction failed: {error:?}"),
        }
    }
}

fn close_endpoint_with_rows_in(
    kernel: &RelationKernel,
    endpoint: Identity,
    rows: &[(RelationId, Tuple)],
) -> Result<(usize, usize), TaskManagerError> {
    loop {
        let mut transaction = kernel.begin();
        for (relation, tuple) in rows {
            transaction.retract(*relation, tuple.clone())?;
        }
        let endpoint_rows = retract_endpoint_rows(&mut transaction, endpoint)?;
        match transaction.commit() {
            Ok(result) => return Ok((result.commit().changes().len(), endpoint_rows)),
            Err(KernelError::Conflict(_)) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn retract_endpoint_rows(
    transaction: &mut Transaction<'_>,
    endpoint: Identity,
) -> Result<usize, KernelError> {
    let endpoint = Value::identity(endpoint);
    if transaction
        .scan(endpoint_open_relation(), &[Some(endpoint.clone())])?
        .is_empty()
    {
        return Ok(0);
    }
    let mut rows = vec![
        (endpoint_relation(), Tuple::from([endpoint.clone()])),
        (endpoint_open_relation(), Tuple::from([endpoint.clone()])),
    ];
    for relation in [
        endpoint_principal_relation(),
        endpoint_actor_relation(),
        endpoint_protocol_relation(),
    ] {
        rows.extend(
            transaction
                .scan(relation, &[Some(endpoint.clone()), None])?
                .into_iter()
                .map(|tuple| (relation, tuple)),
        );
    }
    let removed = rows.len();
    for (relation, tuple) in rows {
        transaction.retract(relation, tuple)?;
    }
    Ok(removed)
}

fn endpoint_binding_in(
    relations: &impl RelationRead,
    endpoint: Identity,
    relation: RelationId,
) -> Result<Option<Identity>, TaskManagerError> {
    let rows = relations.scan_relation(relation, &[Some(Value::identity(endpoint)), None])?;
    let mut bindings = rows
        .iter()
        .filter_map(|row| row.values().get(1).and_then(Value::as_identity))
        .collect::<Vec<_>>();
    bindings.sort();
    bindings.dedup();
    match bindings.as_slice() {
        [] => Ok(None),
        [identity] => Ok(Some(*identity)),
        _ => Err(TaskManagerError::Task(TaskError::Runtime(
            mica_vm::RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("endpoint_context"),
                message: "endpoint has multiple context bindings".to_owned(),
            },
        ))),
    }
}

fn route_effect_targets_in(
    relations: &impl RelationRead,
    target: Identity,
) -> Result<Vec<Identity>, KernelError> {
    let mut endpoints = BTreeSet::new();
    for row in relations.scan_relation(
        endpoint_actor_relation(),
        &[None, Some(Value::identity(target))],
    )? {
        let Some(endpoint) = row.values().first().and_then(Value::as_identity) else {
            continue;
        };
        if endpoint_is_open_in(relations, endpoint)? {
            endpoints.insert(endpoint);
        }
    }
    if endpoint_is_open_in(relations, target)? {
        endpoints.insert(target);
    }
    if endpoints.is_empty() {
        Ok(vec![target])
    } else {
        Ok(endpoints.into_iter().collect())
    }
}

fn endpoint_is_open_in(
    relations: &impl RelationRead,
    endpoint: Identity,
) -> Result<bool, KernelError> {
    Ok(!relations
        .scan_relation(endpoint_open_relation(), &[Some(Value::identity(endpoint))])?
        .is_empty())
}

fn task_status_value(task_id: TaskId, state: Symbol) -> Value {
    let task_id = i64::try_from(task_id)
        .ok()
        .and_then(|task_id| Value::int(task_id).ok())
        .unwrap_or_else(|| Value::string(task_id.to_string()));
    Value::map([
        (Value::symbol(Symbol::intern("id")), task_id),
        (Value::symbol(Symbol::intern("state")), Value::symbol(state)),
    ])
}

pub struct SuspendedTask {
    task_id: TaskId,
    kind: SuspendKind,
    state: crate::task::TaskState,
}

impl SuspendedTask {
    pub fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub fn kind(&self) -> &SuspendKind {
        &self.kind
    }

    pub fn frame_count(&self) -> usize {
        self.state.frame_count()
    }
}
