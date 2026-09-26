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
use crate::task_manager::MailboxRuntimeHandle;
use mica_relation_kernel::{Conflict, KernelError, RelationKernel, Transaction};
use mica_var::Value;
use mica_vm::{
    AuthorityContext, BuiltinRegistry, Emission, MailboxRuntime, MailboxSend, Program,
    ProgramResolver, RegisterVm, RuntimeContext, RuntimeError, RuntimePorts, SuspendKind,
    VmHostContext, VmHostResponse, VmState,
};
use std::sync::Arc;
use std::time::Instant;

pub type TaskId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskLimits {
    pub instruction_budget: usize,
    pub max_retries: u8,
    pub max_call_depth: usize,
}

impl Default for TaskLimits {
    fn default() -> Self {
        Self {
            instruction_budget: 1_000_000,
            max_retries: 10,
            max_call_depth: 50,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskOutcome {
    Complete {
        value: Value,
        effects: Vec<Emission>,
        mailbox_sends: Vec<MailboxSend>,
        retries: u8,
    },
    Suspended {
        kind: SuspendKind,
        effects: Vec<Emission>,
        mailbox_sends: Vec<MailboxSend>,
        retries: u8,
    },
    Aborted {
        error: Value,
        effects: Vec<Emission>,
        mailbox_sends: Vec<MailboxSend>,
        retries: u8,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskError {
    Runtime(RuntimeError),
    ConflictRetriesExceeded { retries: u8 },
    MissingTransaction,
    UnknownRelation(mica_relation_kernel::RelationId),
}

impl From<RuntimeError> for TaskError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<KernelError> for TaskError {
    fn from(value: KernelError) -> Self {
        Self::Runtime(RuntimeError::Kernel(value))
    }
}

pub struct Task<'a> {
    task_id: TaskId,
    kernel: &'a RelationKernel,
    program: Arc<Program>,
    resolver: Arc<ProgramResolver>,
    builtins: Arc<BuiltinRegistry>,
    authority: AuthorityContext,
    vm: RegisterVm,
    tx: Option<Transaction<'a>>,
    retry_state: VmState,
    pending_effects: Vec<Emission>,
    committed_effects: Vec<Emission>,
    pending_mailbox_sends: Vec<MailboxSend>,
    committed_mailbox_sends: Vec<MailboxSend>,
    pending_subscriptions: Vec<mica_vm::SubscriptionOperation>,
    mailbox_runtime: Option<MailboxRuntimeHandle>,
    subscription_runtime: Option<SubscriptionRuntimeHandle>,
    task_snapshot: Vec<Value>,
    runtime_context: RuntimeContext,
    retries: u8,
    limits: TaskLimits,
}

impl<'a> Task<'a> {
    pub fn new(
        task_id: TaskId,
        kernel: &'a RelationKernel,
        program: Arc<Program>,
        resolver: Arc<ProgramResolver>,
        limits: TaskLimits,
    ) -> Self {
        Self::new_with_builtins(
            task_id,
            kernel,
            program,
            resolver,
            Arc::new(BuiltinRegistry::new()),
            limits,
        )
    }

    pub fn new_with_builtins(
        task_id: TaskId,
        kernel: &'a RelationKernel,
        program: Arc<Program>,
        resolver: Arc<ProgramResolver>,
        builtins: Arc<BuiltinRegistry>,
        limits: TaskLimits,
    ) -> Self {
        Self::new_with_authority(
            task_id,
            kernel,
            program,
            resolver,
            builtins,
            AuthorityContext::root(),
            limits,
        )
    }

    pub fn new_with_authority(
        task_id: TaskId,
        kernel: &'a RelationKernel,
        program: Arc<Program>,
        resolver: Arc<ProgramResolver>,
        builtins: Arc<BuiltinRegistry>,
        authority: AuthorityContext,
        limits: TaskLimits,
    ) -> Self {
        let vm = RegisterVm::new(program.clone());
        let retry_state = vm.snapshot_state();
        Self {
            task_id,
            kernel,
            program,
            resolver,
            builtins,
            authority,
            vm,
            tx: Some(kernel.begin()),
            retry_state,
            pending_effects: Vec::new(),
            committed_effects: Vec::new(),
            pending_mailbox_sends: Vec::new(),
            committed_mailbox_sends: Vec::new(),
            pending_subscriptions: Vec::new(),
            mailbox_runtime: None,
            subscription_runtime: None,
            task_snapshot: Vec::new(),
            runtime_context: RuntimeContext::default(),
            retries: 0,
            limits,
        }
    }

    pub(crate) fn from_state_with_authority(
        task_id: TaskId,
        kernel: &'a RelationKernel,
        resolver: Arc<ProgramResolver>,
        builtins: Arc<BuiltinRegistry>,
        state: TaskState,
        authority: AuthorityContext,
    ) -> Self {
        Self {
            task_id,
            kernel,
            vm: RegisterVm::from_state(state.vm_state),
            tx: Some(kernel.begin()),
            program: state.program,
            resolver,
            builtins,
            authority,
            retry_state: state.retry_state,
            pending_effects: Vec::new(),
            committed_effects: Vec::new(),
            pending_mailbox_sends: Vec::new(),
            committed_mailbox_sends: Vec::new(),
            pending_subscriptions: Vec::new(),
            mailbox_runtime: None,
            subscription_runtime: None,
            task_snapshot: Vec::new(),
            runtime_context: RuntimeContext::default(),
            retries: state.retries,
            limits: state.limits,
        }
    }

    pub fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub(crate) fn set_task_snapshot(&mut self, task_snapshot: Vec<Value>) {
        self.task_snapshot = task_snapshot;
    }

    pub(crate) fn set_runtime_context(&mut self, runtime_context: RuntimeContext) {
        self.runtime_context = runtime_context;
    }

    pub(crate) fn set_mailbox_runtime(&mut self, mailbox_runtime: MailboxRuntimeHandle) {
        self.mailbox_runtime = Some(mailbox_runtime);
    }

    pub(crate) fn set_subscription_runtime(
        &mut self,
        subscription_runtime: SubscriptionRuntimeHandle,
    ) {
        self.subscription_runtime = Some(subscription_runtime);
    }

    pub fn retries(&self) -> u8 {
        self.retries
    }

    pub fn vm(&self) -> &RegisterVm {
        &self.vm
    }

    pub fn vm_mut(&mut self) -> &mut RegisterVm {
        &mut self.vm
    }

    pub fn resume_with(&mut self, value: Value) -> Result<(), TaskError> {
        self.vm.resume_with(value)?;
        self.retry_state = self.vm.snapshot_state();
        Ok(())
    }

    pub(crate) fn checkpoint(&self) -> TaskState {
        TaskState {
            program: self.program.clone(),
            vm_state: self.vm.snapshot_state(),
            retry_state: self.retry_state.clone(),
            retries: self.retries,
            limits: self.limits,
        }
    }

    pub fn run(&mut self) -> Result<TaskOutcome, TaskError> {
        let trace_enabled = tracing::enabled!(tracing::Level::TRACE);
        loop {
            let vm_start = trace_enabled.then(Instant::now);
            let response = {
                let tx = self.tx.as_mut().ok_or(TaskError::MissingTransaction)?;
                let mailbox_runtime = self.mailbox_runtime.clone();
                let mut host = VmHostContext::new(
                    tx,
                    &mut self.authority,
                    &self.resolver,
                    &self.builtins,
                    RuntimePorts {
                        pending_effects: &mut self.pending_effects,
                        pending_mailbox_sends: &mut self.pending_mailbox_sends,
                        pending_subscriptions: &mut self.pending_subscriptions,
                        mailbox_runtime: mailbox_runtime
                            .as_ref()
                            .map(|runtime| runtime as &dyn MailboxRuntime),
                    },
                    &self.task_snapshot,
                    self.runtime_context,
                );
                let response = self.vm.run_until_host_response(
                    &mut host,
                    self.limits.instruction_budget,
                    self.limits.max_call_depth,
                );
                host.emit_trace_summary(self.task_id);
                response?
            };
            if let Some(vm_start) = vm_start {
                tracing::trace!(
                    task_id = self.task_id,
                    response = host_response_label(&response),
                    elapsed_us = vm_start.elapsed().as_micros(),
                    "task VM step completed"
                );
            }

            let response_start = trace_enabled.then(Instant::now);
            let outcome = self.outcome_from_host_response(response)?;
            if let Some(response_start) = response_start {
                tracing::trace!(
                    task_id = self.task_id,
                    elapsed_us = response_start.elapsed().as_micros(),
                    "task host response processed"
                );
            }
            if let Some(outcome) = outcome {
                return Ok(outcome);
            }
        }
    }

    fn outcome_from_host_response(
        &mut self,
        response: VmHostResponse,
    ) -> Result<Option<TaskOutcome>, TaskError> {
        match response {
            VmHostResponse::Continue => Ok(None),
            VmHostResponse::Commit => {
                self.commit_boundary()?;
                Ok(None)
            }
            VmHostResponse::Suspend(kind) => {
                if self.commit_boundary()? == BoundaryResult::Retried {
                    return Ok(None);
                }
                Ok(Some(TaskOutcome::Suspended {
                    kind,
                    effects: self.take_committed_effects(),
                    mailbox_sends: self.take_committed_mailbox_sends(),
                    retries: self.retries,
                }))
            }
            VmHostResponse::Spawn(request) => {
                if self.commit_boundary()? == BoundaryResult::Retried {
                    return Ok(None);
                }
                Ok(Some(TaskOutcome::Suspended {
                    kind: SuspendKind::Spawn(request),
                    effects: self.take_committed_effects(),
                    mailbox_sends: self.take_committed_mailbox_sends(),
                    retries: self.retries,
                }))
            }
            VmHostResponse::Complete(value) => {
                if self.terminal_commit_boundary()? == BoundaryResult::Retried {
                    return Ok(None);
                }
                Ok(Some(TaskOutcome::Complete {
                    value,
                    effects: self.take_committed_effects(),
                    mailbox_sends: self.take_committed_mailbox_sends(),
                    retries: self.retries,
                }))
            }
            VmHostResponse::Abort(error) => {
                self.pending_effects.clear();
                self.pending_mailbox_sends.clear();
                self.discard_pending_subscriptions();
                self.tx.take();
                Ok(Some(TaskOutcome::Aborted {
                    error,
                    effects: self.take_committed_effects(),
                    mailbox_sends: self.take_committed_mailbox_sends(),
                    retries: self.retries,
                }))
            }
            VmHostResponse::RollbackRetry => {
                self.retry_from_boundary()?;
                Ok(None)
            }
        }
    }

    fn commit_boundary(&mut self) -> Result<BoundaryResult, TaskError> {
        self.commit_boundary_with_disposition(BoundaryDisposition::Continue)
    }

    fn terminal_commit_boundary(&mut self) -> Result<BoundaryResult, TaskError> {
        self.commit_boundary_with_disposition(BoundaryDisposition::Finish)
    }

    fn commit_boundary_with_disposition(
        &mut self,
        disposition: BoundaryDisposition,
    ) -> Result<BoundaryResult, TaskError> {
        let start = tracing::enabled!(tracing::Level::TRACE).then(Instant::now);
        let tx = self.tx.take().ok_or(TaskError::MissingTransaction)?;
        let retryable = !tx.has_tagged_buffer_apply();
        if tx.is_read_only() {
            if let Some(subscription_runtime) = &self.subscription_runtime {
                self.kernel.at_publication_boundary(|snapshot| {
                    subscription_runtime.apply_boundary(
                        self.kernel,
                        snapshot,
                        &mut self.pending_subscriptions,
                    )
                })?;
            } else {
                self.discard_pending_subscriptions();
            }
            self.finish_successful_boundary(disposition);
            self.trace_successful_boundary(start, true, disposition);
            return Ok(BoundaryResult::Committed);
        }
        let subscription_runtime = self.subscription_runtime.clone();
        let mut subscription_result = Ok(Vec::new());
        let result = tx.commit_with_post_publish(|result| {
            if let Some(subscription_runtime) = &subscription_runtime {
                subscription_result = subscription_runtime.apply_boundary(
                    self.kernel,
                    result.snapshot(),
                    &mut self.pending_subscriptions,
                );
            } else {
                self.discard_pending_subscriptions();
            }
        });
        match result {
            Ok(_) => {
                let _ = subscription_result?;
                self.finish_successful_boundary(disposition);
                self.trace_successful_boundary(start, false, disposition);
                Ok(BoundaryResult::Committed)
            }
            Err(error) if retryable && is_retryable_conflict(&error) => {
                self.retry_from_boundary()?;
                self.trace_retried_boundary(start, disposition);
                Ok(BoundaryResult::Retried)
            }
            Err(error) => {
                self.discard_pending_subscriptions();
                self.tx = Some(self.kernel.begin());
                Err(error.into())
            }
        }
    }

    fn finish_successful_boundary(&mut self, disposition: BoundaryDisposition) {
        self.committed_effects.append(&mut self.pending_effects);
        self.committed_mailbox_sends
            .append(&mut self.pending_mailbox_sends);
        if disposition == BoundaryDisposition::Continue {
            self.retry_state = self.vm.snapshot_state();
            self.tx = Some(self.kernel.begin());
        }
    }

    fn trace_successful_boundary(
        &self,
        start: Option<Instant>,
        read_only: bool,
        disposition: BoundaryDisposition,
    ) {
        let Some(start) = start else {
            return;
        };
        tracing::trace!(
            task_id = self.task_id,
            read_only,
            terminal = disposition == BoundaryDisposition::Finish,
            result = "committed",
            elapsed_us = start.elapsed().as_micros(),
            "task commit boundary"
        );
    }

    fn trace_retried_boundary(&self, start: Option<Instant>, disposition: BoundaryDisposition) {
        if let Some(start) = start {
            tracing::debug!(
                task_id = self.task_id,
                read_only = false,
                terminal = disposition == BoundaryDisposition::Finish,
                result = "retried",
                elapsed_us = start.elapsed().as_micros(),
                "task commit conflict retried"
            );
        } else {
            tracing::debug!(
                task_id = self.task_id,
                read_only = false,
                terminal = disposition == BoundaryDisposition::Finish,
                result = "retried",
                "task commit conflict retried"
            );
        }
    }

    fn retry_from_boundary(&mut self) -> Result<(), TaskError> {
        if self.retries >= self.limits.max_retries {
            return Err(TaskError::ConflictRetriesExceeded {
                retries: self.retries,
            });
        }
        self.pending_effects.clear();
        self.pending_mailbox_sends.clear();
        self.discard_pending_subscriptions();
        self.vm.restore_state(&self.retry_state);
        self.tx = Some(self.kernel.begin());
        self.retries += 1;
        Ok(())
    }

    fn take_committed_effects(&mut self) -> Vec<Emission> {
        std::mem::take(&mut self.committed_effects)
    }

    fn take_committed_mailbox_sends(&mut self) -> Vec<MailboxSend> {
        std::mem::take(&mut self.committed_mailbox_sends)
    }

    fn discard_pending_subscriptions(&mut self) {
        if let Some(mailbox_runtime) = &self.mailbox_runtime {
            mailbox_runtime.discard_pending_subscriptions(&self.pending_subscriptions);
        }
        self.pending_subscriptions.clear();
    }
}

pub(crate) struct TaskState {
    program: Arc<Program>,
    vm_state: VmState,
    retry_state: VmState,
    retries: u8,
    limits: TaskLimits,
}

impl TaskState {
    pub(crate) fn frame_count(&self) -> usize {
        self.vm_state.frames().len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundaryResult {
    Committed,
    Retried,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundaryDisposition {
    Continue,
    Finish,
}

fn host_response_label(response: &VmHostResponse) -> &'static str {
    match response {
        VmHostResponse::Continue => "continue",
        VmHostResponse::Commit => "commit",
        VmHostResponse::Suspend(_) => "suspend",
        VmHostResponse::Spawn(_) => "spawn",
        VmHostResponse::Complete(_) => "complete",
        VmHostResponse::Abort(_) => "abort",
        VmHostResponse::RollbackRetry => "rollback_retry",
    }
}

fn is_retryable_conflict(error: &KernelError) -> bool {
    matches!(
        error,
        KernelError::Conflict(Conflict {
            relation: _,
            tuple: _,
            kind: _
        }) | KernelError::Buffer {
            error: mica_relation_kernel::buffer::BufferError::Conflict { .. },
            ..
        }
    )
}
