// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
use std::io::Write;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use clap::Parser;
use mica_relation_kernel::{
    ConflictPolicy, ExecutionContext, FactChangeKind, KernelError, QueryPlan, RelationKernel,
    RelationMetadata, Transaction, Tuple,
};
use mica_var::{Identity, RelationValue, Symbol, Value};
use serde_json::json;

#[path = "kernel_comparison/rules.rs"]
mod rules;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(about = "Compare Rust relation transactions with the Mica-generated kernel fixture")]
struct Options {
    /// The check executable produced by native-backend --check-fixtures --fixture kernel.
    #[arg(long)]
    native_executable: PathBuf,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, default_value = "128")]
    cases: NonZeroU32,
    #[arg(long)]
    bench: bool,
    /// Measure composed queries over two 1,024-row relations.
    #[arg(long)]
    query_bench: bool,
    /// Measure recursive maintenance, bulk loading, and retention in the native kernel.
    #[arg(long)]
    rule_bench: bool,
    /// Also build a GCC-instrumented diagnostic from this generated kernel directory.
    #[arg(long, requires = "rule_bench")]
    rule_profile: Option<PathBuf>,
    #[arg(long, default_value = "5")]
    samples: NonZeroU32,
    #[arg(long, default_value = "128")]
    rounds: NonZeroU32,
}

fn id(n: u64) -> Identity {
    Identity::new(n).unwrap()
}
fn integer(n: i64) -> Value {
    Value::int(n).unwrap()
}
fn tuple(a: i64, b: i64) -> Tuple {
    Tuple::from([integer(a), integer(b)])
}
fn kernel() -> RelationKernel {
    let kernel = RelationKernel::new();
    for (n, policy) in [
        (
            1,
            ConflictPolicy::Functional {
                key_positions: vec![0],
            },
        ),
        (2, ConflictPolicy::Set),
    ] {
        kernel
            .create_relation(
                RelationMetadata::new(id(n), Symbol::intern(&format!("R{n}")), 2)
                    .with_index([1])
                    .with_conflict_policy(policy),
            )
            .unwrap();
    }
    kernel
}
fn status(result: std::result::Result<(), KernelError>) -> u64 {
    match result {
        Ok(()) => 0,
        Err(KernelError::UnknownRelation(_)) => 1,
        Err(KernelError::ArityMismatch { .. }) => 2,
        Err(KernelError::NonPersistentValue { .. }) => 3,
        Err(KernelError::FunctionalKeyViolation { .. }) => 4,
        Err(KernelError::Conflict(_)) => 5,
        Err(error) => panic!("unhandled reference error: {error:?}"),
    }
}
#[derive(Clone, Copy, Debug)]
struct Step {
    op: char,
    slot: usize,
    relation: u64,
    a: i64,
    b: i64,
    mask: u64,
}
impl Step {
    fn new(op: char, slot: usize, relation: u64, a: i64, b: i64, mask: u64) -> Self {
        Self {
            op,
            slot,
            relation,
            a,
            b,
            mask,
        }
    }
    fn control(op: char, slot: usize) -> Self {
        Self::new(op, slot, 0, 0, 0, 0)
    }
}
fn next(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed >> 32
}
fn corpus(options: &Options) -> Vec<Step> {
    let mut seed = options.seed;
    let mut steps = Vec::new();
    for case in 0..options.cases.get() {
        steps.push(Step::control('b', 0));
        for _ in 0..12 {
            let op = if next(&mut seed).is_multiple_of(3) {
                'r'
            } else {
                'a'
            };
            let relation = 1 + next(&mut seed) % 2;
            let a = (next(&mut seed) % 16) as i64 - 8;
            let b = (next(&mut seed) % 16) as i64 - 8;
            steps.push(Step::new(op, 0, relation, a, b, 0));
            steps.push(Step::new('q', 0, relation, a, b, next(&mut seed) % 4));
            steps.push(Step::new('p', 0, relation, a, b, next(&mut seed) % 32));
        }
        if !case.is_multiple_of(4) {
            steps.push(Step::control('c', 0));
        }
        steps.push(Step::control('e', 0));
        steps.push(Step::control('g', 0));
        steps.push(Step::control('b', 0));
        for relation in [1, 2] {
            steps.push(Step::new('q', 0, relation, 0, 0, 0));
        }
        // Competing functional insertions from the same snapshot.
        let key = 1000 + i64::from(case);
        steps.push(Step::new('a', 0, 1, key, 1, 0));
        steps.push(Step::control('b', 1));
        steps.push(Step::new('a', 1, 1, key, 2, 0));
        steps.push(Step::control('c', 1));
        steps.push(Step::control('e', 1));
        steps.push(Step::control('c', 0));
        steps.push(Step::control('e', 0));
    }
    steps
}
fn query_plan(relation: u64, a: i64, b: i64, options: u64) -> QueryPlan {
    let left = QueryPlan::scan(id(relation), [(options & 8 != 0).then(|| integer(a)), None]);
    let right = QueryPlan::scan(id(3 - relation), [None, None]);
    let lp = [0];
    let rp = [((options >> 4) & 1) as u16];
    match options & 7 {
        0 => left.project([1, 0, 1]),
        1 => QueryPlan::join_eq(left, right, lp, rp),
        2 => QueryPlan::semi_join(left, right, lp, rp),
        3 => QueryPlan::anti_join(left, right, lp, rp),
        4 => QueryPlan::union(left, right),
        5 => QueryPlan::difference(left, right),
        6 => QueryPlan::join_eq(left, right, lp, rp).project([1, 3]),
        _ => QueryPlan::union(
            left,
            QueryPlan::input(
                RelationValue::new([Symbol::intern("a"), Symbol::intern("b")], [tuple(a, b)])
                    .unwrap(),
            ),
        ),
    }
}
fn reference(steps: &[Step], kernel: &RelationKernel, rules: bool) -> Vec<String> {
    let mut transactions: [Option<Transaction<'_>>; 2] = [None, None];
    let mut output = Vec::new();
    for &Step {
        op,
        slot,
        relation,
        a,
        b,
        mask,
    } in steps
    {
        let reply = match op {
            'b' => {
                assert!(transactions[slot].is_none());
                transactions[slot] = Some(kernel.begin());
                "b 0".into()
            }
            'e' => {
                transactions[slot] = None;
                "e 0".into()
            }
            'g' => "g 0".into(),
            'a' | 'r' => {
                let tx = transactions[slot].as_mut().unwrap();
                let result = if op == 'a' {
                    tx.assert(id(relation), tuple(a, b))
                } else {
                    tx.retract(id(relation), tuple(a, b))
                };
                format!("{op} {}", status(result))
            }
            'q' => {
                let mut rows = transactions[slot]
                    .as_ref()
                    .unwrap()
                    .scan(
                        id(relation),
                        &[
                            ((mask & 1) != 0).then(|| integer(a)),
                            ((mask & 2) != 0).then(|| integer(b)),
                        ],
                    )
                    .unwrap();
                rows.sort();
                let mut reply = format!("q 0 {}", rows.len());
                for row in rows {
                    reply += &format!(
                        " {}:{}",
                        row.values()[0].as_int().unwrap(),
                        row.values()[1].as_int().unwrap()
                    );
                }
                reply
            }
            'p' => {
                let rows = query_plan(relation, a, b, mask)
                    .execute(
                        transactions[slot].as_ref().unwrap(),
                        &ExecutionContext::serial(),
                    )
                    .unwrap();
                let mut reply = format!("p 0 {}", rows.len());
                for row in rows {
                    reply.push(' ');
                    reply += &row
                        .values()
                        .iter()
                        .map(|value| value.as_int().unwrap().to_string())
                        .collect::<Vec<_>>()
                        .join(":");
                }
                reply
            }
            'c' => match transactions[slot].take().unwrap().commit() {
                Ok(_) if rules => "c 0".into(),
                Ok(result) => {
                    let mut changes = result
                        .commit()
                        .changes()
                        .iter()
                        .map(|change| {
                            (
                                change.relation.raw(),
                                matches!(change.kind, FactChangeKind::Assert),
                                change.tuple.values()[0].as_int().unwrap(),
                                change.tuple.values()[1].as_int().unwrap(),
                            )
                        })
                        .collect::<Vec<_>>();
                    changes.sort();
                    let mut reply = format!("c 0 {}", changes.len());
                    for (relation, asserted, a, b) in changes {
                        reply += &format!(" {relation}:{}:{a}:{b}", u8::from(asserted));
                    }
                    reply
                }
                Err(KernelError::Conflict(_)) => "c 5 0".into(),
                Err(error) => panic!("commit: {error:?}"),
            },
            _ => unreachable!(),
        };
        output.push(reply);
    }
    output
}
fn compare_trace(options: &Options, mode: &str, steps: &[Step], expected: &[String]) -> Result<()> {
    let input = steps
        .iter()
        .map(|s| {
            format!(
                "{} {} {} {} {} {}\n",
                s.op, s.slot, s.relation, s.a, s.b, s.mask
            )
        })
        .collect::<String>();
    let mut child = Command::new(&options.native_executable)
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    writer.join().expect("trace writer")?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "native trace failed: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let actual = String::from_utf8(output.stdout)?;
    let actual = actual.lines().collect::<Vec<_>>();
    if actual.len() != expected.len() {
        return Err(format!(
            "expected {} trace replies, got {}",
            expected.len(),
            actual.len()
        )
        .into());
    }
    for (i, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
        if actual != expected {
            return Err(format!(
                "seed {}, step {i} {:?}: native {actual:?}, Rust {expected:?}",
                options.seed, steps[i]
            )
            .into());
        }
    }
    println!(
        "{}",
        json!({"correctness":"passed","suite":mode,"seed":options.seed,"steps":steps.len(),
            "canonical_row_checks": steps.iter().filter(|s| s.op == 'q' || s.op == 'p').count()})
    );
    Ok(())
}
fn rust_query_bench(name: &str, rounds: u32) -> (u128, u64, u64) {
    let kernel = kernel();
    let mut tx = kernel.begin();
    for i in 0..1024 {
        tx.assert(id(1), tuple(i, i % 64)).unwrap();
        tx.assert(id(2), tuple(i, i % 32)).unwrap();
    }
    tx.commit().unwrap();
    let tx = kernel.begin();
    let operation = match name {
        "query-project" => 0,
        "query-join" => 1,
        "query-semi" => 2,
        _ => 6,
    };
    let plan = query_plan(1, 0, 0, operation).prepare();
    let expected = if operation == 6 { 64 } else { 1024 };
    let start = Instant::now();
    let mut sum = 0;
    for _ in 0..rounds {
        let rows = plan.execute(&tx, &ExecutionContext::serial()).unwrap();
        assert_eq!(rows.len(), expected);
        sum += rows.len() as u64;
    }
    (start.elapsed().as_nanos(), sum, 0)
}
fn rust_bench(name: &str, rounds: u32, threads: usize) -> (u128, u64, u64) {
    if name.starts_with("query-") {
        return rust_query_bench(name, rounds);
    }
    let kernel = kernel();
    let rows = match name {
        "read" => 4096,
        "update" => 1024,
        _ => 1,
    };
    let mut tx = kernel.begin();
    for i in 0..rows {
        tx.assert(id(1), tuple(i, 0)).unwrap();
    }
    tx.commit().unwrap();
    let start = Instant::now();
    let mut sum = 0;
    let mut conflicts = 0;
    match name {
        "read" => {
            let tx = kernel.begin();
            for i in 0..rounds {
                let result = tx
                    .scan(id(1), &[Some(integer(i64::from(i) % rows)), None])
                    .unwrap();
                assert_eq!(result.len(), 1);
                sum += result[0].values()[0].as_int().unwrap() as u64;
            }
        }
        "update" => {
            for i in 0..rounds {
                let mut tx = kernel.begin();
                tx.retract(id(1), tuple(0, i64::from(i))).unwrap();
                tx.assert(id(1), tuple(0, i64::from(i) + 1)).unwrap();
                tx.commit().unwrap();
            }
            sum = u64::from(rounds);
        }
        _ => {
            conflicts = thread::scope(|scope| {
                let handles = (0..threads)
                    .map(|thread_id| {
                        let kernel = &kernel;
                        scope.spawn(move || {
                            let mut conflicts = 0;
                            for i in 0..rounds {
                                loop {
                                    let mut tx = kernel.begin();
                                    if name == "contended" {
                                        let rows =
                                            tx.scan(id(1), &[Some(integer(0)), None]).unwrap();
                                        let previous = rows[0].values()[1].as_int().unwrap();
                                        tx.retract(id(1), rows[0].clone()).unwrap();
                                        tx.assert(id(1), tuple(0, previous + 1)).unwrap();
                                    } else {
                                        tx.assert(id(2), tuple(thread_id as i64, i64::from(i)))
                                            .unwrap();
                                    }
                                    match tx.commit() {
                                        Ok(_) => break,
                                        Err(KernelError::Conflict(_)) => conflicts += 1,
                                        Err(error) => panic!("commit: {error:?}"),
                                    }
                                }
                            }
                            conflicts
                        })
                    })
                    .collect::<Vec<_>>();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            });
            sum = threads as u64 * u64::from(rounds);
        }
    }
    let elapsed = start.elapsed().as_nanos();
    // Verify the resulting store outside the measured interval.
    if name != "read" {
        let tx = kernel.begin();
        if name == "disjoint" {
            let mut rows = tx.scan(id(2), &[None, None]).unwrap();
            rows.sort();
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(
                    row,
                    &tuple((i / rounds as usize) as i64, (i % rounds as usize) as i64)
                );
            }
            sum = rows.len() as u64;
        } else {
            let rows = tx.scan(id(1), &[Some(integer(0)), None]).unwrap();
            assert_eq!(rows.len(), 1);
            sum = rows[0].values()[1].as_int().unwrap() as u64;
        }
        assert_eq!(sum, threads as u64 * u64::from(rounds));
    }
    (elapsed, sum, conflicts)
}
fn benchmark(options: &Options) -> Result<()> {
    if cfg!(debug_assertions) {
        return Err("benchmarks require --release".into());
    }
    let workloads = if options.query_bench {
        vec![
            ("query-project", 1),
            ("query-join", 1),
            ("query-semi", 1),
            ("query-compose", 1),
        ]
    } else {
        vec![
            ("read", 1),
            ("update", 1),
            ("disjoint", 1),
            ("disjoint", 4),
            ("contended", 4),
        ]
    };
    for (name, threads) in workloads {
        let rounds = options.rounds.get() * if name == "read" { 16 } else { 1 };
        let (mut rust, mut native) = (Vec::new(), Vec::new());
        for sample in 0..options.samples.get() {
            // Alternate order to reduce systematic warming and frequency bias.
            let native_run = || -> Result<(u128, u64, u64)> {
                let output = Command::new(&options.native_executable)
                    .args(["bench", name, &rounds.to_string(), &threads.to_string()])
                    .output()?;
                if !output.status.success() || !output.stderr.is_empty() {
                    return Err(format!(
                        "native benchmark: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )
                    .into());
                }
                let numbers = String::from_utf8(output.stdout)?
                    .split_whitespace()
                    .map(str::parse::<u128>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if numbers.len() != 3 {
                    return Err("invalid native measurement".into());
                }
                Ok((numbers[0], numbers[1] as u64, numbers[2] as u64))
            };
            let (r, n) = if sample.is_multiple_of(2) {
                let r = rust_bench(name, rounds, threads);
                (r, native_run()?)
            } else {
                let n = native_run()?;
                (rust_bench(name, rounds, threads), n)
            };
            assert_eq!(r.1, n.1, "workload checksum");
            rust.push(r.0);
            native.push(n.0);
            println!(
                "{}",
                json!({"workload":name,"threads":threads,"sample":sample,"operations":rounds as usize*threads,
                "rust_ns":r.0,"native_ns":n.0,"rust_conflicts":r.2,"native_conflicts":n.2})
            );
        }
        rust.sort();
        native.sort();
        println!(
            "{}",
            json!({"workload":name,"threads":threads,"median_rust_ns":rust[rust.len()/2],
            "median_native_ns":native[native.len()/2],"native_over_rust":native[native.len()/2] as f64/rust[rust.len()/2] as f64})
        );
    }
    Ok(())
}
fn main() -> Result<()> {
    let options = Options::parse();
    if options.cases.get() > 1024 {
        return Err("--cases must not exceed 1024".into());
    }
    let steps = corpus(&options);
    compare_trace(
        &options,
        "trace",
        &steps,
        &reference(&steps, &kernel(), false),
    )?;
    rules::correctness(&options)?;
    if options.rule_bench {
        rules::benchmark(&options)?;
    }
    if options.bench || options.query_bench {
        benchmark(&options)?;
    }
    Ok(())
}
