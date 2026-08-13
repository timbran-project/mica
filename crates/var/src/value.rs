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
use crate::symbol::Symbol;
use crate::tuple::empty_relation;
use crate::{RelationValue, RelationValueError, Tuple};
use std::sync::Arc;

pub(crate) const TAG_SHIFT: u64 = 56;
pub(crate) const PAYLOAD_MASK: u64 = 0x00ff_ffff_ffff_ffff;

pub(crate) const TAG_EMPTY_RELATION: u8 = 0;
pub(crate) const TAG_BOOL: u8 = 1;
pub(crate) const TAG_INT: u8 = 2;
pub(crate) const TAG_FLOAT: u8 = 3;
pub(crate) const TAG_IDENTITY: u8 = 4;
pub(crate) const TAG_SYMBOL: u8 = 5;
pub(crate) const TAG_ERROR_CODE: u8 = 6;
pub(crate) const TAG_STRING: u8 = 7;
pub(crate) const TAG_BYTES: u8 = 8;
pub(crate) const TAG_LIST: u8 = 9;
pub(crate) const TAG_MAP: u8 = 10;
pub(crate) const TAG_RANGE: u8 = 11;
pub(crate) const TAG_ERROR: u8 = 12;
pub(crate) const TAG_CAPABILITY: u8 = 13;
pub(crate) const TAG_FROB: u8 = 14;
pub(crate) const TAG_FUNCTION: u8 = 15;
pub(crate) const TAG_RELATION: u8 = 16;

pub(crate) const INT_BITS: u32 = 56;
pub(crate) const INT_MIN: i64 = -(1i64 << (INT_BITS - 1));
pub(crate) const INT_MAX: i64 = (1i64 << (INT_BITS - 1)) - 1;
pub(crate) const MAX_PAYLOAD: u64 = PAYLOAD_MASK;

/// The process-local `Value` ABI version. Incremented when the physical layout
/// or invariants of `Value` change so that native code generators and external
/// processes can detect compatibility.
///
/// Version 3: the immediate zero word denotes the zero-column empty relation.
pub const VALUE_ABI_VERSION: u32 = 3;

/// A compact Mica value.
///
/// The layout is private. Use constructors and accessors rather than relying on
/// raw bits. The current representation is a pragmatic tagged word, not a final
/// commitment to this exact bit layout.
#[repr(transparent)]
pub struct Value(pub(crate) u64);

const _: () = assert!(std::mem::size_of::<Value>() == 8);
const _: () = assert!(std::mem::align_of::<Value>() == 8);

/// Stable entity identity payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Identity(u64);

impl Identity {
    pub const MAX: u64 = MAX_PAYLOAD;

    pub const fn new(raw: u64) -> Option<Self> {
        if raw <= Self::MAX {
            Some(Self(raw))
        } else {
            None
        }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Ephemeral authority designation payload.
///
/// Capability ids are ordinary values inside a running VM, but they are not
/// durable world data. They must not be accepted from source text or persisted
/// in relation tuples.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CapabilityId(u64);

impl CapabilityId {
    pub const MAX: u64 = MAX_PAYLOAD;

    pub const fn new(raw: u64) -> Option<Self> {
        if raw <= Self::MAX && raw != 0 {
            Some(Self(raw))
        } else {
            None
        }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Ephemeral VM-local function designation payload.
///
/// Function ids name callable programs inside one running VM. They are not
/// durable world data and must not be persisted in relation tuples.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FunctionId(u64);

impl FunctionId {
    pub const MAX: u64 = MAX_PAYLOAD;

    pub const fn new(raw: u64) -> Option<Self> {
        if raw <= Self::MAX {
            Some(Self(raw))
        } else {
            None
        }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ErrorValue {
    code: Symbol,
    message: Option<Box<str>>,
    value: Option<Value>,
}

impl ErrorValue {
    pub fn new(code: Symbol, message: Option<impl Into<Box<str>>>, value: Option<Value>) -> Self {
        Self {
            code,
            message: message.map(Into::into),
            value,
        }
    }

    pub const fn code(&self) -> Symbol {
        self.code
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FrobValue {
    delegate: Identity,
    value: Value,
}

impl FrobValue {
    pub fn new(delegate: Identity, value: Value) -> Self {
        Self { delegate, value }
    }

    pub const fn delegate(&self) -> Identity {
        self.delegate
    }

    pub const fn value(&self) -> &Value {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum ValueKind {
    Bool = TAG_BOOL,
    Int = TAG_INT,
    Float = TAG_FLOAT,
    Identity = TAG_IDENTITY,
    Symbol = TAG_SYMBOL,
    ErrorCode = TAG_ERROR_CODE,
    String = TAG_STRING,
    Bytes = TAG_BYTES,
    List = TAG_LIST,
    Map = TAG_MAP,
    Range = TAG_RANGE,
    Error = TAG_ERROR,
    Capability = TAG_CAPABILITY,
    Frob = TAG_FROB,
    Function = TAG_FUNCTION,
    Relation = TAG_RELATION,
}

impl ValueKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Identity => "identity",
            Self::Symbol => "symbol",
            Self::ErrorCode => "error_code",
            Self::String => "string",
            Self::Bytes => "bytes",
            Self::List => "list",
            Self::Map => "map",
            Self::Range => "range",
            Self::Error => "error",
            Self::Capability => "capability",
            Self::Frob => "frob",
            Self::Function => "function",
            Self::Relation => "relation",
        }
    }
}

pub const BOOL_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0002);
pub const INTEGER_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0003);
pub const FLOAT_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0004);
pub const IDENTITY_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0005);
pub const SYMBOL_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0006);
pub const ERROR_CODE_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0007);
pub const STRING_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0008);
pub const BYTES_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0009);
pub const LIST_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_000a);
pub const MAP_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_000b);
pub const RANGE_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_000c);
pub const ERROR_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_000d);
pub const CAPABILITY_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_000e);
pub const FROB_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_000f);
pub const FUNCTION_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0010);
pub const RELATION_PROTOTYPE: Identity = primitive_identity(0x00c0_0000_0000_0011);

pub const PRIMITIVE_PROTOTYPES: &[(&str, Identity)] = &[
    ("bool", BOOL_PROTOTYPE),
    ("integer", INTEGER_PROTOTYPE),
    ("float", FLOAT_PROTOTYPE),
    ("identity", IDENTITY_PROTOTYPE),
    ("symbol", SYMBOL_PROTOTYPE),
    ("error_code", ERROR_CODE_PROTOTYPE),
    ("string", STRING_PROTOTYPE),
    ("bytes", BYTES_PROTOTYPE),
    ("list", LIST_PROTOTYPE),
    ("map", MAP_PROTOTYPE),
    ("range", RANGE_PROTOTYPE),
    ("error", ERROR_PROTOTYPE),
    ("capability", CAPABILITY_PROTOTYPE),
    ("frob", FROB_PROTOTYPE),
    ("function", FUNCTION_PROTOTYPE),
    ("relation", RELATION_PROTOTYPE),
];

const fn primitive_identity(raw: u64) -> Identity {
    match Identity::new(raw) {
        Some(identity) => identity,
        None => panic!("primitive prototype identity out of range"),
    }
}

pub const fn primitive_prototype_for_kind(kind: ValueKind) -> Identity {
    match kind {
        ValueKind::Bool => BOOL_PROTOTYPE,
        ValueKind::Int => INTEGER_PROTOTYPE,
        ValueKind::Float => FLOAT_PROTOTYPE,
        ValueKind::Identity => IDENTITY_PROTOTYPE,
        ValueKind::Symbol => SYMBOL_PROTOTYPE,
        ValueKind::ErrorCode => ERROR_CODE_PROTOTYPE,
        ValueKind::String => STRING_PROTOTYPE,
        ValueKind::Bytes => BYTES_PROTOTYPE,
        ValueKind::List => LIST_PROTOTYPE,
        ValueKind::Map => MAP_PROTOTYPE,
        ValueKind::Range => RANGE_PROTOTYPE,
        ValueKind::Error => ERROR_PROTOTYPE,
        ValueKind::Capability => CAPABILITY_PROTOTYPE,
        ValueKind::Frob => FROB_PROTOTYPE,
        ValueKind::Function => FUNCTION_PROTOTYPE,
        ValueKind::Relation => RELATION_PROTOTYPE,
    }
}

pub const fn primitive_prototype_for_value(value: &Value) -> Identity {
    primitive_prototype_for_kind(value.kind())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ValueError {
    IntegerOutOfRange(i64),
    IdentityOutOfRange(u64),
    CapabilityOutOfRange(u64),
    FunctionOutOfRange(u64),
    HeapPointerOutOfRange(usize),
    FloatNotFinite(u32),
}

impl Value {
    #[inline(always)]
    pub(crate) const fn pack(tag: u8, payload: u64) -> Self {
        Self(((tag as u64) << TAG_SHIFT) | (payload & PAYLOAD_MASK))
    }

    #[inline(always)]
    pub const fn nothing() -> Self {
        Self::pack(TAG_EMPTY_RELATION, 0)
    }

    pub fn unit() -> Self {
        Self::relation([], [Tuple::new([])]).expect("the unit relation shape is valid")
    }

    #[inline(always)]
    pub const fn is_empty_relation(&self) -> bool {
        self.0 == 0
    }

    pub fn is_unit(&self) -> bool {
        self.with_relation(|relation| relation.arity() == 0 && relation.len() == 1)
            .unwrap_or(false)
    }

    #[inline(always)]
    pub const fn bool(value: bool) -> Self {
        Self::pack(TAG_BOOL, value as u64)
    }

    #[inline(always)]
    pub const fn int(value: i64) -> Result<Self, ValueError> {
        if value < INT_MIN || value > INT_MAX {
            return Err(ValueError::IntegerOutOfRange(value));
        }
        Ok(Self::pack(TAG_INT, value as u64))
    }

    #[inline(always)]
    pub fn float(value: f32) -> Result<Self, ValueError> {
        if value.is_nan() || value.is_infinite() {
            return Err(ValueError::FloatNotFinite(value.to_bits()));
        }
        let value = if value == 0.0 { 0.0 } else { value };
        Ok(Self::pack(TAG_FLOAT, value.to_bits() as u64))
    }

    /// Checked constructor from raw binary32 bits.
    ///
    /// Used by codecs and execution backends that already hold validated
    /// finite binary32 payloads but need to reconstruct a `Value` without
    /// going through the floating-point constructor.
    #[inline(always)]
    pub(crate) const fn float_from_bits(bits: u32) -> Result<Self, ValueError> {
        let value = f32::from_bits(bits);
        if value.is_nan() || value.is_infinite() {
            return Err(ValueError::FloatNotFinite(bits));
        }
        let bits = if value == 0.0 { 0u32 } else { bits };
        Ok(Self::pack(TAG_FLOAT, bits as u64))
    }

    #[inline(always)]
    pub const fn identity(identity: Identity) -> Self {
        Self::pack(TAG_IDENTITY, identity.raw())
    }

    #[inline(always)]
    pub const fn identity_raw(raw: u64) -> Result<Self, ValueError> {
        match Identity::new(raw) {
            Some(identity) => Ok(Self::identity(identity)),
            None => Err(ValueError::IdentityOutOfRange(raw)),
        }
    }

    #[inline(always)]
    pub const fn capability(capability: CapabilityId) -> Self {
        Self::pack(TAG_CAPABILITY, capability.raw())
    }

    #[inline(always)]
    pub const fn capability_raw(raw: u64) -> Result<Self, ValueError> {
        match CapabilityId::new(raw) {
            Some(capability) => Ok(Self::capability(capability)),
            None => Err(ValueError::CapabilityOutOfRange(raw)),
        }
    }

    #[inline(always)]
    pub const fn function(function: FunctionId) -> Self {
        Self::pack(TAG_FUNCTION, function.raw())
    }

    #[inline(always)]
    pub const fn function_raw(raw: u64) -> Result<Self, ValueError> {
        match FunctionId::new(raw) {
            Some(function) => Ok(Self::function(function)),
            None => Err(ValueError::FunctionOutOfRange(raw)),
        }
    }

    #[inline(always)]
    pub const fn symbol(symbol: Symbol) -> Self {
        Self::pack(TAG_SYMBOL, symbol.id() as u64)
    }

    #[inline(always)]
    pub const fn error_code(symbol: Symbol) -> Self {
        Self::pack(TAG_ERROR_CODE, symbol.id() as u64)
    }

    pub fn error(code: Symbol, message: Option<impl Into<Box<str>>>, value: Option<Value>) -> Self {
        Self::heap(HeapValue::Error(ErrorValue::new(code, message, value)))
    }

    pub fn frob(delegate: Identity, value: Value) -> Self {
        Self::heap(HeapValue::Frob(FrobValue::new(delegate, value)))
    }

    pub fn string(value: impl AsRef<str>) -> Self {
        Self::heap(HeapValue::String(value.as_ref().into()))
    }

    pub fn bytes(value: impl AsRef<[u8]>) -> Self {
        Self::heap(HeapValue::Bytes(value.as_ref().into()))
    }

    pub fn list(values: impl IntoIterator<Item = Value>) -> Self {
        Self::heap(HeapValue::List(
            values.into_iter().collect::<Vec<_>>().into_boxed_slice(),
        ))
    }

    pub fn map(entries: impl IntoIterator<Item = (Value, Value)>) -> Self {
        let mut entries = entries.into_iter().collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        let mut canonical = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            if let Some((last_key, last_value)) = canonical.last_mut()
                && last_key == &key
            {
                *last_value = value;
                continue;
            }
            canonical.push((key, value));
        }
        Self::heap(HeapValue::Map(canonical.into_boxed_slice()))
    }

    pub fn relation(
        heading: impl IntoIterator<Item = Symbol>,
        rows: impl IntoIterator<Item = Tuple>,
    ) -> Result<Self, RelationValueError> {
        RelationValue::new(heading, rows).map(Self::from)
    }

    pub fn range(start: Value, end: Option<Value>) -> Self {
        Self::heap(HeapValue::Range { start, end })
    }

    #[inline(always)]
    pub const fn raw_bits(&self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub const fn kind(&self) -> ValueKind {
        match self.tag() {
            TAG_EMPTY_RELATION => ValueKind::Relation,
            TAG_BOOL => ValueKind::Bool,
            TAG_INT => ValueKind::Int,
            TAG_FLOAT => ValueKind::Float,
            TAG_IDENTITY => ValueKind::Identity,
            TAG_SYMBOL => ValueKind::Symbol,
            TAG_ERROR_CODE => ValueKind::ErrorCode,
            TAG_STRING => ValueKind::String,
            TAG_BYTES => ValueKind::Bytes,
            TAG_LIST => ValueKind::List,
            TAG_MAP => ValueKind::Map,
            TAG_RANGE => ValueKind::Range,
            TAG_ERROR => ValueKind::Error,
            TAG_CAPABILITY => ValueKind::Capability,
            TAG_FROB => ValueKind::Frob,
            TAG_FUNCTION => ValueKind::Function,
            TAG_RELATION => ValueKind::Relation,
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    pub const fn is_immediate(&self) -> bool {
        !matches!(
            self.tag(),
            TAG_STRING
                | TAG_BYTES
                | TAG_LIST
                | TAG_MAP
                | TAG_RANGE
                | TAG_ERROR
                | TAG_FROB
                | TAG_RELATION
        )
    }

    #[inline(always)]
    pub const fn as_bool(&self) -> Option<bool> {
        if self.tag() == TAG_BOOL {
            Some(self.payload() != 0)
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn as_int(&self) -> Option<i64> {
        if self.tag() == TAG_INT {
            let shifted = ((self.payload() << 8) as i64) >> 8;
            Some(shifted)
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn as_float(&self) -> Option<f32> {
        if self.tag() == TAG_FLOAT {
            Some(f32::from_bits(self.payload() as u32))
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn as_identity(&self) -> Option<Identity> {
        if self.tag() == TAG_IDENTITY {
            Some(Identity(self.payload()))
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn as_capability(&self) -> Option<CapabilityId> {
        if self.tag() == TAG_CAPABILITY {
            CapabilityId::new(self.payload())
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn as_function(&self) -> Option<FunctionId> {
        if self.tag() == TAG_FUNCTION {
            FunctionId::new(self.payload())
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn as_symbol(&self) -> Option<Symbol> {
        if self.tag() == TAG_SYMBOL {
            Some(Symbol(self.payload() as u32))
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn as_error_code(&self) -> Option<Symbol> {
        if self.tag() == TAG_ERROR_CODE {
            Some(Symbol(self.payload() as u32))
        } else {
            None
        }
    }

    pub fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::String(value) => Some(f(value)),
            _ => None,
        })?
    }

    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::Bytes(value) => Some(f(value)),
            _ => None,
        })?
    }

    pub fn with_list<R>(&self, f: impl FnOnce(&[Value]) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::List(values) => Some(f(values)),
            _ => None,
        })?
    }

    pub fn with_map<R>(&self, f: impl FnOnce(&[(Value, Value)]) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::Map(entries) => Some(f(entries)),
            _ => None,
        })?
    }

    pub fn with_relation<R>(&self, f: impl FnOnce(&RelationValue) -> R) -> Option<R> {
        self.relation_ref().map(f)
    }

    pub(crate) fn relation_ref(&self) -> Option<&RelationValue> {
        if self.is_empty_relation() {
            return Some(empty_relation());
        }
        match self.heap_ref()? {
            HeapValue::Relation(relation) => Some(relation),
            _ => None,
        }
    }

    pub fn with_range<R>(&self, f: impl FnOnce(&Value, Option<&Value>) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::Range { start, end } => Some(f(start, end.as_ref())),
            _ => None,
        })?
    }

    pub fn with_error<R>(&self, f: impl FnOnce(&ErrorValue) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::Error(error) => Some(f(error)),
            _ => None,
        })?
    }

    pub fn with_frob<R>(&self, f: impl FnOnce(Identity, &Value) -> R) -> Option<R> {
        self.with_heap(|heap| match heap {
            HeapValue::Frob(frob) => Some(f(frob.delegate(), frob.value())),
            _ => None,
        })?
    }

    pub fn frob_delegate(&self) -> Option<Identity> {
        self.with_frob(|delegate, _| delegate)
    }

    pub fn frob_value(&self) -> Option<&Value> {
        match self.heap_ref()? {
            HeapValue::Frob(frob) => Some(frob.value()),
            _ => None,
        }
    }

    pub fn error_code_symbol(&self) -> Option<Symbol> {
        self.as_error_code()
            .or_else(|| self.with_error(ErrorValue::code))
    }

    pub fn is_persistable(&self) -> bool {
        match self.kind() {
            ValueKind::Capability | ValueKind::Function => false,
            ValueKind::List => self
                .with_list(|values| values.iter().all(Self::is_persistable))
                .unwrap_or(false),
            ValueKind::Map => self
                .with_map(|entries| {
                    entries
                        .iter()
                        .all(|(key, value)| key.is_persistable() && value.is_persistable())
                })
                .unwrap_or(false),
            ValueKind::Range => self
                .with_range(|start, end| {
                    start.is_persistable() && end.is_none_or(Self::is_persistable)
                })
                .unwrap_or(false),
            ValueKind::Error => self
                .with_error(|error| error.value().is_none_or(Self::is_persistable))
                .unwrap_or(false),
            ValueKind::Frob => self
                .with_frob(|_, value| value.is_persistable())
                .unwrap_or(false),
            ValueKind::Relation => self
                .with_relation(|relation| {
                    relation
                        .rows()
                        .iter()
                        .flat_map(|row| row.values())
                        .all(Self::is_persistable)
                })
                .unwrap_or(false),
            _ => true,
        }
    }

    pub fn list_len(&self) -> Option<usize> {
        self.with_list(<[Value]>::len)
    }

    pub fn list_get(&self, index: usize) -> Option<Value> {
        self.with_list(|values| values.get(index).cloned())?
    }

    pub fn list_slice(&self, start: usize, end_exclusive: usize) -> Option<Self> {
        self.with_list(|values| {
            if start > end_exclusive || end_exclusive > values.len() {
                return None;
            }
            Some(Self::list(values[start..end_exclusive].iter().cloned()))
        })?
    }

    pub fn list_set(&self, index: usize, value: Value) -> Option<Self> {
        self.with_list(|values| {
            let mut values = values.to_vec();
            let slot = values.get_mut(index)?;
            *slot = value;
            Some(Self::list(values))
        })?
    }

    pub fn map_len(&self) -> Option<usize> {
        self.with_map(<[(Value, Value)]>::len)
    }

    pub fn map_get(&self, key: &Value) -> Option<Value> {
        self.with_map(|entries| {
            entries
                .binary_search_by(|(entry_key, _)| entry_key.cmp(key))
                .ok()
                .map(|index| entries[index].1.clone())
        })?
    }

    pub fn map_set(&self, key: Value, value: Value) -> Option<Self> {
        self.with_map(|entries| {
            let mut entries = entries.to_vec();
            entries.push((key, value));
            Self::map(entries)
        })
    }

    pub fn index_set(&self, index: &Value, value: Value) -> Option<Self> {
        if self.list_len().is_some() {
            let index = usize::try_from(index.as_int()?).ok()?;
            return self.list_set(index, value);
        }
        self.map_set(index.clone(), value)
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        match (self.as_int(), rhs.as_int()) {
            (Some(left), Some(right)) => {
                left.checked_add(right).and_then(|sum| Self::int(sum).ok())
            }
            _ => Some(Self::float_checked(
                self.numeric_as_f32()? + rhs.numeric_as_f32()?,
            )?),
        }
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        match (self.as_int(), rhs.as_int()) {
            (Some(left), Some(right)) => left
                .checked_sub(right)
                .and_then(|diff| Self::int(diff).ok()),
            _ => Some(Self::float_checked(
                self.numeric_as_f32()? - rhs.numeric_as_f32()?,
            )?),
        }
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        match (self.as_int(), rhs.as_int()) {
            (Some(left), Some(right)) => left
                .checked_mul(right)
                .and_then(|product| Self::int(product).ok()),
            _ => Some(Self::float_checked(
                self.numeric_as_f32()? * rhs.numeric_as_f32()?,
            )?),
        }
    }

    pub fn checked_div(&self, rhs: &Self) -> Option<Self> {
        match (self.as_int(), rhs.as_int()) {
            (_, Some(0)) => None,
            (Some(left), Some(right)) if left % right == 0 => Self::int(left / right).ok(),
            _ => {
                let rhs = rhs.numeric_as_f32()?;
                if rhs == 0.0 {
                    None
                } else {
                    Some(Self::float_checked(self.numeric_as_f32()? / rhs)?)
                }
            }
        }
    }

    pub fn checked_rem(&self, rhs: &Self) -> Option<Self> {
        match (self.as_int(), rhs.as_int()) {
            (_, Some(0)) => None,
            (Some(left), Some(right)) => {
                left.checked_rem(right).and_then(|rem| Self::int(rem).ok())
            }
            _ => {
                let rhs = rhs.numeric_as_f32()?;
                if rhs == 0.0 {
                    None
                } else {
                    Some(Self::float_checked(self.numeric_as_f32()? % rhs)?)
                }
            }
        }
    }

    pub fn checked_neg(&self) -> Option<Self> {
        if let Some(value) = self.as_int() {
            value.checked_neg().and_then(|value| Self::int(value).ok())
        } else {
            Some(Self::float_checked(-self.numeric_as_f32()?)?)
        }
    }

    /// Constructs a float from a binary32 result, rejecting non-finite values.
    /// Negative zero canonicalizes to positive zero via `Value::float`.
    #[inline(always)]
    fn float_checked(value: f32) -> Option<Self> {
        Self::float(value).ok()
    }

    #[inline(always)]
    pub(crate) const fn tag(&self) -> u8 {
        (self.0 >> TAG_SHIFT) as u8
    }

    #[inline(always)]
    pub(crate) const fn payload(&self) -> u64 {
        self.0 & PAYLOAD_MASK
    }

    #[inline(always)]
    fn ptr_payload(&self) -> usize {
        self.payload() as usize
    }

    fn heap(value: HeapValue) -> Self {
        let tag = value.tag();
        let ptr = Arc::into_raw(Arc::new(value)) as usize;
        assert!(
            ptr as u64 <= MAX_PAYLOAD,
            "heap pointer exceeded value payload"
        );
        Self::pack(tag, ptr as u64)
    }

    pub(crate) fn with_heap<R>(&self, f: impl FnOnce(&HeapValue) -> R) -> Option<R> {
        if self.is_immediate() {
            return None;
        }
        let ptr = self.ptr_payload() as *const HeapValue;
        Some(unsafe { f(&*ptr) })
    }

    pub(crate) fn heap_ref(&self) -> Option<&HeapValue> {
        if self.is_immediate() {
            return None;
        }
        Some(unsafe { &*self.heap_ptr() })
    }

    pub(crate) fn numeric_as_f32(&self) -> Option<f32> {
        if let Some(value) = self.as_int() {
            Some(value as f32)
        } else {
            self.as_float()
        }
    }

    pub(crate) fn heap_ptr(&self) -> *const HeapValue {
        self.ptr_payload() as *const HeapValue
    }

    #[cfg(test)]
    pub(crate) fn heap_strong_count(&self) -> Option<usize> {
        if self.is_immediate() {
            return None;
        }
        let arc = unsafe { Arc::from_raw(self.heap_ptr()) };
        let count = Arc::strong_count(&arc);
        let _ = Arc::into_raw(arc);
        Some(count)
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        if !self.is_immediate() {
            unsafe { Arc::increment_strong_count(self.heap_ptr()) };
        }
        Self(self.0)
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        if !self.is_immediate() {
            unsafe { Arc::decrement_strong_count(self.heap_ptr()) };
        }
    }
}

impl Default for Value {
    fn default() -> Self {
        Self::nothing()
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::bool(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::int(value as i64).unwrap()
    }
}

impl TryFrom<f32> for Value {
    type Error = ValueError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::float(value)
    }
}

impl From<Symbol> for Value {
    fn from(value: Symbol) -> Self {
        Self::symbol(value)
    }
}

impl From<Identity> for Value {
    fn from(value: Identity) -> Self {
        Self::identity(value)
    }
}

impl From<RelationValue> for Value {
    fn from(value: RelationValue) -> Self {
        if value.arity() == 0 && value.is_empty() {
            return Self::nothing();
        }
        Self::heap(HeapValue::Relation(value))
    }
}

/// Language numeric comparison helpers.
///
/// These provide the numeric comparison semantics used by VM operators and
/// relation rule guards. They differ from `Value`'s canonical `Ord`/`Eq`:
/// when both operands are numeric, integers and floats compare by numeric
/// value rather than by `ValueKind`. Canonical stored values remain
/// distinguishable, so `1 == 1.0` is true in language comparison while `1`
/// and `1.0` remain distinct map and relation keys.
pub mod language_cmp {
    use crate::value::{Value, ValueKind};
    use std::cmp::Ordering;

    /// Boundary constants for the exact mixed comparison algorithm.
    /// Both powers of two are exactly representable as binary32.
    /// -2^55 and 2^55 as f32 bit patterns.
    const INT_LOWER: f32 = f32::from_bits(0xdb00_0000); // -2^55
    const INT_UPPER_EXCLUSIVE: f32 = f32::from_bits(0x5b00_0000); // 2^55

    /// Compares a Mica integer with a finite binary32 float exactly, without
    /// converting the integer to `f32` or `f64`.
    ///
    /// The integer is on the left; reverse the result if the float is on the
    /// left.
    ///
    /// # Preconditions
    ///
    /// `integer` must be a valid Mica integer (`INT_MIN <= integer <= INT_MAX`).
    /// `float` must be a finite binary32 value (not NaN, not infinity). These
    /// are invariants of every constructible `Value`. Callers passing raw
    /// integers or floats from external sources must validate first.
    pub fn compare_int_float(integer: i64, float: f32) -> Ordering {
        debug_assert!(
            !float.is_nan() && !float.is_infinite(),
            "compare_int_float requires a finite float"
        );
        // If float is outside the integer range, every Mica integer is on one
        // side of it.
        if float < INT_LOWER {
            return Ordering::Greater;
        }
        if float >= INT_UPPER_EXCLUSIVE {
            return Ordering::Less;
        }

        // The float is within the Mica integer conversion range. Truncate
        // toward zero; this conversion is exact and cannot saturate because
        // of the preceding bounds checks.
        let truncated = float.trunc() as i64;

        match integer.cmp(&truncated) {
            Ordering::Equal => {
                // Inspect the fractional part to decide the final ordering.
                let fract = float.fract();
                if fract > 0.0 {
                    Ordering::Less
                } else if fract < 0.0 {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            }
            non_equal => non_equal,
        }
    }

    /// Returns the numeric ordering of two values for language comparison.
    ///
    /// Non-numeric pairs retain canonical `Value::cmp` ordering.
    pub fn numeric_cmp(left: &Value, right: &Value) -> Ordering {
        match (left.kind(), right.kind()) {
            (ValueKind::Int, ValueKind::Int) => {
                left.as_int().unwrap().cmp(&right.as_int().unwrap())
            }
            (ValueKind::Float, ValueKind::Float) => left
                .as_float()
                .unwrap()
                .total_cmp(&right.as_float().unwrap()),
            (ValueKind::Int, ValueKind::Float) => {
                compare_int_float(left.as_int().unwrap(), right.as_float().unwrap())
            }
            (ValueKind::Float, ValueKind::Int) => {
                compare_int_float(right.as_int().unwrap(), left.as_float().unwrap()).reverse()
            }
            _ => left.cmp(right),
        }
    }

    /// Returns true if two values are numerically equal for language
    /// comparison.
    ///
    /// Non-numeric pairs retain canonical `Value` equality.
    pub fn numeric_eq(left: &Value, right: &Value) -> bool {
        match (left.kind(), right.kind()) {
            (ValueKind::Int, ValueKind::Int) => left.payload() == right.payload(),
            (ValueKind::Float, ValueKind::Float) => left.payload() == right.payload(),
            (ValueKind::Int, ValueKind::Float) => {
                compare_int_float(left.as_int().unwrap(), right.as_float().unwrap())
                    == Ordering::Equal
            }
            (ValueKind::Float, ValueKind::Int) => {
                compare_int_float(right.as_int().unwrap(), left.as_float().unwrap())
                    == Ordering::Equal
            }
            _ => left == right,
        }
    }
}
