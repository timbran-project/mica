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

use mica_compiler::{CompileContext, CompileError, HostRequestFunction};
use mica_relation_kernel::{ConflictPolicy, RelationDurability, RelationMetadata, Tuple};
use mica_var::{Identity, Symbol, Value};
use mica_vm::AuthorityContext;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex;

use crate::task::{TaskId, TaskLimits, TaskOutcome};
use crate::task_manager::{SharedTaskManager, TaskManager, TaskManagerError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileinMode {
    Add,
    Replace,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileinReport {
    pub reports: Vec<RunReport>,
    pub owned_facts: usize,
    pub owned_rules: usize,
    pub owned_relations: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SourceProjection {
    pub(crate) facts: BTreeSet<(Identity, Tuple)>,
    pub(crate) rules: BTreeSet<Identity>,
    pub(crate) relations: BTreeMap<Identity, RelationMetadata>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SourceDeclarations {
    pub(crate) identities: BTreeSet<String>,
    pub(crate) relations: Vec<SourceRelationDeclaration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceRelationDeclaration {
    pub(crate) name: String,
    pub(crate) arity: u16,
    pub(crate) conflict_policy: ConflictPolicy,
    pub(crate) durability: RelationDurability,
}

pub struct SourceRunner {
    pub(crate) context: CompileContext,
    pub(crate) task_manager: TaskManager,
    pub(crate) host_request_functions: Arc<[(String, HostRequestFunction)]>,
    pub(crate) next_method_identity_id: u64,
}

pub struct SharedSourceRunner {
    pub(crate) task_manager: SharedTaskManager,
    pub(crate) host_request_functions: Arc<[(String, HostRequestFunction)]>,
    pub(crate) filein_lock: Mutex<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRequest {
    pub principal: Option<Identity>,
    pub actor: Option<Identity>,
    pub endpoint: Identity,
    pub authority: AuthorityContext,
    pub input: TaskInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskInput {
    Source(String),
    Invocation {
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    },
    Continuation {
        task_id: TaskId,
        value: Value,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmittedTask {
    pub task_id: TaskId,
    pub outcome: TaskOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceTaskError {
    Compile(CompileError),
    TaskManager(TaskManagerError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadOnlySourceQueryOptions {
    pub max_output_chars: usize,
    pub instruction_budget: usize,
    pub max_call_depth: usize,
}

impl Default for ReadOnlySourceQueryOptions {
    fn default() -> Self {
        Self {
            max_output_chars: 4_000,
            instruction_budget: 50_000,
            max_call_depth: 16,
        }
    }
}

impl ReadOnlySourceQueryOptions {
    pub(crate) fn task_limits(self) -> TaskLimits {
        TaskLimits {
            instruction_budget: self.instruction_budget.max(1),
            max_retries: 0,
            max_call_depth: self.max_call_depth.max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOnlySourceQueryStatus {
    Complete,
    Aborted,
    Suspended,
    Rejected,
    Error,
}

impl ReadOnlySourceQueryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Aborted => "aborted",
            Self::Suspended => "suspended",
            Self::Rejected => "rejected",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadOnlySourceQueryReport {
    pub task_id: Option<TaskId>,
    pub status: ReadOnlySourceQueryStatus,
    pub value: Option<Value>,
    pub error: Option<Value>,
    pub diagnostics: Vec<String>,
    pub rendered: String,
    pub rendered_truncated: bool,
}

impl ReadOnlySourceQueryReport {
    pub fn as_value(&self) -> Value {
        Value::map([
            (
                Value::symbol(Symbol::intern("task_id")),
                self.task_id
                    .and_then(|task_id| Value::int(task_id as i64).ok())
                    .unwrap_or_else(Value::nothing),
            ),
            (
                Value::symbol(Symbol::intern("status")),
                Value::string(self.status.as_str()),
            ),
            (
                Value::symbol(Symbol::intern("value")),
                self.value.clone().unwrap_or_else(Value::nothing),
            ),
            (
                Value::symbol(Symbol::intern("error")),
                self.error.clone().unwrap_or_else(Value::nothing),
            ),
            (
                Value::symbol(Symbol::intern("diagnostics")),
                Value::list(self.diagnostics.iter().cloned().map(Value::string)),
            ),
            (
                Value::symbol(Symbol::intern("rendered")),
                Value::string(self.rendered.clone()),
            ),
            (
                Value::symbol(Symbol::intern("rendered_truncated")),
                Value::bool(self.rendered_truncated),
            ),
        ])
    }
}

impl From<CompileError> for SourceTaskError {
    fn from(value: CompileError) -> Self {
        Self::Compile(value)
    }
}

impl From<TaskManagerError> for SourceTaskError {
    fn from(value: TaskManagerError) -> Self {
        Self::TaskManager(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunReport {
    pub task_id: u64,
    pub outcome: TaskOutcome,
    pub(crate) identity_names: BTreeMap<Identity, String>,
    pub(crate) relation_names: BTreeMap<Identity, String>,
}
