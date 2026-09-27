use crate::{Cli, format_driver_error, format_source_error, open_runner};
use clap::{Args, ValueEnum};
use mica_driver::{
    DriverAdministrator, DriverEventPump, DriverOwner, DriverResources, InvocationOutcome,
};
use mica_runtime::{TaskLimits, TaskOutcome};
use mica_var::{Symbol, Value};
use serde_json::json;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Tier {
    Interpreter,
    Native,
}

#[derive(Args)]
pub struct Options {
    pub file: PathBuf,
    /// Expected result as a JSON value. Every invocation must match.
    #[arg(long)]
    pub expected: String,
    #[arg(long, default_value = "bench")]
    pub selector: String,
    /// Invoke setup() once before warmup and measurement.
    #[arg(long)]
    pub setup: bool,
    #[arg(long, default_value = "21")]
    pub samples: NonZeroUsize,
    #[arg(long, default_value = "8")]
    pub iterations: NonZeroUsize,
    #[arg(long, default_value_t = 2)]
    pub warmup: usize,
    #[arg(long, default_value = "1")]
    pub workers: NonZeroUsize,
    #[arg(long, value_enum, default_value = "interpreter")]
    pub tier: Tier,
}

pub async fn run(cli: &Cli, options: &Options) -> Result<(), String> {
    if cli.actor.is_some() {
        return Err("bench uses administrative authority; --actor is not supported".to_owned());
    }
    let limits = TaskLimits {
        instruction_budget: 100_000_000,
        max_call_depth: 1024,
        ..TaskLimits::default()
    };
    let mut runner = open_runner(cli)?
        .with_interpreter_only(matches!(options.tier, Tier::Interpreter))
        .with_task_limits(limits);
    let source = fs::read_to_string(&options.file).map_err(|error| error.to_string())?;
    let base = options
        .file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for report in runner
        .run_filein_with_include_loader(&source, |path| crate::read_filein_include(base, path))
        .map_err(format_source_error)?
    {
        if !matches!(report.outcome, TaskOutcome::Complete { .. }) {
            return Err(format!("filein did not complete: {:?}", report.outcome));
        }
        runner.forget_terminal_task(report.task_id);
    }
    let expected = expected_value(&options.expected)?;
    let mut resources = DriverResources::new(options.workers);
    resources.task_limits = limits;
    // CPU serial relation execution makes worker scaling independent of query parallelism.
    resources.relation_parallelism = NonZeroUsize::MIN;
    let mut owner = DriverOwner::builder(resources)
        .source_runner(runner)
        .build()
        .map_err(format_driver_error)?;
    let mut pump = owner.take_event_pump().map_err(format_driver_error)?;
    let administrator = owner.administrator();
    let result = measure(&administrator, &mut pump, options, &expected).await;
    let shutdown = owner
        .shutdown(&mut pump, |_| {})
        .await
        .map_err(format_driver_error);
    let samples = result?;
    shutdown?;
    println!(
        "{}",
        json!({
            "format": 1,
            "implementation": "rust",
            "fixture": options.file,
            "selector": options.selector,
            "expected": options.expected,
            "tier": match options.tier { Tier::Interpreter => "interpreter", Tier::Native => "native-enabled" },
            "workers": options.workers.get(),
            "relation_parallelism": 1,
            "accelerator": "disabled",
            "accelerator_placements": 0,
            "storage": match cli.storage { crate::StorageMode::Memory => "memory", crate::StorageMode::Fjall => "fjall" },
            "durability": match cli.storage {
                crate::StorageMode::Memory => "none",
                crate::StorageMode::Fjall => match cli.durability {
                    crate::DurabilityMode::Relaxed => "relaxed",
                    crate::DurabilityMode::Strict => "strict",
                },
            },
            "authority": "root",
            "instruction_budget": limits.instruction_budget,
            "max_call_depth": limits.max_call_depth,
            "warmup_invocations": options.warmup,
            "iterations_per_sample": options.iterations.get(),
            "timed_invocations": options.samples.get().checked_mul(options.iterations.get()).ok_or("invocation count overflow")?,
            "sample_elapsed_ns": samples,
        })
    );
    Ok(())
}

fn expected_value(literal: &str) -> Result<Value, String> {
    mica_runtime::value_from_json_text(literal)
        .map_err(|error| format!("invalid expected JSON: {error:?}"))
}

async fn invoke(
    administrator: &DriverAdministrator,
    pump: &mut DriverEventPump,
    selector: Symbol,
) -> Result<Value, String> {
    let handle = pump
        .drive_until(administrator.invoke(selector, Vec::new()), |_| {})
        .await
        .map_err(format_driver_error)?;
    match pump.drive_until(handle.wait(), |_| {}).await {
        InvocationOutcome::Completed(value) => Ok(value),
        outcome => Err(format!(
            "benchmark invocation did not complete: {outcome:?}"
        )),
    }
}

async fn measure(
    administrator: &DriverAdministrator,
    pump: &mut DriverEventPump,
    options: &Options,
    expected: &Value,
) -> Result<Vec<u64>, String> {
    if options.setup {
        invoke(administrator, pump, Symbol::intern("setup")).await?;
    }
    let selector = Symbol::intern(&options.selector);
    for _ in 0..options.warmup {
        verify(invoke(administrator, pump, selector).await?, expected)?;
    }
    let mut samples = Vec::with_capacity(options.samples.get());
    for _ in 0..options.samples.get() {
        let start = Instant::now();
        for _ in 0..options.iterations.get() {
            verify(invoke(administrator, pump, selector).await?, expected)?;
        }
        samples
            .push(u64::try_from(start.elapsed().as_nanos()).map_err(|_| "elapsed time overflow")?);
    }
    Ok(samples)
}

fn verify(actual: Value, expected: &Value) -> Result<(), String> {
    if &actual != expected {
        return Err(format!(
            "result mismatch: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}
