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

//! Relation property update load test for Mica.
//!
//! This complements the read-heavy dispatch load test by submitting normal Mica
//! invocations that read and update a functional relation. It intentionally
//! drives the compiler/runtime/VM/task/relation path rather than calling the
//! relation kernel directly.

use clap::{Parser, ValueEnum};
use compio::dispatcher::Dispatcher;
use compio::runtime::Runtime;
use mica_driver::{
    DispatcherAffinity, DispatcherConfig, DispatcherPlacement, configure_dispatcher,
};
use mica_relation_kernel::FjallDurabilityMode;
use mica_runtime::{
    AuthorityContext, SharedSourceRunner, SourceRunner, TaskInput, TaskLimits, TaskOutcome,
    TaskRequest,
};
use mica_var::{Identity, Symbol, Value};
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const ENDPOINT_ID_START: u64 = 0x00ee_2000_0000_0000;

#[derive(Clone, Parser, Debug)]
#[command(
    name = "property-update-load-test",
    about = "Load test Mica relation reads and writes through runtime tasks"
)]
struct Args {
    #[arg(long, help = "Optional fresh Fjall store path")]
    store: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = DurabilityMode::Relaxed)]
    durability: DurabilityMode,

    #[arg(long, default_value_t = 1)]
    min_concurrency: usize,

    #[arg(long, default_value_t = 32)]
    max_concurrency: usize,

    #[arg(long, help = "Compio dispatcher worker thread count")]
    dispatcher_threads: Option<NonZeroUsize>,

    #[arg(long, value_enum, default_value_t = DispatcherAffinityArg::Auto)]
    dispatcher_affinity: DispatcherAffinityArg,

    #[arg(long, default_value_t = 100)]
    num_objects: usize,

    #[arg(long, default_value_t = 10)]
    num_properties: usize,

    #[arg(long, default_value_t = 200)]
    ops_per_worker: usize,

    #[arg(long, default_value_t = 0.5)]
    read_ratio: f64,

    #[arg(long, default_value_t = 1_000_000_000)]
    instruction_budget: usize,

    #[arg(long, default_value_t = 10)]
    warmup_ops: usize,

    #[arg(long)]
    output_file: Option<PathBuf>,

    #[arg(long, default_value_t = false)]
    swamp_mode: bool,

    #[arg(long, default_value_t = 30)]
    swamp_duration_seconds: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DurabilityMode {
    Relaxed,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DispatcherAffinityArg {
    Auto,
    Performance,
    None,
}

impl From<DispatcherAffinityArg> for DispatcherAffinity {
    fn from(value: DispatcherAffinityArg) -> Self {
        match value {
            DispatcherAffinityArg::Auto => Self::Auto,
            DispatcherAffinityArg::Performance => Self::Performance,
            DispatcherAffinityArg::None => Self::None,
        }
    }
}

impl From<DurabilityMode> for FjallDurabilityMode {
    fn from(value: DurabilityMode) -> Self {
        match value {
            DurabilityMode::Relaxed => Self::Relaxed,
            DurabilityMode::Strict => Self::Strict,
        }
    }
}

#[derive(Clone, Debug)]
struct WorkloadConfig {
    actor: Identity,
    objects: Vec<Identity>,
    properties: Vec<Symbol>,
    read_ratio: f64,
    read_selector: Symbol,
    update_selector: Symbol,
    actor_role: Symbol,
    item_role: Symbol,
    property_role: Symbol,
    value_role: Symbol,
}

#[derive(Clone, Debug)]
struct Results {
    concurrency: usize,
    ops: usize,
    reads: usize,
    writes: usize,
    retries: usize,
    wall_time: Duration,
    cumulative_time: Duration,
    throughput: f64,
    op_p50: Duration,
    op_p95: Duration,
    op_p99: Duration,
    op_max: Duration,
}

#[derive(Clone, Copy)]
struct ExecutionTarget<'a> {
    runner: &'a Arc<SharedSourceRunner>,
    dispatcher: &'a Dispatcher,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    validate_args(&args)?;

    let mut runner = open_runner(&args)?;
    runner
        .run_filein(&property_update_filein(
            args.num_objects,
            args.num_properties,
        ))
        .map_err(|error| format!("failed to seed property-update world: {error:?}"))?;

    let workload = WorkloadConfig {
        actor: runner
            .named_identity(Symbol::intern("alice"))
            .map_err(|error| format!("failed to resolve #alice: {error:?}"))?,
        objects: (1..=args.num_objects)
            .map(|index| format!("test_object_{index}"))
            .map(|name| {
                runner
                    .named_identity(Symbol::intern(&name))
                    .map_err(|error| format!("failed to resolve #{name}: {error:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        properties: (0..args.num_properties)
            .map(|index| Symbol::intern(&format!("prop_{index}")))
            .collect(),
        read_ratio: args.read_ratio,
        read_selector: Symbol::intern("read_property"),
        update_selector: Symbol::intern("update_property"),
        actor_role: Symbol::intern("actor"),
        item_role: Symbol::intern("item"),
        property_role: Symbol::intern("property"),
        value_role: Symbol::intern("value"),
    };

    let runner = Arc::new(runner.into_shared());
    let (dispatcher_builder, placement) = configure_load_dispatcher(Dispatcher::builder(), &args);
    print_dispatcher_placement(&placement);
    let dispatcher = dispatcher_builder
        .thread_names(|index| format!("mica-property-{index}"))
        .build()
        .map_err(|error| format!("dispatcher failed: {error}"))?;
    let target = ExecutionTarget {
        runner: &runner,
        dispatcher: &dispatcher,
    };
    let results = if args.swamp_mode {
        run_swamp_mode(&args, &workload, target)?
    } else {
        run_stepped_load(&args, &workload, target)?
    };
    write_csv(args.output_file.as_ref(), &results)?;
    Runtime::new()
        .map_err(|error| format!("failed to create compio runtime: {error}"))?
        .block_on(dispatcher.join())
        .map_err(|error| format!("dispatcher shutdown failed: {error}"))?;
    Ok(())
}

fn validate_args(args: &Args) -> Result<(), String> {
    if args.min_concurrency == 0 || args.max_concurrency == 0 {
        return Err("concurrency values must be greater than zero".to_owned());
    }
    if args.min_concurrency > args.max_concurrency {
        return Err("--min-concurrency must be <= --max-concurrency".to_owned());
    }
    if args.num_objects == 0 {
        return Err("--num-objects must be greater than zero".to_owned());
    }
    if args.num_properties == 0 {
        return Err("--num-properties must be greater than zero".to_owned());
    }
    if args.ops_per_worker == 0 {
        return Err("--ops-per-worker must be greater than zero".to_owned());
    }
    if !(0.0..=1.0).contains(&args.read_ratio) {
        return Err("--read-ratio must be between 0.0 and 1.0".to_owned());
    }
    if args.swamp_mode && args.swamp_duration_seconds == 0 {
        return Err("--swamp-duration-seconds must be greater than zero".to_owned());
    }
    Ok(())
}

fn open_runner(args: &Args) -> Result<SourceRunner, String> {
    let runner = match &args.store {
        Some(path) => {
            if path.exists() {
                return Err(format!(
                    "store path {} already exists; use a fresh path for this load test",
                    path.display()
                ));
            }
            SourceRunner::open_fjall(path, args.durability.into())
        }
        None => Ok(SourceRunner::new_empty()),
    }?;
    Ok(runner.with_task_limits(TaskLimits {
        instruction_budget: args.instruction_budget,
        ..TaskLimits::default()
    }))
}

fn configure_load_dispatcher(
    builder: compio::dispatcher::DispatcherBuilder,
    args: &Args,
) -> (compio::dispatcher::DispatcherBuilder, DispatcherPlacement) {
    configure_dispatcher(
        builder,
        DispatcherConfig {
            workers: args.dispatcher_threads,
            affinity: args.dispatcher_affinity.into(),
        },
    )
}

fn print_dispatcher_placement(placement: &DispatcherPlacement) {
    let workers = placement.worker_count.get().to_string();
    if let Some(core_ids) = &placement.pinned_core_ids {
        eprintln!(
            "dispatcher: workers={} affinity=performance cores={:?}",
            workers, core_ids
        );
    } else {
        eprintln!("dispatcher: workers={} affinity=none", workers);
    }
}

fn property_update_filein(num_objects: usize, num_properties: usize) -> String {
    let mut source = String::new();
    source.push_str(
        "make_identity(:player)\n\
         make_identity(:test_object)\n\
         make_identity(:alice)\n",
    );
    for index in 1..=num_objects {
        source.push_str(&format!("make_identity(:test_object_{index})\n"));
    }
    source.push_str(
        "\n\
         make_relation(:Delegates, 3)\n\
         make_functional_relation(:PropertyValue, 3, [0, 1])\n\
         assert Delegates(#alice, #player, 0)\n",
    );
    for index in 1..=num_objects {
        source.push_str(&format!(
            "assert Delegates(#test_object_{index}, #test_object, 0)\n"
        ));
    }
    for index in 1..=num_objects {
        for property in 0..num_properties {
            source.push_str(&format!(
                "assert PropertyValue(#test_object_{index}, :prop_{property}, 0)\n"
            ));
        }
    }
    source.push_str(
        "\n\
         verb read_property(actor @ #player, item @ #test_object, property)\n\
           let exactly {:value -> value} = PropertyValue(item, property, ?value)\n\
           return value\n\
         end\n\
         \n\
         verb update_property(actor @ #player, item @ #test_object, property, value)\n\
           retract PropertyValue(item, property, _)\n\
           assert PropertyValue(item, property, value)\n\
           return value\n\
         end\n",
    );
    source
}

fn run_stepped_load(
    args: &Args,
    workload: &WorkloadConfig,
    target: ExecutionTarget<'_>,
) -> Result<Vec<Results>, String> {
    warm_up(args, workload, target)?;

    let mut rows = Vec::new();
    let mut concurrency = args.min_concurrency as f64;
    while concurrency <= args.max_concurrency as f64 {
        let current = concurrency as usize;
        let result = run_fixed_concurrency(current, args.ops_per_worker, None, workload, target)?;
        print_result(&result);
        rows.push(result);

        let next = (concurrency * 1.25).max(concurrency + 1.0);
        concurrency = next;
    }
    Ok(rows)
}

fn run_swamp_mode(
    args: &Args,
    workload: &WorkloadConfig,
    target: ExecutionTarget<'_>,
) -> Result<Vec<Results>, String> {
    warm_up(args, workload, target)?;
    let result = run_fixed_concurrency(
        args.max_concurrency,
        usize::MAX,
        Some(Duration::from_secs(args.swamp_duration_seconds)),
        workload,
        target,
    )?;
    print_result(&result);
    Ok(vec![result])
}

fn warm_up(
    args: &Args,
    workload: &WorkloadConfig,
    target: ExecutionTarget<'_>,
) -> Result<(), String> {
    if args.warmup_ops == 0 {
        return Ok(());
    }
    let result = run_fixed_concurrency(1, args.warmup_ops, None, workload, target)?;
    eprintln!(
        "warmup: {} ops in {}",
        result.ops,
        format_duration(result.wall_time)
    );
    Ok(())
}

fn run_fixed_concurrency(
    concurrency: usize,
    ops_per_worker: usize,
    duration_limit: Option<Duration>,
    workload: &WorkloadConfig,
    target: ExecutionTarget<'_>,
) -> Result<Results, String> {
    let start = Instant::now();
    let stop_at = duration_limit.map(|duration| start + duration);
    let mut worker_results = Vec::with_capacity(concurrency);

    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(concurrency);
        for worker in 0..concurrency {
            let endpoint = endpoint(worker);
            let runner = Arc::clone(target.runner);
            let dispatcher = target.dispatcher;
            handles.push(scope.spawn(move || {
                run_worker(
                    &runner,
                    dispatcher,
                    workload,
                    endpoint,
                    worker as u64,
                    ops_per_worker,
                    stop_at,
                )
            }));
        }
        for handle in handles {
            worker_results.push(
                handle
                    .join()
                    .map_err(|_| "worker thread panicked".to_owned())??,
            );
        }
        Ok::<(), String>(())
    })?;

    let wall_time = start.elapsed();
    let ops = worker_results.iter().map(|result| result.ops).sum();
    let reads = worker_results.iter().map(|result| result.reads).sum();
    let writes = worker_results.iter().map(|result| result.writes).sum();
    let retries = worker_results.iter().map(|result| result.retries).sum();
    let cumulative_time = worker_results
        .iter()
        .map(|result| result.elapsed)
        .fold(Duration::ZERO, |acc, value| acc + value);
    let mut latencies = worker_results
        .into_iter()
        .flat_map(|result| result.latencies)
        .collect::<Vec<_>>();
    let throughput = if wall_time.is_zero() {
        0.0
    } else {
        ops as f64 / wall_time.as_secs_f64()
    };
    let (p50, p95, p99, max) = percentiles(&mut latencies);

    Ok(Results {
        concurrency,
        ops,
        reads,
        writes,
        retries,
        wall_time,
        cumulative_time,
        throughput,
        op_p50: p50,
        op_p95: p95,
        op_p99: p99,
        op_max: max,
    })
}

#[derive(Debug)]
struct WorkerResult {
    ops: usize,
    reads: usize,
    writes: usize,
    retries: usize,
    elapsed: Duration,
    latencies: Vec<Duration>,
}

fn run_worker(
    runner: &Arc<SharedSourceRunner>,
    dispatcher: &Dispatcher,
    workload: &WorkloadConfig,
    endpoint: Identity,
    seed: u64,
    op_limit: usize,
    stop_at: Option<Instant>,
) -> Result<WorkerResult, String> {
    let wait_runtime =
        Runtime::new().map_err(|error| format!("failed to create worker wait runtime: {error}"))?;
    let start = Instant::now();
    let mut rng = DeterministicRng::new(seed ^ 0x9e37_79b9_7f4a_7c15);
    let mut latencies = Vec::new();
    let mut ops = 0;
    let mut reads = 0;
    let mut writes = 0;
    let mut retries = 0;

    while ops < op_limit && stop_at.is_none_or(|stop_at| Instant::now() < stop_at) {
        let is_read = rng.next_f64() < workload.read_ratio;
        let object = workload.objects[rng.next_index(workload.objects.len())];
        let property = workload.properties[rng.next_index(workload.properties.len())];
        let value = rng.next_i64(1_000_000);
        let request = invocation_request(endpoint, workload, object, property, value, is_read);
        let op_start = Instant::now();
        let runner = Arc::clone(runner);
        let receiver = dispatcher
            .dispatch(move || async move { runner.submit_invocation(request) })
            .map_err(|_| "dispatcher is stopped".to_owned())?;
        let submitted = wait_runtime
            .block_on(receiver)
            .map_err(|_| "dispatched invocation was cancelled".to_owned())?
            .map_err(|error| format!("runtime error: {error:?}"))?;
        latencies.push(op_start.elapsed());
        retries += outcome_retries(&submitted.outcome);
        assert_success(submitted.outcome)?;
        if is_read {
            reads += 1;
        } else {
            writes += 1;
        }
        ops += 1;
    }

    Ok(WorkerResult {
        ops,
        reads,
        writes,
        retries,
        elapsed: start.elapsed(),
        latencies,
    })
}

fn invocation_request(
    endpoint: Identity,
    workload: &WorkloadConfig,
    object: Identity,
    property: Symbol,
    value: i64,
    is_read: bool,
) -> TaskRequest {
    let selector = if is_read {
        workload.read_selector
    } else {
        workload.update_selector
    };
    let mut roles = vec![
        (workload.actor_role, Value::identity(workload.actor)),
        (workload.item_role, Value::identity(object)),
        (workload.property_role, Value::symbol(property)),
    ];
    if !is_read {
        roles.push((
            workload.value_role,
            Value::int(value).expect("generated value out of range"),
        ));
    }
    TaskRequest {
        principal: None,
        actor: None,
        endpoint,
        authority: AuthorityContext::root(),
        input: TaskInput::Invocation { selector, roles },
    }
}

fn assert_success(outcome: TaskOutcome) -> Result<(), String> {
    match outcome {
        TaskOutcome::Complete { .. } => Ok(()),
        TaskOutcome::Aborted { error, .. } => Err(format!("task aborted: {error:?}")),
        TaskOutcome::Suspended { kind, .. } => {
            Err(format!("task suspended unexpectedly: {kind:?}"))
        }
    }
}

fn outcome_retries(outcome: &TaskOutcome) -> usize {
    match outcome {
        TaskOutcome::Complete { retries, .. }
        | TaskOutcome::Suspended { retries, .. }
        | TaskOutcome::Aborted { retries, .. } => usize::from(*retries),
    }
}

#[derive(Clone, Copy, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    fn next_index(&mut self, len: usize) -> usize {
        (self.next_u64() as usize) % len
    }

    fn next_i64(&mut self, max: i64) -> i64 {
        (self.next_u64() % max as u64) as i64
    }

    fn next_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / ((1u64 << 53) as f64);
        ((self.next_u64() >> 11) as f64) * SCALE
    }
}

fn endpoint(worker: usize) -> Identity {
    Identity::new(ENDPOINT_ID_START + worker as u64).expect("worker endpoint identity out of range")
}

fn percentiles(latencies: &mut [Duration]) -> (Duration, Duration, Duration, Duration) {
    if latencies.is_empty() {
        return (
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        );
    }
    latencies.sort_unstable();
    (
        percentile(latencies, 0.50),
        percentile(latencies, 0.95),
        percentile(latencies, 0.99),
        *latencies.last().unwrap(),
    )
}

fn percentile(sorted: &[Duration], percentile: f64) -> Duration {
    let index = ((sorted.len() - 1) as f64 * percentile).round() as usize;
    sorted[index]
}

fn print_result(result: &Results) {
    let retry_pct = if result.ops + result.retries == 0 {
        0.0
    } else {
        (result.retries as f64 / (result.ops + result.retries) as f64) * 100.0
    };
    println!(
        "conc={} ops={} reads={} writes={} retries={} retry_pct={:.2}% wall={} cumulative={} throughput={:.2}/s op_p50={} p95={} p99={} max={}",
        result.concurrency,
        result.ops,
        result.reads,
        result.writes,
        result.retries,
        retry_pct,
        format_duration(result.wall_time),
        format_duration(result.cumulative_time),
        result.throughput,
        format_duration(result.op_p50),
        format_duration(result.op_p95),
        format_duration(result.op_p99),
        format_duration(result.op_max),
    );
}

fn write_csv(output_file: Option<&PathBuf>, results: &[Results]) -> Result<(), String> {
    let Some(output_file) = output_file else {
        return Ok(());
    };
    let mut output =
        "concurrency,ops,reads,writes,retries,wall_time_ns,cumulative_time_ns,throughput_per_sec,op_p50_ns,op_p95_ns,op_p99_ns,op_max_ns\n"
            .to_owned();
    for result in results {
        output.push_str(&format!(
            "{},{},{},{},{},{},{},{:.0},{},{},{},{}\n",
            result.concurrency,
            result.ops,
            result.reads,
            result.writes,
            result.retries,
            result.wall_time.as_nanos(),
            result.cumulative_time.as_nanos(),
            result.throughput,
            result.op_p50.as_nanos(),
            result.op_p95.as_nanos(),
            result.op_p99.as_nanos(),
            result.op_max.as_nanos(),
        ));
    }
    fs::write(output_file, output)
        .map_err(|error| format!("failed to write {}: {error}", output_file.display()))
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.3}s", duration.as_secs_f64())
    } else if duration.as_millis() > 0 {
        format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
    } else if duration.as_micros() > 0 {
        format!("{:.3}us", duration.as_secs_f64() * 1_000_000.0)
    } else {
        format!("{}ns", duration.as_nanos())
    }
}
