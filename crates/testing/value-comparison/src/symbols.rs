// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::hint::black_box;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

use mica_var::Symbol;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use serde_json::json;

use crate::{Native, Result, median};

fn encode_names(names: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (names.len() as u64).to_le_bytes().to_vec();
    for name in names {
        bytes.extend((name.len() as u64).to_le_bytes());
        bytes.extend(name);
    }
    bytes
}

fn invoke(mut command: Command, names: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut child = command
        .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let written = child
        .stdin
        .take()
        .ok_or("missing symbol input pipe")?
        .write_all(&encode_names(names));
    let output = child.wait_with_output()?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "symbol worker {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    written?;
    Ok(output.stdout)
}

fn c_command(native: &Native, mode: &str, rounds: u64) -> Command {
    let mut command = Command::new(native.scratch.0.join("compare"));
    command.args(["symbols", mode, &rounds.to_string()]);
    command
}

fn check_names(native: &Native, names: &[Vec<u8>]) -> Result<()> {
    let mut local_ids = HashMap::new();
    let ids: Vec<_> = names
        .iter()
        .map(|bytes| {
            std::str::from_utf8(bytes).ok().map(|name| {
                let symbol = Symbol::intern(name);
                let next = local_ids.len() as u64;
                let id = *local_ids.entry(symbol).or_insert(next);
                (symbol, id)
            })
        })
        .collect();
    // Read every result after the entire sequence, including table growth.
    let mut expected = Vec::new();
    for id in ids {
        let Some((symbol, id)) = id else {
            expected.push(0);
            continue;
        };
        let name = symbol.name().ok_or("Rust symbol has no name")?;
        let metadata = symbol.metadata().ok_or("Rust symbol has no metadata")?;
        expected.push(1);
        expected.extend(id.to_le_bytes());
        expected.extend((metadata.byte_len as u64).to_le_bytes());
        expected.extend(name.as_bytes());
        expected.extend((metadata.char_len as u64).to_le_bytes());
        expected.push(u8::from(metadata.is_ascii));
    }
    let actual = invoke(c_command(native, "check", 0), names)?;
    if actual != expected {
        return Err(format!(
            "symbol sequence mismatch; replay with --symbol-case '{}'",
            serde_json::to_string(names)?
        )
        .into());
    }
    Ok(())
}

pub fn replay(native: &Native, case: &str) -> Result<()> {
    check_names(native, &serde_json::from_str::<Vec<Vec<u8>>>(case)?)
}

pub fn check(native: &Native, cases: u32, seed: u64) -> Result<()> {
    let mut fixed = vec![
        Vec::new(),
        b"Name".to_vec(),
        b"name".to_vec(),
        "é\0😀".as_bytes().to_vec(),
        "e\u{301}".as_bytes().to_vec(),
        vec![0xc0, 0x80],
        vec![0xed, 0xa0, 0x80],
        vec![0xf4, 0x90, 0x80, 0x80],
        b"x".repeat(70000),
    ];
    fixed.extend((0..4096).map(|n| format!("name-{n}").into_bytes()));
    fixed.extend(fixed.clone().into_iter().rev());
    check_names(native, &fixed)?;
    let text = prop::collection::vec(any::<char>(), 0..48)
        .prop_map(|chars| chars.into_iter().collect::<String>().into_bytes());
    let name = prop_oneof![3 => text, 1 => prop::collection::vec(any::<u8>(), 0..32)];
    let sequences = prop::collection::vec(name, 0..80).prop_map(|mut names| {
        names.extend(names.clone().into_iter().rev());
        names
    });
    let mut runner = TestRunner::new(Config {
        cases,
        rng_seed: RngSeed::Fixed(seed),
        failure_persistence: None,
        max_shrink_iters: 2048,
        ..Config::default()
    });
    runner
        .run(&sequences, |names| {
            check_names(native, &names).map_err(|e| TestCaseError::fail(e.to_string()))
        })
        .map_err(|e| format!("symbol property failure, seed={seed}: {e}").into())
}

fn read_u64(input: &mut impl Read) -> Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Each invocation starts with a fresh Rust process-global interner. Input
/// decoding and name preparation stay outside the measured interval.
pub fn worker(mode: &str, rounds: u64) -> Result<()> {
    let mut input = std::io::stdin().lock();
    let count = usize::try_from(read_u64(&mut input)?)?;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let mut bytes = vec![0; usize::try_from(read_u64(&mut input)?)?];
        input.read_exact(&mut bytes)?;
        names.push(String::from_utf8(bytes)?);
    }
    if count == 0 || rounds == 0 || (mode == "insert" && rounds != 1) {
        return Err("invalid symbol sample size".into());
    }
    let ids: Vec<_> = if mode == "insert" {
        Vec::new()
    } else {
        names.iter().map(|name| Symbol::intern(name)).collect()
    };
    let mut base = ids.first().map(|id| id.id());
    let mut digest = 0u64;
    let start = Instant::now();
    for _ in 0..rounds {
        for (i, name) in names.iter().enumerate() {
            let observed = match mode {
                "insert" | "hit" => {
                    let id = Symbol::intern(black_box(name)).id();
                    u64::from(id - *base.get_or_insert(id))
                }
                "lookup" => {
                    let id = black_box(ids[i]);
                    let text = id.name().ok_or("symbol lookup failed")?.as_bytes();
                    let meta = id.metadata().ok_or("symbol metadata lookup failed")?;
                    let mut observed =
                        (meta.byte_len + meta.char_len) as u64 + u64::from(meta.is_ascii);
                    if let Some(first) = text.first() {
                        observed += u64::from(*first) + u64::from(text[text.len() - 1]);
                    }
                    observed
                }
                _ => return Err("unknown symbol mode".into()),
            };
            digest = digest.wrapping_add(black_box(observed));
        }
    }
    let elapsed = start.elapsed().as_nanos();
    println!("{elapsed} {digest}");
    Ok(())
}

fn sample(
    native: &Native,
    mode: &str,
    names: &[Vec<u8>],
    rounds: u64,
    rust: bool,
) -> Result<(f64, u64)> {
    let command = if rust {
        let mut command = Command::new(std::env::current_exe()?);
        command.args(["--symbol-worker", mode, "--iterations", &rounds.to_string()]);
        command
    } else {
        c_command(native, mode, rounds)
    };
    let bytes = invoke(command, names)?;
    let output = std::str::from_utf8(&bytes)?;
    let fields: Vec<_> = output.split_whitespace().collect();
    if fields.len() != 2 {
        return Err(format!("invalid symbol timing: {output}").into());
    }
    Ok((
        fields[0].parse::<u64>()? as f64 / (rounds as f64 * names.len() as f64),
        fields[1].parse()?,
    ))
}

pub fn benchmark(native: &Native, samples: u32, minimum_rounds: u64, sample_ms: u32) -> Result<()> {
    for (workload, mode, count) in [
        ("symbol_intern_repeated", "hit", 1),
        ("symbol_intern_existing", "hit", 1024),
        ("symbol_reverse_lookup", "lookup", 1024),
        ("symbol_intern_new", "insert", 65536),
    ] {
        let names: Vec<_> = (0..count)
            .map(|i| {
                if i % 4 == 0 {
                    format!("symbol-λ-{i:08}").into_bytes()
                } else {
                    format!("symbol-{i:08}").into_bytes()
                }
            })
            .collect();
        check_names(native, &names)?;
        let mut rounds = if mode == "insert" { 1 } else { minimum_rounds };
        if mode != "insert" {
            for _ in 0..4 {
                let rust = sample(native, mode, &names, rounds, true)?;
                let c = sample(native, mode, &names, rounds, false)?;
                if rust.1 != c.1 {
                    return Err(format!("symbol calibration mismatch in {workload}").into());
                }
                let elapsed = rust.0.min(c.0) * rounds as f64 * count as f64;
                let target = f64::from(sample_ms) * 1_000_000.0;
                if elapsed >= target {
                    break;
                }
                let desired = (rounds as f64 * target / elapsed.max(1.0) * 1.1).ceil();
                if desired > (u64::MAX / count) as f64 {
                    return Err("symbol sample size overflow".into());
                }
                rounds = desired as u64;
            }
        }
        let mut rust_ns = Vec::new();
        let mut c_ns = Vec::new();
        for n in 0..samples {
            let first_rust = n % 2 == 0;
            let first = sample(native, mode, &names, rounds, first_rust)?;
            let second = sample(native, mode, &names, rounds, !first_rust)?;
            if first.1 != second.1 {
                return Err(format!("symbol checksum mismatch in {workload}").into());
            }
            let (rust, c) = if first_rust {
                (first, second)
            } else {
                (second, first)
            };
            rust_ns.push(rust.0);
            c_ns.push(c.0);
        }
        println!(
            "{}",
            json!({"workload":workload,"operations_per_sample":rounds*count,
            "rust_ns_per_op":rust_ns,"c_ns_per_op":c_ns,"rust_median_ns":median(&rust_ns),
            "c_median_ns":median(&c_ns),"c_over_rust":median(&c_ns)/median(&rust_ns),
            "lifetime":"fresh process per sample; table cleanup excluded",
            "calibration":if mode=="insert" { "fixed 65536 distinct names, one insertion each" } else { "duration" }})
        );
    }
    Ok(())
}
