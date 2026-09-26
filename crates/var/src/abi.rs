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

//! Process-local value representation contract for execution backends.
//!
//! This is not a persistence or cross-process ABI. Native code must check
//! [`VALUE_ABI_VERSION`] before execution, and compiled code must not survive a
//! process restart or a version change.

use crate::value::{
    INT_MAX, INT_MIN, PAYLOAD_MASK, TAG_BOOL, TAG_BYTES, TAG_CAPABILITY, TAG_EMPTY_RELATION,
    TAG_ERROR, TAG_FROB, TAG_FUNCTION, TAG_LIST, TAG_MAP, TAG_RANGE, TAG_RELATION, TAG_SHIFT,
    TAG_STRING,
};
use crate::{Value, ValueKind};
use std::cmp::Ordering;
use std::mem::ManuallyDrop;

pub const VALUE_ABI_VERSION: u32 = crate::VALUE_ABI_VERSION;
pub const VALUE_WORD_BYTES: usize = size_of::<Value>();
pub const VALUE_TAG_SHIFT: u64 = TAG_SHIFT;
pub const VALUE_PAYLOAD_MASK: u64 = PAYLOAD_MASK;
pub const VALUE_INT_MIN: i64 = INT_MIN;
pub const VALUE_INT_MAX: i64 = INT_MAX;
pub const VALUE_EMPTY_RELATION_TAG: u8 = TAG_EMPTY_RELATION;
pub const VALUE_BOOL_TAG: u8 = TAG_BOOL;
pub const VALUE_INT_TAG: u8 = ValueKind::Int as u8;
pub const VALUE_FLOAT_TAG: u8 = ValueKind::Float as u8;
pub const VALUE_STRING_TAG: u8 = TAG_STRING;
pub const VALUE_LIST_TAG: u8 = TAG_LIST;
pub const VALUE_CAPABILITY_TAG: u8 = TAG_CAPABILITY;
pub const VALUE_FUNCTION_TAG: u8 = TAG_FUNCTION;
pub const VALUE_RELATION_TAG: u8 = TAG_RELATION;

const HEAP_TAG_MASK: u32 = (1 << TAG_STRING)
    | (1 << TAG_BYTES)
    | (1 << TAG_LIST)
    | (1 << TAG_MAP)
    | (1 << TAG_RANGE)
    | (1 << TAG_ERROR)
    | (1 << TAG_FROB)
    | (1 << TAG_RELATION);

pub const fn value_tag(bits: u64) -> u8 {
    (bits >> VALUE_TAG_SHIFT) as u8
}

pub const fn value_payload(bits: u64) -> u64 {
    bits & VALUE_PAYLOAD_MASK
}

pub const fn value_is_immediate(bits: u64) -> bool {
    let tag = value_tag(bits);
    tag <= VALUE_RELATION_TAG && HEAP_TAG_MASK & (1 << tag) == 0
}

pub const fn pack_value(tag: u8, payload: u64) -> u64 {
    ((tag as u64) << VALUE_TAG_SHIFT) | (payload & VALUE_PAYLOAD_MASK)
}

pub const fn borrowed_value_bits(value: &Value) -> u64 {
    value.raw_bits()
}

/// Compares two borrowed process-local value words using language numeric
/// equality without taking ownership of either word.
///
/// # Safety
///
/// Both words must denote valid live `Value`s for [`VALUE_ABI_VERSION`] for the
/// duration of this call.
pub unsafe fn borrowed_value_numeric_eq(left: u64, right: u64) -> bool {
    let left = ManuallyDrop::new(Value(left));
    let right = ManuallyDrop::new(Value(right));
    crate::language_cmp::numeric_eq(&left, &right)
}

/// Compares two borrowed process-local value words using language numeric
/// ordering without taking ownership of either word.
///
/// # Safety
///
/// Both words must denote valid live `Value`s for [`VALUE_ABI_VERSION`] for the
/// duration of this call.
pub unsafe fn borrowed_value_numeric_cmp(left: u64, right: u64) -> Ordering {
    let left = ManuallyDrop::new(Value(left));
    let right = ManuallyDrop::new(Value(right));
    crate::language_cmp::numeric_cmp(&left, &right)
}

/// Compares two borrowed process-local value words using canonical `Value`
/// ordering without taking ownership of either word.
///
/// # Safety
///
/// Both words must denote valid live `Value`s for [`VALUE_ABI_VERSION`] for the
/// duration of this call.
pub unsafe fn borrowed_value_cmp(left: u64, right: u64) -> Ordering {
    let left = ManuallyDrop::new(Value(left));
    let right = ManuallyDrop::new(Value(right));
    left.cmp(&right)
}

pub fn into_owned_value_bits(value: Value) -> u64 {
    let value = ManuallyDrop::new(value);
    value.raw_bits()
}

/// Reconstructs a value from a process-local owned word.
///
/// # Safety
///
/// `bits` must be a valid value word for [`VALUE_ABI_VERSION`]. If it denotes a
/// heap value, the caller must transfer one live strong reference to the
/// returned `Value` and must not release that reference separately.
pub unsafe fn from_owned_value_bits(bits: u64) -> Value {
    Value(bits)
}

/// Clones a valid process-local value word and returns a new owned word.
///
/// # Safety
///
/// `bits` must denote a valid live `Value` for [`VALUE_ABI_VERSION`].
pub unsafe fn clone_value_bits(bits: u64) -> u64 {
    let value = ManuallyDrop::new(Value(bits));
    into_owned_value_bits(Value::clone(&value))
}

/// Releases one owned process-local value word.
///
/// # Safety
///
/// `bits` must denote a valid owned `Value` for [`VALUE_ABI_VERSION`], and the
/// corresponding ownership must not be released again.
pub unsafe fn drop_value_bits(bits: u64) {
    drop(Value(bits));
}
