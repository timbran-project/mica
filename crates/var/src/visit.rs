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
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::RelationValue;
use crate::heap::HeapValue;
use crate::symbol::Symbol;
use crate::value::{CapabilityId, ErrorValue, FunctionId, Identity, Value, ValueKind};

/// A borrowed, structured view of a `Value`.
///
/// This is the low-copy boundary for code that needs to inspect or encode
/// values without first constructing a parallel DTO tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueRef<'a> {
    Bool(bool),
    Int(i64),
    Float(f32),
    Identity(Identity),
    Symbol(Symbol),
    ErrorCode(Symbol),
    String(&'a str),
    Bytes(&'a [u8]),
    List(&'a [Value]),
    Map(&'a [(Value, Value)]),
    Relation(&'a RelationValue),
    Range {
        start: &'a Value,
        end: Option<&'a Value>,
    },
    Error {
        code: Symbol,
        message: Option<&'a str>,
        value: Option<&'a Value>,
    },
    Capability(CapabilityId),
    Function(FunctionId),
    Frob {
        delegate: Identity,
        value: &'a Value,
    },
}

impl ValueRef<'_> {
    #[inline(always)]
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::Int(_) => ValueKind::Int,
            Self::Float(_) => ValueKind::Float,
            Self::Identity(_) => ValueKind::Identity,
            Self::Symbol(_) => ValueKind::Symbol,
            Self::ErrorCode(_) => ValueKind::ErrorCode,
            Self::String(_) => ValueKind::String,
            Self::Bytes(_) => ValueKind::Bytes,
            Self::List(_) => ValueKind::List,
            Self::Map(_) => ValueKind::Map,
            Self::Relation(_) => ValueKind::Relation,
            Self::Range { .. } => ValueKind::Range,
            Self::Error { .. } => ValueKind::Error,
            Self::Capability(_) => ValueKind::Capability,
            Self::Function(_) => ValueKind::Function,
            Self::Frob { .. } => ValueKind::Frob,
        }
    }

    #[inline(always)]
    pub const fn is_immediate(&self) -> bool {
        matches!(
            self,
            Self::Bool(_)
                | Self::Int(_)
                | Self::Float(_)
                | Self::Identity(_)
                | Self::Symbol(_)
                | Self::ErrorCode(_)
                | Self::Capability(_)
                | Self::Function(_)
        )
    }

    #[inline(always)]
    pub const fn is_heap(&self) -> bool {
        !self.is_immediate()
    }

    #[inline(always)]
    pub fn child_count(&self) -> usize {
        match self {
            Self::List(values) => values.len(),
            Self::Map(entries) => entries.len() * 2,
            Self::Relation(relation) => relation.rows().iter().map(|row| row.arity()).sum(),
            Self::Range { end, .. } => 1 + usize::from(end.is_some()),
            Self::Error { value, .. } => usize::from(value.is_some()),
            Self::Frob { .. } => 1,
            _ => 0,
        }
    }
}

/// Controls whether `Value::walk` descends into a value's children.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisitDecision {
    Descend,
    SkipChildren,
}

impl VisitDecision {
    #[inline(always)]
    fn descends(self) -> bool {
        matches!(self, Self::Descend)
    }
}

/// Visitor for depth-first traversal of a value tree.
///
/// `visit_value` is called before children. `leave_value` is called after
/// children, or immediately if the visitor skips them.
pub trait ValueVisitor {
    type Error;

    fn visit_value(
        &mut self,
        value: &Value,
        value_ref: ValueRef<'_>,
    ) -> Result<VisitDecision, Self::Error>;

    fn leave_value(&mut self, _value: &Value, _value_ref: ValueRef<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Value {
    /// Returns a borrowed structured view of this value.
    #[inline(always)]
    pub fn as_value_ref(&self) -> ValueRef<'_> {
        match self.kind() {
            ValueKind::Bool => ValueRef::Bool(self.as_bool().unwrap()),
            ValueKind::Int => ValueRef::Int(self.as_int().unwrap()),
            ValueKind::Float => ValueRef::Float(self.as_float().unwrap()),
            ValueKind::Identity => ValueRef::Identity(self.as_identity().unwrap()),
            ValueKind::Symbol => ValueRef::Symbol(self.as_symbol().unwrap()),
            ValueKind::ErrorCode => ValueRef::ErrorCode(self.as_error_code().unwrap()),
            ValueKind::Capability => ValueRef::Capability(self.as_capability().unwrap()),
            ValueKind::Function => ValueRef::Function(self.as_function().unwrap()),
            ValueKind::Relation => ValueRef::Relation(self.relation_ref().unwrap()),
            ValueKind::String
            | ValueKind::Bytes
            | ValueKind::List
            | ValueKind::Map
            | ValueKind::Range
            | ValueKind::Error
            | ValueKind::Frob => self.heap_value_ref(),
        }
    }

    /// Walks this value and all nested values in depth-first order.
    pub fn walk<V: ValueVisitor>(&self, visitor: &mut V) -> Result<(), V::Error> {
        walk_value(self, visitor)
    }

    #[inline(always)]
    fn heap_value_ref(&self) -> ValueRef<'_> {
        match self.heap_ref().unwrap() {
            HeapValue::String(value) => ValueRef::String(value.as_str()),
            HeapValue::Bytes(value) => ValueRef::Bytes(value),
            HeapValue::List(values) => ValueRef::List(values.as_slice()),
            HeapValue::Map(entries) => ValueRef::Map(entries),
            HeapValue::Relation(relation) => ValueRef::Relation(relation),
            HeapValue::Range { start, end } => ValueRef::Range {
                start,
                end: end.as_ref(),
            },
            HeapValue::Error(error) => error.as_value_ref(),
            HeapValue::Frob(frob) => ValueRef::Frob {
                delegate: frob.delegate(),
                value: frob.value(),
            },
        }
    }
}

impl ErrorValue {
    #[inline(always)]
    pub fn as_value_ref(&self) -> ValueRef<'_> {
        ValueRef::Error {
            code: self.code(),
            message: self.message(),
            value: self.value(),
        }
    }
}

fn walk_value<V: ValueVisitor>(value: &Value, visitor: &mut V) -> Result<(), V::Error> {
    let value_ref = value.as_value_ref();
    let decision = visitor.visit_value(value, value_ref)?;
    if decision.descends() {
        walk_children(value_ref, visitor)?;
    }
    visitor.leave_value(value, value.as_value_ref())
}

fn walk_children<V: ValueVisitor>(
    value_ref: ValueRef<'_>,
    visitor: &mut V,
) -> Result<(), V::Error> {
    match value_ref {
        ValueRef::List(values) => {
            for value in values {
                walk_value(value, visitor)?;
            }
        }
        ValueRef::Map(entries) => {
            for (key, value) in entries {
                walk_value(key, visitor)?;
                walk_value(value, visitor)?;
            }
        }
        ValueRef::Relation(relation) => {
            for row in relation.rows() {
                for value in row.values() {
                    walk_value(value, visitor)?;
                }
            }
        }
        ValueRef::Range { start, end } => {
            walk_value(start, visitor)?;
            if let Some(end) = end {
                walk_value(end, visitor)?;
            }
        }
        ValueRef::Error {
            value: Some(value), ..
        } => {
            walk_value(value, visitor)?;
        }
        ValueRef::Frob { value, .. } => {
            walk_value(value, visitor)?;
        }
        _ => {}
    }
    Ok(())
}
