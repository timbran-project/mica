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

use fast_telemetry::{
    Counter, DeriveLabel, ExportMetrics, Gauge, LabeledCounter, LabeledHistogram,
    LabeledSampledTimer,
};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::TaskOutcome;

const DEFAULT_SHARDS: usize = 64;
const TIMER_SAMPLE_STRIDE: u64 = 64;
const LATENCY_BUCKETS_US: &[u64] = &[
    10, 50, 100, 500, 1_000, 5_000, 10_000, 50_000, 100_000, 500_000, 1_000_000, 5_000_000,
    10_000_000,
];

static METRICS: LazyLock<RuntimeMetrics> = LazyLock::new(|| RuntimeMetrics::new(DEFAULT_SHARDS));
static ACTIVE_ENDPOINTS: AtomicI64 = AtomicI64::new(0);

#[derive(Copy, Clone, Debug, DeriveLabel)]
#[label_name = "operation"]
pub enum TaskOperation {
    Submit,
    Resume,
    Immediate,
}

#[derive(Copy, Clone, Debug, DeriveLabel)]
#[label_name = "outcome"]
pub enum RuntimeTaskOutcome {
    Complete,
    Suspended,
    Aborted,
    Error,
}

#[derive(Copy, Clone, Debug, DeriveLabel)]
#[label_name = "operation"]
pub enum EndpointOperation {
    Open,
    Close,
}

#[derive(ExportMetrics)]
#[metric_prefix = "mica_runtime"]
pub struct RuntimeMetrics {
    #[help = "Task operations by operation"]
    pub task_operations: LabeledCounter<TaskOperation>,

    #[help = "Task outcomes by outcome"]
    pub task_outcomes: LabeledCounter<RuntimeTaskOutcome>,

    #[help = "Task run duration in microseconds by operation"]
    pub task_run_duration_us: LabeledHistogram<TaskOperation>,

    #[help = "Task run duration by operation"]
    pub task_run_duration: LabeledSampledTimer<TaskOperation>,

    #[help = "Currently suspended tasks"]
    pub suspended_tasks: Gauge,

    #[help = "Completed tasks retained by runtime bookkeeping"]
    pub completed_tasks: Gauge,

    #[help = "Cancelled tasks retained by runtime bookkeeping"]
    pub cancelled_tasks: Gauge,

    #[help = "Endpoint operations by operation"]
    pub endpoint_operations: LabeledCounter<EndpointOperation>,

    #[help = "Currently open runtime endpoints"]
    pub active_endpoints: Gauge,

    #[help = "Effects emitted by tasks"]
    pub task_effects: Counter,

    #[help = "Mailbox sends emitted by tasks"]
    pub mailbox_sends: Counter,

    #[help = "Mailboxes created"]
    pub mailboxes_created: Counter,

    #[help = "Messages delivered to mailboxes"]
    pub mailbox_messages_delivered: Counter,

    #[help = "Mailbox drain operations"]
    pub mailbox_drains: Counter,

    #[help = "Messages drained from mailboxes"]
    pub mailbox_messages_drained: Counter,

    #[help = "Mailbox queues currently retained by the runtime"]
    pub mailboxes: Gauge,

    #[help = "Messages currently queued in mailboxes"]
    pub queued_mailbox_messages: Gauge,

    #[help = "Effects currently queued for host delivery"]
    pub queued_effects: Gauge,
}

impl RuntimeMetrics {
    pub fn new(shard_count: usize) -> Self {
        Self {
            task_operations: LabeledCounter::new(shard_count),
            task_outcomes: LabeledCounter::new(shard_count),
            task_run_duration_us: LabeledHistogram::new(LATENCY_BUCKETS_US, shard_count),
            task_run_duration: LabeledSampledTimer::with_latency_buckets(
                shard_count,
                TIMER_SAMPLE_STRIDE,
            ),
            suspended_tasks: Gauge::new(),
            completed_tasks: Gauge::new(),
            cancelled_tasks: Gauge::new(),
            endpoint_operations: LabeledCounter::new(shard_count),
            active_endpoints: Gauge::new(),
            task_effects: Counter::new(shard_count),
            mailbox_sends: Counter::new(shard_count),
            mailboxes_created: Counter::new(shard_count),
            mailbox_messages_delivered: Counter::new(shard_count),
            mailbox_drains: Counter::new(shard_count),
            mailbox_messages_drained: Counter::new(shard_count),
            mailboxes: Gauge::new(),
            queued_mailbox_messages: Gauge::new(),
            queued_effects: Gauge::new(),
        }
    }
}

pub fn metrics() -> &'static RuntimeMetrics {
    &METRICS
}

pub(crate) fn record_task_result(
    operation: TaskOperation,
    elapsed: Duration,
    result: &Result<TaskOutcome, impl Sized>,
) {
    let metrics = metrics();
    metrics.task_operations.inc(operation);
    metrics
        .task_run_duration_us
        .record(operation, duration_us(elapsed));
    metrics.task_run_duration.record_elapsed(operation, elapsed);
    metrics.task_outcomes.inc(match result {
        Ok(outcome) => outcome_label(outcome),
        Err(_) => RuntimeTaskOutcome::Error,
    });
}

fn duration_us(elapsed: Duration) -> u64 {
    elapsed.as_micros().min(u128::from(u64::MAX)) as u64
}

pub(crate) fn record_outcome_side_effects(outcome: &TaskOutcome) {
    let (effects, mailbox_sends) = match outcome {
        TaskOutcome::Complete {
            effects,
            mailbox_sends,
            ..
        }
        | TaskOutcome::Suspended {
            effects,
            mailbox_sends,
            ..
        }
        | TaskOutcome::Aborted {
            effects,
            mailbox_sends,
            ..
        } => (effects.len(), mailbox_sends.len()),
    };
    metrics().task_effects.add(effects as isize);
    metrics().mailbox_sends.add(mailbox_sends as isize);
}

pub(crate) fn outcome_label(outcome: &TaskOutcome) -> RuntimeTaskOutcome {
    match outcome {
        TaskOutcome::Complete { .. } => RuntimeTaskOutcome::Complete,
        TaskOutcome::Suspended { .. } => RuntimeTaskOutcome::Suspended,
        TaskOutcome::Aborted { .. } => RuntimeTaskOutcome::Aborted,
    }
}

pub(crate) fn endpoint_opened() {
    let active = ACTIVE_ENDPOINTS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.max(0) + 1)
        })
        .unwrap_or(0)
        .max(0)
        + 1;
    metrics().active_endpoints.set(active);
}

pub(crate) fn endpoint_closed() {
    let active = ACTIVE_ENDPOINTS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some((current - 1).max(0))
        })
        .unwrap_or(0)
        .saturating_sub(1)
        .max(0);
    metrics().active_endpoints.set(active);
}
