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

use crate::heap::HeapValue;
use crate::value::{
    TAG_BYTES, TAG_ERROR, TAG_FROB, TAG_LIST, TAG_MAP, TAG_RANGE, TAG_RELATION, TAG_STRING, Value,
    ValueKind,
};
use base64::{Engine, engine::general_purpose};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

pub trait OrderedKeySink {
    fn push_byte(&mut self, byte: u8);

    fn extend_from_slice(&mut self, bytes: &[u8]);
}

impl OrderedKeySink for Vec<u8> {
    fn push_byte(&mut self, byte: u8) {
        self.push(byte);
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes);
    }
}

impl Value {
    pub fn ordered_key_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_ordered(&mut out);
        out
    }

    pub fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_ordered_into(out);
    }

    pub fn encode_ordered_into(&self, out: &mut impl OrderedKeySink) {
        out.push_byte(self.kind() as u8);
        match self.kind() {
            ValueKind::Bool => out.push_byte(self.payload() as u8),
            ValueKind::Int => {
                let normalized = self.as_int().unwrap() ^ i64::MIN;
                out.extend_from_slice(&normalized.to_be_bytes());
            }
            ValueKind::Float => {
                let value = f32::from_bits(self.payload() as u32);
                out.extend_from_slice(&ordered_f32_bits(value).to_be_bytes());
            }
            ValueKind::Identity
            | ValueKind::Symbol
            | ValueKind::ErrorCode
            | ValueKind::Capability
            | ValueKind::Function => {
                out.extend_from_slice(&self.payload().to_be_bytes());
            }
            ValueKind::String => {
                let _ = self.with_str(|value| encode_bytes_terminated(value.as_bytes(), out));
            }
            ValueKind::Bytes => {
                let _ = self.with_bytes(|value| encode_bytes_terminated(value, out));
            }
            ValueKind::List => {
                let _ = self.with_list(|values| {
                    for value in values {
                        out.push_byte(1);
                        value.encode_ordered_into(out);
                    }
                    out.push_byte(0);
                });
            }
            ValueKind::Map => {
                let _ = self.with_map(|entries| {
                    for (key, value) in entries {
                        out.push_byte(1);
                        key.encode_ordered_into(out);
                        value.encode_ordered_into(out);
                    }
                    out.push_byte(0);
                });
            }
            ValueKind::Relation => {
                let _ = self.with_relation(|relation| {
                    for column in relation.heading() {
                        out.push_byte(1);
                        out.extend_from_slice(&(column.id() as u64).to_be_bytes());
                    }
                    out.push_byte(0);
                    for row in relation.rows() {
                        out.push_byte(1);
                        for value in row.values() {
                            value.encode_ordered_into(out);
                        }
                    }
                    out.push_byte(0);
                });
            }
            ValueKind::Range => {
                let _ = self.with_range(|start, end| {
                    start.encode_ordered_into(out);
                    match end {
                        Some(end) => {
                            out.push_byte(1);
                            end.encode_ordered_into(out);
                        }
                        None => out.push_byte(0),
                    }
                });
            }
            ValueKind::Error => {
                let _ = self.with_error(|error| {
                    out.extend_from_slice(&(error.code().id() as u64).to_be_bytes());
                    match error.message() {
                        Some(message) => {
                            out.push_byte(1);
                            encode_bytes_terminated(message.as_bytes(), out);
                        }
                        None => out.push_byte(0),
                    }
                    match error.value() {
                        Some(value) => {
                            out.push_byte(1);
                            value.encode_ordered_into(out);
                        }
                        None => out.push_byte(0),
                    }
                });
            }
            ValueKind::Frob => {
                let _ = self.with_frob(|delegate, value| {
                    out.extend_from_slice(&delegate.raw().to_be_bytes());
                    value.encode_ordered_into(out);
                });
            }
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self.kind(), other.kind()) {
            (ValueKind::Bool, ValueKind::Bool)
            | (ValueKind::Int, ValueKind::Int)
            | (ValueKind::Float, ValueKind::Float)
            | (ValueKind::Identity, ValueKind::Identity)
            | (ValueKind::Symbol, ValueKind::Symbol)
            | (ValueKind::ErrorCode, ValueKind::ErrorCode)
            | (ValueKind::Capability, ValueKind::Capability)
            | (ValueKind::Function, ValueKind::Function) => self.payload() == other.payload(),
            (ValueKind::String, ValueKind::String) => self
                .with_str(|left| other.with_str(|right| left == right).unwrap())
                .unwrap(),
            (ValueKind::Bytes, ValueKind::Bytes) => self
                .with_bytes(|left| other.with_bytes(|right| left == right).unwrap())
                .unwrap(),
            (ValueKind::List, ValueKind::List) => self
                .with_list(|left| other.with_list(|right| left == right).unwrap())
                .unwrap(),
            (ValueKind::Map, ValueKind::Map) => self
                .with_map(|left| other.with_map(|right| left == right).unwrap())
                .unwrap(),
            (ValueKind::Relation, ValueKind::Relation) => self
                .with_relation(|left| other.with_relation(|right| left == right).unwrap())
                .unwrap(),
            (ValueKind::Range, ValueKind::Range) => self
                .with_range(|left_start, left_end| {
                    other
                        .with_range(|right_start, right_end| {
                            left_start == right_start && left_end == right_end
                        })
                        .unwrap()
                })
                .unwrap(),
            (ValueKind::Error, ValueKind::Error) => self
                .with_error(|left| other.with_error(|right| left == right).unwrap())
                .unwrap(),
            (ValueKind::Frob, ValueKind::Frob) => self
                .with_frob(|left_delegate, left_value| {
                    other
                        .with_frob(|right_delegate, right_value| {
                            left_delegate == right_delegate && left_value == right_value
                        })
                        .unwrap()
                })
                .unwrap(),
            _ => false,
        }
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        let left_kind = self.kind();
        let right_kind = other.kind();
        if left_kind != right_kind {
            return left_kind.cmp(&right_kind);
        }

        match left_kind {
            ValueKind::Bool => self.as_bool().cmp(&other.as_bool()),
            ValueKind::Int => self.as_int().cmp(&other.as_int()),
            ValueKind::Float => {
                let left = f32::from_bits(self.payload() as u32);
                let right = f32::from_bits(other.payload() as u32);
                left.total_cmp(&right)
            }
            ValueKind::Identity
            | ValueKind::Symbol
            | ValueKind::ErrorCode
            | ValueKind::Capability
            | ValueKind::Function => self.payload().cmp(&other.payload()),
            ValueKind::String => self
                .with_str(|left| other.with_str(|right| left.cmp(right)).unwrap())
                .unwrap(),
            ValueKind::Bytes => self
                .with_bytes(|left| other.with_bytes(|right| left.cmp(right)).unwrap())
                .unwrap(),
            ValueKind::List => self
                .with_list(|left| other.with_list(|right| left.cmp(right)).unwrap())
                .unwrap(),
            ValueKind::Map => self
                .with_map(|left| other.with_map(|right| left.cmp(right)).unwrap())
                .unwrap(),
            ValueKind::Relation => self
                .with_relation(|left| other.with_relation(|right| left.cmp(right)).unwrap())
                .unwrap(),
            ValueKind::Range => self
                .with_range(|left_start, left_end| {
                    other
                        .with_range(|right_start, right_end| {
                            left_start
                                .cmp(right_start)
                                .then_with(|| left_end.cmp(&right_end))
                        })
                        .unwrap()
                })
                .unwrap(),
            ValueKind::Error => self
                .with_error(|left| other.with_error(|right| left.cmp(right)).unwrap())
                .unwrap(),
            ValueKind::Frob => self
                .with_frob(|left_delegate, left_value| {
                    other
                        .with_frob(|right_delegate, right_value| {
                            left_delegate
                                .cmp(&right_delegate)
                                .then_with(|| left_value.cmp(right_value))
                        })
                        .unwrap()
                })
                .unwrap(),
        }
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind().hash(state);
        match self.kind() {
            ValueKind::Bool
            | ValueKind::Int
            | ValueKind::Float
            | ValueKind::Identity
            | ValueKind::Symbol
            | ValueKind::ErrorCode
            | ValueKind::Capability
            | ValueKind::Function => {
                self.payload().hash(state);
            }
            ValueKind::String => {
                let _ = self.with_str(|value| value.hash(state));
            }
            ValueKind::Bytes => {
                let _ = self.with_bytes(|value| value.hash(state));
            }
            ValueKind::List => {
                let _ = self.with_list(|values| values.hash(state));
            }
            ValueKind::Map => {
                let _ = self.with_map(|entries| entries.hash(state));
            }
            ValueKind::Relation => {
                let _ = self.with_relation(|relation| relation.hash(state));
            }
            ValueKind::Range => {
                let _ = self.with_range(|start, end| {
                    start.hash(state);
                    end.hash(state);
                });
            }
            ValueKind::Error => {
                let _ = self.with_error(|error| error.hash(state));
            }
            ValueKind::Frob => {
                let _ = self.with_frob(|delegate, value| {
                    delegate.hash(state);
                    value.hash(state);
                });
            }
        };
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            ValueKind::Bool => write!(f, "{:?}", self.as_bool().unwrap()),
            ValueKind::Int => write!(f, "{:?}", self.as_int().unwrap()),
            ValueKind::Float => write!(f, "{:?}", self.as_float().unwrap()),
            ValueKind::Identity => write!(f, "#{}", self.as_identity().unwrap().raw()),
            ValueKind::Capability => f.write_str("<cap>"),
            ValueKind::Function => f.write_str("<function>"),
            ValueKind::Symbol => match self.as_symbol().unwrap().name() {
                Some(name) => write!(f, ":{name}"),
                None => write!(f, ":#{}", self.as_symbol().unwrap().id()),
            },
            ValueKind::ErrorCode => match self.as_error_code().unwrap().name() {
                Some(name) => f.write_str(name),
                None => write!(f, "E_#{}", self.as_error_code().unwrap().id()),
            },
            ValueKind::String => self.with_str(|value| write!(f, "{value:?}")).unwrap(),
            ValueKind::Bytes => self
                .with_bytes(|value| write!(f, "b\"{}\"", general_purpose::URL_SAFE.encode(value)))
                .unwrap(),
            ValueKind::List => self
                .with_list(|values| f.debug_list().entries(values).finish())
                .unwrap(),
            ValueKind::Map => self
                .with_map(|entries| {
                    let mut map = f.debug_map();
                    for (key, value) in entries {
                        map.entry(key, value);
                    }
                    map.finish()
                })
                .unwrap(),
            ValueKind::Relation if self.is_unit() => f.write_str("()"),
            ValueKind::Relation if self.is_empty_relation() => f.write_str("[] {}"),
            ValueKind::Relation => self
                .with_relation(|relation| {
                    f.debug_struct("Relation")
                        .field("heading", &relation.heading())
                        .field("rows", &relation.rows())
                        .finish()
                })
                .unwrap(),
            ValueKind::Range => self
                .with_range(|start, end| write_range(start, end, f))
                .unwrap(),
            ValueKind::Error => self
                .with_error(|error| write_error_value(error, f))
                .unwrap(),
            ValueKind::Frob => self
                .with_frob(|delegate, value| write!(f, "#{}<{value:?}>", delegate.raw()))
                .unwrap(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            ValueKind::Bool => write!(f, "{}", self.as_bool().unwrap()),
            ValueKind::Int => write!(f, "{}", self.as_int().unwrap()),
            ValueKind::Float => write!(f, "{}", self.as_float().unwrap()),
            ValueKind::Identity => write!(f, "#{}", self.as_identity().unwrap().raw()),
            ValueKind::Capability => f.write_str("<cap>"),
            ValueKind::Function => f.write_str("<function>"),
            ValueKind::Symbol => match self.as_symbol().unwrap().name() {
                Some(name) => write!(f, ":{name}"),
                None => write!(f, ":#{}", self.as_symbol().unwrap().id()),
            },
            ValueKind::ErrorCode => match self.as_error_code().unwrap().name() {
                Some(name) => f.write_str(name),
                None => write!(f, "E_#{}", self.as_error_code().unwrap().id()),
            },
            ValueKind::String => self.with_str(|value| f.write_str(value)).unwrap(),
            ValueKind::Bytes => self
                .with_bytes(|value| write!(f, "b\"{}\"", general_purpose::URL_SAFE.encode(value)))
                .unwrap(),
            ValueKind::List => self
                .with_list(|values| {
                    f.write_str("{")?;
                    for (index, value) in values.iter().enumerate() {
                        if index != 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{value}")?;
                    }
                    f.write_str("}")
                })
                .unwrap(),
            ValueKind::Map => self
                .with_map(|entries| {
                    f.write_str("[")?;
                    for (index, (key, value)) in entries.iter().enumerate() {
                        if index != 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{key}: {value}")?;
                    }
                    f.write_str("]")
                })
                .unwrap(),
            ValueKind::Relation if self.is_unit() => f.write_str("()"),
            ValueKind::Relation if self.is_empty_relation() => f.write_str("[] {}"),
            ValueKind::Relation => self
                .with_relation(|relation| {
                    write!(f, "<relation {}x{}>", relation.len(), relation.arity())
                })
                .unwrap(),
            ValueKind::Range => self
                .with_range(|start, end| write_range(start, end, f))
                .unwrap(),
            ValueKind::Error => self
                .with_error(|error| write_error_value(error, f))
                .unwrap(),
            ValueKind::Frob => self
                .with_frob(|delegate, value| write!(f, "#{}<{value}>", delegate.raw()))
                .unwrap(),
        }
    }
}

#[inline(always)]
fn ordered_f32_bits(value: f32) -> u32 {
    let bits = value.to_bits();
    if (bits & 0x8000_0000) != 0 {
        !bits
    } else {
        bits ^ 0x8000_0000
    }
}

fn write_range(start: &Value, end: Option<&Value>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{start}..")?;
    match end {
        Some(end) => write!(f, "{end}"),
        None => f.write_str("_"),
    }
}

fn write_error_value(error: &crate::ErrorValue, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("error(")?;
    match error.code().name() {
        Some(name) => f.write_str(name)?,
        None => write!(f, "E_#{}", error.code().id())?,
    }
    if let Some(message) = error.message() {
        write!(f, ", {message:?}")?;
    }
    if let Some(value) = error.value() {
        if error.message().is_none() {
            f.write_str(", none")?;
        }
        write!(f, ", {value:?}")?;
    }
    f.write_str(")")
}

fn encode_bytes_terminated(bytes: &[u8], out: &mut impl OrderedKeySink) {
    for byte in bytes {
        if *byte == 0 {
            out.extend_from_slice(&[0, 0xff]);
        } else {
            out.push_byte(*byte);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

const _: () = {
    fn _heap_value_is_used(heap: &HeapValue) -> u8 {
        heap.tag()
    }
    let _ = _heap_value_is_used;
    let _ = (
        TAG_STRING,
        TAG_BYTES,
        TAG_LIST,
        TAG_MAP,
        TAG_RANGE,
        TAG_ERROR,
        TAG_FROB,
        TAG_RELATION,
    );
};
