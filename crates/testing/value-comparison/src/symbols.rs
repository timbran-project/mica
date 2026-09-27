// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use mica_var::Symbol;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};

use crate::{Native, Result};

fn encode_names(names: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (names.len() as u64).to_le_bytes().to_vec();
    for name in names {
        bytes.extend((name.len() as u64).to_le_bytes());
        bytes.extend(name);
    }
    bytes
}

pub(crate) fn invoke(mut command: Command, input: &[u8]) -> Result<Vec<u8>> {
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
        .write_all(input);
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
    let mut command = Command::new(native.scratch.0.join("compare"));
    command.arg("symbols");
    let actual = invoke(command, &encode_names(names))?;
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
