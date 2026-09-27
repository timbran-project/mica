// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::io::Read;
use std::process::Command;
use std::sync::Barrier;
use std::thread;
use std::time::Instant;

use mica_var::Symbol;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Native, Result, median, symbols};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[repr(u8)]
enum Kind {
    Intern,
    Text,
    Metadata,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Operation {
    kind: Kind,
    name: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Load {
    names: Vec<Vec<u8>>,
    preintern: usize,
    workers: Vec<Vec<Operation>>,
}

fn number(out: &mut Vec<u8>, value: usize) {
    out.extend((value as u64).to_le_bytes());
}

fn text_checksum(bytes: &[u8]) -> u64 {
    bytes.len() as u64
        + bytes.first().map_or(0, |v| u64::from(*v))
        + bytes.last().map_or(0, |v| u64::from(*v))
}

impl Load {
    fn validate(&self) -> Result<()> {
        if self.workers.is_empty() || self.preintern > self.names.len() {
            return Err("invalid symbol load dimensions".into());
        }
        let mut names = HashSet::new();
        for (index, name) in self.names.iter().enumerate() {
            if !names.insert(name) || (index < self.preintern && std::str::from_utf8(name).is_err())
            {
                return Err(
                    "symbol load needs a unique catalog and valid pre-interned names".into(),
                );
            }
        }
        for worker in &self.workers {
            if worker.is_empty() {
                return Err("symbol worker has no operations".into());
            }
            for op in worker {
                if op.name >= self.names.len()
                    || (!matches!(op.kind, Kind::Intern) && op.name >= self.preintern)
                {
                    return Err("symbol operation references an unavailable name".into());
                }
            }
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        number(&mut out, self.names.len());
        number(&mut out, self.preintern);
        for bytes in &self.names {
            number(&mut out, bytes.len());
            out.extend(bytes);
            let text = std::str::from_utf8(bytes).ok();
            out.push(u8::from(text.is_some()));
            number(&mut out, text.map_or(0, |text| text.chars().count()));
            out.push(u8::from(bytes.is_ascii()));
        }
        number(&mut out, self.workers.len());
        for worker in &self.workers {
            number(&mut out, worker.len());
            for op in worker {
                out.push(op.kind as u8);
                number(&mut out, op.name);
            }
        }
        out
    }

    fn operations(&self) -> usize {
        self.workers.iter().map(Vec::len).sum()
    }

    fn expected(&self, rounds: u64) -> (u64, usize) {
        let mut digest = 0u64;
        let mut seen: HashSet<_> = (0..self.preintern).collect();
        for op in self.workers.iter().flatten() {
            let bytes = &self.names[op.name];
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };
            seen.insert(op.name);
            digest = digest.wrapping_add(match op.kind {
                Kind::Intern => 1,
                Kind::Text => text_checksum(bytes),
                Kind::Metadata => {
                    (bytes.len() + text.chars().count()) as u64 + u64::from(text.is_ascii())
                }
            });
        }
        (digest.wrapping_mul(rounds), seen.len())
    }
}

fn read_number(input: &mut impl Read) -> Result<usize> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(usize::try_from(u64::from_le_bytes(bytes))?)
}

fn read_byte(input: &mut impl Read) -> Result<u8> {
    let mut byte = [0];
    input.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn decode(input: &mut impl Read) -> Result<Load> {
    let count = read_number(input)?;
    let preintern = read_number(input)?;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let mut bytes = vec![0; read_number(input)?];
        input.read_exact(&mut bytes)?;
        // The C oracle uses these Rust-computed UTF-8 properties. Rust verifies
        // the supplied properties too, before any worker or timer starts.
        let valid = read_byte(input)? != 0;
        let scalars = read_number(input)?;
        let ascii = read_byte(input)? != 0;
        let text = std::str::from_utf8(&bytes).ok();
        if valid != text.is_some()
            || scalars != text.map_or(0, |s| s.chars().count())
            || ascii != bytes.is_ascii()
        {
            return Err("incorrect symbol load metadata".into());
        }
        names.push(bytes);
    }
    let mut workers = Vec::new();
    for _ in 0..read_number(input)? {
        let mut ops = Vec::new();
        for _ in 0..read_number(input)? {
            let kind = match read_byte(input)? {
                0 => Kind::Intern,
                1 => Kind::Text,
                2 => Kind::Metadata,
                _ => return Err("invalid symbol operation".into()),
            };
            ops.push(Operation {
                kind,
                name: read_number(input)?,
            });
        }
        workers.push(ops);
    }
    let load = Load {
        names,
        preintern,
        workers,
    };
    load.validate()?;
    Ok(load)
}

struct Completed {
    ids: Vec<Option<Symbol>>,
    digest: u64,
    stable: bool,
}

fn exercise(
    ops: &[Operation],
    texts: &[Option<&str>],
    known: &[Symbol],
    rounds: u64,
    verify: bool,
    mut result: Completed,
) -> Completed {
    for round in 0..rounds {
        for (i, op) in ops.iter().enumerate() {
            let (id, observed) = match op.kind {
                Kind::Intern => {
                    let id = texts[op.name].map(|s| Symbol::intern(black_box(s)));
                    (black_box(id), u64::from(id.is_some()))
                }
                Kind::Text => {
                    let id = black_box(known[op.name]);
                    let text = id.name();
                    if verify && text != texts[op.name] {
                        result.stable = false;
                    }
                    (Some(id), text.map_or(0, |s| text_checksum(s.as_bytes())))
                }
                Kind::Metadata => {
                    let id = black_box(known[op.name]);
                    let meta = id.metadata();
                    if verify
                        && !meta.is_some_and(|m| {
                            let expected = texts[op.name].unwrap();
                            m.byte_len == expected.len()
                                && m.char_len == expected.chars().count()
                                && m.is_ascii == expected.is_ascii()
                        })
                    {
                        result.stable = false;
                    }
                    (
                        Some(id),
                        meta.map_or(0, |m| {
                            (m.byte_len + m.char_len) as u64 + u64::from(m.is_ascii)
                        }),
                    )
                }
            };
            if verify {
                if id.is_some() != texts[op.name].is_some() {
                    result.stable = false;
                }
                if let Some(id) = id {
                    let expected = texts[op.name].unwrap_or("");
                    if id.name() != texts[op.name]
                        || !id.metadata().is_some_and(|m| {
                            m.byte_len == expected.len()
                                && m.char_len == expected.chars().count()
                                && m.is_ascii == expected.is_ascii()
                        })
                    {
                        result.stable = false;
                    }
                }
            }
            if round != 0 && result.ids[i] != id {
                result.stable = false;
            }
            result.ids[i] = id;
            result.digest = result.digest.wrapping_add(black_box(observed));
        }
    }
    result
}

fn run_rust(load: &Load, rounds: u64) -> Result<(u64, u64, usize)> {
    let verify = rounds == 0;
    let rounds = if verify { 2 } else { rounds };
    let texts: Vec<_> = load
        .names
        .iter()
        .map(|s| std::str::from_utf8(s).ok())
        .collect();
    let known: Vec<_> = texts[..load.preintern]
        .iter()
        .map(|s| Symbol::intern(s.unwrap()))
        .collect();
    let ready = Barrier::new(load.workers.len() + 1);
    let start = Barrier::new(load.workers.len() + 1);
    let finish = Barrier::new(load.workers.len() + 1);
    let (elapsed, completed) = thread::scope(|scope| {
        let handles: Vec<_> = load
            .workers
            .iter()
            .map(|ops| {
                let (texts, known, ready, start, finish) =
                    (&texts, &known, &ready, &start, &finish);
                scope.spawn(move || {
                    // Populate only existing names in this worker's TLS cache.
                    for op in ops {
                        if op.name < known.len() {
                            match op.kind {
                                Kind::Intern => {
                                    black_box(Symbol::intern(texts[op.name].unwrap()));
                                }
                                Kind::Text => {
                                    black_box(known[op.name].name());
                                }
                                Kind::Metadata => {
                                    black_box(known[op.name].metadata());
                                }
                            }
                        }
                    }
                    let prepared = Completed {
                        ids: vec![None; ops.len()],
                        digest: 0,
                        stable: true,
                    };
                    ready.wait();
                    start.wait();
                    let result = exercise(ops, texts, known, rounds, verify, prepared);
                    finish.wait();
                    result
                })
            })
            .collect();
        ready.wait();
        let began = Instant::now();
        start.wait();
        finish.wait();
        let elapsed = began.elapsed().as_nanos() as u64;
        let results = handles
            .into_iter()
            .map(|h| h.join().expect("symbol worker panicked"))
            .collect::<Vec<_>>();
        (elapsed, results)
    });
    let mut canonical = HashMap::new();
    for (index, id) in known.iter().enumerate() {
        canonical.insert(index, *id);
    }
    let mut digest = 0u64;
    for (ops, result) in load.workers.iter().zip(completed) {
        if !result.stable {
            return Err("symbol identity, text, or metadata mismatch within a worker".into());
        }
        digest = digest.wrapping_add(result.digest);
        for (op, actual) in ops.iter().zip(result.ids) {
            if actual.is_some() != texts[op.name].is_some() {
                return Err("symbol validity mismatch".into());
            }
            let Some(id) = actual else {
                continue;
            };
            if let Some(previous) = canonical.insert(op.name, id)
                && previous != id
            {
                return Err("equal names received different IDs across workers".into());
            }
        }
    }
    let mut distinct = HashSet::new();
    for (index, id) in &canonical {
        let name = texts[*index].unwrap();
        let meta = id.metadata().ok_or("missing symbol metadata")?;
        if !distinct.insert(*id)
            || id.name() != Some(name)
            || meta.byte_len != name.len()
            || meta.char_len != name.chars().count()
            || meta.is_ascii != name.is_ascii()
            || Symbol::intern(name) != *id
        {
            return Err("symbol identity, text, or metadata mismatch after concurrent load".into());
        }
    }
    if (digest, distinct.len()) != load.expected(rounds) {
        return Err("symbol load checksum or cardinality mismatch".into());
    }
    Ok((elapsed, digest, distinct.len()))
}

pub fn worker(rounds: u64) -> Result<()> {
    let load = decode(&mut std::io::stdin().lock())?;
    let (elapsed, digest, count) = run_rust(&load, rounds)?;
    println!("symbol_load_result {elapsed} {digest} {count}");
    Ok(())
}

fn rust_command(rounds: u64) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    {
        command.arg("--symbol-load-worker");
        if rounds == 0 {
            command.arg("--symbol-load-verify");
        } else {
            command.args(["--iterations", &rounds.to_string()]);
        }
    }
    #[cfg(test)]
    {
        command.args([
            "--exact",
            "symbol_loads::tests::worker_process",
            "--ignored",
            "--nocapture",
        ]);
        command.env("MICA_SYMBOL_WORKER_ROUNDS", rounds.to_string());
    }
    Ok(command)
}

fn sample(native: &Native, load: &Load, rounds: u64, rust: bool) -> Result<f64> {
    let mut command = if rust {
        rust_command(rounds)?
    } else {
        let mut c = Command::new(native.scratch.0.join("compare"));
        c.args(["symbol-load", &rounds.to_string()]);
        c
    };
    command.env("TSAN_OPTIONS", "halt_on_error=1");
    let output = symbols::invoke(command, &load.encode())?;
    let output = std::str::from_utf8(&output)?;
    let response = output
        .lines()
        .find_map(|line| line.strip_prefix("symbol_load_result "))
        .ok_or_else(|| format!("missing symbol load response: {output}"))?;
    let fields: Vec<_> = response.split_whitespace().collect();
    if fields.len() != 3 {
        return Err(format!("invalid symbol load response: {output}").into());
    }
    let actual = (fields[1].parse::<u64>()?, fields[2].parse::<usize>()?);
    let rounds = if rounds == 0 { 2 } else { rounds };
    if actual != load.expected(rounds) {
        return Err("symbol load digest or cardinality disagrees with oracle".into());
    }
    Ok(fields[0].parse::<u64>()? as f64 / (load.operations() as f64 * rounds as f64))
}

fn random(state: &mut u64) -> usize {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut n = *state;
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
    (n ^ (n >> 31)) as usize
}

const WORKLOADS: [&str; 8] = [
    "one", "hot", "uniform", "text", "metadata", "unique", "shared", "mixed",
];

fn workload(mode: &str, threads: usize, seed: u64, small: bool) -> Load {
    let mut rng = seed;
    let existing = if mode == "one" {
        1
    } else if small {
        64
    } else {
        1024
    };
    let count = match mode {
        "unique" => {
            if small {
                512
            } else {
                65536
            }
        }
        "shared" | "mixed" => {
            if small {
                512
            } else {
                8192
            }
        }
        _ => existing,
    };
    let preintern = match mode {
        "unique" | "shared" => 0,
        _ => existing,
    };
    let names = (0..count)
        .map(|i| {
            let text = match i % 4 {
                0 => format!("λ\0-{seed:016x}-{i:08x}"),
                1 => format!("name-{seed:016x}-{i:08x}"),
                2 => format!("😀e\u{301}-{seed:016x}-{i:08x}"),
                _ => format!("name-{}-{i:08x}", "long".repeat(i % 16)),
            };
            text.into_bytes()
        })
        .collect();
    let mut workers = vec![Vec::new(); threads];
    match mode {
        "unique" => {
            for i in 0..count {
                workers[i % threads].push(Operation {
                    kind: Kind::Intern,
                    name: i,
                });
            }
        }
        "shared" => {
            for worker in &mut workers {
                let mut order: Vec<_> = (0..count).collect();
                for i in (1..count).rev() {
                    order.swap(i, random(&mut rng) % (i + 1));
                }
                *worker = order
                    .into_iter()
                    .map(|name| Operation {
                        kind: Kind::Intern,
                        name,
                    })
                    .collect();
            }
        }
        "mixed" => {
            for i in existing..count {
                for (j, kind) in [
                    Kind::Intern,
                    Kind::Text,
                    Kind::Metadata,
                    Kind::Intern,
                    Kind::Text,
                ]
                .into_iter()
                .enumerate()
                {
                    let name = if j == 0 {
                        i
                    } else {
                        random(&mut rng) % existing
                    };
                    workers[((i - existing) * 5 + j) % threads].push(Operation { kind, name });
                }
            }
        }
        _ => {
            for worker in &mut workers {
                for _ in 0..if small { 256 } else { 4096 } {
                    let n = random(&mut rng);
                    let name = if mode == "hot" && !n.is_multiple_of(10) {
                        random(&mut rng) % 8
                    } else {
                        n % existing
                    };
                    let kind = match mode {
                        "text" => Kind::Text,
                        "metadata" => Kind::Metadata,
                        _ => Kind::Intern,
                    };
                    worker.push(Operation { kind, name });
                }
            }
        }
    }
    Load {
        names,
        preintern,
        workers,
    }
}

fn check_load(native: &Native, load: &Load) -> Result<()> {
    load.validate()?;
    // Both implementations need fresh tables, including during unit tests.
    sample(native, load, 0, true)?;
    sample(native, load, 0, false)?;
    Ok(())
}

pub fn replay(native: &Native, json: &str) -> Result<()> {
    check_load(native, &serde_json::from_str(json)?)
}

pub fn check(native: &Native, cases: u32, seed: u64, threads: &[usize]) -> Result<()> {
    for &threads in threads {
        for mode in WORKLOADS {
            check_load(native, &workload(mode, threads, seed, true)).map_err(|error| {
                format!("symbol {mode}, threads={threads}, seed={seed}: {error}")
            })?;
        }
    }
    for case in 0..cases {
        let mut load = workload(
            if case % 2 == 0 { "shared" } else { "mixed" },
            threads[case as usize % threads.len()],
            seed.wrapping_add(u64::from(case)),
            true,
        );
        // Invalid UTF-8 races exercise failure paths without consuming IDs.
        let invalid = load.names.len();
        load.names.push(vec![0xed, 0xa0, 0x80]);
        let empty = load.names.len();
        load.names.push(Vec::new());
        for worker in &mut load.workers {
            worker.insert(
                0,
                Operation {
                    kind: Kind::Intern,
                    name: empty,
                },
            );
            worker.insert(
                0,
                Operation {
                    kind: Kind::Intern,
                    name: invalid,
                },
            );
        }
        check_load(native, &load).map_err(|error| format!("{error}\nReplay inputs with --symbol-load-case '{}'. Thread scheduling is nondeterministic.", serde_json::to_string(&load).unwrap()))?;
    }
    Ok(())
}

pub fn benchmark(
    native: &Native,
    threads: &[usize],
    seed: u64,
    samples: u32,
    minimum_rounds: u64,
    sample_ms: u32,
) -> Result<()> {
    for mode in WORKLOADS {
        for &threads in threads {
            let load = workload(mode, threads, seed, false);
            load.validate()?;
            sample(native, &load, 0, true)?;
            sample(native, &load, 0, false)?;
            let insertion = matches!(mode, "unique" | "shared" | "mixed");
            let mut repetitions = [1u64; 2];
            if !insertion {
                // Calibrate each implementation separately: lock contention can
                // otherwise turn a short target into minutes for the slower side.
                for (index, rust) in [true, false].into_iter().enumerate() {
                    let mut rounds = minimum_rounds;
                    for _ in 0..4 {
                        let ns = sample(native, &load, rounds, rust)?;
                        let elapsed = ns * load.operations() as f64 * rounds as f64;
                        let target = f64::from(sample_ms) * 1_000_000.0;
                        if elapsed >= target {
                            break;
                        }
                        let desired = (rounds as f64 * target / elapsed.max(1.0) * 1.1).ceil();
                        if desired > (u64::MAX / load.operations() as u64) as f64 {
                            return Err("symbol load repetition overflow".into());
                        }
                        rounds = desired as u64;
                    }
                    repetitions[index] = rounds;
                }
            }
            let mut rust_ns = Vec::new();
            let mut c_ns = Vec::new();
            for n in 0..samples {
                let rust_first = n % 2 == 0;
                let first = sample(
                    native,
                    &load,
                    repetitions[usize::from(!rust_first)],
                    rust_first,
                )?;
                let second = sample(
                    native,
                    &load,
                    repetitions[usize::from(rust_first)],
                    !rust_first,
                )?;
                let (rust, c) = if rust_first {
                    (first, second)
                } else {
                    (second, first)
                };
                rust_ns.push(rust);
                c_ns.push(c);
            }
            let rust_median = median(&rust_ns);
            let c_median = median(&c_ns);
            println!(
                "{}",
                json!({"workload":format!("symbol_{mode}"),"threads":threads,"seed":seed,
                "rust_operations_per_sample":load.operations() as u64*repetitions[0],"c_operations_per_sample":load.operations() as u64*repetitions[1],"rust_ns_per_op":rust_ns,"c_ns_per_op":c_ns,
                "rust_median_ns":rust_median,"c_median_ns":c_median,"c_over_rust":c_median/rust_median,
                "rust_ops_per_second":1e9/rust_median,"c_ops_per_second":1e9/c_median,
                "metric":"wall time divided by total operations; reciprocal throughput, not per-call latency",
                "timing":"shared table; real worker threads; barriers included; creation, warmup, joins, validation, and cleanup excluded",
                "calibration":if insertion { "one pass, fresh table per sample" } else { "duration" }})
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn detects_incorrect_metadata(native: &Native) -> Result<()> {
    let mut load = workload("text", 2, 37, true);
    load.workers[0][0] = Operation {
        kind: Kind::Text,
        name: 0,
    };
    let mut encoded = load.encode();
    // Corrupt the oracle scalar count for the first pre-interned name.
    // The C worker must reject the mismatch while other workers still finish.
    encoded[16 + 8 + load.names[0].len() + 1] ^= 1;
    let mut command = Command::new(native.scratch.0.join("compare"));
    command.args(["symbol-load", "0"]);
    let error =
        symbols::invoke(command, &encoded).expect_err("incorrect metadata must fail verification");
    assert!(error.to_string().contains("incorrect text"), "{error}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    #[ignore = "subprocess entry point for a fresh process-global Rust symbol table"]
    fn worker_process() {
        let Ok(rounds) = std::env::var("MICA_SYMBOL_WORKER_ROUNDS") else {
            return;
        };
        match worker(rounds.parse().expect("invalid worker round count")) {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }

    #[test]
    fn load_protocol_round_trips_and_rejects_unavailable_reads() -> Result<()> {
        let mut load = workload("mixed", 4, 17, true);
        let decoded = decode(&mut Cursor::new(load.encode()))?;
        assert_eq!(decoded.encode(), load.encode());
        load.workers[0][0] = Operation {
            kind: Kind::Text,
            name: load.preintern,
        };
        assert!(load.validate().is_err());
        Ok(())
    }
}
