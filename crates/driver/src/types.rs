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

use mica_runtime::ExternalRequest;
use mica_runtime::SourceTaskError;
use mica_runtime::TaskRequest;
use mica_runtime::format_source_task_error;
use mica_runtime::{
    AuthorityContext, Effect, SubscriptionInitialDelivery, SubscriptionSubject, SuspendKind, TaskId,
};
use mica_var::{Identity, Value};
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type ExternalRequestFuture = Pin<Box<dyn Future<Output = Value> + 'static>>;
pub type ExternalRequestHandler =
    Arc<dyn Fn(ExternalRequest) -> ExternalRequestFuture + Send + Sync>;
pub type ExternalStreamEmitFuture = Pin<Box<dyn Future<Output = Result<(), String>> + 'static>>;

#[derive(Clone)]
pub struct ExternalStreamEmitter {
    emit: Arc<dyn Fn(Value) -> ExternalStreamEmitFuture + Send + Sync>,
}

impl ExternalStreamEmitter {
    pub(crate) fn new(emit: Arc<dyn Fn(Value) -> ExternalStreamEmitFuture + Send + Sync>) -> Self {
        Self { emit }
    }

    pub async fn emit(&self, value: Value) -> Result<(), String> {
        (self.emit)(value).await
    }
}

pub type ExternalStreamRequestHandler =
    Arc<dyn Fn(ExternalRequest, ExternalStreamEmitter) -> ExternalRequestFuture + Send + Sync>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskContext {
    pub principal: Option<Identity>,
    pub actor: Option<Identity>,
    pub endpoint: Identity,
    pub authority: AuthorityContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverSubscriptionRequest {
    pub subject: SubscriptionSubject,
    pub initial_delivery: SubscriptionInitialDelivery,
    pub cursor: Option<u64>,
    pub queue_budget: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub struct DriverSubscriptionMailbox {
    pub(crate) mailbox: u64,
    pub(crate) receiver: Value,
    pub(crate) sender: Value,
}

impl DriverSubscriptionMailbox {
    pub fn id(&self) -> u64 {
        self.mailbox
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DriverEvent {
    TaskCompleted {
        task_id: TaskId,
        value: Value,
    },
    TaskAborted {
        task_id: TaskId,
        error: Value,
    },
    TaskCancelled {
        task_id: TaskId,
        reason: TaskCancellationReason,
    },
    TaskFailed {
        task_id: TaskId,
        error: String,
    },
    TaskSuspended {
        task_id: TaskId,
        kind: SuspendKind,
    },
    SubscriptionReady {
        mailbox: u64,
    },
    Effect(Effect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskCancellationReason {
    Requested,
    EndpointClosed,
    DriverShutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointCloseReport {
    pub relation_changes: usize,
    pub cancelled_tasks: Vec<TaskId>,
}

#[derive(Debug)]
pub enum DriverError {
    Source(SourceTaskError),
    Join(String),
    MissingTaskContext(TaskId),
    TaskCancelled(TaskId),
    EndpointClosed(Identity),
    DriverStopped,
}

impl TaskContext {
    pub(crate) fn from_request(request: &TaskRequest, endpoint: Identity) -> Self {
        Self {
            principal: request.principal,
            actor: request.actor,
            endpoint,
            authority: request.authority.clone(),
        }
    }
}

impl DriverError {
    pub fn source(&self) -> Option<&SourceTaskError> {
        match self {
            Self::Source(error) => Some(error),
            Self::Join(_)
            | Self::MissingTaskContext(_)
            | Self::TaskCancelled(_)
            | Self::EndpointClosed(_)
            | Self::DriverStopped => None,
        }
    }
}

impl Display for DriverError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => write!(f, "{}", format_source_task_error(error)),
            Self::Join(error) => write!(f, "driver task failed: {error}"),
            Self::MissingTaskContext(task_id) => {
                write!(f, "missing task context for task {task_id}")
            }
            Self::TaskCancelled(task_id) => write!(f, "task {task_id} was cancelled"),
            Self::EndpointClosed(endpoint) => write!(f, "endpoint {endpoint:?} is closed"),
            Self::DriverStopped => write!(f, "task driver is stopped"),
        }
    }
}

impl std::error::Error for DriverError {}
