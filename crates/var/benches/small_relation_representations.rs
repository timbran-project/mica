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

use mica_var::{Symbol, Tuple, Value};
use micromeasure::{
    BenchContext, BenchmarkMainOptions, ConcurrentBenchContext, ConcurrentBenchControl,
    ConcurrentWorker, ConcurrentWorkerResult, Throughput, benchmark_main, black_box,
};
use std::time::Duration;

const CONCURRENT_THREADS: usize = 4;

#[derive(Clone, Copy)]
enum Representation {
    BoxedRelation,
    KnownShapeRelation,
    Components,
}

impl Representation {
    const ALL: [Self; 3] = [
        Self::BoxedRelation,
        Self::KnownShapeRelation,
        Self::Components,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::BoxedRelation => "boxed_relation",
            Self::KnownShapeRelation => "known_shape_relation",
            Self::Components => "components",
        }
    }
}

#[derive(Clone, Copy)]
enum Shape {
    None,
    Some,
    Ok,
    Error,
}

impl Shape {
    const ALL: [Self; 4] = [Self::None, Self::Some, Self::Ok, Self::Error];

    const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Some => "some",
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }

    const fn is_present(self) -> bool {
        !matches!(self, Self::None)
    }

    const fn is_error(self) -> bool {
        matches!(self, Self::Error)
    }
}

#[derive(Clone)]
struct ComponentValue {
    present: bool,
    error: bool,
    payload: Value,
}

enum ConstructedValue {
    Boxed(Value),
    Components(ComponentValue),
}

struct SmallRelationContext {
    representation: Representation,
    shape: Shape,
    payload: Value,
    value_column: Symbol,
    case_column: Symbol,
    ok_symbol: Symbol,
    error_symbol: Symbol,
}

impl SmallRelationContext {
    fn new(representation: Representation, shape: Shape) -> Self {
        Self {
            representation,
            shape,
            payload: Value::int(42).unwrap(),
            value_column: Symbol::intern("value"),
            case_column: Symbol::intern("case"),
            ok_symbol: Symbol::intern("ok"),
            error_symbol: Symbol::intern("error"),
        }
    }

    #[inline(never)]
    fn construct(&self) -> ConstructedValue {
        match self.representation {
            Representation::BoxedRelation => ConstructedValue::Boxed(self.boxed_relation()),
            Representation::KnownShapeRelation => {
                ConstructedValue::Boxed(self.known_shape_relation())
            }
            Representation::Components => ConstructedValue::Components(ComponentValue {
                present: self.shape.is_present(),
                error: self.shape.is_error(),
                payload: self.payload.clone(),
            }),
        }
    }

    fn boxed_relation(&self) -> Value {
        match self.shape {
            Shape::None => Value::relation([self.value_column], []).unwrap(),
            Shape::Some => {
                Value::relation([self.value_column], [Tuple::from([self.payload.clone()])]).unwrap()
            }
            Shape::Ok | Shape::Error => {
                let case = if self.shape.is_error() {
                    self.error_symbol
                } else {
                    self.ok_symbol
                };
                Value::relation(
                    [self.case_column, self.value_column],
                    [Tuple::from([Value::symbol(case), self.payload.clone()])],
                )
                .unwrap()
            }
        }
    }

    fn known_shape_relation(&self) -> Value {
        match self.shape {
            Shape::None => Value::small_relation([self.value_column], None).unwrap(),
            Shape::Some => Value::small_relation(
                [self.value_column],
                Some(Tuple::from([self.payload.clone()])),
            )
            .unwrap(),
            Shape::Ok | Shape::Error => {
                let case = if self.shape.is_error() {
                    self.error_symbol
                } else {
                    self.ok_symbol
                };
                Value::small_relation(
                    [self.case_column, self.value_column],
                    Some(Tuple::from([Value::symbol(case), self.payload.clone()])),
                )
                .unwrap()
            }
        }
    }

    #[inline(never)]
    fn branch_and_extract(&self) -> Option<Value> {
        match self.construct() {
            ConstructedValue::Boxed(value) => value.with_relation(|relation| {
                let row = relation.rows().first()?;
                let payload = if relation.arity() == 1 {
                    row.values().first()
                } else {
                    let position = relation.column_position(self.value_column)?;
                    row.values().get(position)
                }?;
                Some(payload.clone())
            })?,
            ConstructedValue::Components(value) => {
                black_box(value.error);
                value.present.then_some(value.payload)
            }
        }
    }
}

impl BenchContext for SmallRelationContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self::new(Representation::BoxedRelation, Shape::Some)
    }
}

impl ConcurrentBenchContext for SmallRelationContext {
    fn prepare(_num_threads: usize) -> Self {
        Self::new(Representation::BoxedRelation, Shape::Some)
    }
}

fn construct_and_drop(context: &mut SmallRelationContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        black_box(context.construct());
    }
}

fn construct_branch_and_extract(
    context: &mut SmallRelationContext,
    chunk_size: usize,
    _chunk_num: usize,
) {
    for _ in 0..chunk_size {
        black_box(context.branch_and_extract());
    }
}

fn construct_concurrently(
    context: &SmallRelationContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut operations = 0_u64;
    while !control.should_stop() {
        black_box(context.construct());
        operations = operations.wrapping_add(1);
    }
    ConcurrentWorkerResult::operations(operations)
}

fn extract_concurrently(
    context: &SmallRelationContext,
    control: &ConcurrentBenchControl,
) -> ConcurrentWorkerResult {
    let mut operations = 0_u64;
    while !control.should_stop() {
        black_box(context.branch_and_extract());
        operations = operations.wrapping_add(1);
    }
    ConcurrentWorkerResult::operations(operations)
}

benchmark_main!(
    BenchmarkMainOptions {
        filter_help: Some(
            "all, construct, extract, boxed_relation, known_shape_relation, components, none, some, ok, error, or any benchmark name substring"
                .to_owned()
        ),
        runtime: micromeasure::BenchmarkRuntimeOptions {
            warm_up_duration: Duration::from_millis(100),
            benchmark_duration: Duration::from_secs(1),
            min_samples: 5,
            max_samples: 10,
        },
        ..Default::default()
    },
    |runner| {
        runner.group::<SmallRelationContext>("small relation representation", |group| {
            for representation in Representation::ALL {
                for shape in Shape::ALL {
                    let factory = move || SmallRelationContext::new(representation, shape);
                    let suffix = format!("{}_{}", representation.name(), shape.name());
                    group
                        .throughput(Throughput::per_operation(1, "value"))
                        .factory(&factory)
                        .bench(&format!("construct_{suffix}"), construct_and_drop);
                    group
                        .throughput(Throughput::per_operation(1, "value"))
                        .factory(&factory)
                        .bench(
                            &format!("extract_{suffix}"),
                            construct_branch_and_extract,
                        );
                }
            }
        });

        let one_construct = [ConcurrentWorker {
            name: "construct",
            threads: 1,
            run: construct_concurrently,
        }];
        let four_construct = [ConcurrentWorker {
            name: "construct",
            threads: CONCURRENT_THREADS,
            run: construct_concurrently,
        }];
        let one_extract = [ConcurrentWorker {
            name: "extract",
            threads: 1,
            run: extract_concurrently,
        }];
        let four_extract = [ConcurrentWorker {
            name: "extract",
            threads: CONCURRENT_THREADS,
            run: extract_concurrently,
        }];

        runner.concurrent_group::<SmallRelationContext>(
            "small relation representation concurrent",
            |group| {
                for representation in Representation::ALL {
                    for shape in Shape::ALL {
                        for (operation, threads, workers) in [
                            ("construct", 1, one_construct.as_slice()),
                            ("construct", CONCURRENT_THREADS, four_construct.as_slice()),
                            ("extract", 1, one_extract.as_slice()),
                            ("extract", CONCURRENT_THREADS, four_extract.as_slice()),
                        ] {
                            let factory = move |_| {
                                SmallRelationContext::new(representation, shape)
                            };
                            let name = format!(
                                "{}_{}_{}_{}_threads",
                                operation,
                                representation.name(),
                                shape.name(),
                                threads
                            );
                            group
                                .sample_duration(Duration::from_millis(50))
                                .throughput(Throughput::per_operation(1, "value"))
                                .metadata("operation", operation)
                                .metadata("representation", representation.name())
                                .metadata("shape", shape.name())
                                .metadata("threads", threads.to_string())
                                .factory(&factory)
                                .bench(&name, workers);
                        }
                    }
                }
            },
        );
    }
);
