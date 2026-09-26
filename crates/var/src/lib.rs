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

//! Compact values for Mica's relation kernel.
//!
//! `Value` is intentionally one machine word wide. Immediate identities,
//! symbols, error codes, booleans, small integers, and reduced-precision floats
//! stay inline; strings, bytes, collections, and finite relations are immutable
//! heap values shared with `Arc`.

mod codec;
mod heap;
mod string;
mod symbol;
mod traits;
mod tuple;
mod value;
mod visit;

pub mod abi;

#[cfg(test)]
mod tests;

pub use codec::{
    SymbolEncoding, ValueCodecError, ValueCodecOptions, ValueSegment, ValueSegments, ValueSink,
    decode_value, decode_value_exact, decode_value_exact_with_options, decode_value_with_options,
    encode_value, encode_value_segments, encode_value_segments_with_options, encode_value_to_sink,
    encode_value_with_options,
};
pub use symbol::{Symbol, SymbolMetadata};
pub use traits::OrderedKeySink;
pub use tuple::{RelationValue, RelationValueError, Tuple};
pub use value::{
    BOOL_PROTOTYPE, BYTES_PROTOTYPE, CAPABILITY_PROTOTYPE, CapabilityId, ERROR_CODE_PROTOTYPE,
    ERROR_PROTOTYPE, ErrorValue, FLOAT_PROTOTYPE, FROB_PROTOTYPE, FUNCTION_PROTOTYPE, FrobValue,
    FunctionId, IDENTITY_PROTOTYPE, INTEGER_PROTOTYPE, Identity, LIST_PROTOTYPE, MAP_PROTOTYPE,
    PRIMITIVE_PROTOTYPES, RANGE_PROTOTYPE, RELATION_PROTOTYPE, STRING_PROTOTYPE, SYMBOL_PROTOTYPE,
    VALUE_ABI_VERSION, Value, ValueError, ValueKind, language_cmp, primitive_prototype_for_kind,
    primitive_prototype_for_value,
};
pub use visit::{ValueRef, ValueVisitor, VisitDecision};
