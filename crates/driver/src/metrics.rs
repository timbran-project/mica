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
    Counter, DeriveLabel, ExportMetrics, Gauge, Histogram, LabeledCounter, LabeledGauge,
    LabeledHistogram, LabeledSampledTimer,
};
use mica_runtime::SuspendKind;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

const DEFAULT_SHARDS: usize = 64;
const TIMER_SAMPLE_STRIDE: u64 = 64;
const LATENCY_BUCKETS_US: &[u64] = &[
    10, 50, 100, 500, 1_000, 5_000, 10_000, 50_000, 100_000, 500_000, 1_000_000, 5_000_000,
    10_000_000,
];

static METRICS: LazyLock<DriverMetrics> = LazyLock::new(|| DriverMetrics::new(DEFAULT_SHARDS));
static ACTIVE_ASYNC_WORKERS: AtomicI64 = AtomicI64::new(0);
static ACTIVE_TIMER_RESUME_WORKERS: AtomicI64 = AtomicI64::new(0);
static ACTIVE_MAILBOX_TIMEOUT_WORKERS: AtomicI64 = AtomicI64::new(0);
static ACTIVE_EXTERNAL_REQUEST_WORKERS: AtomicI64 = AtomicI64::new(0);
static ACTIVE_MAILBOX_RECV_WORKERS: AtomicI64 = AtomicI64::new(0);
static ACTIVE_MAILBOX_WAKE_WORKERS: AtomicI64 = AtomicI64::new(0);
static ACTIVE_SPAWN_CHILD_WORKERS: AtomicI64 = AtomicI64::new(0);

#[derive(Copy, Clone, Debug, DeriveLabel)]
#[label_name = "operation"]
pub enum DispatchOperation {
    Submit,
    RootSubmit,
    Invoke,
    Spawn,
    Resume,
    Filein,
    Fileout,
}

#[derive(Copy, Clone, Debug, DeriveLabel)]
#[label_name = "worker"]
pub enum AsyncWorkerKind {
    TimerResume,
    MailboxTimeout,
    ExternalRequest,
    MailboxRecv,
    MailboxWake,
    SpawnChild,
}

#[derive(Copy, Clone, Debug)]
pub enum WorkerOutcome {
    Complete,
    Error,
    Timeout,
    Cancelled,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, DeriveLabel)]
#[label_name = "outcome"]
pub enum ParallelAdmissionOutcome {
    Admitted,
    Capacity,
    TaskWaiting,
    Contended,
}

#[derive(Copy, Clone, Debug, DeriveLabel)]
#[label_name = "kind"]
pub enum SuspendKindMetric {
    Commit,
    Never,
    TimedMillis,
    WaitingForInput,
    MailboxRecv,
    Spawn,
    ExternalRequest,
}

#[derive(ExportMetrics)]
#[metric_prefix = "mica_driver"]
pub struct DriverMetrics {
    #[help = "Task driver instances started"]
    pub drivers_started: Counter,

    #[help = "Configured dispatcher workers for started drivers"]
    pub dispatcher_workers_configured: Gauge,

    #[help = "Dispatcher jobs submitted by operation"]
    pub dispatch_jobs: LabeledCounter<DispatchOperation>,

    #[help = "Dispatcher job failures by operation"]
    pub dispatch_failures: LabeledCounter<DispatchOperation>,

    #[help = "Dispatcher job duration in microseconds by operation"]
    pub dispatch_duration_us: LabeledHistogram<DispatchOperation>,

    #[help = "Dispatcher job duration by operation"]
    pub dispatch_duration: LabeledSampledTimer<DispatchOperation>,

    #[help = "Task segments that waited for shared CPU admission"]
    pub task_admission_waits: Counter,

    #[help = "Task segment CPU admission wait duration in microseconds"]
    pub task_admission_wait_duration_us: Histogram,

    #[help = "Parallel relation worker admission attempts by outcome"]
    pub parallel_admission: LabeledCounter<ParallelAdmissionOutcome>,

    #[help = "Async workers spawned by worker kind"]
    pub async_workers_started: LabeledCounter<AsyncWorkerKind>,

    #[help = "Async workers completed by worker kind"]
    pub async_workers_completed: LabeledCounter<AsyncWorkerKind>,

    #[help = "Async workers failed by worker kind"]
    pub async_worker_errors: LabeledCounter<AsyncWorkerKind>,

    #[help = "Async workers timed out by worker kind"]
    pub async_worker_timeouts: LabeledCounter<AsyncWorkerKind>,

    #[help = "Async workers cancelled by worker kind"]
    pub async_worker_cancelled: LabeledCounter<AsyncWorkerKind>,

    #[help = "Async worker duration in microseconds by worker kind"]
    pub async_worker_duration_us: LabeledHistogram<AsyncWorkerKind>,

    #[help = "Async worker duration by worker kind"]
    pub async_worker_duration: LabeledSampledTimer<AsyncWorkerKind>,

    #[help = "Currently active async workers by worker kind"]
    pub active_async_workers: LabeledGauge<AsyncWorkerKind>,

    #[help = "Currently active async workers across all kinds"]
    pub active_async_workers_total: Gauge,

    #[help = "Task suspensions observed by kind"]
    pub task_suspensions: LabeledCounter<SuspendKindMetric>,

    #[help = "Mailbox waiters currently registered"]
    pub mailbox_waiters: Gauge,

    #[help = "Input waiters currently registered"]
    pub input_waiters: Gauge,

    #[help = "Buffered driver events waiting for delivery"]
    pub buffered_events: Gauge,
}

impl DriverMetrics {
    pub fn new(shard_count: usize) -> Self {
        Self {
            drivers_started: Counter::new(shard_count),
            dispatcher_workers_configured: Gauge::new(),
            dispatch_jobs: LabeledCounter::new(shard_count),
            dispatch_failures: LabeledCounter::new(shard_count),
            dispatch_duration_us: LabeledHistogram::new(LATENCY_BUCKETS_US, shard_count),
            dispatch_duration: LabeledSampledTimer::with_latency_buckets(
                shard_count,
                TIMER_SAMPLE_STRIDE,
            ),
            task_admission_waits: Counter::new(shard_count),
            task_admission_wait_duration_us: Histogram::new(LATENCY_BUCKETS_US, shard_count),
            parallel_admission: LabeledCounter::new(shard_count),
            async_workers_started: LabeledCounter::new(shard_count),
            async_workers_completed: LabeledCounter::new(shard_count),
            async_worker_errors: LabeledCounter::new(shard_count),
            async_worker_timeouts: LabeledCounter::new(shard_count),
            async_worker_cancelled: LabeledCounter::new(shard_count),
            async_worker_duration_us: LabeledHistogram::new(LATENCY_BUCKETS_US, shard_count),
            async_worker_duration: LabeledSampledTimer::with_latency_buckets(
                shard_count,
                TIMER_SAMPLE_STRIDE,
            ),
            active_async_workers: LabeledGauge::new(),
            active_async_workers_total: Gauge::new(),
            task_suspensions: LabeledCounter::new(shard_count),
            mailbox_waiters: Gauge::new(),
            input_waiters: Gauge::new(),
            buffered_events: Gauge::new(),
        }
    }
}

pub fn metrics() -> &'static DriverMetrics {
    &METRICS
}

pub(crate) fn duration_us(elapsed: Duration) -> u64 {
    elapsed.as_micros().min(u128::from(u64::MAX)) as u64
}

pub(crate) fn record_task_admission_wait() {
    metrics().task_admission_waits.inc();
}

pub(crate) fn record_task_admission_wait_duration(elapsed: Duration) {
    metrics()
        .task_admission_wait_duration_us
        .record(duration_us(elapsed));
}

pub(crate) fn record_parallel_admission(outcome: ParallelAdmissionOutcome) {
    metrics().parallel_admission.inc(outcome);
}

pub(crate) fn dispatch_started(operation: DispatchOperation) {
    metrics().dispatch_jobs.inc(operation);
}

pub(crate) fn record_dispatch_result<T>(
    operation: DispatchOperation,
    elapsed: Duration,
    result: &Result<T, impl Sized>,
) {
    let metrics = metrics();
    metrics
        .dispatch_duration_us
        .record(operation, duration_us(elapsed));
    metrics.dispatch_duration.record_elapsed(operation, elapsed);
    if result.is_err() {
        metrics.dispatch_failures.inc(operation);
    }
}

pub(crate) fn record_suspend(kind: &SuspendKind) {
    metrics().task_suspensions.inc(match kind {
        SuspendKind::Commit => SuspendKindMetric::Commit,
        SuspendKind::Never => SuspendKindMetric::Never,
        SuspendKind::TimedMillis(_) => SuspendKindMetric::TimedMillis,
        SuspendKind::WaitingForInput(_) => SuspendKindMetric::WaitingForInput,
        SuspendKind::MailboxRecv(_) => SuspendKindMetric::MailboxRecv,
        SuspendKind::Spawn(_) => SuspendKindMetric::Spawn,
        SuspendKind::ExternalRequest(_) => SuspendKindMetric::ExternalRequest,
    });
}

pub(crate) fn record_waiting_state(
    input_waiters: usize,
    mailbox_waiters: usize,
    buffered_events: usize,
) {
    let metrics = metrics();
    metrics.input_waiters.set(input_waiters as i64);
    metrics.mailbox_waiters.set(mailbox_waiters as i64);
    metrics.buffered_events.set(buffered_events as i64);
}

pub(crate) fn async_worker_started(kind: AsyncWorkerKind) {
    let metrics = metrics();
    metrics.async_workers_started.inc(kind);
    let active = active_worker_counter(kind).fetch_add(1, Ordering::Relaxed) + 1;
    metrics.active_async_workers.set(kind, active);
    let total = ACTIVE_ASYNC_WORKERS.fetch_add(1, Ordering::Relaxed) + 1;
    metrics.active_async_workers_total.set(total);
}

pub(crate) fn async_worker_finished(
    kind: AsyncWorkerKind,
    outcome: WorkerOutcome,
    elapsed: Duration,
) {
    let metrics = metrics();
    match outcome {
        WorkerOutcome::Complete => metrics.async_workers_completed.inc(kind),
        WorkerOutcome::Error => metrics.async_worker_errors.inc(kind),
        WorkerOutcome::Timeout => metrics.async_worker_timeouts.inc(kind),
        WorkerOutcome::Cancelled => metrics.async_worker_cancelled.inc(kind),
    }
    metrics
        .async_worker_duration_us
        .record(kind, duration_us(elapsed));
    metrics.async_worker_duration.record_elapsed(kind, elapsed);
    let active = active_worker_counter(kind)
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some((current - 1).max(0))
        })
        .unwrap_or(0)
        .saturating_sub(1)
        .max(0);
    metrics.active_async_workers.set(kind, active);
    let total = ACTIVE_ASYNC_WORKERS.fetch_sub(1, Ordering::Relaxed) - 1;
    metrics.active_async_workers_total.set(total.max(0));
}

fn active_worker_counter(kind: AsyncWorkerKind) -> &'static AtomicI64 {
    match kind {
        AsyncWorkerKind::TimerResume => &ACTIVE_TIMER_RESUME_WORKERS,
        AsyncWorkerKind::MailboxTimeout => &ACTIVE_MAILBOX_TIMEOUT_WORKERS,
        AsyncWorkerKind::ExternalRequest => &ACTIVE_EXTERNAL_REQUEST_WORKERS,
        AsyncWorkerKind::MailboxRecv => &ACTIVE_MAILBOX_RECV_WORKERS,
        AsyncWorkerKind::MailboxWake => &ACTIVE_MAILBOX_WAKE_WORKERS,
        AsyncWorkerKind::SpawnChild => &ACTIVE_SPAWN_CHILD_WORKERS,
    }
}
