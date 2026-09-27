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
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// Expected result as a JSON value. Every invocation must match.
    #[arg(long)]
    pub expected: Option<String>,
    #[arg(long, default_value = "bench")]
    pub selector: String,
    /// Invoke setup() once before warmup and measurement.
    #[arg(long)]
    pub setup: bool,
    #[arg(long, default_value = "21")]
    pub samples: NonZeroUsize,
    #[arg(long, default_value = "8")]
    pub iterations: NonZeroUsize,
    /// Calibrate invocations per sample to this duration instead of using --iterations.
    #[arg(long, conflicts_with = "iterations")]
    pub budget_ms: Option<NonZeroUsize>,
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
    let expected = options
        .expected
        .as_deref()
        .map(expected_value)
        .transpose()?;
    for file in &options.files {
        run_file(cli, options, file, expected.as_ref()).await?;
    }
    Ok(())
}

async fn run_file(
    cli: &Cli,
    options: &Options,
    file: &std::path::Path,
    expected: Option<&Value>,
) -> Result<(), String> {
    let limits = TaskLimits {
        instruction_budget: 100_000_000,
        max_call_depth: 1024,
        ..TaskLimits::default()
    };
    let mut runner = open_runner(cli)?
        .with_interpreter_only(matches!(options.tier, Tier::Interpreter))
        .with_task_limits(limits);
    let source = fs::read_to_string(file).map_err(|error| error.to_string())?;
    let base = file.parent().unwrap_or_else(|| std::path::Path::new("."));
    for report in runner
        .run_filein_with_include_loader(&source, |path| crate::read_filein_include(base, path))
        .map_err(format_source_error)?
    {
        if !matches!(report.outcome, TaskOutcome::Complete { .. }) {
            return Err(format!("filein did not complete: {:?}", report.outcome));
        }
        runner.forget_terminal_task(report.task_id);
    }
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
    let result = measure(&administrator, &mut pump, options, expected).await;
    let shutdown = owner
        .shutdown(&mut pump, |_| {})
        .await
        .map_err(format_driver_error);
    let (samples, iterations) = result?;
    shutdown?;
    println!(
        "{}",
        json!({
            "format": 1,
            "implementation": "rust",
            "fixture": file,
            "result_validated": expected.is_some(),
            "calibration_invocations": usize::from(options.budget_ms.is_some()),
            "budget_ms": options.budget_ms.map(NonZeroUsize::get),
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
            "iterations_per_sample": iterations,
            "timed_invocations": options.samples.get().checked_mul(iterations).ok_or("invocation count overflow")?,
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
    expected: Option<&Value>,
) -> Result<(Vec<u64>, usize), String> {
    if options.setup {
        invoke(administrator, pump, Symbol::intern("setup")).await?;
    }
    let selector = Symbol::intern(&options.selector);
    for _ in 0..options.warmup {
        verify(invoke(administrator, pump, selector).await?, expected)?;
    }
    let iterations = if let Some(budget_ms) = options.budget_ms {
        let start = Instant::now();
        verify(invoke(administrator, pump, selector).await?, expected)?;
        calibrate(start.elapsed().as_nanos(), budget_ms.get())?
    } else {
        options.iterations.get()
    };
    let mut samples = Vec::with_capacity(options.samples.get());
    for _ in 0..options.samples.get() {
        let start = Instant::now();
        for _ in 0..iterations {
            verify(invoke(administrator, pump, selector).await?, expected)?;
        }
        samples
            .push(u64::try_from(start.elapsed().as_nanos()).map_err(|_| "elapsed time overflow")?);
    }
    Ok((samples, iterations))
}

fn verify(actual: Value, expected: Option<&Value>) -> Result<(), String> {
    if let Some(expected) = expected
        && &actual != expected
    {
        return Err(format!(
            "result mismatch: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

fn calibrate(one_call_ns: u128, budget_ms: usize) -> Result<usize, String> {
    let count = (budget_ms as u128 * 1_000_000 / one_call_ns.max(1)).max(1);
    usize::try_from(count).map_err(|_| "calibrated invocation count overflow".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_uses_target_and_floor() {
        assert_eq!(calibrate(2_000_000, 20).unwrap(), 10);
        assert_eq!(calibrate(1_000_000_000, 1).unwrap(), 1);
    }
}
