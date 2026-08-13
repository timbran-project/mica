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
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

pub type ExternalRequestFuture = Pin<Box<dyn Future<Output = Value> + 'static>>;
pub type ExternalRequestHandler =
    Arc<dyn Fn(ExternalRequestContext, ExternalRequest) -> ExternalRequestFuture + Send + Sync>;
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

pub type ExternalStreamRequestHandler = Arc<
    dyn Fn(ExternalRequestContext, ExternalRequest, ExternalStreamEmitter) -> ExternalRequestFuture
        + Send
        + Sync,
>;
pub type FileinIncludeLoader = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Clone)]
pub struct ExternalRequestCancellation {
    inner: Arc<ExternalRequestCancellationState>,
}

struct ExternalRequestCancellationState {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<std::task::Waker>>,
}

impl ExternalRequestCancellation {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ExternalRequestCancellationState {
                cancelled: AtomicBool::new(false),
                wakers: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        std::future::poll_fn(|context| {
            if self.is_cancelled() {
                return Poll::Ready(());
            }
            let mut wakers = self.inner.wakers.lock().unwrap();
            if self.is_cancelled() {
                return Poll::Ready(());
            }
            let waker = context.waker().clone();
            if !wakers.iter().any(|entry| entry.will_wake(&waker)) {
                wakers.push(waker);
            }
            Poll::Pending
        })
        .await
    }

    pub(crate) fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        for waker in std::mem::take(&mut *self.inner.wakers.lock().unwrap()) {
            waker.wake();
        }
    }
}

#[derive(Clone)]
pub struct ExternalRequestContext {
    pub task_id: TaskId,
    pub principal: Option<Identity>,
    pub actor: Option<Identity>,
    pub endpoint: Identity,
    pub cancellation: ExternalRequestCancellation,
}

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
    /// Overrides the driver's default subscription queue budget.
    pub queue_budget: Option<NonZeroUsize>,
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

/// An ordered notification from the driver.
///
/// Task submission and resumption return their immediate runtime outcome and
/// also enqueue the corresponding lifecycle event. The return value supports
/// request/response hosts, while this event stream is the authoritative source
/// for asynchronous lifecycle changes, effects, and subscription readiness.
/// Effects committed by a task precede that task's lifecycle event, and
/// subscription readiness caused by the outcome follows its lifecycle event.
/// Concurrent producers are ordered by admission to the queue.
/// Terminal task events and effects are retained until a host drains them;
/// repeated suspension and subscription-ready notifications may be coalesced.
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
    Storage(String),
    Configuration(String),
    Join(String),
    MissingTaskContext(TaskId),
    EphemeralIdentityExhausted,
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
            Self::Storage(_)
            | Self::Configuration(_)
            | Self::Join(_)
            | Self::MissingTaskContext(_)
            | Self::EphemeralIdentityExhausted
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
            Self::Storage(error) => write!(f, "failed to open driver storage: {error}"),
            Self::Configuration(error) => write!(f, "invalid driver configuration: {error}"),
            Self::Join(error) => write!(f, "driver task failed: {error}"),
            Self::MissingTaskContext(task_id) => {
                write!(f, "missing task context for task {task_id}")
            }
            Self::EphemeralIdentityExhausted => {
                write!(f, "ephemeral host identity space is exhausted")
            }
            Self::TaskCancelled(task_id) => write!(f, "task {task_id} was cancelled"),
            Self::EndpointClosed(endpoint) => write!(f, "endpoint {endpoint:?} is closed"),
            Self::DriverStopped => write!(f, "task driver is stopped"),
        }
    }
}

impl std::error::Error for DriverError {}
