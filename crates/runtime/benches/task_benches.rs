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

use mica_compiler::{CompileContext, MethodRelations, compile_source, install_methods_from_source};
use mica_relation_kernel::{
    ConflictPolicy, DispatchRelations, RelationId, RelationKernel, RelationMetadata, Tuple,
};
use mica_runtime::{
    BuiltinRegistry, Instruction, Operand, Program, ProgramResolver, QueryBinding, Register,
    RuntimeBinaryOp, Task, TaskLimits, TaskOutcome,
};
use mica_var::{Identity, Symbol, Value};
use micromeasure::{
    BenchContext, BenchmarkMainOptions, ConcurrentBenchContext, ConcurrentBenchControl,
    ConcurrentWorker, ConcurrentWorkerResult, DiagnosticError, DiagnosticResult, MetricValue,
    Throughput, benchmark_main, black_box,
};
use std::sync::Arc;
use std::time::Duration;

const INTEGER_LOOP_ITERATIONS: u64 = 16_384;
const INTEGER_LOOP_BYTECODES: u64 = (INTEGER_LOOP_ITERATIONS * 3) + 4;
const COMPILER_COUNTER_LOOP_BYTECODES: u64 = (INTEGER_LOOP_ITERATIONS * 7) + 6;
const COMPILER_ACCUMULATOR_LOOP_BYTECODES: u64 = (INTEGER_LOOP_ITERATIONS * 9) + 7;
const COMPILER_COUNTDOWN_LOOP_BYTECODES: u64 = (INTEGER_LOOP_ITERATIONS * 9) + 7;
const COMPILER_ARITHMETIC_LOOP_BYTECODES: u64 = (INTEGER_LOOP_ITERATIONS * 11) + 7;
const COMPILER_INTEGER_SURFACE_LOOP_BYTECODES: u64 = (INTEGER_LOOP_ITERATIONS * 19) + 6;
const RANGE_ADMISSION_ITERATIONS: [u64; 4] = [2_048, 4_096, 8_192, 16_384];
const RANGE_ADMISSION_CONCURRENT_ITERATIONS: [u64; 2] = [8_192, 16_384];
// Keep this above the VM's measured 8,192-element collection-loop JIT threshold.
const COLLECTION_BINDING_CHECK_ITERATIONS: u64 = 16_384;
const REPEATED_DISPATCH_ITERATIONS: u64 = 1_024;
const THREE_SITE_DISPATCH_ROUNDS: u64 = REPEATED_DISPATCH_ITERATIONS / 3;
const THREE_SITE_DISPATCHES_PER_TASK: u64 = THREE_SITE_DISPATCH_ROUNDS * 3;
const CALL_PATH_INVOCATIONS: u64 = 8_192;
const STRUCTURAL_MATCH_ITERATIONS: u64 = 4_096;
const CONCURRENT_DISPATCH_THREADS: usize = 4;
const RELATION_SCAN_ROWS: usize = 1_024;
const MAX_CALL_DEPTH: usize = 64;

#[derive(Clone, Copy)]
enum ScanCellKind {
    Immediate,
    SharedHeap,
}

impl ScanCellKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::SharedHeap => "shared_heap",
        }
    }
}

#[derive(Clone, Copy)]
enum CallPath {
    Dispatch,
    DirectCall,
    InterpretedInline,
    NativeInline,
}

impl CallPath {
    const ALL: [Self; 4] = [
        Self::Dispatch,
        Self::DirectCall,
        Self::InterpretedInline,
        Self::NativeInline,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::DirectCall => "direct_call",
            Self::InterpretedInline => "interpreted_inline",
            Self::NativeInline => "native_enabled_inline",
        }
    }

    const fn interpret_only(self) -> bool {
        matches!(self, Self::InterpretedInline)
    }
}

#[derive(Clone, Copy)]
struct Workload {
    executed_bytecodes: u64,
    // Count calls that cross from the VM into the relation workspace. Kernel-internal index
    // probes performed by one host call are intentionally not separate operations here.
    relation_reads: u64,
    relation_rows: u64,
    relation_writes: u64,
    commit_boundaries: u64,
    kernel_commits: u64,
    dispatch_cache_lookups: u64,
    method_program_cache_lookups: u64,
    vm_resolved_program_lookups: u64,
    program_resolver_lookups: u64,
    call_path_invocations: u64,
}

impl Workload {
    const fn task(executed_bytecodes: u64) -> Self {
        Self {
            executed_bytecodes,
            relation_reads: 0,
            relation_rows: 0,
            relation_writes: 0,
            commit_boundaries: 1,
            kernel_commits: 0,
            dispatch_cache_lookups: 0,
            method_program_cache_lookups: 0,
            vm_resolved_program_lookups: 0,
            program_resolver_lookups: 0,
            call_path_invocations: 0,
        }
    }
}

struct TaskBenchContext {
    kernel: RelationKernel,
    program: Arc<Program>,
    resolver: Arc<ProgramResolver>,
    builtins: Arc<BuiltinRegistry>,
    next_task_id: u64,
    workload: Workload,
    interpret_only: bool,
}

struct ConcurrentTaskBenchContext {
    kernel: RelationKernel,
    program: Arc<Program>,
    resolver: Arc<ProgramResolver>,
    builtins: Arc<BuiltinRegistry>,
    dispatches_per_task: u64,
    call_path_invocations_per_task: u64,
    bytecodes_per_task: u64,
    relation_rows_per_task: u64,
    instruction_budget: usize,
    interpret_only: bool,
}

struct ColdLoopBenchContext {
    register_count: usize,
    instructions: Arc<[Instruction]>,
    workload: Workload,
    interpret_only: bool,
}

impl ColdLoopBenchContext {
    fn from_task_context(context: TaskBenchContext) -> Self {
        Self {
            register_count: context.program.register_count(),
            instructions: context.program.instructions().into(),
            workload: context.workload,
            interpret_only: context.interpret_only,
        }
    }

    fn counter() -> Self {
        Self::from_task_context(TaskBenchContext::compiler_counter_loop())
    }

    fn accumulator() -> Self {
        Self::accumulator_with_annotations(false, false)
    }

    fn accumulator_with_annotations(annotated: bool, interpret_only: bool) -> Self {
        Self::from_task_context(
            TaskBenchContext::compiler_accumulator_loop_with_annotations(annotated, interpret_only),
        )
    }

    fn countdown() -> Self {
        Self::from_task_context(TaskBenchContext::compiler_countdown_loop())
    }

    fn arithmetic() -> Self {
        Self::from_task_context(TaskBenchContext::compiler_arithmetic_loop())
    }

    fn integer_surface() -> Self {
        Self::from_task_context(TaskBenchContext::compiler_integer_surface_loop())
    }

    fn range(iterations: u64, interpret_only: bool) -> Self {
        let mut context = TaskBenchContext::compiler_range_loop_with_iterations(iterations);
        context.interpret_only = interpret_only;
        Self::from_task_context(context)
    }

    fn indexed_range(iterations: u64, interpret_only: bool) -> Self {
        let mut context = TaskBenchContext::compiler_indexed_range_loop_with_iterations(iterations);
        context.interpret_only = interpret_only;
        Self::from_task_context(context)
    }

    fn collection(context: TaskBenchContext, interpret_only: bool) -> Self {
        let mut context = context;
        context.interpret_only = interpret_only;
        Self::from_task_context(context)
    }
}

impl BenchContext for ColdLoopBenchContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::counter()
    }
}

impl ConcurrentTaskBenchContext {
    fn from_task_context(context: TaskBenchContext) -> Self {
        Self {
            kernel: context.kernel,
            program: context.program,
            resolver: context.resolver,
            builtins: context.builtins,
            dispatches_per_task: context.workload.dispatch_cache_lookups,
            call_path_invocations_per_task: context.workload.call_path_invocations,
            bytecodes_per_task: context.workload.executed_bytecodes,
            relation_rows_per_task: context.workload.relation_rows,
            instruction_budget: context.workload.executed_bytecodes as usize + 1,
            interpret_only: context.interpret_only,
        }
    }

    fn run_one(&self, task_id: u64) {
        let mut task = Task::new_with_builtins(
            task_id,
            &self.kernel,
            Arc::clone(&self.program),
            Arc::clone(&self.resolver),
            Arc::clone(&self.builtins),
            TaskLimits {
                instruction_budget: self.instruction_budget,
                max_retries: 0,
                max_call_depth: MAX_CALL_DEPTH,
            },
        );
        if self.interpret_only {
            task.vm_mut().disable_native_execution();
        }
        let outcome = task.run().unwrap();
        debug_assert!(matches!(outcome, TaskOutcome::Complete { .. }));
        black_box(outcome);
    }
}

impl ConcurrentBenchContext for ConcurrentTaskBenchContext {
    fn prepare(_num_threads: usize) -> Self {
        Self::from_task_context(TaskBenchContext::warm_positional_dispatch())
    }
}

impl TaskBenchContext {
    fn from_program(kernel: RelationKernel, program: Program, workload: Workload) -> Self {
        Self {
            kernel,
            program: Arc::new(program),
            resolver: Arc::new(ProgramResolver::new()),
            builtins: Arc::new(BuiltinRegistry::new()),
            next_task_id: 1,
            workload,
            interpret_only: false,
        }
    }

    fn minimal_return() -> Self {
        let program = Program::new(
            0,
            [Instruction::Return {
                value: value(Value::empty_relation()),
            }],
        )
        .unwrap();
        Self::from_program(RelationKernel::new(), program, Workload::task(1))
    }

    fn integer_loop() -> Self {
        let program = Program::new(
            4,
            [
                Instruction::Load {
                    dst: register(0),
                    value: int(0),
                },
                Instruction::Load {
                    dst: register(1),
                    value: int(1),
                },
                Instruction::Load {
                    dst: register(2),
                    value: int(INTEGER_LOOP_ITERATIONS as i64),
                },
                Instruction::Binary {
                    dst: register(0),
                    op: RuntimeBinaryOp::Add,
                    left: register(0),
                    right: register(1),
                },
                Instruction::Binary {
                    dst: register(3),
                    op: RuntimeBinaryOp::Lt,
                    left: register(0),
                    right: register(2),
                },
                Instruction::Branch {
                    condition: register(3),
                    if_true: 3,
                    if_false: 6,
                },
                Instruction::Return { value: operand(0) },
            ],
        )
        .unwrap();
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task(INTEGER_LOOP_BYTECODES),
        )
    }

    fn interpreted_integer_loop() -> Self {
        let mut context = Self::integer_loop();
        context.interpret_only = true;
        context
    }

    fn compiler_counter_loop() -> Self {
        let source = format!(
            "let i = 0\n\
             while i < {INTEGER_LOOP_ITERATIONS}\n\
               i = i + 1\n\
             end\n\
             return i",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task(COMPILER_COUNTER_LOOP_BYTECODES),
        )
    }

    fn interpreted_compiler_counter_loop() -> Self {
        let mut context = Self::compiler_counter_loop();
        context.interpret_only = true;
        context
    }

    fn compiler_accumulator_loop() -> Self {
        Self::compiler_accumulator_loop_with_annotations(false, false)
    }

    fn compiler_accumulator_loop_with_annotations(annotated: bool, interpret_only: bool) -> Self {
        let annotation = if annotated { ": int" } else { "" };
        let source = format!(
            "let i{annotation} = 0\n\
             let total{annotation} = 0\n\
             while i < {INTEGER_LOOP_ITERATIONS}\n\
               i = i + 1\n\
               total = total + i\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        let mut context = Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task(COMPILER_ACCUMULATOR_LOOP_BYTECODES),
        );
        context.interpret_only = interpret_only;
        context
    }

    fn interpreted_compiler_accumulator_loop() -> Self {
        Self::compiler_accumulator_loop_with_annotations(false, true)
    }

    fn compiler_countdown_loop() -> Self {
        let source = format!(
            "let i = {INTEGER_LOOP_ITERATIONS}\n\
             let total = 0\n\
             while i > 0\n\
               total = total + i\n\
               i = i - 1\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task(COMPILER_COUNTDOWN_LOOP_BYTECODES),
        )
    }

    fn interpreted_compiler_countdown_loop() -> Self {
        let mut context = Self::compiler_countdown_loop();
        context.interpret_only = true;
        context
    }

    fn compiler_arithmetic_loop() -> Self {
        let source = format!(
            "let i = {INTEGER_LOOP_ITERATIONS}\n\
             let total = 0\n\
             while i > 0\n\
               let scaled = i * 3\n\
               total = total + scaled\n\
               i = i - 1\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task(COMPILER_ARITHMETIC_LOOP_BYTECODES),
        )
    }

    fn interpreted_compiler_arithmetic_loop() -> Self {
        let mut context = Self::compiler_arithmetic_loop();
        context.interpret_only = true;
        context
    }

    fn compiler_integer_surface_loop() -> Self {
        let source = format!(
            "let i = 0\n\
             let total = 0\n\
             while i < {INTEGER_LOOP_ITERATIONS}\n\
               let scaled = i * 6\n\
               let quotient = scaled / 3\n\
               let remainder = i % 7\n\
               let negative = -remainder\n\
               let ignored = not i\n\
               total = total + quotient + remainder + negative\n\
               i = i + 1\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task(COMPILER_INTEGER_SURFACE_LOOP_BYTECODES),
        )
    }

    fn interpreted_compiler_integer_surface_loop() -> Self {
        let mut context = Self::compiler_integer_surface_loop();
        context.interpret_only = true;
        context
    }

    fn compiler_range_loop() -> Self {
        Self::compiler_range_loop_with_iterations(INTEGER_LOOP_ITERATIONS)
    }

    fn structural_constructor_match_loop(eliminate: bool) -> Self {
        let setup = if eliminate {
            String::new()
        } else {
            "fn escape(value)\n  return value\nend\n".to_owned()
        };
        let scrutinee = if eliminate {
            "some(i)"
        } else {
            "escape(some(i))"
        };
        let fallback = if eliminate {
            String::new()
        } else {
            "case _\n  0\n".to_owned()
        };
        let source = format!(
            "{setup}let i = 0\n\
             let total = 0\n\
             while i < {STRUCTURAL_MATCH_ITERATIONS}\n\
               let value = match {scrutinee}\n\
               case some(payload)\n\
                 payload\n\
               {fallback}end\n\
               total = total + value\n\
               i = i + 1\n\
             end\n\
             return total"
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(RelationKernel::new(), program, Workload::task(1_000_000))
    }

    fn compiler_range_loop_with_iterations(iterations: u64) -> Self {
        let source = format!(
            "let total = 0\n\
             for number in 1..{iterations}\n\
               total = total + number\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task((iterations * 8) + 10),
        )
    }

    fn interpreted_compiler_range_loop() -> Self {
        let mut context = Self::compiler_range_loop();
        context.interpret_only = true;
        context
    }

    fn interpreted_compiler_range_loop_with_iterations(iterations: u64) -> Self {
        let mut context = Self::compiler_range_loop_with_iterations(iterations);
        context.interpret_only = true;
        context
    }

    fn compiler_indexed_range_loop() -> Self {
        Self::compiler_indexed_range_loop_with_iterations(INTEGER_LOOP_ITERATIONS)
    }

    fn compiler_indexed_range_loop_with_iterations(iterations: u64) -> Self {
        let source = format!(
            "let total = 0\n\
             for index, number in 1..{iterations}\n\
               total = total + index + number\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task((iterations * 10) + 11),
        )
    }

    fn interpreted_compiler_indexed_range_loop() -> Self {
        let mut context = Self::compiler_indexed_range_loop();
        context.interpret_only = true;
        context
    }

    fn compiler_list_binding_loop(annotated: bool, interpret_only: bool) -> Self {
        let values = (1..=COLLECTION_BINDING_CHECK_ITERATIONS)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let annotation = if annotated { ": int" } else { "" };
        let source = format!(
            "let values = [{values}]\n\
             let total = 0\n\
             for value{annotation} in values\n\
               total = total + value\n\
             end\n\
             return total",
        );
        let program = compile_source(&source, &CompileContext::new())
            .unwrap()
            .program;
        let per_iteration = if annotated { 9 } else { 8 };
        let mut context = Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task((COLLECTION_BINDING_CHECK_ITERATIONS * per_iteration) + 10),
        );
        context.interpret_only = interpret_only;
        context
    }

    fn indexed_collection_traversal(collection: Value, iterations: u64) -> Self {
        let program = Program::new(
            11,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::CollectionLen {
                    dst: register(1),
                    collection: register(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: int(0),
                },
                Instruction::Load {
                    dst: register(3),
                    value: int(0),
                },
                Instruction::Load {
                    dst: register(4),
                    value: int(1),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 15,
                },
                Instruction::CollectionKeyAt {
                    dst: register(6),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::CollectionValueAt {
                    dst: register(7),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(6),
                },
                Instruction::Binary {
                    dst: register(9),
                    op: RuntimeBinaryOp::Add,
                    left: register(8),
                    right: register(7),
                },
                Instruction::Move {
                    dst: register(3),
                    src: register(9),
                },
                Instruction::Binary {
                    dst: register(10),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(10),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return { value: operand(3) },
            ],
        )
        .unwrap();
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task((iterations * 10) + 8),
        )
    }

    fn list_traversal() -> Self {
        Self::list_traversal_with_iterations(INTEGER_LOOP_ITERATIONS)
    }

    fn list_traversal_with_iterations(iterations: u64) -> Self {
        Self::indexed_collection_traversal(
            Value::list((1..=iterations as i64).map(int)),
            iterations,
        )
    }

    fn interpreted_list_traversal() -> Self {
        let mut context = Self::list_traversal();
        context.interpret_only = true;
        context
    }

    fn map_traversal() -> Self {
        Self::map_traversal_with_iterations(INTEGER_LOOP_ITERATIONS)
    }

    fn map_traversal_with_iterations(iterations: u64) -> Self {
        Self::indexed_collection_traversal(
            Value::map((0..iterations as i64).map(|index| (int(index), int(index + 1)))),
            iterations,
        )
    }

    fn interpreted_map_traversal() -> Self {
        let mut context = Self::map_traversal();
        context.interpret_only = true;
        context
    }

    fn list_index_loop() -> Self {
        Self::list_index_loop_with_iterations(INTEGER_LOOP_ITERATIONS)
    }

    fn list_index_loop_with_iterations(iterations: u64) -> Self {
        let collection = Value::list((1..=iterations as i64).map(int));
        Self::index_loop(collection, iterations)
    }

    fn map_index_loop() -> Self {
        Self::map_index_loop_with_iterations(INTEGER_LOOP_ITERATIONS)
    }

    fn map_index_loop_with_iterations(iterations: u64) -> Self {
        let collection =
            Value::map((0..iterations as i64).map(|index| (int(index), int(index + 1))));
        Self::index_loop(collection, iterations)
    }

    fn index_loop(collection: Value, iterations: u64) -> Self {
        let program = Program::new(
            9,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::Load {
                    dst: register(1),
                    value: int(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: int(0),
                },
                Instruction::Load {
                    dst: register(3),
                    value: int(iterations as i64),
                },
                Instruction::Load {
                    dst: register(4),
                    value: int(1),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(1),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 13,
                },
                Instruction::Index {
                    dst: register(6),
                    collection: register(0),
                    index: operand(1),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(6),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(7),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(8),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return { value: operand(2) },
            ],
        )
        .unwrap();
        Self::from_program(
            RelationKernel::new(),
            program,
            Workload::task((iterations * 8) + 8),
        )
    }

    fn interpreted_list_index_loop() -> Self {
        let mut context = Self::list_index_loop();
        context.interpret_only = true;
        context
    }

    fn interpreted_map_index_loop() -> Self {
        let mut context = Self::map_index_loop();
        context.interpret_only = true;
        context
    }

    fn indexed_relation_read() -> Self {
        let relation = relation_id(1);
        let key = int(7);
        let expected = int(11);
        let kernel = functional_kernel(relation, "Counter");
        let mut seed = kernel.begin();
        seed.replace_functional(relation, Tuple::from([key.clone(), expected.clone()]))
            .unwrap();
        seed.commit().unwrap();

        let program = Program::new(
            1,
            [
                Instruction::ScanValue {
                    dst: register(0),
                    relation,
                    key: value(key),
                },
                Instruction::Return { value: operand(0) },
            ],
        )
        .unwrap();
        let mut workload = Workload::task(2);
        workload.relation_reads = 1;
        Self::from_program(kernel, program, workload)
    }

    fn relation_value_scan(rows: usize, cell_kind: ScanCellKind) -> Self {
        let relation = relation_id(2);
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                relation,
                Symbol::intern("ScanBench"),
                3,
            ))
            .unwrap();
        let mut seed = kernel.begin();
        for index in 0..rows {
            let payload = match cell_kind {
                ScanCellKind::Immediate => int(index as i64),
                ScanCellKind::SharedHeap => Value::string(format!("scan-row-{index}")),
            };
            seed.assert(
                relation,
                Tuple::from([int(index as i64), payload, int((index % 32) as i64)]),
            )
            .unwrap();
        }
        seed.commit().unwrap();

        let program = Program::new(
            1,
            [
                Instruction::ScanBindings {
                    dst: register(0),
                    relation,
                    bindings: vec![None, None, None],
                    outputs: vec![
                        QueryBinding {
                            name: Symbol::intern("row"),
                            position: 0,
                        },
                        QueryBinding {
                            name: Symbol::intern("payload"),
                            position: 1,
                        },
                        QueryBinding {
                            name: Symbol::intern("bucket"),
                            position: 2,
                        },
                    ],
                },
                Instruction::Return { value: operand(0) },
            ],
        )
        .unwrap();
        let mut workload = Workload::task(2);
        workload.relation_reads = 1;
        workload.relation_rows = rows as u64;
        Self::from_program(kernel, program, workload)
    }

    fn read_modify_write() -> Self {
        let relation = relation_id(1);
        let key = int(7);
        let kernel = functional_kernel(relation, "Counter");
        let mut seed = kernel.begin();
        seed.replace_functional(relation, Tuple::from([key.clone(), int(0)]))
            .unwrap();
        seed.commit().unwrap();

        let program = Program::new(
            3,
            [
                Instruction::ScanValue {
                    dst: register(0),
                    relation,
                    key: value(key.clone()),
                },
                Instruction::Load {
                    dst: register(2),
                    value: int(1),
                },
                Instruction::Binary {
                    dst: register(1),
                    op: RuntimeBinaryOp::Add,
                    left: register(0),
                    right: register(2),
                },
                Instruction::ReplaceFunctional {
                    relation,
                    values: vec![value(key), operand(1)],
                },
                Instruction::Return { value: operand(1) },
            ],
        )
        .unwrap();
        let mut workload = Workload::task(5);
        workload.relation_reads = 1;
        workload.relation_writes = 1;
        workload.kernel_commits = 1;
        Self::from_program(kernel, program, workload)
    }

    fn warm_positional_dispatch() -> Self {
        let (kernel, relations, receiver, resolver) = Self::positional_dispatch_fixture();
        let program = Arc::new(
            Program::new(
                1,
                [
                    Instruction::PositionalDispatch {
                        dst: register(0),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::Return { value: operand(0) },
                ],
            )
            .unwrap(),
        );
        let mut workload = Workload::task(4);
        workload.dispatch_cache_lookups = 1;
        workload.method_program_cache_lookups = 1;
        workload.vm_resolved_program_lookups = 1;
        workload.program_resolver_lookups = 1;
        Self::warm_dispatch_context(kernel, program, resolver, workload)
    }

    fn repeated_positional_dispatch() -> Self {
        let (kernel, relations, receiver, resolver) = Self::positional_dispatch_fixture();
        let program = Arc::new(
            Program::new(
                5,
                [
                    Instruction::Load {
                        dst: register(0),
                        value: int(0),
                    },
                    Instruction::Load {
                        dst: register(1),
                        value: int(1),
                    },
                    Instruction::Load {
                        dst: register(2),
                        value: int(REPEATED_DISPATCH_ITERATIONS as i64),
                    },
                    Instruction::PositionalDispatch {
                        dst: register(3),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::Binary {
                        dst: register(0),
                        op: RuntimeBinaryOp::Add,
                        left: register(0),
                        right: register(1),
                    },
                    Instruction::Binary {
                        dst: register(4),
                        op: RuntimeBinaryOp::Lt,
                        left: register(0),
                        right: register(2),
                    },
                    Instruction::Branch {
                        condition: register(4),
                        if_true: 3,
                        if_false: 7,
                    },
                    Instruction::Return { value: operand(3) },
                ],
            )
            .unwrap(),
        );
        let mut workload = Workload::task((REPEATED_DISPATCH_ITERATIONS * 6) + 4);
        workload.dispatch_cache_lookups = REPEATED_DISPATCH_ITERATIONS;
        workload.method_program_cache_lookups = REPEATED_DISPATCH_ITERATIONS;
        workload.vm_resolved_program_lookups = REPEATED_DISPATCH_ITERATIONS;
        workload.program_resolver_lookups = 1;
        Self::warm_dispatch_context(kernel, program, resolver, workload)
    }

    fn repeated_call_path(path: CallPath) -> Self {
        let (kernel, relations, receiver, resolver) = Self::positional_dispatch_fixture();
        let callee = constant_return_program(7);
        let invocation = match path {
            CallPath::Dispatch => Instruction::PositionalDispatch {
                dst: register(3),
                relations: relations.dispatch,
                program_relation: relations.method_program,
                program_bytes: relations.program_bytes,
                selector: value(Value::symbol(Symbol::intern("benchmark"))),
                args: vec![value(Value::identity(receiver))],
            },
            CallPath::DirectCall => Instruction::Call {
                dst: register(3),
                program: callee,
                args: vec![value(Value::identity(receiver))],
            },
            CallPath::InterpretedInline | CallPath::NativeInline => Instruction::Load {
                dst: register(3),
                value: int(7),
            },
        };
        let program = Arc::new(
            Program::new(
                5,
                [
                    Instruction::Load {
                        dst: register(0),
                        value: int(0),
                    },
                    Instruction::Load {
                        dst: register(1),
                        value: int(1),
                    },
                    Instruction::Load {
                        dst: register(2),
                        value: int(CALL_PATH_INVOCATIONS as i64),
                    },
                    invocation,
                    Instruction::Binary {
                        dst: register(0),
                        op: RuntimeBinaryOp::Add,
                        left: register(0),
                        right: register(1),
                    },
                    Instruction::Binary {
                        dst: register(4),
                        op: RuntimeBinaryOp::Lt,
                        left: register(0),
                        right: register(2),
                    },
                    Instruction::Branch {
                        condition: register(4),
                        if_true: 3,
                        if_false: 7,
                    },
                    Instruction::Return { value: operand(3) },
                ],
            )
            .unwrap(),
        );
        let bytecodes_per_invocation = match path {
            CallPath::Dispatch | CallPath::DirectCall => 6,
            CallPath::InterpretedInline | CallPath::NativeInline => 4,
        };
        let mut workload = Workload::task((CALL_PATH_INVOCATIONS * bytecodes_per_invocation) + 4);
        workload.call_path_invocations = CALL_PATH_INVOCATIONS;
        if matches!(path, CallPath::Dispatch) {
            workload.dispatch_cache_lookups = CALL_PATH_INVOCATIONS;
            workload.method_program_cache_lookups = CALL_PATH_INVOCATIONS;
            workload.vm_resolved_program_lookups = CALL_PATH_INVOCATIONS;
            workload.program_resolver_lookups = 1;
        }
        let mut context = Self::warm_dispatch_context(kernel, program, resolver, workload);
        context.interpret_only = path.interpret_only();
        context
    }

    fn alternating_positional_dispatch() -> Self {
        let (kernel, relations, receiver, resolver) =
            Self::alternating_positional_dispatch_fixture();
        let loop_iterations = REPEATED_DISPATCH_ITERATIONS / 2;
        let program = Arc::new(
            Program::new(
                6,
                [
                    Instruction::Load {
                        dst: register(0),
                        value: int(0),
                    },
                    Instruction::Load {
                        dst: register(1),
                        value: int(1),
                    },
                    Instruction::Load {
                        dst: register(2),
                        value: int(loop_iterations as i64),
                    },
                    Instruction::PositionalDispatch {
                        dst: register(3),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark_a"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::PositionalDispatch {
                        dst: register(4),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark_b"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::Binary {
                        dst: register(0),
                        op: RuntimeBinaryOp::Add,
                        left: register(0),
                        right: register(1),
                    },
                    Instruction::Binary {
                        dst: register(5),
                        op: RuntimeBinaryOp::Lt,
                        left: register(0),
                        right: register(2),
                    },
                    Instruction::Branch {
                        condition: register(5),
                        if_true: 3,
                        if_false: 8,
                    },
                    Instruction::Return { value: operand(4) },
                ],
            )
            .unwrap(),
        );
        let mut workload = Workload::task((loop_iterations * 9) + 4);
        workload.dispatch_cache_lookups = REPEATED_DISPATCH_ITERATIONS;
        workload.method_program_cache_lookups = REPEATED_DISPATCH_ITERATIONS;
        workload.vm_resolved_program_lookups = REPEATED_DISPATCH_ITERATIONS;
        workload.program_resolver_lookups = 2;
        Self::warm_dispatch_context(kernel, program, resolver, workload)
    }

    fn alternating_call_path(path: CallPath) -> Self {
        let (kernel, relations, receiver, resolver) =
            Self::alternating_positional_dispatch_fixture();
        let first = match path {
            CallPath::Dispatch => Instruction::PositionalDispatch {
                dst: register(3),
                relations: relations.dispatch,
                program_relation: relations.method_program,
                program_bytes: relations.program_bytes,
                selector: value(Value::symbol(Symbol::intern("benchmark_a"))),
                args: vec![value(Value::identity(receiver))],
            },
            CallPath::DirectCall => Instruction::Call {
                dst: register(3),
                program: constant_return_program(7),
                args: vec![value(Value::identity(receiver))],
            },
            CallPath::InterpretedInline | CallPath::NativeInline => Instruction::Load {
                dst: register(3),
                value: int(7),
            },
        };
        let second = match path {
            CallPath::Dispatch => Instruction::PositionalDispatch {
                dst: register(3),
                relations: relations.dispatch,
                program_relation: relations.method_program,
                program_bytes: relations.program_bytes,
                selector: value(Value::symbol(Symbol::intern("benchmark_b"))),
                args: vec![value(Value::identity(receiver))],
            },
            CallPath::DirectCall => Instruction::Call {
                dst: register(3),
                program: constant_return_program(8),
                args: vec![value(Value::identity(receiver))],
            },
            CallPath::InterpretedInline | CallPath::NativeInline => Instruction::Load {
                dst: register(3),
                value: int(8),
            },
        };
        let rounds = CALL_PATH_INVOCATIONS / 2;
        let program = Arc::new(
            Program::new(
                5,
                [
                    Instruction::Load {
                        dst: register(0),
                        value: int(0),
                    },
                    Instruction::Load {
                        dst: register(1),
                        value: int(1),
                    },
                    Instruction::Load {
                        dst: register(2),
                        value: int(rounds as i64),
                    },
                    first,
                    second,
                    Instruction::Binary {
                        dst: register(0),
                        op: RuntimeBinaryOp::Add,
                        left: register(0),
                        right: register(1),
                    },
                    Instruction::Binary {
                        dst: register(4),
                        op: RuntimeBinaryOp::Lt,
                        left: register(0),
                        right: register(2),
                    },
                    Instruction::Branch {
                        condition: register(4),
                        if_true: 3,
                        if_false: 8,
                    },
                    Instruction::Return { value: operand(3) },
                ],
            )
            .unwrap(),
        );
        let bytecodes_per_round = match path {
            CallPath::Dispatch | CallPath::DirectCall => 9,
            CallPath::InterpretedInline | CallPath::NativeInline => 5,
        };
        let mut workload = Workload::task((rounds * bytecodes_per_round) + 4);
        workload.call_path_invocations = CALL_PATH_INVOCATIONS;
        if matches!(path, CallPath::Dispatch) {
            workload.dispatch_cache_lookups = CALL_PATH_INVOCATIONS;
            workload.method_program_cache_lookups = CALL_PATH_INVOCATIONS;
            workload.vm_resolved_program_lookups = CALL_PATH_INVOCATIONS;
            workload.program_resolver_lookups = 2;
        }
        let mut context = Self::warm_dispatch_context(kernel, program, resolver, workload);
        context.interpret_only = path.interpret_only();
        context
    }

    fn three_site_positional_dispatch() -> Self {
        let (kernel, relations, receiver, resolver) =
            Self::three_site_positional_dispatch_fixture();
        // The first A/B/C round fills the two-entry transaction cache with B and C. Later
        // rounds keep A as a stable miss without replacement churn.
        let program = Arc::new(
            Program::new(
                7,
                [
                    Instruction::Load {
                        dst: register(0),
                        value: int(0),
                    },
                    Instruction::Load {
                        dst: register(1),
                        value: int(1),
                    },
                    Instruction::Load {
                        dst: register(2),
                        value: int(THREE_SITE_DISPATCH_ROUNDS as i64),
                    },
                    Instruction::PositionalDispatch {
                        dst: register(3),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark_a"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::PositionalDispatch {
                        dst: register(4),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark_b"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::PositionalDispatch {
                        dst: register(5),
                        relations: relations.dispatch,
                        program_relation: relations.method_program,
                        program_bytes: relations.program_bytes,
                        selector: value(Value::symbol(Symbol::intern("benchmark_c"))),
                        args: vec![value(Value::identity(receiver))],
                    },
                    Instruction::Binary {
                        dst: register(0),
                        op: RuntimeBinaryOp::Add,
                        left: register(0),
                        right: register(1),
                    },
                    Instruction::Binary {
                        dst: register(6),
                        op: RuntimeBinaryOp::Lt,
                        left: register(0),
                        right: register(2),
                    },
                    Instruction::Branch {
                        condition: register(6),
                        if_true: 3,
                        if_false: 9,
                    },
                    Instruction::Return { value: operand(5) },
                ],
            )
            .unwrap(),
        );
        let mut workload = Workload::task((THREE_SITE_DISPATCH_ROUNDS * 12) + 4);
        workload.dispatch_cache_lookups = THREE_SITE_DISPATCHES_PER_TASK;
        workload.method_program_cache_lookups = THREE_SITE_DISPATCHES_PER_TASK;
        workload.vm_resolved_program_lookups = THREE_SITE_DISPATCHES_PER_TASK;
        workload.program_resolver_lookups = 3;
        Self::warm_dispatch_context(kernel, program, resolver, workload)
    }

    fn positional_dispatch_fixture() -> (
        RelationKernel,
        MethodRelations,
        Identity,
        Arc<ProgramResolver>,
    ) {
        let relations = method_relations();
        let method = identity(100);
        let method_program = identity(101);
        let prototype = identity(200);
        let receiver = identity(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel, relations);

        let context = CompileContext::new()
            .with_method_relations(relations)
            .with_identity("benchmark_method", method)
            .with_program_identity("benchmark_method", method_program)
            .with_identity("benchmark_prototype", prototype);
        let mut install = kernel.begin();
        let installation = install_methods_from_source(
            "method #benchmark_method :benchmark\n\
               roles receiver @ #benchmark_prototype\n\
             do\n\
               return 7\n\
             end",
            &context,
            &mut install,
        )
        .unwrap();
        install
            .assert(
                relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(receiver),
                    Value::identity(prototype),
                    int(0),
                ]),
            )
            .unwrap();
        install.commit().unwrap();

        let installed = installation.methods.into_iter().next().unwrap();
        let method_bytecodes = installed.compiled.program.instructions();
        assert!(matches!(method_bytecodes[0], Instruction::Load { .. }));
        assert!(matches!(method_bytecodes[1], Instruction::Return { .. }));
        let resolver = Arc::new(
            ProgramResolver::new().with_program(installed.program, installed.compiled.program),
        );
        (kernel, relations, receiver, resolver)
    }

    fn alternating_positional_dispatch_fixture() -> (
        RelationKernel,
        MethodRelations,
        Identity,
        Arc<ProgramResolver>,
    ) {
        let relations = method_relations();
        let method_a = identity(100);
        let method_program_a = identity(101);
        let method_b = identity(102);
        let method_program_b = identity(103);
        let prototype = identity(200);
        let receiver = identity(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel, relations);

        let context = CompileContext::new()
            .with_method_relations(relations)
            .with_identity("benchmark_method_a", method_a)
            .with_program_identity("benchmark_method_a", method_program_a)
            .with_identity("benchmark_method_b", method_b)
            .with_program_identity("benchmark_method_b", method_program_b)
            .with_identity("benchmark_prototype", prototype);
        let mut install = kernel.begin();
        let installation = install_methods_from_source(
            "method #benchmark_method_a :benchmark_a\n\
               roles receiver @ #benchmark_prototype\n\
             do\n\
               return 7\n\
             end\n\
             method #benchmark_method_b :benchmark_b\n\
               roles receiver @ #benchmark_prototype\n\
             do\n\
               return 8\n\
             end",
            &context,
            &mut install,
        )
        .unwrap();
        install
            .assert(
                relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(receiver),
                    Value::identity(prototype),
                    int(0),
                ]),
            )
            .unwrap();
        install.commit().unwrap();

        assert_eq!(installation.methods.len(), 2);
        let resolver =
            installation
                .methods
                .into_iter()
                .fold(ProgramResolver::new(), |resolver, installed| {
                    resolver.with_program(installed.program, installed.compiled.program)
                });
        (kernel, relations, receiver, Arc::new(resolver))
    }

    fn three_site_positional_dispatch_fixture() -> (
        RelationKernel,
        MethodRelations,
        Identity,
        Arc<ProgramResolver>,
    ) {
        let relations = method_relations();
        let method_a = identity(100);
        let method_program_a = identity(101);
        let method_b = identity(102);
        let method_program_b = identity(103);
        let method_c = identity(104);
        let method_program_c = identity(105);
        let prototype = identity(200);
        let receiver = identity(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel, relations);

        let context = CompileContext::new()
            .with_method_relations(relations)
            .with_identity("benchmark_method_a", method_a)
            .with_program_identity("benchmark_method_a", method_program_a)
            .with_identity("benchmark_method_b", method_b)
            .with_program_identity("benchmark_method_b", method_program_b)
            .with_identity("benchmark_method_c", method_c)
            .with_program_identity("benchmark_method_c", method_program_c)
            .with_identity("benchmark_prototype", prototype);
        let mut install = kernel.begin();
        let installation = install_methods_from_source(
            "method #benchmark_method_a :benchmark_a\n\
               roles receiver @ #benchmark_prototype\n\
             do\n\
               return 7\n\
             end\n\
             method #benchmark_method_b :benchmark_b\n\
               roles receiver @ #benchmark_prototype\n\
             do\n\
               return 8\n\
             end\n\
             method #benchmark_method_c :benchmark_c\n\
               roles receiver @ #benchmark_prototype\n\
             do\n\
               return 9\n\
             end",
            &context,
            &mut install,
        )
        .unwrap();
        install
            .assert(
                relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(receiver),
                    Value::identity(prototype),
                    int(0),
                ]),
            )
            .unwrap();
        install.commit().unwrap();

        assert_eq!(installation.methods.len(), 3);
        let resolver =
            installation
                .methods
                .into_iter()
                .fold(ProgramResolver::new(), |resolver, installed| {
                    resolver.with_program(installed.program, installed.compiled.program)
                });
        (kernel, relations, receiver, Arc::new(resolver))
    }

    fn warm_dispatch_context(
        kernel: RelationKernel,
        program: Arc<Program>,
        resolver: Arc<ProgramResolver>,
        workload: Workload,
    ) -> Self {
        let mut context = Self {
            kernel,
            program,
            resolver,
            builtins: Arc::new(BuiltinRegistry::new()),
            next_task_id: 1,
            workload,
            interpret_only: false,
        };

        // Populate the snapshot dispatch caches before micromeasure opens its timing window.
        context.run_one();
        context
    }

    fn run_one(&mut self) {
        let task_id = self.next_task_id;
        self.next_task_id += 1;
        let mut task = Task::new_with_builtins(
            task_id,
            &self.kernel,
            Arc::clone(&self.program),
            Arc::clone(&self.resolver),
            Arc::clone(&self.builtins),
            TaskLimits {
                instruction_budget: self.workload.executed_bytecodes as usize + 1,
                max_retries: 0,
                max_call_depth: MAX_CALL_DEPTH,
            },
        );
        if self.interpret_only {
            task.vm_mut().disable_native_execution();
        }
        let outcome = task.run().unwrap();
        debug_assert!(matches!(outcome, TaskOutcome::Complete { .. }));
        black_box(outcome);
    }
}

impl BenchContext for TaskBenchContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::minimal_return()
    }
}

fn run_tasks(context: &mut TaskBenchContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        context.run_one();
    }
}

fn run_cold_loop(context: &mut ColdLoopBenchContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        let program =
            Program::new(context.register_count, context.instructions.iter().cloned()).unwrap();
        let mut task =
            TaskBenchContext::from_program(RelationKernel::new(), program, context.workload);
        task.interpret_only = context.interpret_only;
        task.run_one();
    }
}

fn run_concurrent_tasks(
    context: &ConcurrentTaskBenchContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut task_id = ((control.thread_index() as u64) + 1) << 48;
    let mut tasks = 0_u64;
    let mut dispatches = 0_u64;
    while !control.should_stop() {
        context.run_one(task_id);
        task_id = task_id.wrapping_add(1);
        tasks = tasks.wrapping_add(1);
        dispatches = dispatches.wrapping_add(context.dispatches_per_task);
    }
    ConcurrentWorkerResult::operations(dispatches).with_counter("tasks", tasks)
}

fn run_concurrent_call_path(
    context: &ConcurrentTaskBenchContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut task_id = ((control.thread_index() as u64) + 1) << 48;
    let mut tasks = 0_u64;
    let mut invocations = 0_u64;
    while !control.should_stop() {
        context.run_one(task_id);
        task_id = task_id.wrapping_add(1);
        tasks = tasks.wrapping_add(1);
        invocations = invocations.wrapping_add(context.call_path_invocations_per_task);
    }
    ConcurrentWorkerResult::operations(invocations).with_counter("tasks", tasks)
}

fn run_concurrent_integer_tasks(
    context: &ConcurrentTaskBenchContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut task_id = ((control.thread_index() as u64) + 1) << 48;
    let mut tasks = 0_u64;
    let mut bytecodes = 0_u64;
    while !control.should_stop() {
        context.run_one(task_id);
        task_id = task_id.wrapping_add(1);
        tasks = tasks.wrapping_add(1);
        bytecodes = bytecodes.wrapping_add(context.bytecodes_per_task);
    }
    ConcurrentWorkerResult::operations(bytecodes).with_counter("tasks", tasks)
}

fn run_concurrent_structural_match_tasks(
    context: &ConcurrentTaskBenchContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut task_id = ((control.thread_index() as u64) + 1) << 48;
    let mut tasks = 0_u64;
    while !control.should_stop() {
        context.run_one(task_id);
        task_id = task_id.wrapping_add(1);
        tasks = tasks.wrapping_add(1);
    }
    ConcurrentWorkerResult::operations(tasks.wrapping_mul(STRUCTURAL_MATCH_ITERATIONS))
        .with_counter("tasks", tasks)
}

fn run_concurrent_relation_scans(
    context: &ConcurrentTaskBenchContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut task_id = ((control.thread_index() as u64) + 1) << 48;
    let mut tasks = 0_u64;
    let mut rows = 0_u64;
    while !control.should_stop() {
        context.run_one(task_id);
        task_id = task_id.wrapping_add(1);
        tasks = tasks.wrapping_add(1);
        rows = rows.wrapping_add(context.relation_rows_per_task);
    }
    ConcurrentWorkerResult::operations(rows).with_counter("tasks", tasks)
}

fn run_concurrent_collection_binding_tasks(
    context: &ConcurrentTaskBenchContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut task_id = ((control.thread_index() as u64) + 1) << 48;
    let mut tasks = 0_u64;
    let mut elements = 0_u64;
    while !control.should_stop() {
        context.run_one(task_id);
        task_id = task_id.wrapping_add(1);
        tasks = tasks.wrapping_add(1);
        elements = elements.wrapping_add(COLLECTION_BINDING_CHECK_ITERATIONS);
    }
    ConcurrentWorkerResult::operations(elements).with_counter("tasks", tasks)
}

fn workload_diagnostics(
    context: &mut TaskBenchContext,
    _chunk_size: usize,
    _chunk_num: usize,
) -> Result<DiagnosticResult, DiagnosticError> {
    let workload = context.workload;
    Ok(DiagnosticResult::new("work per task")
        .push_metric(MetricValue::integer(
            "executed_bytecodes_per_task",
            workload.executed_bytecodes as i64,
            "bytecodes",
        ))
        .push_metric(MetricValue::integer(
            "relation_operations_per_task",
            (workload.relation_reads + workload.relation_writes) as i64,
            "operations",
        ))
        .push_metric(MetricValue::integer(
            "relation_reads_per_task",
            workload.relation_reads as i64,
            "reads",
        ))
        .push_metric(MetricValue::integer(
            "relation_rows_per_task",
            workload.relation_rows as i64,
            "rows",
        ))
        .push_metric(MetricValue::integer(
            "relation_writes_per_task",
            workload.relation_writes as i64,
            "writes",
        ))
        .push_metric(MetricValue::integer(
            "commit_boundaries_per_task",
            workload.commit_boundaries as i64,
            "boundaries",
        ))
        .push_metric(MetricValue::integer(
            "kernel_commits_per_task",
            workload.kernel_commits as i64,
            "commits",
        ))
        .push_metric(MetricValue::integer(
            "dispatch_cache_lookups_per_task",
            workload.dispatch_cache_lookups as i64,
            "lookups",
        ))
        .push_metric(MetricValue::integer(
            "method_program_cache_lookups_per_task",
            workload.method_program_cache_lookups as i64,
            "lookups",
        ))
        .push_metric(MetricValue::integer(
            "vm_resolved_program_lookups_per_task",
            workload.vm_resolved_program_lookups as i64,
            "lookups",
        ))
        .push_metric(MetricValue::integer(
            "program_resolver_lookups_per_task",
            workload.program_resolver_lookups as i64,
            "lookups",
        )))
}

fn functional_kernel(relation: RelationId, name: &str) -> RelationKernel {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(relation, Symbol::intern(name), 2)
                .with_index([0])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![0],
                }),
        )
        .unwrap();
    kernel
}

fn method_relations() -> MethodRelations {
    MethodRelations {
        dispatch: DispatchRelations {
            method_selector: relation_id(40),
            param: relation_id(41),
            delegates: relation_id(42),
        },
        method_program: relation_id(43),
        program_bytes: relation_id(44),
    }
}

fn create_method_relations(kernel: &RelationKernel, relations: MethodRelations) {
    kernel
        .create_relation(
            RelationMetadata::new(
                relations.dispatch.method_selector,
                Symbol::intern("MethodSelector"),
                2,
            )
            .with_index([1, 0]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(relations.dispatch.param, Symbol::intern("Param"), 4)
                .with_index([0, 1]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(relations.dispatch.delegates, Symbol::intern("Delegates"), 3)
                .with_index([0, 2, 1]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(relations.method_program, Symbol::intern("MethodProgram"), 2)
                .with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(relations.program_bytes, Symbol::intern("ProgramBytes"), 2)
                .with_index([0]),
        )
        .unwrap();
}

fn relation_id(raw: u64) -> RelationId {
    identity(raw)
}

fn identity(raw: u64) -> Identity {
    Identity::new(raw).unwrap()
}

fn register(index: u16) -> Register {
    Register(index)
}

fn operand(index: u16) -> Operand {
    Operand::Register(register(index))
}

fn value(value: Value) -> Operand {
    Operand::Value(value)
}

fn int(value: i64) -> Value {
    Value::int(value).unwrap()
}

fn constant_return_program(result: i64) -> Arc<Program> {
    Arc::new(
        Program::new(
            2,
            [
                Instruction::Load {
                    dst: register(1),
                    value: int(result),
                },
                Instruction::Return { value: operand(1) },
            ],
        )
        .unwrap(),
    )
}

benchmark_main!(
    BenchmarkMainOptions {
        filter_help: Some(
            "all, minimal, integer, read, write, dispatch, call_path, value_kind, collection_binding, or a benchmark name substring"
                .to_owned(),
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
        runner.group::<TaskBenchContext>("task", |group| {
            let minimal = || TaskBenchContext::minimal_return();
            group
                .throughput(Throughput::ops())
                .factory(&minimal)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_minimal_return", run_tasks);

            let interpreted_integer = || TaskBenchContext::interpreted_integer_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_integer)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_integer_loop", run_tasks);

            let cranelift_integer = || TaskBenchContext::integer_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_integer)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_integer_loop", run_tasks);

            let interpreted_compiler_counter =
                || TaskBenchContext::interpreted_compiler_counter_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_counter)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_counter_loop", run_tasks);

            let cranelift_compiler_counter = || TaskBenchContext::compiler_counter_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_counter)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_counter_loop", run_tasks);

            let interpreted_compiler_accumulator =
                || TaskBenchContext::interpreted_compiler_accumulator_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_accumulator)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_accumulator_loop", run_tasks);

            let cranelift_compiler_accumulator = || TaskBenchContext::compiler_accumulator_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_accumulator)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_accumulator_loop", run_tasks);

            let interpreted_compiler_countdown =
                || TaskBenchContext::interpreted_compiler_countdown_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_countdown)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_countdown_loop", run_tasks);

            let cranelift_compiler_countdown = || TaskBenchContext::compiler_countdown_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_countdown)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_countdown_loop", run_tasks);

            let interpreted_compiler_arithmetic =
                || TaskBenchContext::interpreted_compiler_arithmetic_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_arithmetic)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_arithmetic_loop", run_tasks);

            let cranelift_compiler_arithmetic = || TaskBenchContext::compiler_arithmetic_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_arithmetic)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_arithmetic_loop", run_tasks);

            let interpreted_compiler_integer_surface =
                || TaskBenchContext::interpreted_compiler_integer_surface_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_integer_surface)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_integer_surface_loop", run_tasks);

            let cranelift_compiler_integer_surface =
                || TaskBenchContext::compiler_integer_surface_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_integer_surface)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_integer_surface_loop", run_tasks);

            let interpreted_compiler_range = || TaskBenchContext::interpreted_compiler_range_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_range)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_range_loop", run_tasks);

            let cranelift_compiler_range = || TaskBenchContext::compiler_range_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_range)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_range_loop", run_tasks);

            let interpreted_compiler_indexed_range =
                || TaskBenchContext::interpreted_compiler_indexed_range_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_compiler_indexed_range)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_compiler_indexed_range_loop", run_tasks);

            let cranelift_compiler_indexed_range =
                || TaskBenchContext::compiler_indexed_range_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_compiler_indexed_range)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_compiler_indexed_range_loop", run_tasks);

            let interpreted_list_traversal = || TaskBenchContext::interpreted_list_traversal();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_list_traversal)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_list_traversal", run_tasks);

            let cranelift_list_traversal = || TaskBenchContext::list_traversal();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_list_traversal)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_list_traversal", run_tasks);

            let interpreted_list_index = || TaskBenchContext::interpreted_list_index_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_list_index)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_list_index_loop", run_tasks);

            let cranelift_list_index = || TaskBenchContext::list_index_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_list_index)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_list_index_loop", run_tasks);

            let interpreted_map_traversal = || TaskBenchContext::interpreted_map_traversal();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_map_traversal)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_map_traversal", run_tasks);

            let cranelift_map_traversal = || TaskBenchContext::map_traversal();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_map_traversal)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_map_traversal", run_tasks);

            let interpreted_map_index = || TaskBenchContext::interpreted_map_index_loop();
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_map_index)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_interpreter_map_index_loop", run_tasks);

            let cranelift_map_index = || TaskBenchContext::map_index_loop();
            group
                .throughput(Throughput::ops())
                .factory(&cranelift_map_index)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_cranelift_map_index_loop", run_tasks);

            let read = || TaskBenchContext::indexed_relation_read();
            group
                .throughput(Throughput::ops())
                .factory(&read)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_indexed_relation_read", run_tasks);

            for rows in [32, RELATION_SCAN_ROWS] {
                for cell_kind in [ScanCellKind::Immediate, ScanCellKind::SharedHeap] {
                    let scan = move || TaskBenchContext::relation_value_scan(rows, cell_kind);
                    let name = format!(
                        "task_scan_bindings_relation_value_{}_{}_rows",
                        cell_kind.name(),
                        rows
                    );
                    group
                        .throughput(Throughput::per_operation(rows as u64, "rows"))
                        .factory(&scan)
                        .diagnostic_pass(workload_diagnostics)
                        .bench(&name, run_tasks);
                }
            }

            let write = || TaskBenchContext::read_modify_write();
            group
                .throughput(Throughput::ops())
                .factory(&write)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_read_modify_write_commit", run_tasks);

            let dispatch = || TaskBenchContext::warm_positional_dispatch();
            group
                .throughput(Throughput::ops())
                .factory(&dispatch)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_warm_positional_dispatch", run_tasks);

            let repeated_dispatch = || TaskBenchContext::repeated_positional_dispatch();
            group
                .throughput(Throughput::ops())
                .factory(&repeated_dispatch)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_repeated_positional_dispatch", run_tasks);

            let alternating_dispatch = || TaskBenchContext::alternating_positional_dispatch();
            group
                .throughput(Throughput::ops())
                .factory(&alternating_dispatch)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_alternating_positional_dispatch", run_tasks);

            let three_site_dispatch = || TaskBenchContext::three_site_positional_dispatch();
            group
                .throughput(Throughput::ops())
                .factory(&three_site_dispatch)
                .diagnostic_pass(workload_diagnostics)
                .bench("task_three_site_positional_dispatch", run_tasks);
        });

        runner.group::<TaskBenchContext>("collection binding checks warm", |group| {
            for (backend, interpret_only) in [("interpreter", true), ("cranelift", false)] {
                for (shape, annotated) in
                    [("dynamic_unannotated", false), ("dynamic_checked", true)]
                {
                    let factory = move || {
                        TaskBenchContext::compiler_list_binding_loop(annotated, interpret_only)
                    };
                    let name = format!("collection_binding_checks_{backend}_{shape}");
                    group
                        .throughput(Throughput::per_operation(
                            COLLECTION_BINDING_CHECK_ITERATIONS,
                            "elements",
                        ))
                        .factory(&factory)
                        .diagnostic_pass(workload_diagnostics)
                        .bench(&name, run_tasks);
                }
            }
        });

        runner.group::<TaskBenchContext>("structural constructor match warm", |group| {
            for (name, eliminate) in [("boxed", false), ("eliminated", true)] {
                let factory = move || TaskBenchContext::structural_constructor_match_loop(eliminate);
                group
                    .throughput(Throughput::per_operation(
                        STRUCTURAL_MATCH_ITERATIONS,
                        "matches",
                    ))
                    .factory(&factory)
                    .bench(&format!("structural_constructor_match_{name}"), run_tasks);
            }
        });

        runner.group::<TaskBenchContext>("value kind facts warm", |group| {
            for (backend, interpret_only) in [("interpreter", true), ("cranelift", false)] {
                for (fact_source, annotated) in [("inferred", false), ("annotated_proven", true)] {
                    let factory = move || {
                        TaskBenchContext::compiler_accumulator_loop_with_annotations(
                            annotated,
                            interpret_only,
                        )
                    };
                    group
                        .throughput(Throughput::per_operation(
                            COMPILER_ACCUMULATOR_LOOP_BYTECODES,
                            "bytecodes",
                        ))
                        .factory(&factory)
                        .bench(
                            &format!("value_kind_facts_{backend}_{fact_source}_warm"),
                            run_tasks,
                        );
                }
            }
        });

        runner.group::<TaskBenchContext>("verb call path experiment warm", |group| {
            for (shape, context) in [
                (
                    "repeated",
                    TaskBenchContext::repeated_call_path as fn(CallPath) -> TaskBenchContext,
                ),
                (
                    "alternating",
                    TaskBenchContext::alternating_call_path as fn(CallPath) -> TaskBenchContext,
                ),
            ] {
                for path in CallPath::ALL {
                    let factory = move || context(path);
                    let name = format!("verb_call_path_{shape}_{}", path.name());
                    group
                        .throughput(Throughput::per_operation(
                            CALL_PATH_INVOCATIONS,
                            "invocations",
                        ))
                        .factory(&factory)
                        .diagnostic_pass(workload_diagnostics)
                        .bench(&name, run_tasks);
                }
            }
        });

        runner.group::<TaskBenchContext>("range JIT admission warm", |group| {
            for iterations in RANGE_ADMISSION_ITERATIONS {
                let interpreted = || {
                    TaskBenchContext::interpreted_compiler_range_loop_with_iterations(iterations)
                };
                let interpreted_name =
                    format!("range_admission_warm_interpreter_{iterations}_iterations");
                group
                    .throughput(Throughput::ops())
                    .factory(&interpreted)
                    .bench(&interpreted_name, run_tasks);

                let native_enabled =
                    || TaskBenchContext::compiler_range_loop_with_iterations(iterations);
                let native_enabled_name =
                    format!("range_admission_warm_native_enabled_{iterations}_iterations");
                group
                    .throughput(Throughput::ops())
                    .factory(&native_enabled)
                    .bench(&native_enabled_name, run_tasks);
            }
        });

        runner.group::<ColdLoopBenchContext>("task compiler loop cold", |group| {
            // These cases rebuild the compiler-produced Program so the timing
            // includes program creation, JIT compilation, and task execution.
            let counter = || ColdLoopBenchContext::counter();
            group
                .throughput(Throughput::ops())
                .factory(&counter)
                .bench("task_cranelift_compiler_counter_loop_cold", run_cold_loop);

            let accumulator = || ColdLoopBenchContext::accumulator();
            group
                .throughput(Throughput::ops())
                .factory(&accumulator)
                .bench(
                    "task_cranelift_compiler_accumulator_loop_cold",
                    run_cold_loop,
                );

            let countdown = || ColdLoopBenchContext::countdown();
            group
                .throughput(Throughput::ops())
                .factory(&countdown)
                .bench("task_cranelift_compiler_countdown_loop_cold", run_cold_loop);

            let arithmetic = || ColdLoopBenchContext::arithmetic();
            group
                .throughput(Throughput::ops())
                .factory(&arithmetic)
                .bench(
                    "task_cranelift_compiler_arithmetic_loop_cold",
                    run_cold_loop,
                );

            let integer_surface = || ColdLoopBenchContext::integer_surface();
            group
                .throughput(Throughput::ops())
                .factory(&integer_surface)
                .bench(
                    "task_cranelift_compiler_integer_surface_loop_cold",
                    run_cold_loop,
                );
        });

        runner.group::<ColdLoopBenchContext>("value kind facts cold", |group| {
            for (backend, interpret_only) in [("interpreter", true), ("cranelift", false)] {
                for (fact_source, annotated) in [("inferred", false), ("annotated_proven", true)] {
                    let factory = move || {
                        ColdLoopBenchContext::accumulator_with_annotations(
                            annotated,
                            interpret_only,
                        )
                    };
                    group
                        .throughput(Throughput::per_operation(1, "task"))
                        .factory(&factory)
                        .bench(
                            &format!("value_kind_facts_{backend}_{fact_source}_cold"),
                            run_cold_loop,
                        );
                }
            }
        });

        runner.group::<ColdLoopBenchContext>("collection binding checks cold", |group| {
            for (backend, interpret_only) in [("interpreter", true), ("cranelift", false)] {
                for (shape, annotated) in
                    [("dynamic_unannotated", false), ("dynamic_checked", true)]
                {
                    let factory = move || {
                        ColdLoopBenchContext::collection(
                            TaskBenchContext::compiler_list_binding_loop(
                                annotated,
                                interpret_only,
                            ),
                            interpret_only,
                        )
                    };
                    group
                        .throughput(Throughput::per_operation(
                            COLLECTION_BINDING_CHECK_ITERATIONS,
                            "elements",
                        ))
                        .factory(&factory)
                        .bench(
                            &format!("collection_binding_checks_{backend}_{shape}_cold"),
                            run_cold_loop,
                        );
                }
            }
        });

        runner.group::<ColdLoopBenchContext>("range JIT admission cold", |group| {
            for iterations in RANGE_ADMISSION_ITERATIONS {
                let interpreted = || ColdLoopBenchContext::range(iterations, true);
                let interpreted_name =
                    format!("range_admission_cold_interpreter_{iterations}_iterations");
                group
                    .throughput(Throughput::ops())
                    .factory(&interpreted)
                    .bench(&interpreted_name, run_cold_loop);

                let native_enabled = || ColdLoopBenchContext::range(iterations, false);
                let native_enabled_name =
                    format!("range_admission_cold_native_enabled_{iterations}_iterations");
                group
                    .throughput(Throughput::ops())
                    .factory(&native_enabled)
                    .bench(&native_enabled_name, run_cold_loop);
            }
        });

        runner.group::<ColdLoopBenchContext>("indexed range loop cold", |group| {
            let interpreted = || ColdLoopBenchContext::indexed_range(8_192, true);
            group
                .throughput(Throughput::ops())
                .factory(&interpreted)
                .bench(
                    "indexed_range_cold_interpreter_8192_iterations",
                    run_cold_loop,
                );

            let native_enabled = || ColdLoopBenchContext::indexed_range(8_192, false);
            group
                .throughput(Throughput::ops())
                .factory(&native_enabled)
                .bench(
                    "indexed_range_cold_native_enabled_8192_iterations",
                    run_cold_loop,
                );
        });

        runner.group::<ColdLoopBenchContext>("collection loop cold", |group| {
            let interpreted_list = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::list_traversal_with_iterations(8_192),
                    true,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_list)
                .bench("list_traversal_cold_interpreter", run_cold_loop);
            let native_list = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::list_traversal_with_iterations(8_192),
                    false,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&native_list)
                .bench("list_traversal_cold_cranelift", run_cold_loop);

            let interpreted_index = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::list_index_loop_with_iterations(8_192),
                    true,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_index)
                .bench("list_index_cold_interpreter", run_cold_loop);
            let native_index = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::list_index_loop_with_iterations(8_192),
                    false,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&native_index)
                .bench("list_index_cold_cranelift", run_cold_loop);

            let interpreted_map = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::map_traversal_with_iterations(8_192),
                    true,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_map)
                .bench("map_traversal_cold_interpreter", run_cold_loop);
            let native_map = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::map_traversal_with_iterations(8_192),
                    false,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&native_map)
                .bench("map_traversal_cold_cranelift", run_cold_loop);

            let interpreted_map_index = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::map_index_loop_with_iterations(8_192),
                    true,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&interpreted_map_index)
                .bench("map_index_cold_interpreter", run_cold_loop);
            let native_map_index = || {
                ColdLoopBenchContext::collection(
                    TaskBenchContext::map_index_loop_with_iterations(8_192),
                    false,
                )
            };
            group
                .throughput(Throughput::ops())
                .factory(&native_map_index)
                .bench("map_index_cold_cranelift", run_cold_loop);
        });

        let single_worker = [ConcurrentWorker {
            name: "dispatch",
            threads: 1,
            run: run_concurrent_tasks,
        }];
        let concurrent_workers = [ConcurrentWorker {
            name: "dispatch",
            threads: CONCURRENT_DISPATCH_THREADS,
            run: run_concurrent_tasks,
        }];
        let single_call_path_worker = [ConcurrentWorker {
            name: "call path",
            threads: 1,
            run: run_concurrent_call_path,
        }];
        let concurrent_call_path_workers = [ConcurrentWorker {
            name: "call path",
            threads: CONCURRENT_DISPATCH_THREADS,
            run: run_concurrent_call_path,
        }];
        let single_integer_worker = [ConcurrentWorker {
            name: "integer loop",
            threads: 1,
            run: run_concurrent_integer_tasks,
        }];
        let concurrent_integer_workers = [ConcurrentWorker {
            name: "integer loop",
            threads: CONCURRENT_DISPATCH_THREADS,
            run: run_concurrent_integer_tasks,
        }];
        let single_structural_match_worker = [ConcurrentWorker {
            name: "structural match",
            threads: 1,
            run: run_concurrent_structural_match_tasks,
        }];
        let concurrent_structural_match_workers = [ConcurrentWorker {
            name: "structural match",
            threads: CONCURRENT_DISPATCH_THREADS,
            run: run_concurrent_structural_match_tasks,
        }];
        let single_relation_scan_worker = [ConcurrentWorker {
            name: "relation scan",
            threads: 1,
            run: run_concurrent_relation_scans,
        }];
        let concurrent_relation_scan_workers = [ConcurrentWorker {
            name: "relation scan",
            threads: CONCURRENT_DISPATCH_THREADS,
            run: run_concurrent_relation_scans,
        }];
        let single_collection_binding_worker = [ConcurrentWorker {
            name: "collection binding",
            threads: 1,
            run: run_concurrent_collection_binding_tasks,
        }];
        let concurrent_collection_binding_workers = [ConcurrentWorker {
            name: "collection binding",
            threads: CONCURRENT_DISPATCH_THREADS,
            run: run_concurrent_collection_binding_tasks,
        }];
        let interpreted_integer = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_integer_loop(),
            )
        };
        let cranelift_integer =
            |_| ConcurrentTaskBenchContext::from_task_context(TaskBenchContext::integer_loop());
        let interpreted_compiler_counter = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_counter_loop(),
            )
        };
        let cranelift_compiler_counter = |_| {
            ConcurrentTaskBenchContext::from_task_context(TaskBenchContext::compiler_counter_loop())
        };
        let interpreted_compiler_accumulator = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_accumulator_loop(),
            )
        };
        let cranelift_compiler_accumulator = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::compiler_accumulator_loop(),
            )
        };
        let interpreted_compiler_countdown = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_countdown_loop(),
            )
        };
        let cranelift_compiler_countdown = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::compiler_countdown_loop(),
            )
        };
        let interpreted_compiler_arithmetic = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_arithmetic_loop(),
            )
        };
        let cranelift_compiler_arithmetic = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::compiler_arithmetic_loop(),
            )
        };
        let interpreted_compiler_integer_surface = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_integer_surface_loop(),
            )
        };
        let cranelift_compiler_integer_surface = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::compiler_integer_surface_loop(),
            )
        };
        let interpreted_compiler_range = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_range_loop(),
            )
        };
        let cranelift_compiler_range = |_| {
            ConcurrentTaskBenchContext::from_task_context(TaskBenchContext::compiler_range_loop())
        };
        let interpreted_compiler_indexed_range = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::interpreted_compiler_indexed_range_loop(),
            )
        };
        let cranelift_compiler_indexed_range = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::compiler_indexed_range_loop(),
            )
        };
        let one_shot = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::warm_positional_dispatch(),
            )
        };
        let repeated = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::repeated_positional_dispatch(),
            )
        };
        let alternating = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::alternating_positional_dispatch(),
            )
        };
        let three_site = |_| {
            ConcurrentTaskBenchContext::from_task_context(
                TaskBenchContext::three_site_positional_dispatch(),
            )
        };
        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "structural constructor match concurrent",
            |group| {
                for (name, eliminate) in [("boxed", false), ("eliminated", true)] {
                    let factory = move |_| {
                        ConcurrentTaskBenchContext::from_task_context(
                            TaskBenchContext::structural_constructor_match_loop(eliminate),
                        )
                    };
                    for (threads, workers) in [
                        (1, single_structural_match_worker.as_slice()),
                        (
                            CONCURRENT_DISPATCH_THREADS,
                            concurrent_structural_match_workers.as_slice(),
                        ),
                    ] {
                        group
                            .sample_duration(Duration::from_millis(50))
                            .throughput(Throughput::per_operation(1, "matches"))
                            .metadata("representation", name)
                            .metadata("threads", threads.to_string())
                            .factory(&factory)
                            .bench(
                                &format!(
                                    "structural_constructor_match_{name}_{threads}_threads"
                                ),
                                workers,
                            );
                    }
                }
            },
        );
        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "relation scan construction concurrent",
            |group| {
                for cell_kind in [ScanCellKind::Immediate, ScanCellKind::SharedHeap] {
                    let scan = move |_| {
                        ConcurrentTaskBenchContext::from_task_context(
                            TaskBenchContext::relation_value_scan(
                                RELATION_SCAN_ROWS,
                                cell_kind,
                            ),
                        )
                    };
                    for (threads, workers) in [
                        (1, single_relation_scan_worker.as_slice()),
                        (
                            CONCURRENT_DISPATCH_THREADS,
                            concurrent_relation_scan_workers.as_slice(),
                        ),
                    ] {
                        let name = format!(
                            "task_scan_bindings_relation_value_{}_{}_rows_concurrent_{}_threads",
                            cell_kind.name(),
                            RELATION_SCAN_ROWS,
                            threads,
                        );
                        group
                            .sample_duration(Duration::from_millis(50))
                            .throughput(Throughput::per_operation(1, "rows"))
                            .metadata("cell_kind", cell_kind.name())
                            .metadata("rows", RELATION_SCAN_ROWS.to_string())
                            .metadata("threads", threads.to_string())
                            .factory(&scan)
                            .bench(&name, workers);
                    }
                }
            },
        );
        runner.concurrent_group::<ConcurrentTaskBenchContext>("task concurrent", |group| {
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("threads", "1")
                .factory(&interpreted_integer)
                .bench(
                    "task_concurrent_integer_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_integer)
                .bench(
                    "task_concurrent_integer_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("threads", "1")
                .factory(&cranelift_integer)
                .bench(
                    "task_concurrent_cranelift_integer_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_integer)
                .bench(
                    "task_concurrent_cranelift_integer_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler counter loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_counter)
                .bench(
                    "task_concurrent_interpreter_compiler_counter_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler counter loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_counter)
                .bench(
                    "task_concurrent_interpreter_compiler_counter_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler counter loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_counter)
                .bench(
                    "task_concurrent_cranelift_compiler_counter_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler counter loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_counter)
                .bench(
                    "task_concurrent_cranelift_compiler_counter_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler accumulator loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_accumulator)
                .bench(
                    "task_concurrent_interpreter_compiler_accumulator_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler accumulator loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_accumulator)
                .bench(
                    "task_concurrent_interpreter_compiler_accumulator_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler accumulator loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_accumulator)
                .bench(
                    "task_concurrent_cranelift_compiler_accumulator_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler accumulator loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_accumulator)
                .bench(
                    "task_concurrent_cranelift_compiler_accumulator_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler countdown loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_countdown)
                .bench(
                    "task_concurrent_interpreter_compiler_countdown_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler countdown loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_countdown)
                .bench(
                    "task_concurrent_interpreter_compiler_countdown_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler countdown loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_countdown)
                .bench(
                    "task_concurrent_cranelift_compiler_countdown_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler countdown loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_countdown)
                .bench(
                    "task_concurrent_cranelift_compiler_countdown_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler arithmetic loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_arithmetic)
                .bench(
                    "task_concurrent_interpreter_compiler_arithmetic_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler arithmetic loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_arithmetic)
                .bench(
                    "task_concurrent_interpreter_compiler_arithmetic_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler arithmetic loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_arithmetic)
                .bench(
                    "task_concurrent_cranelift_compiler_arithmetic_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler arithmetic loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_arithmetic)
                .bench(
                    "task_concurrent_cranelift_compiler_arithmetic_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler integer surface loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_integer_surface)
                .bench(
                    "task_concurrent_interpreter_compiler_integer_surface_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler integer surface loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_integer_surface)
                .bench(
                    "task_concurrent_interpreter_compiler_integer_surface_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler integer surface loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_integer_surface)
                .bench(
                    "task_concurrent_cranelift_compiler_integer_surface_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler integer surface loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_integer_surface)
                .bench(
                    "task_concurrent_cranelift_compiler_integer_surface_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler range loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_range)
                .bench(
                    "task_concurrent_interpreter_compiler_range_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler range loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_range)
                .bench(
                    "task_concurrent_interpreter_compiler_range_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler range loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_range)
                .bench(
                    "task_concurrent_cranelift_compiler_range_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler range loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_range)
                .bench(
                    "task_concurrent_cranelift_compiler_range_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler indexed range loop")
                .metadata("threads", "1")
                .factory(&interpreted_compiler_indexed_range)
                .bench(
                    "task_concurrent_interpreter_compiler_indexed_range_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "interpreter")
                .metadata("source", "compiler indexed range loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&interpreted_compiler_indexed_range)
                .bench(
                    "task_concurrent_interpreter_compiler_indexed_range_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler indexed range loop")
                .metadata("threads", "1")
                .factory(&cranelift_compiler_indexed_range)
                .bench(
                    "task_concurrent_cranelift_compiler_indexed_range_loop_1_thread",
                    &single_integer_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "bytecodes"))
                .metadata("backend", "cranelift")
                .metadata("source", "compiler indexed range loop")
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .factory(&cranelift_compiler_indexed_range)
                .bench(
                    "task_concurrent_cranelift_compiler_indexed_range_loop_4_threads",
                    &concurrent_integer_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", "1")
                .metadata("dispatches_per_task", "1")
                .factory(&one_shot)
                .bench(
                    "task_concurrent_warm_positional_dispatch_1_thread",
                    &single_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .metadata("dispatches_per_task", "1")
                .factory(&one_shot)
                .bench(
                    "task_concurrent_warm_positional_dispatch_4_threads",
                    &concurrent_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", "1")
                .metadata(
                    "dispatches_per_task",
                    REPEATED_DISPATCH_ITERATIONS.to_string(),
                )
                .factory(&repeated)
                .bench(
                    "task_concurrent_repeated_positional_dispatch_1_thread",
                    &single_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .metadata(
                    "dispatches_per_task",
                    REPEATED_DISPATCH_ITERATIONS.to_string(),
                )
                .factory(&repeated)
                .bench(
                    "task_concurrent_repeated_positional_dispatch_4_threads",
                    &concurrent_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", "1")
                .metadata(
                    "dispatches_per_task",
                    REPEATED_DISPATCH_ITERATIONS.to_string(),
                )
                .factory(&alternating)
                .bench(
                    "task_concurrent_alternating_positional_dispatch_1_thread",
                    &single_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .metadata(
                    "dispatches_per_task",
                    REPEATED_DISPATCH_ITERATIONS.to_string(),
                )
                .factory(&alternating)
                .bench(
                    "task_concurrent_alternating_positional_dispatch_4_threads",
                    &concurrent_workers,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", "1")
                .metadata(
                    "dispatches_per_task",
                    THREE_SITE_DISPATCHES_PER_TASK.to_string(),
                )
                .factory(&three_site)
                .bench(
                    "task_concurrent_three_site_positional_dispatch_1_thread",
                    &single_worker,
                );
            group
                .sample_duration(Duration::from_millis(50))
                .throughput(Throughput::per_operation(1, "dispatches"))
                .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                .metadata(
                    "dispatches_per_task",
                    THREE_SITE_DISPATCHES_PER_TASK.to_string(),
                )
                .factory(&three_site)
                .bench(
                    "task_concurrent_three_site_positional_dispatch_4_threads",
                    &concurrent_workers,
                );
        });

        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "verb call path experiment concurrent",
            |group| {
                for (shape, sites, context) in [
                    (
                        "repeated",
                        1,
                        TaskBenchContext::repeated_call_path as fn(CallPath) -> TaskBenchContext,
                    ),
                    (
                        "alternating",
                        2,
                        TaskBenchContext::alternating_call_path as fn(CallPath) -> TaskBenchContext,
                    ),
                ] {
                    for path in CallPath::ALL {
                        let factory =
                            move |_| ConcurrentTaskBenchContext::from_task_context(context(path));
                        for (threads, workers) in [
                            (1, single_call_path_worker.as_slice()),
                            (
                                CONCURRENT_DISPATCH_THREADS,
                                concurrent_call_path_workers.as_slice(),
                            ),
                        ] {
                            let name = format!(
                                "verb_call_path_{shape}_{}_concurrent_{threads}_threads",
                                path.name()
                            );
                            group
                                .sample_duration(Duration::from_millis(50))
                                .throughput(Throughput::per_operation(1, "invocations"))
                                .metadata("call_path", path.name())
                                .metadata("sites", sites.to_string())
                                .metadata("threads", threads.to_string())
                                .factory(&factory)
                                .bench(&name, workers);
                        }
                    }
                }
            },
        );

        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "collection loop concurrent",
            |group| {
                for (name, interpreted_context, native_context) in [
                    (
                        "list_traversal",
                        TaskBenchContext::interpreted_list_traversal as fn() -> TaskBenchContext,
                        TaskBenchContext::list_traversal as fn() -> TaskBenchContext,
                    ),
                    (
                        "list_index",
                        TaskBenchContext::interpreted_list_index_loop as fn() -> TaskBenchContext,
                        TaskBenchContext::list_index_loop as fn() -> TaskBenchContext,
                    ),
                    (
                        "map_traversal",
                        TaskBenchContext::interpreted_map_traversal as fn() -> TaskBenchContext,
                        TaskBenchContext::map_traversal as fn() -> TaskBenchContext,
                    ),
                    (
                        "map_index",
                        TaskBenchContext::interpreted_map_index_loop as fn() -> TaskBenchContext,
                        TaskBenchContext::map_index_loop as fn() -> TaskBenchContext,
                    ),
                ] {
                    let interpreted = move |_| {
                        ConcurrentTaskBenchContext::from_task_context(interpreted_context())
                    };
                    let interpreted_one = format!("{name}_concurrent_interpreter_1_thread");
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "interpreter")
                        .metadata("threads", "1")
                        .factory(&interpreted)
                        .bench(&interpreted_one, &single_integer_worker);
                    let interpreted_four = format!(
                        "{name}_concurrent_interpreter_{CONCURRENT_DISPATCH_THREADS}_threads"
                    );
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "interpreter")
                        .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                        .factory(&interpreted)
                        .bench(&interpreted_four, &concurrent_integer_workers);

                    let native =
                        move |_| ConcurrentTaskBenchContext::from_task_context(native_context());
                    let native_one = format!("{name}_concurrent_cranelift_1_thread");
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "cranelift")
                        .metadata("threads", "1")
                        .factory(&native)
                        .bench(&native_one, &single_integer_worker);
                    let native_four = format!(
                        "{name}_concurrent_cranelift_{CONCURRENT_DISPATCH_THREADS}_threads"
                    );
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "cranelift")
                        .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                        .factory(&native)
                        .bench(&native_four, &concurrent_integer_workers);
                }
            },
        );

        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "value kind facts concurrent",
            |group| {
                for (backend, interpret_only) in [("interpreter", true), ("cranelift", false)] {
                    for (fact_source, annotated) in
                        [("inferred", false), ("annotated_proven", true)]
                    {
                        let factory = move |_| {
                            ConcurrentTaskBenchContext::from_task_context(
                                TaskBenchContext::compiler_accumulator_loop_with_annotations(
                                    annotated,
                                    interpret_only,
                                ),
                            )
                        };
                        for (threads, workers) in [
                            (1, single_integer_worker.as_slice()),
                            (
                                CONCURRENT_DISPATCH_THREADS,
                                concurrent_integer_workers.as_slice(),
                            ),
                        ] {
                            group
                                .sample_duration(Duration::from_millis(50))
                                .throughput(Throughput::per_operation(1, "bytecodes"))
                                .metadata("backend", backend)
                                .metadata("fact_source", fact_source)
                                .metadata("threads", threads.to_string())
                                .factory(&factory)
                                .bench(
                                    &format!(
                                        "value_kind_facts_{backend}_{fact_source}_{threads}_threads"
                                    ),
                                    workers,
                                );
                        }
                    }
                }
            },
        );

        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "collection binding checks concurrent",
            |group| {
                for (backend, interpret_only) in [("interpreter", true), ("cranelift", false)] {
                    for (shape, annotated) in
                        [("dynamic_unannotated", false), ("dynamic_checked", true)]
                    {
                        let factory = move |_| {
                            ConcurrentTaskBenchContext::from_task_context(
                                TaskBenchContext::compiler_list_binding_loop(
                                    annotated,
                                    interpret_only,
                                ),
                            )
                        };
                        for (threads, workers) in [
                            (1, single_collection_binding_worker.as_slice()),
                            (
                                CONCURRENT_DISPATCH_THREADS,
                                concurrent_collection_binding_workers.as_slice(),
                            ),
                        ] {
                            let name = format!(
                                "collection_binding_checks_{backend}_{shape}_{threads}_threads"
                            );
                            group
                                .sample_duration(Duration::from_millis(50))
                                .throughput(Throughput::per_operation(1, "elements"))
                                .metadata("backend", backend)
                                .metadata("binding", shape)
                                .metadata("threads", threads.to_string())
                                .factory(&factory)
                                .bench(&name, workers);
                        }
                    }
                }
            },
        );

        runner.concurrent_group::<ConcurrentTaskBenchContext>(
            "range JIT admission concurrent",
            |group| {
                for iterations in RANGE_ADMISSION_CONCURRENT_ITERATIONS {
                    let interpreted = |_| {
                        ConcurrentTaskBenchContext::from_task_context(
                            TaskBenchContext::interpreted_compiler_range_loop_with_iterations(
                                iterations,
                            ),
                        )
                    };
                    let interpreted_one = format!(
                        "range_admission_concurrent_interpreter_{iterations}_iterations_1_thread"
                    );
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "interpreter")
                        .metadata("iterations", iterations.to_string())
                        .metadata("threads", "1")
                        .factory(&interpreted)
                        .bench(&interpreted_one, &single_integer_worker);
                    let interpreted_four = format!(
                        "range_admission_concurrent_interpreter_{iterations}_iterations_4_threads"
                    );
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "interpreter")
                        .metadata("iterations", iterations.to_string())
                        .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                        .factory(&interpreted)
                        .bench(&interpreted_four, &concurrent_integer_workers);

                    let cranelift = |_| {
                        ConcurrentTaskBenchContext::from_task_context(
                            TaskBenchContext::compiler_range_loop_with_iterations(iterations),
                        )
                    };
                    let cranelift_one = format!(
                        "range_admission_concurrent_cranelift_{iterations}_iterations_1_thread"
                    );
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "cranelift")
                        .metadata("iterations", iterations.to_string())
                        .metadata("threads", "1")
                        .factory(&cranelift)
                        .bench(&cranelift_one, &single_integer_worker);
                    let cranelift_four = format!(
                        "range_admission_concurrent_cranelift_{iterations}_iterations_4_threads"
                    );
                    group
                        .sample_duration(Duration::from_millis(50))
                        .throughput(Throughput::per_operation(1, "bytecodes"))
                        .metadata("backend", "cranelift")
                        .metadata("iterations", iterations.to_string())
                        .metadata("threads", CONCURRENT_DISPATCH_THREADS.to_string())
                        .factory(&cranelift)
                        .bench(&cranelift_four, &concurrent_integer_workers);
                }
            },
        );
    }
);
