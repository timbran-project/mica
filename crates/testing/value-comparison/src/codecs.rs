// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::Write;
use std::process::{Command, Stdio};

use mica_var::{
    Symbol, SymbolEncoding, Tuple, Value, ValueCodecOptions, decode_value_exact,
    decode_value_exact_with_options, encode_value, encode_value_with_options,
};
use proptest::prelude::*;

use crate::cases::{Case, Input, Operation};
use crate::{Native, Result};

pub fn encode(value: &Value, allow_capabilities: bool) -> Option<Value> {
    encoded(value, allow_capabilities).map(Value::bytes)
}

fn encoded(value: &Value, allow_capabilities: bool) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_value_with_options(value, &mut bytes, options(allow_capabilities)).ok()?;
    Some(bytes)
}

pub fn decode(value: &Value, allow_capabilities: bool) -> Option<Value> {
    value
        .with_bytes(|bytes| {
            decode_value_exact_with_options(bytes, options(allow_capabilities)).ok()
        })
        .flatten()
}

fn options(allow_capabilities: bool) -> ValueCodecOptions {
    ValueCodecOptions {
        symbol_encoding: SymbolEncoding::Id,
        allow_capabilities,
    }
}

fn case(op: Operation, left: Input, allow: bool) -> Case {
    Case {
        op,
        left,
        right: Input::Bool(allow),
    }
}

pub fn strategy() -> BoxedStrategy<Case> {
    let values = (crate::cases::input_strategy(), any::<bool>(), 0u8..4).prop_map(
        |(input, allow, operation)| match operation {
            0 => case(Operation::Encode, input, allow),
            1 => case(Operation::IsPersistable, input, allow),
            _ => {
                let mut bytes = input
                    .build()
                    .and_then(|v| encoded(&v, allow))
                    .unwrap_or_default();
                if operation == 3 {
                    bytes.pop();
                }
                case(Operation::Decode, Input::Bytes(bytes), allow)
            }
        },
    );
    let malformed = (prop::collection::vec(any::<u8>(), 0..128), any::<bool>())
        .prop_map(|(bytes, allow)| case(Operation::Decode, Input::Bytes(bytes), allow));
    prop_oneof![4 => values, 1 => malformed].boxed()
}

fn words(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn extended(kind: u8, aux: u64) -> u64 {
    0xff00_0000_0000_0000 | (u64::from(kind) << 48) | aux
}

pub fn fixed_cases() -> Vec<Case> {
    let scalar = vec![
        Input::Empty,
        Input::Bool(false),
        Input::Bool(true),
        Input::Int(-(1 << 55)),
        Input::Int((1 << 55) - 1),
        Input::Float(1),
        Input::Float(f32::MAX.to_bits()),
        Input::Identity(42),
        Input::Symbol(u32::MAX),
        Input::ErrorCode(19),
        Input::String("é水🦀\0\n".repeat(256)),
        Input::Bytes(vec![0, 255, 128, 13]),
        Input::Capability(1),
        Input::Function(0),
    ];
    let mut inputs = scalar.clone();
    for child in scalar {
        inputs.extend([
            Input::List(vec![Input::Empty, child.clone()]),
            Input::Map(vec![(child.clone(), Input::Int(2))]),
            Input::Map(vec![(Input::Int(1), child.clone())]),
            Input::Range(Box::new(child.clone()), None),
            Input::Range(Box::new(Input::Int(0)), Some(Box::new(child.clone()))),
            Input::Error(17, Some("é\0\"".into()), Some(Box::new(child.clone()))),
            Input::Frob(42, Box::new(child.clone())),
            Input::Relation(vec![9, 2], vec![vec![child, Input::Empty]]),
        ]);
    }
    inputs.extend([
        Input::List(vec![]),
        Input::Map(vec![]),
        Input::Relation(vec![], vec![]),
        Input::Relation(vec![], vec![vec![]]),
        Input::Relation(vec![2], vec![]),
        Input::Error(17, None, None),
        Input::Error(17, None, Some(Box::new(Input::Int(1)))),
        Input::Error(17, Some(String::new()), None),
    ]);
    let mut cases = Vec::new();
    for (input_index, input) in inputs.into_iter().enumerate() {
        cases.push(case(Operation::IsPersistable, input.clone(), false));
        for allow in [false, true] {
            cases.push(case(Operation::Encode, input.clone(), allow));
            if let Some(bytes) = input.build().and_then(|v| encoded(&v, allow)) {
                cases.push(case(Operation::Decode, Input::Bytes(bytes.clone()), allow));
                // Scalar headers exercise every short prefix. Nested containers
                // also lose their last byte, including optional child payloads.
                let prefixes = if input_index < 14 {
                    bytes.len().min(24)
                } else {
                    0
                };
                for length in (0..prefixes).chain([bytes.len() - 1]) {
                    cases.push(case(
                        Operation::Decode,
                        Input::Bytes(bytes[..length].to_vec()),
                        allow,
                    ));
                }
                let mut trailing = bytes;
                trailing.push(0);
                cases.push(case(Operation::Decode, Input::Bytes(trailing), allow));
            }
        }
    }
    let mut malformed = vec![
        words(&[1]), // Nonzero empty relation payload.
        words(&[(1 << 56) | 2]),
        words(&[(3 << 56) | (1 << 32)]),
        words(&[(3 << 56) | 0x7f80_0000]),
        words(&[(3 << 56) | 0x7fc0_0000]),
        words(&[(5 << 56) | (1 << 32)]),
        words(&[(6 << 56) | (1 << 32)]),
        words(&[13 << 56]),
        words(&[extended(11, 2)]),
        words(&[extended(12, 4)]),
        words(&[extended(14, 1)]),
        words(&[extended(12, 0), 2 << 56]), // Error code has wrong kind.
        words(&[extended(14, 0), 2 << 56, 0]), // Frob delegate has wrong kind.
        words(&[extended(9, 0xffff_ffff_ffff)]),
        words(&[extended(10, 0xffff_ffff_ffff)]),
        words(&[extended(16, 0), 2]),
        words(&[extended(16, 1), 5 << 56, u64::MAX]),
        words(&[extended(16, 2), 5 << 56, 5 << 56, 0]), // Duplicate columns.
        words(&[extended(16, 1), 2 << 56, 0]),
        words(&[extended(7, 1), 0xff]), // Invalid UTF-8 (and trailing bytes).
        words(&[extended(5, 0)]),       // Named symbol in ID mode.
        words(&[extended(6, 0)]),
    ];
    for tag in 0u8..=255 {
        if !matches!(tag, 0..=6 | 13) {
            malformed.push(words(&[(u64::from(tag) << 56) | 0xdead_beef]));
        }
        if !matches!(tag, 5..=12 | 14 | 16) {
            malformed.push(words(&[extended(tag, 0)]));
        }
    }
    // Noncanonical but valid inputs: float -0, map duplicate keys, relation
    // columns and rows out of order. Decode must use the ordinary constructors.
    malformed.extend([
        words(&[(3 << 56) | 0x8000_0000]),
        words(&[
            extended(10, 2),
            (2 << 56) | 1,
            (2 << 56) | 2,
            (2 << 56) | 1,
            (2 << 56) | 3,
        ]),
        words(&[
            extended(16, 2),
            (5 << 56) | 9,
            (5 << 56) | 2,
            3,
            (2 << 56) | 3,
            (2 << 56) | 4,
            (2 << 56) | 1,
            (2 << 56) | 2,
            (2 << 56) | 3,
            (2 << 56) | 4,
        ]),
    ]);
    for bytes in malformed {
        for allow in [false, true] {
            cases.push(case(Operation::Decode, Input::Bytes(bytes.clone()), allow));
        }
    }
    cases
}

pub fn check_names(native: &Native) -> Result<()> {
    let a = Symbol::intern("codec α");
    let b = Symbol::intern("codec 🦀\0");
    let symbol = Value::symbol(a);
    let error = Value::error(b, Some("message\n\0"), Some(symbol.clone()));
    let relation = Value::relation(
        [b, a],
        [
            Tuple::new([error.clone(), symbol.clone()]),
            Tuple::new([symbol.clone(), error.clone()]),
        ],
    )?;
    let values = [
        Value::symbol(Symbol::intern("")),
        symbol.clone(),
        Value::error_code(b),
        error,
        Value::list([symbol.clone(), Value::string("é水🦀")]),
        Value::map([
            (Value::symbol(b), symbol.clone()),
            (symbol.clone(), Value::int(3).unwrap()),
        ]),
        relation,
    ];
    for value in values {
        let mut bytes = Vec::new();
        encode_value(&value, &mut bytes)?;
        let output =
            named_round_trip(native, &bytes)?.ok_or("C named codec rejected Rust encoding")?;
        let decoded = decode_value_exact(&output)?;
        if decoded != value {
            return Err(format!("named codec mismatch: {decoded:?} != {value:?}").into());
        }
    }
    let mut invalid_name = words(&[extended(5, 2)]);
    invalid_name.extend([0xc0, 0xaf]);
    let malformed = [
        words(&[5 << 56]),
        words(&[6 << 56]),
        words(&[(13 << 56) | 1]),
        words(&[15 << 56]),
        words(&[extended(5, 3)]),
        invalid_name,
        words(&[extended(12, 0), extended(5, 0)]),
    ];
    for bytes in malformed {
        assert!(decode_value_exact(&bytes).is_err());
        if named_round_trip(native, &bytes)?.is_some() {
            return Err(format!("C named codec accepted invalid bytes: {bytes:?}").into());
        }
    }
    Ok(())
}

fn named_round_trip(native: &Native, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut child = Command::new(native.scratch.0.join("compare"))
        .arg("codec")
        .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = (bytes.len() as u64).to_le_bytes().to_vec();
    input.extend(bytes);
    child
        .stdin
        .take()
        .ok_or("missing codec input pipe")?
        .write_all(&input)?;
    let output = child.wait_with_output()?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "named codec driver: {} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if output.stdout == [0] {
        return Ok(None);
    }
    if output.stdout.len() < 9 || output.stdout[0] != 1 {
        return Err("invalid named codec response".into());
    }
    let length = u64::from_le_bytes(output.stdout[1..9].try_into()?);
    if length != (output.stdout.len() - 9) as u64 {
        return Err("invalid named codec output length".into());
    }
    Ok(Some(output.stdout[9..].to_vec()))
}
