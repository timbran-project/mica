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

use mica_var::Value;
use mica_var::abi::borrowed_value_bits;
use mica_var::language_cmp;
use mica_vm_cranelift::{
    CompiledFloatLoop, CompiledIntegerLoop, CompiledNaturalLoop, FloatArithmetic, FloatComparison,
    FloatLoopOutcome, FloatLoopPlan, IntegerLoopOutcome, NaturalLoopInstruction,
    NaturalLoopOutcome, NaturalLoopPlan, ScalarComparison,
};
use micromeasure::{
    BenchContext, BenchmarkMainOptions, ConcurrentBenchContext, ConcurrentBenchControl,
    ConcurrentWorker, ConcurrentWorkerResult, DiagnosticError, DiagnosticResult, MetricValue,
    NoContext, Throughput, benchmark_main, black_box,
};
use std::time::Duration;

const INTEGER_LOOP_ITERATIONS: u64 = 16_384;
const NATURAL_INTEGER_LOOP_INSTRUCTIONS: u64 = INTEGER_LOOP_ITERATIONS * 9;
const CONCURRENT_THREADS: usize = 4;

const CURRENT_SLOT: u16 = 0;
const TOTAL_SLOT: u16 = 1;
const LIMIT_SLOT: u16 = 2;
const CONDITION_SLOT: u16 = 3;
const STEP_SLOT: u16 = 4;
const NEXT_SLOT: u16 = 5;
const NEXT_TOTAL_SLOT: u16 = 6;

#[derive(Clone, Copy)]
enum FloatLoopKind {
    Add,
    Multiply,
}

impl FloatLoopKind {
    const fn arithmetic(self) -> FloatArithmetic {
        match self {
            Self::Add => FloatArithmetic::Add,
            Self::Multiply => FloatArithmetic::Multiply,
        }
    }

    const fn inputs(self) -> (f32, f32, f32) {
        match self {
            Self::Add => (0.0, 0.5, 8_192.0),
            Self::Multiply => (1.0, 1.0001, 5.0),
        }
    }
}

enum LoopBackend {
    Rust,
    Cranelift(Box<CompiledIntegerLoop>),
}

struct ValueLoopContext {
    backend: LoopBackend,
    start: Value,
    step: Value,
    limit: Value,
}

enum FloatLoopBackend {
    Rust,
    Cranelift(Box<CompiledFloatLoop>),
}

struct FloatValueLoopContext {
    backend: FloatLoopBackend,
    kind: FloatLoopKind,
    start: Value,
    step: Value,
    limit: Value,
    iterations: u64,
}

struct NaturalIntegerLoopContext {
    compiled: Box<CompiledNaturalLoop>,
    initial_scratch: [u64; 7],
}

impl NaturalIntegerLoopContext {
    fn unboxed() -> Self {
        Self::new(true)
    }

    fn new(unboxed: bool) -> Self {
        let plan = natural_integer_loop_plan(unboxed);
        let int_bits = |value| borrowed_value_bits(&Value::int(value).unwrap());
        Self {
            compiled: Box::new(CompiledNaturalLoop::compile(&plan).unwrap()),
            initial_scratch: [
                int_bits(0),
                int_bits(0),
                int_bits(INTEGER_LOOP_ITERATIONS as i64),
                borrowed_value_bits(&Value::bool(true)),
                borrowed_value_bits(&Value::empty_relation()),
                borrowed_value_bits(&Value::empty_relation()),
                borrowed_value_bits(&Value::empty_relation()),
            ],
        }
    }

    fn run_once(&self) -> u64 {
        let mut scratch = self.initial_scratch;
        let NaturalLoopOutcome::Complete { instructions, .. } =
            self.compiled
                .run(&mut scratch, &[], NATURAL_INTEGER_LOOP_INSTRUCTIONS)
        else {
            panic!("natural integer benchmark left the generated fast path");
        };
        debug_assert_eq!(instructions, NATURAL_INTEGER_LOOP_INSTRUCTIONS);
        debug_assert_eq!(
            scratch[TOTAL_SLOT as usize],
            borrowed_value_bits(
                &Value::int(
                    ((INTEGER_LOOP_ITERATIONS * (INTEGER_LOOP_ITERATIONS + 1)) / 2) as i64,
                )
                .unwrap(),
            ),
        );
        black_box(scratch);
        instructions
    }
}

impl FloatValueLoopContext {
    fn rust(kind: FloatLoopKind) -> Self {
        Self::new(FloatLoopBackend::Rust, kind)
    }

    fn cranelift(kind: FloatLoopKind) -> Self {
        let plan = FloatLoopPlan::new(kind.arithmetic(), FloatComparison::LessThan);
        Self::new(
            FloatLoopBackend::Cranelift(Box::new(CompiledFloatLoop::compile(plan).unwrap())),
            kind,
        )
    }

    fn new(backend: FloatLoopBackend, kind: FloatLoopKind) -> Self {
        let (start, step, limit) = kind.inputs();
        let start = Value::float(start).unwrap();
        let step = Value::float(step).unwrap();
        let limit = Value::float(limit).unwrap();
        let (_, iterations) = interpreted_float_loop(kind, &start, &step, &limit);
        Self {
            backend,
            kind,
            start,
            step,
            limit,
            iterations,
        }
    }

    fn run_once(&self) -> u64 {
        let (current, iterations) = match &self.backend {
            FloatLoopBackend::Rust => interpreted_float_loop(
                self.kind,
                black_box(&self.start),
                black_box(&self.step),
                black_box(&self.limit),
            ),
            FloatLoopBackend::Cranelift(compiled) => {
                let FloatLoopOutcome::Complete {
                    current,
                    iterations,
                    ..
                } = compiled.run(
                    black_box(&self.start),
                    black_box(&self.step),
                    black_box(&self.limit),
                    self.iterations + 1,
                )
                else {
                    panic!("float benchmark left the generated fast path");
                };
                (current, iterations)
            }
        };
        debug_assert_eq!(iterations, self.iterations);
        black_box(current);
        iterations
    }
}

impl ValueLoopContext {
    fn rust() -> Self {
        Self {
            backend: LoopBackend::Rust,
            start: Value::int(0).unwrap(),
            step: Value::int(1).unwrap(),
            limit: Value::int(INTEGER_LOOP_ITERATIONS as i64).unwrap(),
        }
    }

    fn cranelift() -> Self {
        Self {
            backend: LoopBackend::Cranelift(Box::new(CompiledIntegerLoop::compile().unwrap())),
            start: Value::int(0).unwrap(),
            step: Value::int(1).unwrap(),
            limit: Value::int(INTEGER_LOOP_ITERATIONS as i64).unwrap(),
        }
    }

    fn run_once(&self) -> Value {
        match &self.backend {
            LoopBackend::Rust => interpreted_integer_loop(
                black_box(&self.start),
                black_box(&self.step),
                black_box(&self.limit),
            ),
            LoopBackend::Cranelift(compiled) => {
                let IntegerLoopOutcome::Complete {
                    current,
                    iterations,
                    ..
                } = compiled.run(
                    black_box(&self.start),
                    black_box(&self.step),
                    black_box(&self.limit),
                    INTEGER_LOOP_ITERATIONS,
                )
                else {
                    panic!("integer benchmark left the generated fast path");
                };
                debug_assert_eq!(iterations, INTEGER_LOOP_ITERATIONS);
                current
            }
        }
    }
}

impl BenchContext for ValueLoopContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::rust()
    }
}

impl ConcurrentBenchContext for ValueLoopContext {
    fn prepare(_num_threads: usize) -> Self {
        Self::cranelift()
    }
}

impl BenchContext for FloatValueLoopContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::rust(FloatLoopKind::Add)
    }
}

impl ConcurrentBenchContext for FloatValueLoopContext {
    fn prepare(_num_threads: usize) -> Self {
        Self::cranelift(FloatLoopKind::Add)
    }
}

impl BenchContext for NaturalIntegerLoopContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::unboxed()
    }
}

impl ConcurrentBenchContext for NaturalIntegerLoopContext {
    fn prepare(_num_threads: usize) -> Self {
        Self::unboxed()
    }
}

fn interpreted_integer_loop(start: &Value, step: &Value, limit: &Value) -> Value {
    let mut current = start.clone();
    while &current < limit {
        current = current.checked_add(step).unwrap();
    }
    current
}

fn interpreted_float_loop(
    kind: FloatLoopKind,
    start: &Value,
    step: &Value,
    limit: &Value,
) -> (Value, u64) {
    let mut current = start.clone();
    let mut iterations = 0_u64;
    loop {
        current = match kind {
            FloatLoopKind::Add => current.checked_add(step).unwrap(),
            FloatLoopKind::Multiply => current.checked_mul(step).unwrap(),
        };
        iterations += 1;
        if !language_cmp::numeric_cmp(&current, limit).is_lt() {
            return (current, iterations);
        }
    }
}

fn run_value_loops(context: &mut ValueLoopContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        black_box(context.run_once());
    }
}

fn compile_integer_loops(_context: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        let compiled = CompiledIntegerLoop::compile().unwrap();
        black_box((compiled.code_size(), compiled.imported_helper_count()));
    }
}

fn compile_float_loops(_context: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        let compiled = CompiledFloatLoop::compile(FloatLoopPlan::new(
            FloatArithmetic::Add,
            FloatComparison::LessThan,
        ))
        .unwrap();
        black_box((compiled.code_size(), compiled.imported_helper_count()));
    }
}

fn natural_integer_loop_plan(unboxed: bool) -> NaturalLoopPlan {
    let int_bits = |value| borrowed_value_bits(&Value::int(value).unwrap());
    let integer_slots = (1_u32 << CURRENT_SLOT)
        | (1_u32 << TOTAL_SLOT)
        | (1_u32 << LIMIT_SLOT)
        | (1_u32 << STEP_SLOT)
        | (1_u32 << NEXT_SLOT)
        | (1_u32 << NEXT_TOTAL_SLOT);
    let entry_slots = (1_u32 << CURRENT_SLOT) | (1_u32 << TOTAL_SLOT);

    let plan = NaturalLoopPlan::new(
        7,
        0,
        3,
        [
            NaturalLoopInstruction::Load {
                dst: LIMIT_SLOT,
                value: int_bits(INTEGER_LOOP_ITERATIONS as i64),
            },
            NaturalLoopInstruction::Compare {
                dst: CONDITION_SLOT,
                comparison: ScalarComparison::LessThan,
                left: CURRENT_SLOT,
                right: LIMIT_SLOT,
            },
            NaturalLoopInstruction::Branch {
                condition: CONDITION_SLOT,
                if_true: 3,
                if_false: 9,
            },
            NaturalLoopInstruction::Load {
                dst: STEP_SLOT,
                value: int_bits(1),
            },
            NaturalLoopInstruction::Add {
                dst: NEXT_SLOT,
                left: CURRENT_SLOT,
                right: STEP_SLOT,
            },
            NaturalLoopInstruction::Move {
                dst: CURRENT_SLOT,
                src: NEXT_SLOT,
            },
            NaturalLoopInstruction::Add {
                dst: NEXT_TOTAL_SLOT,
                left: TOTAL_SLOT,
                right: CURRENT_SLOT,
            },
            NaturalLoopInstruction::Move {
                dst: TOTAL_SLOT,
                src: NEXT_TOTAL_SLOT,
            },
            NaturalLoopInstruction::Jump { target: 0 },
        ],
    )
    .unwrap();
    if unboxed {
        plan.with_unboxed_slots(integer_slots, entry_slots, 0, 0)
            .unwrap()
    } else {
        plan
    }
}

fn compile_natural_integer_loops(
    _context: &mut NoContext,
    chunk_size: usize,
    _chunk_num: usize,
    unboxed: bool,
) {
    for _ in 0..chunk_size {
        let compiled = CompiledNaturalLoop::compile(&natural_integer_loop_plan(unboxed)).unwrap();
        black_box((compiled.code_size(), compiled.imported_helper_count()));
    }
}

fn compile_tagged_natural_integer_loops(
    context: &mut NoContext,
    chunk_size: usize,
    chunk_num: usize,
) {
    compile_natural_integer_loops(context, chunk_size, chunk_num, false);
}

fn compile_unboxed_natural_integer_loops(
    context: &mut NoContext,
    chunk_size: usize,
    chunk_num: usize,
) {
    compile_natural_integer_loops(context, chunk_size, chunk_num, true);
}

fn run_natural_integer_loops(
    context: &mut NaturalIntegerLoopContext,
    chunk_size: usize,
    _chunk_num: usize,
) {
    for _ in 0..chunk_size {
        black_box(context.run_once());
    }
}

fn natural_integer_code_diagnostics(
    context: &mut NaturalIntegerLoopContext,
    _chunk_size: usize,
    _chunk_num: usize,
) -> Result<DiagnosticResult, DiagnosticError> {
    Ok(DiagnosticResult::new("generated natural integer code")
        .push_metric(MetricValue::integer(
            "code_size",
            context.compiled.code_size() as i64,
            "bytes",
        ))
        .push_metric(MetricValue::integer(
            "imported_helpers",
            context.compiled.imported_helper_count() as i64,
            "helpers",
        )))
}

fn run_concurrent_natural_integer_loops(
    context: &NaturalIntegerLoopContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut instructions = 0_u64;
    let mut loops = 0_u64;
    while !control.should_stop() {
        instructions = instructions.wrapping_add(context.run_once());
        loops = loops.wrapping_add(1);
    }
    ConcurrentWorkerResult::operations(instructions).with_counter("loops", loops)
}

fn generated_code_diagnostics(
    context: &mut ValueLoopContext,
    _chunk_size: usize,
    _chunk_num: usize,
) -> Result<DiagnosticResult, DiagnosticError> {
    let LoopBackend::Cranelift(compiled) = &context.backend else {
        return Err(DiagnosticError::new(
            "generated-code diagnostics require a Cranelift context",
        ));
    };
    Ok(DiagnosticResult::new("generated code")
        .push_metric(MetricValue::integer(
            "value_abi_version",
            i64::from(compiled.value_abi_version()),
            "version",
        ))
        .push_metric(MetricValue::integer(
            "code_size",
            compiled.code_size() as i64,
            "bytes",
        ))
        .push_metric(MetricValue::integer(
            "imported_helpers",
            compiled.imported_helper_count() as i64,
            "helpers",
        )))
}

fn generated_float_code_diagnostics(
    context: &mut FloatValueLoopContext,
    _chunk_size: usize,
    _chunk_num: usize,
) -> Result<DiagnosticResult, DiagnosticError> {
    let FloatLoopBackend::Cranelift(compiled) = &context.backend else {
        return Err(DiagnosticError::new(
            "generated-code diagnostics require a Cranelift context",
        ));
    };
    Ok(DiagnosticResult::new("generated float code")
        .push_metric(MetricValue::integer(
            "value_abi_version",
            i64::from(compiled.value_abi_version()),
            "version",
        ))
        .push_metric(MetricValue::integer(
            "code_size",
            compiled.code_size() as i64,
            "bytes",
        ))
        .push_metric(MetricValue::integer(
            "imported_helpers",
            compiled.imported_helper_count() as i64,
            "helpers",
        ))
        .push_metric(MetricValue::integer(
            "iterations_per_loop",
            context.iterations as i64,
            "iterations",
        )))
}

fn float_loop_iterations(kind: FloatLoopKind) -> u64 {
    FloatValueLoopContext::rust(kind).iterations
}

fn run_concurrent_value_loops(
    context: &ValueLoopContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut loops = 0_u64;
    while !control.should_stop() {
        black_box(context.run_once());
        loops = loops.wrapping_add(1);
    }
    ConcurrentWorkerResult::operations(loops.wrapping_mul(INTEGER_LOOP_ITERATIONS))
        .with_counter("loops", loops)
}

fn run_float_value_loops(
    context: &mut FloatValueLoopContext,
    chunk_size: usize,
    _chunk_num: usize,
) {
    for _ in 0..chunk_size {
        black_box(context.run_once());
    }
}

fn run_concurrent_float_value_loops(
    context: &FloatValueLoopContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut operations = 0_u64;
    let mut loops = 0_u64;
    while !control.should_stop() {
        operations = operations.wrapping_add(context.run_once());
        loops = loops.wrapping_add(1);
    }
    ConcurrentWorkerResult::operations(operations).with_counter("loops", loops)
}

benchmark_main!(
    BenchmarkMainOptions {
        filter_help: Some(
            "all, rust, cranelift, compile, concurrent, or a benchmark name substring".to_owned(),
        ),
        runtime: micromeasure::BenchmarkRuntimeOptions {
            warm_up_duration: Duration::from_millis(250),
            benchmark_duration: Duration::from_secs(1),
            min_samples: 10,
            max_samples: 30,
        },
        ..Default::default()
    },
    |runner| {
        runner.group::<ValueLoopContext>("value codegen", |group| {
            let rust = || ValueLoopContext::rust();
            group
                .throughput(Throughput::per_operation(
                    INTEGER_LOOP_ITERATIONS,
                    "additions",
                ))
                .factory(&rust)
                .bench("rust_value_integer_loop", run_value_loops);

            let cranelift = || ValueLoopContext::cranelift();
            group
                .throughput(Throughput::per_operation(
                    INTEGER_LOOP_ITERATIONS,
                    "additions",
                ))
                .factory(&cranelift)
                .diagnostic_pass(generated_code_diagnostics)
                .bench("cranelift_value_integer_loop", run_value_loops);
        });

        runner.group::<FloatValueLoopContext>("float value codegen", |group| {
            for (rust_name, cranelift_name, kind) in [
                (
                    "rust_value_float_add_loop",
                    "cranelift_value_float_add_loop",
                    FloatLoopKind::Add,
                ),
                (
                    "rust_value_float_multiply_loop",
                    "cranelift_value_float_multiply_loop",
                    FloatLoopKind::Multiply,
                ),
            ] {
                let iterations = float_loop_iterations(kind);
                let rust = move || FloatValueLoopContext::rust(kind);
                group
                    .throughput(Throughput::per_operation(iterations, "float_operations"))
                    .factory(&rust)
                    .bench(rust_name, run_float_value_loops);

                let cranelift = move || FloatValueLoopContext::cranelift(kind);
                group
                    .throughput(Throughput::per_operation(iterations, "float_operations"))
                    .factory(&cranelift)
                    .diagnostic_pass(generated_float_code_diagnostics)
                    .bench(cranelift_name, run_float_value_loops);
            }
        });

        runner.group::<NaturalIntegerLoopContext>("natural integer codegen", |group| {
            for (name, unboxed) in [
                ("cranelift_tagged_natural_integer_accumulator", false),
                ("cranelift_unboxed_natural_integer_accumulator", true),
            ] {
                let factory = move || NaturalIntegerLoopContext::new(unboxed);
                group
                    .throughput(Throughput::per_operation(
                        NATURAL_INTEGER_LOOP_INSTRUCTIONS,
                        "bytecode_instruction",
                    ))
                    .factory(&factory)
                    .diagnostic_pass(natural_integer_code_diagnostics)
                    .bench(name, run_natural_integer_loops);
            }
        });

        runner.group::<NoContext>("value codegen compile", |group| {
            group
                .throughput(Throughput::per_operation(1, "compilation"))
                .bench("cranelift_compile_integer_loop", compile_integer_loops);
            group
                .throughput(Throughput::per_operation(1, "compilation"))
                .bench("cranelift_compile_float_loop", compile_float_loops);
            group
                .throughput(Throughput::per_operation(1, "compilation"))
                .bench(
                    "cranelift_compile_tagged_natural_integer_loop",
                    compile_tagged_natural_integer_loops,
                );
            group
                .throughput(Throughput::per_operation(1, "compilation"))
                .bench(
                    "cranelift_compile_unboxed_natural_integer_loop",
                    compile_unboxed_natural_integer_loops,
                );
        });

        let one_thread = [ConcurrentWorker {
            name: "value loop",
            threads: 1,
            run: run_concurrent_value_loops,
        }];
        let four_threads = [ConcurrentWorker {
            name: "value loop",
            threads: CONCURRENT_THREADS,
            run: run_concurrent_value_loops,
        }];
        let rust = |_| ValueLoopContext::rust();
        let cranelift = |_| ValueLoopContext::cranelift();
        runner.concurrent_group::<ValueLoopContext>("value codegen concurrent", |group| {
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "additions"))
                .metadata("backend", "rust")
                .metadata("threads", "1")
                .factory(&rust)
                .bench("rust_value_integer_loop_1_thread", &one_thread);
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "additions"))
                .metadata("backend", "rust")
                .metadata("threads", CONCURRENT_THREADS.to_string())
                .factory(&rust)
                .bench("rust_value_integer_loop_4_threads", &four_threads);
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "additions"))
                .metadata("backend", "cranelift")
                .metadata("threads", "1")
                .metadata("side_exits", "0")
                .factory(&cranelift)
                .bench("cranelift_value_integer_loop_1_thread", &one_thread);
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "additions"))
                .metadata("backend", "cranelift")
                .metadata("threads", CONCURRENT_THREADS.to_string())
                .metadata("side_exits", "0")
                .factory(&cranelift)
                .bench("cranelift_value_integer_loop_4_threads", &four_threads);
        });

        let one_natural_thread = [ConcurrentWorker {
            name: "natural integer loop",
            threads: 1,
            run: run_concurrent_natural_integer_loops,
        }];
        let four_natural_threads = [ConcurrentWorker {
            name: "natural integer loop",
            threads: CONCURRENT_THREADS,
            run: run_concurrent_natural_integer_loops,
        }];
        runner.concurrent_group::<NaturalIntegerLoopContext>(
            "natural integer codegen concurrent",
            |group| {
                for (representation, unboxed) in [("tagged", false), ("unboxed", true)] {
                    for (threads, workers) in [
                        (1, &one_natural_thread[..]),
                        (CONCURRENT_THREADS, &four_natural_threads[..]),
                    ] {
                        let factory = move |_| NaturalIntegerLoopContext::new(unboxed);
                        group
                            .sample_duration(Duration::from_millis(50))
                            .throughput(Throughput::per_operation(1, "bytecode_instruction"))
                            .metadata("representation", representation)
                            .metadata("threads", threads.to_string())
                            .factory(&factory)
                            .bench(
                                &format!(
                                    "cranelift_{representation}_natural_integer_accumulator_{threads}_threads"
                                ),
                                workers,
                            );
                    }
                }
            },
        );

        let one_float_thread = [ConcurrentWorker {
            name: "float value loop",
            threads: 1,
            run: run_concurrent_float_value_loops,
        }];
        let four_float_threads = [ConcurrentWorker {
            name: "float value loop",
            threads: CONCURRENT_THREADS,
            run: run_concurrent_float_value_loops,
        }];
        runner.concurrent_group::<FloatValueLoopContext>(
            "float value codegen concurrent",
            |group| {
                for (kind, rust_one, rust_four, cranelift_one, cranelift_four) in [
                    (
                        FloatLoopKind::Add,
                        "rust_value_float_add_loop_1_thread",
                        "rust_value_float_add_loop_4_threads",
                        "cranelift_value_float_add_loop_1_thread",
                        "cranelift_value_float_add_loop_4_threads",
                    ),
                    (
                        FloatLoopKind::Multiply,
                        "rust_value_float_multiply_loop_1_thread",
                        "rust_value_float_multiply_loop_4_threads",
                        "cranelift_value_float_multiply_loop_1_thread",
                        "cranelift_value_float_multiply_loop_4_threads",
                    ),
                ] {
                    for (backend, cranelift, one_name, four_name) in [
                        ("rust", false, rust_one, rust_four),
                        ("cranelift", true, cranelift_one, cranelift_four),
                    ] {
                        let one = move |_| {
                            if cranelift {
                                FloatValueLoopContext::cranelift(kind)
                            } else {
                                FloatValueLoopContext::rust(kind)
                            }
                        };
                        group
                            .sample_duration(Duration::from_millis(50))
                            .throughput(Throughput::per_operation(1, "float_operations"))
                            .metadata("backend", backend)
                            .metadata("threads", "1")
                            .factory(&one)
                            .bench(one_name, &one_float_thread);

                        let four = move |_| {
                            if cranelift {
                                FloatValueLoopContext::cranelift(kind)
                            } else {
                                FloatValueLoopContext::rust(kind)
                            }
                        };
                        group
                            .sample_duration(Duration::from_millis(50))
                            .throughput(Throughput::per_operation(1, "float_operations"))
                            .metadata("backend", backend)
                            .metadata("threads", CONCURRENT_THREADS.to_string())
                            .factory(&four)
                            .bench(four_name, &four_float_threads);
                    }
                }
            },
        );
    }
);
