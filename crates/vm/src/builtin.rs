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

use crate::{AuthorityContext, CapabilityGrant, Emission, MailboxSend, RuntimeError, TypeContract};
use mica_relation_kernel::{RelationId, RelationKernel, RelationWorkspace, Transaction, Tuple};
use mica_var::{Identity, Symbol, Value, ValueKind};
use std::collections::BTreeMap;
use std::sync::Arc;

const SYSTEM_ENDPOINT_ID: u64 = 0x00ef_0000_0000_0000;

pub const SYSTEM_ENDPOINT: Identity = match Identity::new(SYSTEM_ENDPOINT_ID) {
    Some(identity) => identity,
    None => panic!("system endpoint id is outside the identity payload range"),
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeContext {
    principal: Option<Identity>,
    actor: Option<Identity>,
    endpoint: Identity,
}

impl RuntimeContext {
    pub fn new(principal: Option<Identity>, actor: Option<Identity>, endpoint: Identity) -> Self {
        Self {
            principal,
            actor,
            endpoint,
        }
    }

    pub fn principal(&self) -> Option<Identity> {
        self.principal
    }

    pub fn actor(&self) -> Option<Identity> {
        self.actor
    }

    pub fn endpoint(&self) -> Identity {
        self.endpoint
    }
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self::new(None, None, SYSTEM_ENDPOINT)
    }
}

pub struct BuiltinContext<'ctx, 'kernel> {
    kernel: &'kernel RelationKernel,
    tx: &'ctx mut Transaction<'kernel>,
    authority: &'ctx mut AuthorityContext,
    ports: RuntimePorts<'ctx>,
    task_snapshot: &'ctx [Value],
    runtime_context: RuntimeContext,
}

pub struct RuntimePorts<'ctx> {
    pub pending_effects: &'ctx mut Vec<Emission>,
    pub pending_mailbox_sends: &'ctx mut Vec<MailboxSend>,
    pub pending_subscriptions: &'ctx mut Vec<SubscriptionOperation>,
    pub mailbox_runtime: Option<&'ctx dyn MailboxRuntime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionSubject {
    Catalogue,
    Facts {
        relation: RelationId,
        bindings: Vec<Option<Value>>,
    },
    Relation {
        relation: RelationId,
        bindings: Vec<Option<Value>>,
    },
}

impl SubscriptionSubject {
    pub fn relation(&self) -> Option<RelationId> {
        match self {
            Self::Catalogue => None,
            Self::Facts { relation, .. } | Self::Relation { relation, .. } => Some(*relation),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionInitialDelivery {
    ChangesOnly,
    SnapshotThenChanges,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionRequest {
    pub sender: Value,
    pub subject: SubscriptionSubject,
    pub initial_delivery: SubscriptionInitialDelivery,
    pub cursor: Option<u64>,
    pub queue_budget: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionOperation {
    Register {
        subscription: Value,
        request: SubscriptionRequest,
        runtime_context: RuntimeContext,
        root_authority: bool,
    },
    Cancel {
        subscription: Value,
    },
}

pub trait MailboxRuntime {
    fn create_mailbox(&self) -> Result<(Value, Value), RuntimeError>;

    fn close_mailbox(&self, receiver: &Value) -> Result<(), RuntimeError>;

    fn validate_mailbox_sender(&self, sender: &Value) -> Result<(), RuntimeError>;

    fn validate_mailbox_receiver(&self, receiver: &Value) -> Result<(), RuntimeError>;

    fn allocate_subscription(&self, sender: &Value) -> Result<Value, RuntimeError>;

    fn validate_subscription(&self, subscription: &Value) -> Result<(), RuntimeError>;
}

impl<'ctx, 'kernel> BuiltinContext<'ctx, 'kernel> {
    pub(crate) fn new(
        kernel: &'kernel RelationKernel,
        tx: &'ctx mut Transaction<'kernel>,
        authority: &'ctx mut AuthorityContext,
        ports: RuntimePorts<'ctx>,
        task_snapshot: &'ctx [Value],
        runtime_context: RuntimeContext,
    ) -> Self {
        Self {
            kernel,
            tx,
            authority,
            ports,
            task_snapshot,
            runtime_context,
        }
    }

    pub fn kernel(&self) -> &'kernel RelationKernel {
        self.kernel
    }

    pub fn tx(&mut self) -> &mut Transaction<'kernel> {
        self.tx
    }

    pub fn authority(&self) -> &AuthorityContext {
        self.authority
    }

    pub fn authority_mut(&mut self) -> &mut AuthorityContext {
        self.authority
    }

    pub fn task_snapshot(&self) -> &[Value] {
        self.task_snapshot
    }

    pub fn runtime_context(&self) -> RuntimeContext {
        self.runtime_context
    }

    pub fn mint_capability(&mut self, grant: CapabilityGrant) -> Value {
        self.authority.mint(grant)
    }

    pub fn emit(&mut self, target: Identity, value: Value) -> Result<(), RuntimeError> {
        if !self.authority.can_effect() {
            return Err(RuntimeError::PermissionDenied {
                operation: "effect",
                target: Value::identity(target),
            });
        }
        self.ports
            .pending_effects
            .push(Emission::new(target, value));
        Ok(())
    }

    pub fn create_mailbox(&mut self) -> Result<(Value, Value), RuntimeError> {
        let Some(mailbox_runtime) = self.ports.mailbox_runtime.as_ref() else {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("mailbox"),
                message: "mailbox runtime is not available".to_owned(),
            });
        };
        mailbox_runtime.create_mailbox()
    }

    pub fn close_mailbox(&mut self, receiver: &Value) -> Result<(), RuntimeError> {
        let Some(mailbox_runtime) = self.ports.mailbox_runtime.as_ref() else {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("mailbox_close"),
                message: "mailbox runtime is not available".to_owned(),
            });
        };
        mailbox_runtime.close_mailbox(receiver)
    }

    pub fn send_mailbox(&mut self, sender: Value, value: Value) -> Result<(), RuntimeError> {
        let Some(mailbox_runtime) = self.ports.mailbox_runtime.as_ref() else {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("mailbox_send"),
                message: "mailbox runtime is not available".to_owned(),
            });
        };
        mailbox_runtime.validate_mailbox_sender(&sender)?;
        self.ports
            .pending_mailbox_sends
            .push(MailboxSend { sender, value });
        Ok(())
    }

    pub fn subscribe_changes(
        &mut self,
        request: SubscriptionRequest,
    ) -> Result<Value, RuntimeError> {
        if !self.authority.is_root()
            && self.runtime_context.actor().is_none()
            && self.runtime_context.principal().is_none()
        {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("subscribe_changes"),
                message: "non-root subscriptions require a refreshable actor or principal context"
                    .to_owned(),
            });
        }
        if let Some(relation) = request.subject.relation()
            && !self.authority.can_read_relation(relation)
        {
            return Err(RuntimeError::PermissionDenied {
                operation: "subscribe",
                target: Value::identity(relation),
            });
        }
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
        let Some(mailbox_runtime) = self.ports.mailbox_runtime.as_ref() else {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("subscribe_changes"),
                message: "mailbox runtime is not available".to_owned(),
            });
        };
        let subscription = mailbox_runtime.allocate_subscription(&request.sender)?;
        self.ports
            .pending_subscriptions
            .push(SubscriptionOperation::Register {
                subscription: subscription.clone(),
                request,
                runtime_context: self.runtime_context,
                root_authority: self.authority.is_root(),
            });
        Ok(subscription)
    }

    pub fn cancel_subscription(&mut self, subscription: Value) -> Result<(), RuntimeError> {
        let Some(mailbox_runtime) = self.ports.mailbox_runtime.as_ref() else {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("cancel_subscription"),
                message: "mailbox runtime is not available".to_owned(),
            });
        };
        mailbox_runtime.validate_subscription(&subscription)?;
        self.ports
            .pending_subscriptions
            .push(SubscriptionOperation::Cancel { subscription });
        Ok(())
    }
}

pub trait Builtin: Send + Sync {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError>;
}

impl<F> Builtin for F
where
    F: for<'ctx, 'kernel> Fn(
            &mut BuiltinContext<'ctx, 'kernel>,
            &[Value],
        ) -> Result<Value, RuntimeError>
        + Send
        + Sync,
{
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        self(context, args)
    }
}

pub struct ClientBuiltinContext<'ctx> {
    workspace: &'ctx mut dyn RelationWorkspace,
    authority: &'ctx mut AuthorityContext,
    pending_effects: &'ctx mut Vec<Emission>,
    runtime_context: RuntimeContext,
}

impl<'ctx> ClientBuiltinContext<'ctx> {
    pub(crate) fn new(
        workspace: &'ctx mut dyn RelationWorkspace,
        authority: &'ctx mut AuthorityContext,
        pending_effects: &'ctx mut Vec<Emission>,
        runtime_context: RuntimeContext,
    ) -> Self {
        Self {
            workspace,
            authority,
            pending_effects,
            runtime_context,
        }
    }

    pub fn authority(&self) -> &AuthorityContext {
        self.authority
    }

    pub fn authority_mut(&mut self) -> &mut AuthorityContext {
        self.authority
    }

    pub fn runtime_context(&self) -> RuntimeContext {
        self.runtime_context
    }

    pub fn emit(&mut self, target: Identity, value: Value) -> Result<(), RuntimeError> {
        if !self.authority.can_effect() {
            return Err(RuntimeError::PermissionDenied {
                operation: "effect",
                target: Value::identity(target),
            });
        }
        self.pending_effects.push(Emission::new(target, value));
        Ok(())
    }

    pub fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, RuntimeError> {
        self.workspace
            .scan_relation(relation, bindings)
            .map_err(RuntimeError::Kernel)
    }

    pub fn assert_tuple(&mut self, relation: RelationId, tuple: Tuple) -> Result<(), RuntimeError> {
        self.workspace
            .assert_tuple(relation, tuple)
            .map_err(RuntimeError::Kernel)
    }

    pub fn retract_tuple(
        &mut self,
        relation: RelationId,
        tuple: Tuple,
    ) -> Result<(), RuntimeError> {
        self.workspace
            .retract_tuple(relation, tuple)
            .map_err(RuntimeError::Kernel)
    }

    pub fn replace_functional_tuple(
        &mut self,
        relation: RelationId,
        tuple: Tuple,
    ) -> Result<(), RuntimeError> {
        self.workspace
            .replace_functional_tuple(relation, tuple)
            .map_err(RuntimeError::Kernel)
    }
}

pub trait ClientBuiltin: Send + Sync {
    fn call(
        &self,
        context: &mut ClientBuiltinContext<'_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError>;
}

impl<F> ClientBuiltin for F
where
    F: for<'ctx> Fn(&mut ClientBuiltinContext<'ctx>, &[Value]) -> Result<Value, RuntimeError>
        + Send
        + Sync,
{
    fn call(
        &self,
        context: &mut ClientBuiltinContext<'_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        self(context, args)
    }
}

/// Successful result-kind contract for a host builtin.
///
/// Raised errors do not contribute to this contract. `Exact` results are validated by the VM
/// before the destination register receives the value. `Structural` publishes a full successful
/// result contract for compiler checks, and `Dynamic` makes no successful-result claim.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum BuiltinResultKind {
    #[default]
    Dynamic,
    Exact(ValueKind),
    Structural(TypeContract),
}

#[derive(Clone)]
struct BuiltinEntry {
    result_kind: BuiltinResultKind,
    implementation: Arc<dyn Builtin>,
}

#[derive(Clone, Default)]
pub struct BuiltinRegistry {
    builtins: BTreeMap<Symbol, BuiltinEntry>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin(
        mut self,
        name: impl AsRef<str>,
        result_kind: BuiltinResultKind,
        builtin: impl Builtin + 'static,
    ) -> Self {
        self.insert(name, result_kind, builtin);
        self
    }

    pub fn insert(
        &mut self,
        name: impl AsRef<str>,
        result_kind: BuiltinResultKind,
        builtin: impl Builtin + 'static,
    ) {
        self.builtins.insert(
            Symbol::intern(name.as_ref()),
            BuiltinEntry {
                result_kind,
                implementation: Arc::new(builtin),
            },
        );
    }

    pub fn get(&self, name: Symbol) -> Option<Arc<dyn Builtin>> {
        self.builtins
            .get(&name)
            .map(|entry| Arc::clone(&entry.implementation))
    }

    pub fn result_kind(&self, name: Symbol) -> Option<BuiltinResultKind> {
        self.builtins
            .get(&name)
            .map(|entry| entry.result_kind.clone())
    }

    pub fn result_kinds(&self) -> impl Iterator<Item = (Symbol, BuiltinResultKind)> + '_ {
        self.builtins
            .iter()
            .map(|(name, entry)| (*name, entry.result_kind.clone()))
    }

    pub fn contains(&self, name: Symbol) -> bool {
        self.builtins.contains_key(&name)
    }

    pub fn len(&self) -> usize {
        self.builtins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.builtins.is_empty()
    }
}

#[derive(Clone, Default)]
pub struct ClientBuiltinRegistry {
    builtins: BTreeMap<Symbol, Arc<dyn ClientBuiltin>>,
}

impl ClientBuiltinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin(
        mut self,
        name: impl AsRef<str>,
        builtin: impl ClientBuiltin + 'static,
    ) -> Self {
        self.insert(name, builtin);
        self
    }

    pub fn insert(&mut self, name: impl AsRef<str>, builtin: impl ClientBuiltin + 'static) {
        self.builtins
            .insert(Symbol::intern(name.as_ref()), Arc::new(builtin));
    }

    pub fn get(&self, name: Symbol) -> Option<Arc<dyn ClientBuiltin>> {
        self.builtins.get(&name).cloned()
    }

    pub fn contains(&self, name: Symbol) -> bool {
        self.builtins.contains_key(&name)
    }

    pub fn len(&self) -> usize {
        self.builtins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.builtins.is_empty()
    }
}
