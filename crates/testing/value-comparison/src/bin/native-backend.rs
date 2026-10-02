// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
use std::fs;
use std::num::NonZeroU32;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use clap::Parser;
use mica_runtime::{SourceRunner, TaskLimits, TaskOutcome};
use mica_var::{Symbol, Value};

#[derive(Parser)]
#[command(about = "Bootstrap the Mica-generated libgccjit backend and compile IR modules")]
struct Options {
    #[arg(long, default_value = "target/native-gccjit")]
    output: PathBuf,
    #[arg(long)]
    sanitize: bool,
    /// Reuse the generated backend executable; still regenerate module IR data.
    #[arg(long)]
    reuse_backend: bool,
    #[arg(long, default_value = "native/scalar_example()")]
    module: String,
    /// Check the backends, managed heap, values, and tail-call execution.
    #[arg(long, conflicts_with_all = ["module", "load_symbol"])]
    check_fixtures: bool,
    /// Run one named fixture instead of the complete fixture suite.
    #[arg(long, requires = "check_fixtures")]
    fixture: Option<String>,
    /// Rebuild B1/B2, qualify both, and measure repeated compiler lifecycles.
    #[arg(long, conflicts_with_all = ["module", "load_symbol", "check_fixtures", "reuse_backend"])]
    self_rebuild: bool,
    /// Compiler lifecycles per dataset; sanitized runs exercise only the callable fixture.
    #[arg(long, default_value = "40", requires = "self_rebuild")]
    lifecycle_rounds: NonZeroU32,
    /// Also compile a shared library, load it, and resolve this exported symbol.
    #[arg(long)]
    load_symbol: Option<String>,
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn eval(runner: &mut SourceRunner, source: &str) -> Result<Value> {
    let report = runner
        .run_source(source)
        .map_err(|e| runner.render_source_task_error(&e))?;
    let TaskOutcome::Complete { value, .. } = report.outcome else {
        return Err(report.render().into());
    };
    Ok(value)
}

fn text(value: &Value) -> Result<String> {
    value
        .with_str(str::to_owned)
        .ok_or_else(|| "expected generated string".into())
}

fn run() -> Result<()> {
    let options = Options::parse();
    if options.lifecycle_rounds.get() > 1000 {
        return Err("--lifecycle-rounds must be between 1 and 1000".into());
    }
    fs::create_dir_all(&options.output)?;
    if options.self_rebuild {
        let summary = options.output.join("self-rebuild.json");
        if summary.exists() {
            fs::remove_file(summary)?;
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut runner = SourceRunner::new_empty()
        .with_interpreter_only(true)
        .with_task_limits(TaskLimits {
            // Kernel generation includes rule maintenance and backend IR export.
            instruction_budget: 12_000_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
    for source in [
        "ir",
        "builders",
        "generation",
        "sequences",
        "types",
        "numeric",
        "layout",
        "storage",
        "contracts",
        "execution",
        "check",
        "flow",
        "c_syntax",
        "c_scopes",
        "c_flow",
        "c",
        "examples/scalars",
        "examples/values",
        "tests/callables",
        "tests/surface",
        "value/program",
        "memory/program",
        "memory/allocation",
        "memory/collection",
        "memory/lifecycle",
        "memory/platform",
        "memory/tests",
        "tests/execution",
        "value/immediates",
        "value/numbers",
        "value/symbol_storage",
        "value/memory",
        "value/tracing",
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
        "value/hash",
        "value/copy",
        "value/buffer",
        "value/codec",
        "value/codec_decode",
        "value/persistence",
        "memory/roots",
        "rules/schema",
        "rules/builders",
        "rules/validate",
        "rules/stratify",
        "rules/catalog",
        "rules/program",
        "kernel/program",
        "kernel/execution",
        "kernel/authority",
        "kernel/policy",
        "kernel/indexes",
        "kernel/relations",
        "kernel/transactions",
        "kernel/rule_indexes",
        "kernel/rules",
        "kernel/lifecycle",
        "query/builders",
        "query/roots",
        "query/indexes",
        "query/operators",
        "query/sources",
        "query/probes",
        "query/program",
        "rules/execution_builders",
        "rules/lower",
        "rules/bindings",
        "rules/evaluate",
        "rules/maintain",
        "../compiler/lex",
        "../compiler/parse",
        "../compiler/ast",
        "value/literals",
        "rules/source",
        "rules/install",
        "tests/rule_sources",
        "gccjit/data",
        "gccjit/bindings",
        "gccjit/program",
        "gccjit/declarations",
        "gccjit/helpers",
        "gccjit/operations",
        "gccjit/checked",
        "gccjit/instructions",
    ] {
        let source_text = fs::read_to_string(root.join(format!("apps/native/{source}.mica")))?;
        let reports = runner
            .run_filein(&source_text)
            .map_err(|e| format!("{source}: {}", runner.render_source_task_error(&e)))?;
        for report in reports {
            if !matches!(report.outcome, TaskOutcome::Complete { .. }) {
                return Err(format!("{source}: {}", report.render()).into());
            }
        }
    }
    if !options.reuse_backend {
        bootstrap(&mut runner, &options, &root)?;
    }
    if options.self_rebuild {
        return self_rebuild(&mut runner, &options, &root);
    }
    if options.check_fixtures {
        return check_fixtures(
            &mut runner,
            &options,
            &root,
            &options.output.join("backend"),
        );
    }
    generate_module(&mut runner, &options.module, &options.output, "module")?;
    let backend = options.output.join("backend");
    compile_module(&backend, &options.output, options.sanitize, None)?;
    if let Some(symbol) = &options.load_symbol {
        compile_module(&backend, &options.output, options.sanitize, Some(symbol))?;
    }
    Ok(())
}

fn compiler(sanitize: bool) -> Command {
    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let mut command = Command::new(cc);
    command.args([
        "-std=c11",
        "-O2",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-ffp-contract=off",
        "-fno-fast-math",
    ]);
    if let Some(include) = std::env::var_os("GCCJIT_INCLUDE") {
        command.arg("-I").arg(include);
    }
    if sanitize {
        command.args([
            "-fsanitize=address,undefined,float-cast-overflow",
            "-fno-sanitize-recover=all",
        ]);
    }
    command
}

fn checked_command(command: &mut Command) -> Result<()> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "{command:?}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn bootstrap(runner: &mut SourceRunner, options: &Options, root: &Path) -> Result<()> {
    generate_module(runner, "native_jit/program()", &options.output, "backend")?;
    let bindings = text(&eval(runner, "return native_jit/bindings_c()")?)?;
    let driver = text(&eval(runner, "return native_jit/driver_c()")?)?;
    let memory = text(&eval(runner, "return native_memory/platform()")?)?;
    fs::write(
        options.output.join("support.c"),
        format!(
            "#define _POSIX_C_SOURCE 200809L\n#include \"backend.h\"\n{bindings}\n{driver}\n{memory}"
        ),
    )?;
    for (name, source) in [
        ("backend", options.output.join("backend.c")),
        ("support", options.output.join("support.c")),
        ("allocation", root.join("native/platform/allocation.c")),
        ("mutex", root.join("native/platform/mutex.c")),
    ] {
        checked_command(
            compiler(options.sanitize)
                .arg("-c")
                .arg(source)
                .arg("-o")
                .arg(options.output.join(format!("{name}.o"))),
        )?;
    }
    link_backend(
        &options.output,
        &options.output.join("backend.o"),
        &options.output.join("backend"),
        options.sanitize,
    )
}

fn link_backend(support: &Path, object: &Path, binary: &Path, sanitize: bool) -> Result<()> {
    checked_command(
        compiler(sanitize)
            .arg(object)
            .arg(support.join("support.o"))
            .arg(support.join("allocation.o"))
            .arg(support.join("mutex.o"))
            .args([
                "-Wl,-l:libgccjit.so.0",
                "-lm",
                "-ldl",
                "-pthread",
                "-rdynamic",
                "-o",
            ])
            .arg(binary),
    )
}

fn generate_module(
    runner: &mut SourceRunner,
    expression: &str,
    directory: &Path,
    stem: &str,
) -> Result<()> {
    let begin = Instant::now();
    let module = eval(
        runner,
        &format!(
            "let state = {}\nlet output = native/emit_module(state, \"{stem}\")\nreturn [native_jit/data(state), output[:source], output[:header]]",
            expression
        ),
    )?;
    let data = module.list_get(0).ok_or("missing module data")?;
    let words = data
        .map_get(&Value::symbol(Symbol::intern("words")))
        .ok_or("missing words")?;
    let strings = data
        .map_get(&Value::symbol(Symbol::intern("strings")))
        .ok_or("missing strings")?;
    let mut encoded = Vec::new();
    words
        .with_list(|items| -> Result<()> {
            for word in items.iter() {
                let word = u64::try_from(word.as_int().ok_or("expected serialized integer")?)?;
                encoded.extend_from_slice(&word.to_le_bytes());
            }
            Ok(())
        })
        .ok_or("expected word list")??;
    encoded.extend_from_slice(text(&strings)?.as_bytes());
    fs::write(directory.join(format!("{stem}.ir")), encoded)?;
    fs::write(
        directory.join(format!("{stem}.c")),
        text(&module.list_get(1).ok_or("missing C")?)?,
    )?;
    fs::write(
        directory.join(format!("{stem}.h")),
        text(&module.list_get(2).ok_or("missing header")?)?,
    )?;
    eprintln!("{stem} generation: {:.3}s", begin.elapsed().as_secs_f64());
    Ok(())
}

fn compile_module(
    backend: &Path,
    directory: &Path,
    sanitize: bool,
    symbol: Option<&str>,
) -> Result<()> {
    let command = backend_command(
        backend,
        &directory.join("module.ir"),
        &fs::canonicalize(directory)?.join(if symbol.is_some() {
            "module.so"
        } else {
            "module.o"
        }),
        sanitize,
        symbol,
    )?;
    let stem = if symbol.is_some() {
        "loading"
    } else {
        "compilation"
    };
    run_backend(command, directory, &format!("{stem}.json"))
}

fn backend_command(
    backend: &Path,
    input: &Path,
    output: &Path,
    sanitize: bool,
    symbol: Option<&str>,
) -> Result<Command> {
    let directory = output
        .parent()
        .ok_or("output requires a parent directory")?;
    let mut command = Command::new(fs::canonicalize(backend)?);
    command.arg(fs::canonicalize(input)?).arg(output);
    if let Some(symbol) = symbol {
        command.env("MICA_JIT_LOAD_SYMBOL", symbol);
    }
    if sanitize {
        command.env("MICA_JIT_SANITIZE", "1");
    }
    // GCC 14.2 retains compiler allocations after context release. Suppress only
    // stacks inside that library, only in the compiler process. Generated test
    // executables retain the normal LeakSanitizer checks.
    let suppressions = directory.join("compiler-lsan.supp");
    fs::write(&suppressions, "leak:libgccjit.so\n")?;
    let lsan_options = std::env::var("LSAN_OPTIONS").unwrap_or_default();
    command.env(
        "LSAN_OPTIONS",
        format!(
            "{lsan_options}:suppressions={}:print_suppressions=0",
            fs::canonicalize(suppressions)?.display()
        ),
    );
    Ok(command)
}

fn run_backend(mut command: Command, directory: &Path, output_name: &str) -> Result<()> {
    let output = command.output()?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    fs::write(directory.join(output_name), &output.stdout)?;
    let log = directory.join(output_name).with_extension("log");
    fs::write(&log, &output.stderr)?;
    if !output.status.success() {
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        for line in diagnostics.lines().take(40) {
            eprintln!("{line}");
        }
        return Err(format!(
            "generated backend failed ({}); diagnostics: {}",
            output.status,
            log.display()
        )
        .into());
    }
    Ok(())
}

fn check_fixtures(
    runner: &mut SourceRunner,
    options: &Options,
    root: &Path,
    backend: &Path,
) -> Result<()> {
    let mut checked = false;
    for (name, expression, symbol, traps) in [
        ("scalar", "native/scalar_example()", "mica_sum", ""),
        (
            "allocation",
            "native/value_examples()",
            "mica_arena_create",
            "",
        ),
        (
            "surface",
            "native/surface_program()",
            "mica_checked_add",
            "nsdaphuem",
        ),
        ("callable", "native/callable_example()", "mica_chain", "n"),
        (
            "execution",
            "native/execution_example()",
            "mica_execution_step",
            "",
        ),
        (
            "memory",
            "native_memory/test_program()",
            "mica_memory_collect_stopped",
            "",
        ),
        (
            "value-memory",
            "native_value/program()",
            "mica_value_root_get",
            "",
        ),
        (
            "kernel",
            "native_rules/test_program()",
            "mica_kernel_commit",
            "",
        ),
    ] {
        if options
            .fixture
            .as_deref()
            .is_some_and(|fixture| fixture != name)
        {
            continue;
        }
        checked = true;
        let directory = options.output.join(name);
        fs::create_dir_all(&directory)?;
        generate_module(runner, expression, &directory, "module")?;
        compile_module(backend, &directory, options.sanitize, None)?;
        compile_module(backend, &directory, options.sanitize, Some(symbol))?;
        let oracle = match name {
            "scalar" => String::from(
                "#include <assert.h>\nint main(void) {\n\
                 assert(mica_tag(UINT64_MAX) == 255);\n\
                 assert(mica_add(UINT64_MAX, 1) == 0);\n\
                 assert(mica_add(41, 1) == 42);\n\
                 for (uint64_t n = 0; n < 100; ++n) assert(mica_sum(n) == n * (n - 1) / 2);\n\
                 return 0; }\n",
            ),
            "allocation" => format!(
                "#define mica_foreign_allocate native_test_allocate\n\
                 #define mica_foreign_release native_test_release\n{}\n\
                 #undef mica_foreign_allocate\n#undef mica_foreign_release\n{}",
                fs::read_to_string(root.join("native/platform/allocation.c"))?,
                fs::read_to_string(root.join("apps/native/tests/values.c"))?,
            ),
            "surface" => fs::read_to_string(root.join("apps/native/tests/surface.c"))?,
            "callable" => fs::read_to_string(root.join("apps/native/tests/callables.c"))?,
            "execution" => format!(
                "{}\n{}",
                text(&eval(runner, "return native_memory/platform()")?)?,
                fs::read_to_string(root.join("apps/native/tests/execution.c"))?,
            ),
            "memory" => format!(
                "#define MICA_MEMORY_TEST_ALLOCATOR\n{}\n{}",
                text(&eval(runner, "return native_memory/platform()")?)?,
                fs::read_to_string(root.join("apps/native/memory/tests.c"))?,
            ),
            "value-memory" => format!(
                "{}\n{}\n{}\n{}",
                text(&eval(runner, "return native_memory/platform()")?)?,
                fs::read_to_string(root.join("native/platform/allocation.c"))?,
                fs::read_to_string(root.join("native/platform/mutex.c"))?,
                fs::read_to_string(root.join("apps/native/value/gc_tests.c"))?,
            ),
            "kernel" => format!(
                "#define MICA_MEMORY_TEST_ALLOCATOR\n{}\n{}\n{}\n{}",
                text(&eval(runner, "return native_memory/platform()")?)?,
                fs::read_to_string(root.join("native/platform/allocation.c"))?,
                fs::read_to_string(root.join("native/platform/mutex.c"))?,
                fs::read_to_string(root.join("apps/native/kernel/tests.c"))?,
            ),
            _ => unreachable!(),
        };
        if name == "kernel" {
            fs::copy(
                root.join("apps/native/kernel/measurements.c"),
                directory.join("measurements.c"),
            )?;
        }
        fs::write(
            directory.join("check.c"),
            format!("#include \"module.h\"\n{oracle}"),
        )?;
        for input in ["module.c", "module.o", "module.so"] {
            let cc = if name == "execution" && input == "module.c" {
                std::env::var_os("CLANG").unwrap_or_else(|| "clang".into())
            } else {
                std::env::var_os("CC").unwrap_or_else(|| "cc".into())
            };
            let mut command = Command::new(cc);
            command.current_dir(&directory).args([
                "-std=c11",
                "-O3",
                "-g",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-ffp-contract=off",
                "-fno-fast-math",
                "-pthread",
                "-rdynamic",
                "-Wl,-rpath,$ORIGIN",
            ]);
            if options.sanitize {
                command.args([
                    "-fsanitize=address,undefined,float-cast-overflow",
                    "-fno-sanitize-recover=all",
                ]);
            }
            let compiled = command
                .args(["check.c", input, "-lm", "-o", "check"])
                .output()?;
            if !compiled.status.success() {
                return Err(format!(
                    "{name}/{input}: {}",
                    String::from_utf8_lossy(&compiled.stderr)
                )
                .into());
            }
            let binary = fs::canonicalize(directory.join("check"))?;
            let output = Command::new(&binary).current_dir(&directory).output()?;
            if !output.status.success() || !output.stderr.is_empty() {
                return Err(format!("{name}/{input}: {output:?}").into());
            }
            for trap in traps.chars() {
                let output = Command::new(&binary)
                    .current_dir(&directory)
                    .arg(trap.to_string())
                    .output()?;
                #[cfg(unix)]
                if output.status.signal() != Some(6) {
                    return Err(
                        format!("{name}/{input}/{trap}: expected SIGABRT, got {output:?}").into(),
                    );
                }
                if output.status.success() || !output.stderr.is_empty() {
                    return Err(format!("{name}/{input}/{trap}: {output:?}").into());
                }
            }
        }
        println!(
            "{}",
            serde_json::json!({"fixture":name,"correctness":"passed","backends":["c", "gccjit-object", "gccjit-shared"],"sanitized":options.sanitize})
        );
    }
    if !checked {
        return Err(format!("unknown fixture: {:?}", options.fixture).into());
    }
    Ok(())
}

fn self_rebuild(runner: &mut SourceRunner, options: &Options, root: &Path) -> Result<()> {
    let comparison = std::env::current_exe()?.with_file_name("mica-value-comparison");
    if !comparison.is_file() {
        return Err(
            "self-rebuild requires cargo build --release -p mica-value-comparison --bins".into(),
        );
    }
    let support = fs::canonicalize(&options.output)?;
    let input = support.join("backend.ir");
    let mut parent = support.join("backend");
    for generation in ["b1", "b2"] {
        let directory = support.join(generation);
        fs::create_dir_all(&directory)?;
        let object = directory.join("backend.o");
        run_backend(
            backend_command(&parent, &input, &object, options.sanitize, None)?,
            &directory,
            "rebuild.json",
        )?;
        let backend = directory.join("backend");
        link_backend(&support, &object, &backend, options.sanitize)?;
        parent = backend;
    }
    // Exercise B2 on the backend input too, without creating another stage.
    let reproduced = support.join("b2/reproduced.o");
    run_backend(
        backend_command(&parent, &input, &reproduced, options.sanitize, None)?,
        &support.join("b2"),
        "reproduction.json",
    )?;
    let identical = fs::read(support.join("b1/backend.o"))?
        == fs::read(support.join("b2/backend.o"))?
        && fs::read(support.join("b2/backend.o"))? == fs::read(&reproduced)?;
    for generation in ["b1", "b2"] {
        let stage = Options {
            output: support.join(generation),
            sanitize: options.sanitize,
            reuse_backend: true,
            module: "native_value/program()".into(),
            check_fixtures: false,
            fixture: None,
            self_rebuild: false,
            lifecycle_rounds: options.lifecycle_rounds,
            load_symbol: None,
        };
        let backend = stage.output.join("backend");
        check_fixtures(runner, &stage, root, &backend)?;
        let values = stage.output.join("values");
        fs::create_dir_all(&values)?;
        generate_module(runner, &stage.module, &values, "module")?;
        compile_module(&backend, &values, options.sanitize, None)?;
        compile_module(&backend, &values, options.sanitize, Some("mica_value_tag"))?;
        let mut check = Command::new(&comparison);
        check
            .args(["--backend", "gccjit", "--native-module"])
            .arg(&values)
            .args(["--all-corpora", "--cases", "128", "--seed", "19"]);
        if options.sanitize {
            check.arg("--sanitize");
        }
        run_backend(check, &stage.output, "correctness.json")?;
        println!(
            "{}",
            serde_json::json!({"generation":generation,"correctness":"passed","sanitized":options.sanitize})
        );
    }
    let lifecycle_result =
        measure_lifecycles(&support, options.lifecycle_rounds.get(), options.sanitize);
    let summary = serde_json::json!({
        "self_rebuild": "passed",
        "b1_b2_reproduction_objects_identical": identical,
        "sanitized": options.sanitize,
        "lifecycle_rounds": options.lifecycle_rounds.get(),
        "lifecycles": if lifecycle_result.is_ok() { "passed" } else { "failed" },
    });
    fs::write(support.join("self-rebuild.json"), format!("{summary}\n"))?;
    println!("{summary}");
    lifecycle_result
}

fn measure_lifecycles(support: &Path, rounds: u32, sanitize: bool) -> Result<()> {
    // Reuse exactly the same inputs for all three compilers. A process retains
    // only its input table between iterations; each GCC context is fresh.
    for (generation, backend) in [
        ("b0", support.join("backend")),
        ("b1", support.join("b1/backend")),
        ("b2", support.join("b2/backend")),
    ] {
        let directory = support.join("lifecycles").join(generation);
        fs::create_dir_all(&directory)?;
        for (name, input, symbol, probe) in [
            (
                "backend",
                support.join("backend.ir"),
                "mica_jit_build",
                false,
            ),
            (
                "values",
                support.join("b1/values/module.ir"),
                "mica_value_tag",
                false,
            ),
            (
                "callable",
                support.join("b1/callable/module.ir"),
                "mica_chain",
                true,
            ),
        ] {
            // Sanitized runs exercise the loaded callback, but allocator/RSS
            // measurements require the unsanitized compiler and its allocator.
            if sanitize && !probe {
                continue;
            }
            eprintln!("{generation}: {rounds} {name} compiler lifecycles");
            let mut command = backend_command(
                &backend,
                &input,
                &directory.join(format!("{name}.so")),
                sanitize,
                Some(symbol),
            )?;
            command.env("MICA_JIT_ITERATIONS", rounds.to_string());
            if probe {
                command
                    .env("MICA_JIT_PROBE_INPUT", "41")
                    .env("MICA_JIT_PROBE_EXPECTED", "42");
            }
            run_backend(command, &directory, &format!("{name}.jsonl"))?;
            let records = fs::read_to_string(directory.join(format!("{name}.jsonl")))?;
            let records: Vec<serde_json::Value> = records
                .lines()
                .map(serde_json::from_str)
                .collect::<std::result::Result<_, _>>()?;
            if records.len() != rounds as usize
                || records
                    .iter()
                    .any(|row| row["failed"] != false || row["probe"] != probe)
            {
                return Err(
                    format!("{generation}/{name}: incomplete lifecycle measurements").into(),
                );
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
