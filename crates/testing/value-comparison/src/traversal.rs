// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_var::{Tuple, Value, ValueRef};

const SEED: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(PRIME)
}

fn bytes(hash: u64, value: &[u8]) -> u64 {
    value.iter().fold(hash, |h, byte| mix(h, u64::from(*byte)))
}

fn tuple_hash(values: &[Value]) -> u64 {
    values
        .iter()
        .fold(mix(SEED, values.len() as u64), |h, v| mix(h, hash(v)))
}

/// Omica's canonical hash algorithm over Rust values. Rust's standard Hash
/// implementation deliberately does not prescribe a portable hash function.
pub fn hash(value: &Value) -> u64 {
    let mut h = mix(SEED, value.kind() as u64);
    match value.as_value_ref() {
        ValueRef::String(text) => bytes(h, text.as_bytes()),
        ValueRef::Bytes(data) => bytes(h, data),
        ValueRef::List(values) => values
            .iter()
            .fold(mix(h, values.len() as u64), |h, v| mix(h, hash(v))),
        ValueRef::Map(entries) => entries
            .iter()
            .fold(mix(h, entries.len() as u64), |h, (k, v)| {
                mix(mix(h, hash(k)), hash(v))
            }),
        ValueRef::Relation(relation) => {
            h = mix(h, relation.arity() as u64);
            for symbol in relation.heading() {
                h = mix(h, u64::from(symbol.id()));
            }
            h = mix(h, relation.len() as u64);
            for row in relation.rows() {
                h = mix(h, tuple_hash(row.values()));
            }
            h
        }
        ValueRef::Range { start, end } => {
            h = mix(h, hash(start));
            h = mix(h, u64::from(end.is_some()));
            end.map_or(h, |end| mix(h, hash(end)))
        }
        ValueRef::Error {
            code,
            message,
            value,
        } => {
            h = mix(h, u64::from(code.id()));
            h = mix(h, u64::from(message.is_some()));
            if let Some(message) = message {
                h = bytes(h, message.as_bytes());
            }
            h = mix(h, u64::from(value.is_some()));
            value.map_or(h, |value| mix(h, hash(value)))
        }
        ValueRef::Frob { delegate, value } => mix(mix(h, delegate.raw()), hash(value)),
        _ => mix(h, value.raw_bits() & 0x00ff_ffff_ffff_ffff),
    }
}

/// Rebuilds independent value storage. Executable closures cannot be copied to
/// another heap without retaining their runtime's code ownership; publication
/// within that runtime uses the collector instead.
pub fn copy(value: &Value) -> Option<Value> {
    Some(match value.as_value_ref() {
        ValueRef::Function(_) => return None,
        ValueRef::String(text) => Value::string(text),
        ValueRef::Bytes(data) => Value::bytes(data),
        ValueRef::List(values) => Value::list(values.iter().map(copy).collect::<Option<Vec<_>>>()?),
        ValueRef::Map(entries) => Value::map(
            entries
                .iter()
                .map(|(key, value)| Some((copy(key)?, copy(value)?)))
                .collect::<Option<Vec<_>>>()?,
        ),
        ValueRef::Relation(relation) => Value::relation(
            relation.heading().iter().copied(),
            relation
                .rows()
                .iter()
                .map(|row| {
                    Some(Tuple::new(
                        row.values().iter().map(copy).collect::<Option<Vec<_>>>()?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?,
        )
        .expect("source relation is canonical"),
        ValueRef::Range { start, end } => Value::range(
            copy(start)?,
            match end {
                Some(end) => Some(copy(end)?),
                None => None,
            },
        ),
        ValueRef::Error {
            code,
            message,
            value,
        } => Value::error(
            code,
            message,
            match value {
                Some(value) => Some(copy(value)?),
                None => None,
            },
        ),
        ValueRef::Frob { delegate, value } => Value::frob(delegate, copy(value)?),
        _ => value.clone(),
    })
}
