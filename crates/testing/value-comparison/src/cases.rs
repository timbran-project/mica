// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use mica_var::{CapabilityId, FunctionId, Identity, Symbol, Value, ValueRef, language_cmp};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

/// Test inputs preserve map input order and float bits. This is a test protocol,
/// not the runtime persistence codec or either implementation's heap ABI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Input {
    Empty,
    Bool(bool),
    Int(i64),
    Float(u32),
    Identity(u64),
    Symbol(u32),
    ErrorCode(u32),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<Input>),
    Map(Vec<(Input, Input)>),
    Range(Box<Input>, Option<Box<Input>>),
    Error(u32, Option<String>, Option<Box<Input>>),
    Capability(u64),
    Frob(u64, Box<Input>),
    Function(u64),
}

impl Input {
    pub fn build(&self) -> Option<Value> {
        Some(match self {
            Self::Empty => Value::empty_relation(),
            Self::Bool(v) => Value::bool(*v),
            Self::Int(v) => Value::int(*v).ok()?,
            Self::Float(v) => Value::float(f32::from_bits(*v)).ok()?,
            Self::Identity(v) => Value::identity(Identity::new(*v)?),
            Self::Symbol(v) => Value::symbol(Symbol::from_id(*v)),
            Self::ErrorCode(v) => Value::error_code(Symbol::from_id(*v)),
            Self::String(v) => Value::string(v),
            Self::Bytes(v) => Value::bytes(v),
            Self::List(v) => Value::list(v.iter().map(Self::build).collect::<Option<Vec<_>>>()?),
            Self::Map(v) => Value::map(
                v.iter()
                    .map(|(k, v)| Some((k.build()?, v.build()?)))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Self::Range(start, end) => Value::range(
                start.build()?,
                match end {
                    Some(v) => Some(v.build()?),
                    None => None,
                },
            ),
            Self::Error(code, message, value) => Value::error(
                Symbol::from_id(*code),
                message.clone(),
                match value {
                    Some(v) => Some(v.build()?),
                    None => None,
                },
            ),
            Self::Capability(v) => Value::capability(CapabilityId::new(*v)?),
            Self::Frob(id, v) => Value::frob(Identity::new(*id)?, v.build()?),
            Self::Function(v) => Value::function(FunctionId::new(*v)?),
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        let tag = match self {
            Self::Empty => 0,
            Self::Bool(_) => 1,
            Self::Int(_) => 2,
            Self::Float(_) => 3,
            Self::Identity(_) => 4,
            Self::Symbol(_) => 5,
            Self::ErrorCode(_) => 6,
            Self::String(_) => 7,
            Self::Bytes(_) => 8,
            Self::List(_) => 9,
            Self::Map(_) => 10,
            Self::Range(..) => 11,
            Self::Error(..) => 12,
            Self::Capability(_) => 13,
            Self::Frob(..) => 14,
            Self::Function(_) => 15,
        };
        out.push(tag);
        match self {
            Self::Empty => {}
            Self::Bool(v) => out.push(u8::from(*v)),
            Self::Int(v) => out.extend(v.to_le_bytes()),
            Self::Float(v) | Self::Symbol(v) | Self::ErrorCode(v) => out.extend(v.to_le_bytes()),
            Self::Identity(v) | Self::Capability(v) | Self::Function(v) => {
                out.extend(v.to_le_bytes())
            }
            Self::String(v) => bytes(out, v.as_bytes()),
            Self::Bytes(v) => bytes(out, v),
            Self::List(v) => {
                count(out, v.len());
                for value in v {
                    value.encode(out);
                }
            }
            Self::Map(v) => {
                count(out, v.len());
                for (k, v) in v {
                    k.encode(out);
                    v.encode(out);
                }
            }
            Self::Range(start, end) => {
                start.encode(out);
                out.push(u8::from(end.is_some()));
                if let Some(end) = end {
                    end.encode(out);
                }
            }
            Self::Error(code, message, value) => {
                out.extend(code.to_le_bytes());
                out.push(u8::from(message.is_some()));
                if let Some(message) = message {
                    bytes(out, message.as_bytes());
                }
                out.push(u8::from(value.is_some()));
                if let Some(value) = value {
                    value.encode(out);
                }
            }
            Self::Frob(id, value) => {
                out.extend(id.to_le_bytes());
                value.encode(out);
            }
        }
    }
}

fn count(out: &mut Vec<u8>, n: usize) {
    out.extend((n as u64).to_le_bytes());
}
fn bytes(out: &mut Vec<u8>, v: &[u8]) {
    count(out, v.len());
    out.extend(v);
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum Operation {
    Construct,
    Compare,
    LanguageCompare,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    MapGet,
    MapBuild,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Case {
    pub op: Operation,
    pub left: Input,
    pub right: Input,
}

pub struct Prepared {
    pub op: Operation,
    pub left: Option<Value>,
    pub right: Option<Value>,
}

pub enum Outcome {
    Value(Option<Value>),
    Order(Ordering),
}

impl Case {
    pub fn prepare(&self) -> Prepared {
        Prepared {
            op: self.op,
            left: self.left.build(),
            right: self.right.build(),
        }
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.op as u8);
        self.left.encode(out);
        self.right.encode(out);
    }
}

impl Prepared {
    pub fn run(&self) -> Outcome {
        let (Some(left), Some(right)) = (&self.left, &self.right) else {
            return Outcome::Value(None);
        };
        let value = match self.op {
            Operation::Construct => Some(left.clone()),
            Operation::Compare => return Outcome::Order(left.cmp(right)),
            Operation::LanguageCompare => {
                return Outcome::Order(language_cmp::numeric_cmp(left, right));
            }
            Operation::Add => left.checked_add(right),
            Operation::Subtract => left.checked_sub(right),
            Operation::Multiply => left.checked_mul(right),
            Operation::Divide => left.checked_div(right),
            Operation::Remainder => left.checked_rem(right),
            Operation::MapGet => left.map_get(right),
            Operation::MapBuild => left
                .with_list(|items| {
                    let (pairs, remainder) = items.as_chunks::<2>();
                    if !remainder.is_empty() {
                        return None;
                    }
                    Some(Value::map(
                        pairs.iter().map(|p| (p[0].clone(), p[1].clone())),
                    ))
                })
                .flatten(),
        };
        Outcome::Value(value)
    }
}

impl Outcome {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Order(order) => {
                out.push(2);
                out.extend((*order as i64).to_le_bytes());
            }
            Self::Value(None) => out.push(0),
            Self::Value(Some(v)) => {
                out.push(1);
                encode_value(v, out);
            }
        }
    }
    /// Cheap consumption of measured results; full semantic checking happens
    /// before timing. Heap addresses never enter cross-process checksums.
    pub fn checksum(&self) -> u64 {
        match self {
            Self::Order(order) => (*order as i64) as u64,
            Self::Value(None) => 0,
            Self::Value(Some(v)) => match v.as_value_ref() {
                ValueRef::Map(v) => 10 ^ v.len() as u64,
                ValueRef::List(v) => 9 ^ v.len() as u64,
                ValueRef::String(v) => 7 ^ v.len() as u64,
                ValueRef::Bytes(v) => 8 ^ v.len() as u64,
                ValueRef::Range { .. } => 11,
                ValueRef::Error { .. } => 12,
                ValueRef::Frob { .. } => 14,
                ValueRef::Relation(_) => 0,
                _ => v.raw_bits(),
            },
        }
    }
}

fn encode_value(value: &Value, out: &mut Vec<u8>) {
    match value.as_value_ref() {
        ValueRef::Relation(_) => Input::Empty.encode(out),
        ValueRef::Bool(v) => Input::Bool(v).encode(out),
        ValueRef::Int(v) => Input::Int(v).encode(out),
        ValueRef::Float(v) => Input::Float(v.to_bits()).encode(out),
        ValueRef::Identity(v) => Input::Identity(v.raw()).encode(out),
        ValueRef::Symbol(v) => Input::Symbol(v.id()).encode(out),
        ValueRef::ErrorCode(v) => Input::ErrorCode(v.id()).encode(out),
        ValueRef::Capability(v) => Input::Capability(v.raw()).encode(out),
        ValueRef::Function(v) => Input::Function(v.raw()).encode(out),
        ValueRef::String(v) => {
            out.push(7);
            bytes(out, v.as_bytes());
        }
        ValueRef::Bytes(v) => {
            out.push(8);
            bytes(out, v);
        }
        ValueRef::List(v) => {
            out.push(9);
            count(out, v.len());
            for x in v {
                encode_value(x, out);
            }
        }
        ValueRef::Map(v) => {
            out.push(10);
            count(out, v.len());
            for (k, v) in v {
                encode_value(k, out);
                encode_value(v, out);
            }
        }
        ValueRef::Range { start, end } => {
            out.push(11);
            encode_value(start, out);
            out.push(u8::from(end.is_some()));
            if let Some(v) = end {
                encode_value(v, out);
            }
        }
        ValueRef::Error {
            code,
            message,
            value,
        } => {
            out.push(12);
            out.extend(code.id().to_le_bytes());
            out.push(u8::from(message.is_some()));
            if let Some(v) = message {
                bytes(out, v.as_bytes());
            }
            out.push(u8::from(value.is_some()));
            if let Some(v) = value {
                encode_value(v, out);
            }
        }
        ValueRef::Frob { delegate, value } => {
            out.push(14);
            out.extend(delegate.raw().to_le_bytes());
            encode_value(value, out);
        }
    }
}

pub fn strategy() -> BoxedStrategy<Case> {
    let leaf = prop_oneof![
        Just(Input::Empty),
        any::<bool>().prop_map(Input::Bool),
        (-(1i64 << 55)..(1i64 << 55)).prop_map(Input::Int),
        any::<u32>()
            .prop_filter("finite float", |b| f32::from_bits(*b).is_finite())
            .prop_map(Input::Float),
        (0..(1u64 << 56)).prop_map(Input::Identity),
        (0..32u32).prop_map(Input::Symbol),
        (0..32u32).prop_map(Input::ErrorCode),
        (1..(1u64 << 56)).prop_map(Input::Capability),
        (0..(1u64 << 56)).prop_map(Input::Function),
        "[a-zé😀\\x00]{0,12}".prop_map(Input::String),
        prop::collection::vec(any::<u8>(), 0..16).prop_map(Input::Bytes),
    ];
    let value = leaf
        .prop_recursive(3, 48, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Input::List),
                prop::collection::vec((inner.clone(), inner.clone()), 0..6).prop_map(Input::Map),
                (inner.clone(), prop::option::of(inner.clone()))
                    .prop_map(|(a, b)| Input::Range(Box::new(a), b.map(Box::new))),
                (
                    0..32u32,
                    prop::option::of("[a-zé\\x00]{0,8}"),
                    prop::option::of(inner.clone())
                )
                    .prop_map(|(a, b, c)| Input::Error(a, b, c.map(Box::new))),
                (0..(1u64 << 56), inner).prop_map(|(a, b)| Input::Frob(a, Box::new(b))),
            ]
        })
        .boxed();
    let op = prop::sample::select(vec![
        Operation::Construct,
        Operation::Compare,
        Operation::LanguageCompare,
        Operation::Add,
        Operation::Subtract,
        Operation::Multiply,
        Operation::Divide,
        Operation::Remainder,
        Operation::MapGet,
    ]);
    let general =
        (op, value.clone(), value.clone()).prop_map(|(op, left, right)| Case { op, left, right });
    let equal = value.clone().prop_map(|value| Case {
        op: Operation::Compare,
        left: value.clone(),
        right: value,
    });
    let construct = value.clone().prop_map(|left| Case {
        op: Operation::Construct,
        left,
        right: Input::Empty,
    });
    let numeric = prop_oneof![
        (-(1i64 << 55)..(1i64 << 55)).prop_map(Input::Int),
        any::<u32>()
            .prop_filter("finite float", |b| f32::from_bits(*b).is_finite())
            .prop_map(Input::Float),
    ];
    let arithmetic = (
        prop::sample::select(vec![
            Operation::Add,
            Operation::Subtract,
            Operation::Multiply,
            Operation::Divide,
            Operation::Remainder,
            Operation::LanguageCompare,
        ]),
        numeric.clone(),
        numeric,
    )
        .prop_map(|(op, left, right)| Case { op, left, right });
    // A small key domain deliberately produces duplicates. Lookup alternates
    // between keys present in the input and keys outside that domain.
    let maps = (
        prop::collection::vec(((-8i64..8).prop_map(Input::Int), value), 0..16),
        any::<usize>(),
        any::<bool>(),
    )
        .prop_map(|(entries, index, hit)| {
            let key = if hit && !entries.is_empty() {
                entries[index % entries.len()].0.clone()
            } else {
                Input::Int(99)
            };
            Case {
                op: Operation::MapGet,
                left: Input::Map(entries),
                right: key,
            }
        });
    prop_oneof![general, equal, construct, arithmetic, maps].boxed()
}

pub fn fixed_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for input in [
        Input::Int(i64::MIN),
        Input::Int(i64::MAX),
        Input::Int(-(1 << 55)),
        Input::Int((1 << 55) - 1),
        Input::Float(f32::NAN.to_bits()),
        Input::Float(f32::INFINITY.to_bits()),
        Input::Float((-0.0f32).to_bits()),
        Input::Identity(1 << 56),
        Input::Capability(0),
        Input::Function(0),
    ] {
        cases.push(Case {
            op: Operation::Construct,
            left: input,
            right: Input::Empty,
        });
    }
    for a in [-(1i64 << 55), -16777217, -1, 0, 1, 16777217, (1 << 55) - 1] {
        for b in [
            -f32::MAX,
            -((1u64 << 55) as f32),
            -1.5,
            0.0,
            1.5,
            16777216.0,
            (1u64 << 55) as f32,
            f32::MAX,
        ] {
            cases.push(Case {
                op: Operation::LanguageCompare,
                left: Input::Int(a),
                right: Input::Float(b.to_bits()),
            });
        }
        for b in [-1, 0, 1, 2] {
            for op in [
                Operation::Add,
                Operation::Subtract,
                Operation::Multiply,
                Operation::Divide,
                Operation::Remainder,
            ] {
                cases.push(Case {
                    op,
                    left: Input::Int(a),
                    right: Input::Int(b),
                });
            }
        }
    }
    let nested_key = Input::List(vec![Input::Int(1), Input::String("é\0😀".into())]);
    let map = Input::Map(vec![
        (nested_key.clone(), Input::Int(1)),
        (Input::Float(1.0f32.to_bits()), Input::Int(2)),
        (Input::Int(1), Input::Int(3)),
        (nested_key.clone(), Input::Int(4)),
    ]);
    for key in [
        nested_key,
        Input::Int(1),
        Input::Float(1.0f32.to_bits()),
        Input::Empty,
    ] {
        cases.push(Case {
            op: Operation::MapGet,
            left: map.clone(),
            right: key,
        });
    }
    for input in [
        map,
        Input::Bytes(vec![0, 255, 128]),
        Input::Range(Box::new(Input::Int(-1)), Some(Box::new(Input::Int(3)))),
        Input::Error(31, Some("é\0😀".into()), Some(Box::new(Input::Bool(false)))),
        Input::Frob(42, Box::new(Input::List(vec![Input::Empty]))),
        Input::Symbol(u32::MAX),
        Input::ErrorCode(u32::MAX),
    ] {
        cases.push(Case {
            op: Operation::Construct,
            left: input,
            right: Input::Empty,
        });
    }
    for op in [
        Operation::Add,
        Operation::Subtract,
        Operation::Multiply,
        Operation::Divide,
        Operation::Remainder,
    ] {
        cases.push(Case {
            op,
            left: Input::Float(2.5f32.to_bits()),
            right: Input::Float(2.0f32.to_bits()),
        });
    }
    cases
}

pub fn workloads() -> Vec<(&'static str, Vec<Case>)> {
    let pairs: Vec<_> = (0..128)
        .rev()
        .map(|i| (Input::Int(i % 31), Input::Int(i)))
        .collect();
    let flattened = Input::List(
        pairs
            .iter()
            .flat_map(|(a, b)| [a.clone(), b.clone()])
            .collect(),
    );
    vec![
        (
            "integer_add",
            (0..64)
                .map(|i| Case {
                    op: Operation::Add,
                    left: Input::Int(i),
                    right: Input::Int(i + 1),
                })
                .collect(),
        ),
        (
            "mixed_numeric_compare",
            (0..64)
                .map(|i| Case {
                    op: Operation::LanguageCompare,
                    left: Input::Int(16777217 + i),
                    right: Input::Float((16777216.0 + i as f32).to_bits()),
                })
                .collect(),
        ),
        (
            "nested_compare",
            (0..64)
                .map(|i| Case {
                    op: Operation::Compare,
                    left: Input::List(vec![Input::String("shared prefix é".into()), Input::Int(i)]),
                    right: Input::List(vec![
                        Input::String("shared prefix é".into()),
                        Input::Int(i + 1),
                    ]),
                })
                .collect(),
        ),
        (
            "map_lookup",
            (0..64)
                .map(|i| Case {
                    op: Operation::MapGet,
                    left: Input::Map(pairs.clone()),
                    right: Input::Int(i),
                })
                .collect(),
        ),
        (
            "map_construct_and_release",
            vec![
                Case {
                    op: Operation::MapBuild,
                    left: flattened,
                    right: Input::Empty
                };
                16
            ],
        ),
    ]
}
