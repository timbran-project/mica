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

//! Measures helper-backed canonical map indexing from compiled natural loops.

#[allow(dead_code)]
mod fixtures;

use fixtures::{BenchmarkHost, MAX_CALL_DEPTH, ProgramFixture};
use mica_var::Value;
use mica_vm::{
    Instruction, Operand, Program, Register, RegisterVm, RuntimeBinaryOp, VmHostResponse,
};
use micromeasure::{
    BenchContext, BenchmarkMainOptions, ConcurrentBenchContext, ConcurrentBenchControl,
    ConcurrentWorker, ConcurrentWorkerResult, NoContext, Throughput, benchmark_main, black_box,
};
use std::sync::Arc;
use std::time::Duration;

const LOOKUPS: usize = 16_384;
const INSTRUCTION_COUNT: u64 = (LOOKUPS as u64 * 6) + 8;
const CONCURRENT_THREADS: usize = 4;

#[derive(Clone, Copy)]
struct WorkloadCase {
    map_size: usize,
    hit: bool,
}

impl WorkloadCase {
    fn name(self) -> String {
        let outcome = if self.hit { "hit" } else { "miss" };
        format!("{outcome}_{}", self.map_size)
    }

    fn fixture(self) -> MapIndexFixture {
        let entries = (0..self.map_size).map(|index| {
            (
                Value::string(format!("key-{index:08}")),
                Value::int(index as i64).unwrap(),
            )
        });
        let map = Value::map(entries);
        let key = if self.hit {
            Value::string(format!("key-{:08}", self.map_size - 1))
        } else {
            Value::string("key-99999999")
        };
        let expected = if self.hit {
            Value::int((self.map_size - 1) as i64).unwrap()
        } else {
            Value::empty_relation()
        };
        let program = Program::new(
            8,
            [
                Instruction::Load {
                    dst: register(0),
                    value: map,
                },
                Instruction::Load {
                    dst: register(1),
                    value: key,
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(LOOKUPS as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 11,
                },
                Instruction::Index {
                    dst: register(6),
                    collection: register(0),
                    index: Operand::Register(register(1)),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(7),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return {
                    value: Operand::Register(register(6)),
                },
            ],
        )
        .unwrap();
        MapIndexFixture {
            fixture: ProgramFixture {
                program: Arc::new(program),
                instruction_count: INSTRUCTION_COUNT,
            },
            expected,
        }
    }
}

const CASES: [WorkloadCase; 6] = [
    WorkloadCase {
        map_size: 16,
        hit: true,
    },
    WorkloadCase {
        map_size: 16,
        hit: false,
    },
    WorkloadCase {
        map_size: 256,
        hit: true,
    },
    WorkloadCase {
        map_size: 256,
        hit: false,
    },
    WorkloadCase {
        map_size: 4_096,
        hit: true,
    },
    WorkloadCase {
        map_size: 4_096,
        hit: false,
    },
];

struct MapIndexFixture {
    fixture: ProgramFixture,
    expected: Value,
}

struct VmContext {
    workload: MapIndexFixture,
    host: BenchmarkHost,
    native: bool,
}

impl VmContext {
    fn new(case: WorkloadCase, native: bool) -> Self {
        Self {
            workload: case.fixture(),
            host: BenchmarkHost::default(),
            native,
        }
    }
}

impl BenchContext for VmContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::new(CASES[2], true)
    }
}

struct ConcurrentContext {
    workload: MapIndexFixture,
    native: bool,
}

impl ConcurrentBenchContext for ConcurrentContext {
    fn prepare(_num_threads: usize) -> Self {
        Self {
            workload: CASES[2].fixture(),
            native: true,
        }
    }
}

fn register(index: u16) -> Register {
    Register(index)
}

fn execute(workload: &MapIndexFixture, host: &mut BenchmarkHost, native: bool) -> Value {
    let mut vm = if native {
        RegisterVm::new(Arc::clone(&workload.fixture.program))
    } else {
        RegisterVm::new_interpreted(Arc::clone(&workload.fixture.program))
    };
    let response = vm
        .run_until_host_response(
            host,
            workload.fixture.instruction_count as usize + LOOKUPS,
            MAX_CALL_DEPTH,
        )
        .unwrap();
    let VmHostResponse::Complete(value) = response else {
        panic!("map index fixture did not complete: {response:?}");
    };
    value
}

fn bench_warm(context: &mut VmContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        let value = execute(&context.workload, &mut context.host, context.native);
        assert_eq!(value, context.workload.expected);
        black_box(value);
    }
}

fn bench_cold(case: WorkloadCase, native: bool, chunk_size: usize) {
    for _ in 0..chunk_size {
        let workload = case.fixture();
        let value = execute(&workload, &mut BenchmarkHost::default(), native);
        assert_eq!(value, workload.expected);
        black_box(value);
    }
}

fn bench_interpreter_hit_cold(_context: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    bench_cold(CASES[2], false, chunk_size);
}

fn bench_native_hit_cold(_context: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    bench_cold(CASES[2], true, chunk_size);
}

fn bench_interpreter_miss_cold(_context: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    bench_cold(CASES[3], false, chunk_size);
}

fn bench_native_miss_cold(_context: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    bench_cold(CASES[3], true, chunk_size);
}

fn run_concurrent(
    context: &ConcurrentContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut host = BenchmarkHost::default();
    let mut lookups = 0_u64;
    while !control.should_stop() {
        let value = execute(&context.workload, &mut host, context.native);
        assert_eq!(value, context.workload.expected);
        black_box(value);
        lookups = lookups.wrapping_add(LOOKUPS as u64);
    }
    ConcurrentWorkerResult::operations(lookups)
}

benchmark_main!(
    BenchmarkMainOptions {
        filter_help: Some(
            "all, warm, cold, concurrent, interpreter, native, hit, miss, or a map size".to_owned(),
        ),
        runtime: micromeasure::BenchmarkRuntimeOptions {
            warm_up_duration: Duration::from_millis(100),
            benchmark_duration: Duration::from_secs(1),
            min_samples: 10,
            max_samples: 30,
        },
        ..Default::default()
    },
    |runner| {
        runner.group::<VmContext>("string map index warm", |group| {
            for case in CASES {
                for (backend_name, native) in [("interpreter", false), ("native", true)] {
                    let factory = move || VmContext::new(case, native);
                    group
                        .throughput(Throughput::per_operation(LOOKUPS as u64, "lookup"))
                        .factory(&factory)
                        .bench(&format!("{backend_name}_{}", case.name()), bench_warm);
                }
            }
        });

        runner.group::<NoContext>("string map index cold", |group| {
            for (name, bench) in [
                (
                    "interpreter_hit_256_cold",
                    bench_interpreter_hit_cold as fn(&mut NoContext, usize, usize),
                ),
                ("native_hit_256_cold", bench_native_hit_cold),
                ("interpreter_miss_256_cold", bench_interpreter_miss_cold),
                ("native_miss_256_cold", bench_native_miss_cold),
            ] {
                group
                    .throughput(Throughput::per_operation(1, "setup_and_run"))
                    .bench(name, bench);
            }
        });

        let one_thread = [ConcurrentWorker {
            name: "string map index",
            threads: 1,
            run: run_concurrent,
        }];
        let four_threads = [ConcurrentWorker {
            name: "string map index",
            threads: CONCURRENT_THREADS,
            run: run_concurrent,
        }];
        runner.concurrent_group::<ConcurrentContext>("string map index concurrent", |group| {
            for case in CASES {
                for (backend_name, native) in [("interpreter", false), ("native", true)] {
                    for (threads, workers) in [(1, &one_thread[..]), (4, &four_threads[..])] {
                        let factory = move |_| ConcurrentContext {
                            workload: case.fixture(),
                            native,
                        };
                        group
                            .sample_duration(Duration::from_millis(50))
                            .throughput(Throughput::per_operation(1, "lookup"))
                            .metadata("backend", backend_name)
                            .metadata("threads", threads.to_string())
                            .factory(&factory)
                            .bench(
                                &format!("{backend_name}_{}_{threads}_threads", case.name()),
                                workers,
                            );
                    }
                }
            }
        });
    }
);
