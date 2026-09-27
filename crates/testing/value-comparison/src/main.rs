// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

mod cases;
mod symbol_loads;
mod symbols;

use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::io::Write;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use mica_runtime::{SourceRunner, TaskLimits, TaskOutcome};
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestError, TestRunner};
use serde_json::json;

use cases::{Case, Prepared};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(about = "Compare Rust values with Mica-generated C values")]
struct Options {
    #[arg(long, default_value_t = 256)]
    cases: u32,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Generate only UTF-8 and string property cases.
    #[arg(long)]
    strings: bool,
    /// Generate only list and map property cases.
    #[arg(long, conflicts_with_all = ["strings", "symbol_loads"])]
    collections: bool,
    /// Generate only relation property cases.
    #[arg(long, conflicts_with_all = ["strings", "collections", "symbol_loads"])]
    relations: bool,
    /// Run paired timings after correctness checks. Requires a release build.
    #[arg(long)]
    bench: bool,
    #[arg(long, default_value = "7")]
    samples: NonZeroU32,
    /// Minimum repetitions before duration calibration.
    #[arg(long, default_value = "1")]
    iterations: NonZeroU64,
    /// Target sample duration; symbol loads calibrate each implementation independently.
    #[arg(long, default_value = "100")]
    sample_ms: NonZeroU32,
    /// Check generated C with address and undefined-behaviour sanitizers.
    #[arg(long, conflicts_with = "bench")]
    sanitize: bool,
    /// Check C symbol loads with ThreadSanitizer.
    #[arg(long, conflicts_with_all = ["bench", "sanitize"])]
    thread_sanitize: bool,
    /// Run only symbol concurrency checks and benchmarks.
    #[arg(long, conflicts_with = "strings")]
    symbol_loads: bool,
    /// Worker counts for shared-table symbol loads.
    #[arg(long, value_delimiter = ',', default_value = "1,2,4,8")]
    symbol_threads: Vec<NonZeroUsize>,
    /// Replay a JSON concurrent symbol load.
    #[arg(long)]
    symbol_load_case: Option<String>,
    /// Replay one JSON case printed by a failed property check.
    #[arg(long)]
    case: Option<String>,
    /// Replay a JSON array of byte strings from a failed symbol sequence.
    #[arg(long)]
    symbol_case: Option<String>,
    #[arg(long, hide = true)]
    symbol_load_worker: bool,
    #[arg(long, hide = true)]
    symbol_load_verify: bool,
    /// Save the generated standalone C source at this path.
    #[arg(long)]
    emit_c: Option<PathBuf>,
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mica-value-comparison-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy)]
enum Sanitizer {
    None,
    Address,
    Thread,
}

struct Native {
    scratch: Scratch,
    compiler: String,
}
impl Native {
    fn build(sanitizer: Sanitizer, emit_c: Option<&Path>) -> Result<Self> {
        let mut runner = SourceRunner::new_empty()
            .with_interpreter_only(true)
            .with_task_limits(TaskLimits {
                instruction_budget: 500_000_000,
                ..TaskLimits::default()
            });
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for source in [
            "ir",
            "builders",
            "types",
            "numeric",
            "layout",
            "storage",
            "contracts",
            "check",
            "c",
            "value/program",
            "value/immediates",
            "value/numbers",
            "value/arena",
            "value/heap",
            "value/utf8",
            "value/strings",
            "value/symbols",
            "value/string_append",
            "value/string_search",
            "value/compare",
            "value/maps",
            "value/collections",
            "value/relations",
        ] {
            let text = fs::read_to_string(root.join(format!("apps/native/{source}.mica")))?;
            let reports = runner
                .run_filein(&text)
                .map_err(|e| runner.render_source_task_error(&e))?;
            for report in reports {
                if !matches!(report.outcome, TaskOutcome::Complete { .. }) {
                    return Err(report.render().into());
                }
            }
        }
        let report = runner
            .run_source("return native/emit_c(native_value/program())")
            .map_err(|e| runner.render_source_task_error(&e))?;
        let TaskOutcome::Complete { ref value, .. } = report.outcome else {
            return Err(report.render().into());
        };
        let generated = value
            .with_str(str::to_owned)
            .ok_or("C generator did not return source")?;
        let scratch = Scratch::new()?;
        fs::write(scratch.0.join("value.c"), &generated)?;
        if let Some(path) = emit_c {
            fs::write(path, &generated)?;
        }
        fs::copy(
            root.join("native/platform/allocation.c"),
            scratch.0.join("allocation.c"),
        )?;
        fs::copy(
            root.join("native/platform/mutex.c"),
            scratch.0.join("mutex.c"),
        )?;
        fs::write(
            scratch.0.join("symbol_loads.c"),
            include_str!("symbol_loads.c"),
        )?;
        fs::write(scratch.0.join("driver.c"), include_str!("driver.c"))?;
        let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let version = Command::new(&cc).arg("--version").output()?;
        if !version.status.success() {
            return Err("C compiler version query failed".into());
        }
        let compiler = String::from_utf8_lossy(&version.stdout)
            .lines()
            .next()
            .unwrap_or("unknown")
            .to_owned();
        let mut command = Command::new(&cc);
        command.current_dir(&scratch.0).args([
            "-std=c11",
            "-pthread",
            "-O3",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pedantic",
            "-ffp-contract=off",
            "-fno-fast-math",
        ]);
        if !matches!(sanitizer, Sanitizer::None) {
            command.args([
                "-g",
                match sanitizer {
                    Sanitizer::Address => "-fsanitize=address,undefined,float-cast-overflow",
                    Sanitizer::Thread => "-fsanitize=thread",
                    Sanitizer::None => unreachable!(),
                },
                "-fno-sanitize-recover=all",
                "-fno-omit-frame-pointer",
            ]);
        }
        let output = command
            .args(["driver.c", "-lm", "-o", "compare"])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "C compilation failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(Self { scratch, compiler })
    }

    fn run(&self, cases: &[Case], iterations: u64) -> Result<Vec<u8>> {
        let mut input = Vec::new();
        input.extend((cases.len() as u64).to_le_bytes());
        for case in cases {
            case.encode(&mut input);
        }
        let mut child = Command::new(self.scratch.0.join("compare"))
            .current_dir(&self.scratch.0)
            .arg(iterations.to_string())
            .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let write_result = child
            .stdin
            .take()
            .ok_or("missing C driver input pipe")?
            .write_all(&input);
        let output = child.wait_with_output()?;
        if !output.status.success() || !output.stderr.is_empty() {
            return Err(format!(
                "C driver {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        write_result?;
        Ok(output.stdout)
    }

    fn check(&self, cases: &[Case]) -> Result<()> {
        let mut expected = Vec::new();
        for case in cases {
            case.prepare().run().encode(&mut expected);
        }
        let actual = self.run(cases, 0)?;
        if actual != expected {
            let offset = actual
                .iter()
                .zip(&expected)
                .position(|(a, b)| a != b)
                .unwrap_or(actual.len().min(expected.len()));
            return Err(format!(
                "semantic result mismatch at byte {offset}: C {:?}, Rust {:?}",
                actual.get(offset..(offset + 24).min(actual.len())),
                expected.get(offset..(offset + 24).min(expected.len()))
            )
            .into());
        }
        Ok(())
    }

    fn sample(&self, cases: &[Case], rounds: u64) -> Result<(f64, u64)> {
        let output = String::from_utf8(self.run(cases, rounds)?)?;
        let fields: Vec<_> = output.split_whitespace().collect();
        if fields.len() != 2 {
            return Err(format!("invalid timing response: {output}").into());
        }
        Ok((
            fields[0].parse::<u64>()? as f64 / (rounds as f64 * cases.len() as f64),
            fields[1].parse()?,
        ))
    }
}

fn check_generated(
    native: &Native,
    count: u32,
    seed: u64,
    strings: bool,
    collections: bool,
    relations: bool,
) -> Result<()> {
    let mut runner = TestRunner::new(Config {
        cases: count,
        rng_seed: RngSeed::Fixed(seed),
        failure_persistence: None,
        max_shrink_iters: 2048,
        ..Config::default()
    });
    let strategy = if relations {
        cases::relation_strategy()
    } else if collections {
        cases::collection_strategy()
    } else if strings {
        cases::string_strategy()
    } else {
        cases::strategy()
    };
    match runner.run(&strategy, |case| {
        native
            .check(std::slice::from_ref(&case))
            .map_err(|e| TestCaseError::fail(e.to_string()))
    }) {
        Ok(()) => Ok(()),
        Err(TestError::Fail(reason, case)) => Err(format!(
            "{reason}\nseed={seed}\nReplay with --case '{}':\n{}",
            serde_json::to_string(&case)?,
            serde_json::to_string_pretty(&case)?
        )
        .into()),
        Err(error) => Err(error.to_string().into()),
    }
}

fn rust_sample(cases: &[Prepared], rounds: u64) -> (f64, u64) {
    for case in cases {
        black_box(case.run());
    }
    let start = Instant::now();
    let mut digest = 0u64;
    for _ in 0..rounds {
        for case in cases {
            let result = case.run();
            digest = digest.wrapping_add(black_box(result.checksum()));
            // Result destruction is inside the measured interval.
            drop(result);
        }
    }
    (
        start.elapsed().as_nanos() as f64 / (rounds as f64 * cases.len() as f64),
        digest,
    )
}

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}

fn benchmark_metadata(
    native: &Native,
    samples: u32,
    minimum_rounds: u64,
    sample_ms: u32,
) -> Result<()> {
    let rustc = Command::new("rustc").arg("--version").output()?;
    let affinity = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("Cpus_allowed_list:")
                    .map(|value| value.trim().to_owned())
            })
        });
    println!(
        "{}",
        json!({"metadata": {"c_compiler":native.compiler,"rust_compiler":String::from_utf8_lossy(&rustc.stdout).trim(),
        "c_flags":"-std=c11 -pthread -O3 -ffp-contract=off -fno-fast-math", "rust_profile":"release",
        "os":std::env::consts::OS,"arch":std::env::consts::ARCH,"cpu_affinity":affinity,"samples":samples,"minimum_rounds":minimum_rounds,"target_sample_ms":sample_ms,
        "rustflags":std::env::var("RUSTFLAGS").unwrap_or_default(),
        "timing":"monotonic wall time; input preparation and subprocess startup excluded; alternating implementation order"}})
    );
    Ok(())
}

fn benchmark(native: &Native, samples: u32, minimum_rounds: u64, sample_ms: u32) -> Result<()> {
    benchmark_metadata(native, samples, minimum_rounds, sample_ms)?;
    for (name, cases) in cases::workloads() {
        native.check(&cases)?;
        let prepared: Vec<_> = cases.iter().map(Case::prepare).collect();
        let mut rounds = minimum_rounds;
        // Calibrate both implementations; the faster one sets the repetition
        // count, so tiny scalar operations do not become microsecond samples.
        for _ in 0..4 {
            let rust = rust_sample(&prepared, rounds);
            let c = native.sample(&cases, rounds)?;
            if rust.1 != c.1 {
                return Err(format!("calibration checksum mismatch in {name}").into());
            }
            let elapsed = rust.0.min(c.0) * rounds as f64 * cases.len() as f64;
            let target = f64::from(sample_ms) * 1_000_000.0;
            if elapsed >= target {
                break;
            }
            let desired = (rounds as f64 * target / elapsed.max(1.0) * 1.1).ceil();
            if desired > (u64::MAX / cases.len() as u64) as f64 {
                return Err("benchmark repetition count overflow".into());
            }
            rounds = desired as u64;
        }
        let mut rust_ns = Vec::new();
        let mut c_ns = Vec::new();
        for sample in 0..samples {
            let (rust, c) = if sample % 2 == 0 {
                let rust = rust_sample(&prepared, rounds);
                (rust, native.sample(&cases, rounds)?)
            } else {
                let c = native.sample(&cases, rounds)?;
                (rust_sample(&prepared, rounds), c)
            };
            if rust.1 != c.1 {
                return Err(format!(
                    "benchmark checksum mismatch in {name}: Rust {}, C {}",
                    rust.1, c.1
                )
                .into());
            }
            rust_ns.push(rust.0);
            c_ns.push(c.0);
        }
        println!(
            "{}",
            json!({"workload":name,"operations_per_sample":rounds*cases.len() as u64,
            "rust_ns_per_op":rust_ns,"c_ns_per_op":c_ns,"rust_median_ns":median(&rust_ns),"c_median_ns":median(&c_ns),
            "c_over_rust":median(&c_ns)/median(&rust_ns)})
        );
    }
    Ok(())
}

fn run() -> Result<()> {
    let options = Options::parse();
    if options.symbol_load_worker {
        return symbol_loads::worker(if options.symbol_load_verify {
            0
        } else {
            options.iterations.get()
        });
    }
    if options.bench && cfg!(debug_assertions) {
        return Err("benchmarking requires cargo run --release".into());
    }
    let sanitizer = if options.thread_sanitize {
        Sanitizer::Thread
    } else if options.sanitize {
        Sanitizer::Address
    } else {
        Sanitizer::None
    };
    let native = Native::build(sanitizer, options.emit_c.as_deref())?;
    let threads: Vec<_> = options.symbol_threads.iter().map(|n| n.get()).collect();
    if let Some(case) = &options.symbol_load_case {
        symbol_loads::replay(&native, case)?;
        println!("{}", json!({"symbol_load_replay":"passed"}));
        return Ok(());
    }
    if options.symbol_loads {
        symbol_loads::check(&native, options.cases, options.seed, &threads)?;
        println!(
            "{}",
            json!({"symbol_load_correctness":"passed","threads":threads,"cases":options.cases,"seed":options.seed,
            "sanitizers":options.sanitize,"thread_sanitizer":options.thread_sanitize})
        );
        if options.bench {
            benchmark_metadata(
                &native,
                options.samples.get(),
                options.iterations.get(),
                options.sample_ms.get(),
            )?;
            symbol_loads::benchmark(
                &native,
                &threads,
                options.seed,
                options.samples.get(),
                options.iterations.get(),
                options.sample_ms.get(),
            )?;
        }
        return Ok(());
    }
    if let Some(case) = &options.symbol_case {
        symbols::replay(&native, case)?;
        println!("{}", json!({"symbol_replay":"passed"}));
        return Ok(());
    }
    if let Some(case) = options.case {
        native.check(&[serde_json::from_str(&case)?])?;
        println!("{}", json!({"replay":"passed"}));
        return Ok(());
    }
    let fixed = cases::fixed_cases();
    // Keep individual failures directly replayable rather than reporting a byte
    // offset into an entire corpus without identifying its case.
    for case in &fixed {
        native
            .check(std::slice::from_ref(case))
            .map_err(|e| format!("{e}\ncase={}", serde_json::to_string(case).unwrap()))?;
    }
    check_generated(
        &native,
        options.cases,
        options.seed,
        options.strings,
        options.collections,
        options.relations,
    )?;
    symbols::check(&native, options.cases, options.seed)?;
    symbol_loads::check(&native, options.cases, options.seed, &threads)?;
    for (_, cases) in cases::workloads() {
        native.check(&cases)?;
    }
    println!(
        "{}",
        json!({"correctness":"passed","fixed_cases":fixed.len(),"generated_cases":options.cases,"string_corpus":options.strings,"collection_corpus":options.collections,"relation_corpus":options.relations,"symbol_sequences":options.cases,"symbol_load_cases":options.cases,"symbol_threads":threads,"seed":options.seed,"sanitizers":options.sanitize,"thread_sanitizer":options.thread_sanitize})
    );
    if options.bench {
        benchmark(
            &native,
            options.samples.get(),
            options.iterations.get(),
            options.sample_ms.get(),
        )?;
        symbol_loads::benchmark(
            &native,
            &threads,
            options.seed,
            options.samples.get(),
            options.iterations.get(),
            options.sample_ms.get(),
        )?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_shared_values_and_collections() -> Result<()> {
        let native = Native::build(Sanitizer::Address, None)?;
        native.check(&cases::fixed_cases())?;
        check_generated(&native, 64, 1, false, false, false)?;
        check_generated(&native, 256, 41, false, false, true)?;
        check_generated(&native, 256, 17, false, true, false)?;
        check_generated(&native, 128, 1, true, false, false)?;
        symbols::check(&native, 128, 1)?;
        symbol_loads::check(&native, 16, 1, &[1, 2, 4, 8])?;
        symbol_loads::detects_incorrect_metadata(&native)?;
        for (_, cases) in cases::workloads() {
            native.check(&cases)?;
        }
        Ok(())
    }
}
