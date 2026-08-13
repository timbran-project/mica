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

use cranelift_codegen::ir::{
    FuncRef, InstBuilder, MemFlagsData, Value as CraneliftValue,
    condcodes::{FloatCC, IntCC},
    immediates::Ieee32,
    types,
};
use cranelift_frontend::FunctionBuilder;
use mica_var::abi::{
    VALUE_BOOL_TAG, VALUE_CAPABILITY_TAG, VALUE_EMPTY_RELATION_TAG, VALUE_FLOAT_TAG,
    VALUE_FUNCTION_TAG, VALUE_INT_MAX, VALUE_INT_MIN, VALUE_INT_TAG, VALUE_LIST_TAG,
    VALUE_PAYLOAD_MASK, VALUE_RELATION_TAG, VALUE_STRING_TAG, VALUE_TAG_SHIFT,
};

/// A generated operation result and a predicate indicating whether it completed
/// without leaving the native immediate-value path.
#[derive(Clone, Copy, Debug)]
pub struct EmittedValue {
    word: CraneliftValue,
    is_fast: CraneliftValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerComparison {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

impl IntegerComparison {
    const fn condition_code(self) -> IntCC {
        match self {
            Self::Equal => IntCC::Equal,
            Self::NotEqual => IntCC::NotEqual,
            Self::LessThan => IntCC::SignedLessThan,
            Self::LessThanOrEqual => IntCC::SignedLessThanOrEqual,
            Self::GreaterThan => IntCC::SignedGreaterThan,
            Self::GreaterThanOrEqual => IntCC::SignedGreaterThanOrEqual,
        }
    }
}

/// Language comparison operations supported by the native binary32 path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatComparison {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

/// Language comparisons supported across immediate scalar values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarComparison {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

impl ScalarComparison {
    const fn signed_condition_code(self) -> IntCC {
        match self {
            Self::Equal => IntCC::Equal,
            Self::NotEqual => IntCC::NotEqual,
            Self::LessThan => IntCC::SignedLessThan,
            Self::LessThanOrEqual => IntCC::SignedLessThanOrEqual,
            Self::GreaterThan => IntCC::SignedGreaterThan,
            Self::GreaterThanOrEqual => IntCC::SignedGreaterThanOrEqual,
        }
    }

    const fn unsigned_condition_code(self) -> IntCC {
        match self {
            Self::Equal => IntCC::Equal,
            Self::NotEqual => IntCC::NotEqual,
            Self::LessThan => IntCC::UnsignedLessThan,
            Self::LessThanOrEqual => IntCC::UnsignedLessThanOrEqual,
            Self::GreaterThan => IntCC::UnsignedGreaterThan,
            Self::GreaterThanOrEqual => IntCC::UnsignedGreaterThanOrEqual,
        }
    }

    const fn float_condition_code(self) -> FloatCC {
        match self {
            Self::Equal => FloatCC::Equal,
            Self::NotEqual => FloatCC::NotEqual,
            Self::LessThan => FloatCC::LessThan,
            Self::LessThanOrEqual => FloatCC::LessThanOrEqual,
            Self::GreaterThan => FloatCC::GreaterThan,
            Self::GreaterThanOrEqual => FloatCC::GreaterThanOrEqual,
        }
    }
}

impl FloatComparison {
    const fn condition_code(self) -> FloatCC {
        match self {
            Self::Equal => FloatCC::Equal,
            Self::NotEqual => FloatCC::NotEqual,
            Self::LessThan => FloatCC::LessThan,
            Self::LessThanOrEqual => FloatCC::LessThanOrEqual,
            Self::GreaterThan => FloatCC::GreaterThan,
            Self::GreaterThanOrEqual => FloatCC::GreaterThanOrEqual,
        }
    }
}

impl EmittedValue {
    pub const fn word(self) -> CraneliftValue {
        self.word
    }

    pub const fn is_fast(self) -> CraneliftValue {
        self.is_fast
    }

    pub(crate) fn always_fast(builder: &mut FunctionBuilder<'_>, word: CraneliftValue) -> Self {
        Self {
            word,
            is_fast: builder.ins().iconst(types::I8, 1),
        }
    }
}

/// Emits operations over the process-local [`mica_var::Value`] word format.
pub struct ValueEmitter;

#[derive(Clone, Copy)]
enum NumericArithmetic {
    Add,
    Subtract,
    Multiply,
}

#[derive(Clone, Copy)]
enum NumericQuotient {
    Divide,
    Remainder,
}

impl ValueEmitter {
    pub fn emit_tag(builder: &mut FunctionBuilder<'_>, word: CraneliftValue) -> CraneliftValue {
        builder.ins().ushr_imm(word, VALUE_TAG_SHIFT as i64)
    }

    pub fn emit_payload(builder: &mut FunctionBuilder<'_>, word: CraneliftValue) -> CraneliftValue {
        let mask = builder.ins().iconst(types::I64, VALUE_PAYLOAD_MASK as i64);
        builder.ins().band(word, mask)
    }

    pub fn emit_is_int(builder: &mut FunctionBuilder<'_>, word: CraneliftValue) -> CraneliftValue {
        let tag = Self::emit_tag(builder, word);
        builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_INT_TAG))
    }

    pub fn emit_is_float(
        builder: &mut FunctionBuilder<'_>,
        word: CraneliftValue,
    ) -> CraneliftValue {
        let tag = Self::emit_tag(builder, word);
        builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_FLOAT_TAG))
    }

    pub fn emit_is_immediate(
        builder: &mut FunctionBuilder<'_>,
        word: CraneliftValue,
    ) -> CraneliftValue {
        let tag = Self::emit_tag(builder, word);
        let before_heap =
            builder
                .ins()
                .icmp_imm(IntCC::UnsignedLessThan, tag, i64::from(VALUE_STRING_TAG));
        let capability = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_CAPABILITY_TAG));
        let function = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_FUNCTION_TAG));
        let before_heap_or_capability = builder.ins().bor(before_heap, capability);
        builder.ins().bor(before_heap_or_capability, function)
    }

    pub fn emit_pack(
        builder: &mut FunctionBuilder<'_>,
        tag: u8,
        payload: CraneliftValue,
    ) -> CraneliftValue {
        let masked = Self::emit_payload(builder, payload);
        let tag = builder
            .ins()
            .iconst(types::I64, i64::from(tag) << VALUE_TAG_SHIFT);
        builder.ins().bor(tag, masked)
    }

    pub fn emit_bool(
        builder: &mut FunctionBuilder<'_>,
        predicate: CraneliftValue,
    ) -> CraneliftValue {
        let payload = builder.ins().uextend(types::I64, predicate);
        Self::emit_pack(builder, VALUE_BOOL_TAG, payload)
    }

    pub fn emit_unbox_int(
        builder: &mut FunctionBuilder<'_>,
        word: CraneliftValue,
    ) -> CraneliftValue {
        let shifted = builder.ins().ishl_imm(word, 64 - VALUE_TAG_SHIFT as i64);
        builder.ins().sshr_imm(shifted, 64 - VALUE_TAG_SHIFT as i64)
    }

    /// Unboxes the low binary32 payload bits as an `F32`. Callers must first
    /// prove that the word has the float tag.
    pub fn emit_unbox_float(
        builder: &mut FunctionBuilder<'_>,
        word: CraneliftValue,
    ) -> CraneliftValue {
        let payload = Self::emit_payload(builder, word);
        let bits = builder.ins().ireduce(types::I32, payload);
        builder.ins().bitcast(types::F32, MemFlagsData::new(), bits)
    }

    /// Packs an `F32` result into a canonical finite Mica float word.
    ///
    /// The returned fast predicate rejects infinity and NaN. Negative zero is
    /// selected to positive zero before its bits enter the value word.
    pub fn emit_pack_checked_float(
        builder: &mut FunctionBuilder<'_>,
        value: CraneliftValue,
    ) -> EmittedValue {
        let (_, bits, is_finite) = Self::emit_canonical_float(builder, value);
        let payload = builder.ins().uextend(types::I64, bits);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_FLOAT_TAG, payload),
            is_fast: is_finite,
        }
    }

    /// Returns a canonical binary32 value, its bits, and a finite predicate.
    pub(crate) fn emit_canonical_float(
        builder: &mut FunctionBuilder<'_>,
        value: CraneliftValue,
    ) -> (CraneliftValue, CraneliftValue, CraneliftValue) {
        let zero = builder.ins().f32const(Ieee32::with_bits(0));
        let is_zero = builder.ins().fcmp(FloatCC::Equal, value, zero);
        let value = builder.ins().select(is_zero, zero, value);
        let bits = builder
            .ins()
            .bitcast(types::I32, MemFlagsData::new(), value);
        let magnitude_mask = builder.ins().iconst(types::I32, 0x7fff_ffff);
        let magnitude = builder.ins().band(bits, magnitude_mask);
        let is_finite = builder.ins().icmp_imm(
            IntCC::UnsignedLessThan,
            magnitude,
            i64::from(0x7f80_0000u32),
        );
        (value, bits, is_finite)
    }

    pub fn emit_checked_float_add(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let left = Self::emit_unbox_float(builder, left);
        let right = Self::emit_unbox_float(builder, right);
        let value = builder.ins().fadd(left, right);
        let result = Self::emit_pack_checked_float(builder, value);
        Self::emit_float_result(builder, left_is_float, right_is_float, result, None)
    }

    pub fn emit_checked_float_sub(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let left = Self::emit_unbox_float(builder, left);
        let right = Self::emit_unbox_float(builder, right);
        let value = builder.ins().fsub(left, right);
        let result = Self::emit_pack_checked_float(builder, value);
        Self::emit_float_result(builder, left_is_float, right_is_float, result, None)
    }

    pub fn emit_checked_float_mul(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let left = Self::emit_unbox_float(builder, left);
        let right = Self::emit_unbox_float(builder, right);
        let value = builder.ins().fmul(left, right);
        let result = Self::emit_pack_checked_float(builder, value);
        Self::emit_float_result(builder, left_is_float, right_is_float, result, None)
    }

    pub fn emit_checked_float_div(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let left = Self::emit_unbox_float(builder, left);
        let right = Self::emit_unbox_float(builder, right);
        let zero = builder.ins().f32const(Ieee32::with_bits(0));
        let divisor_is_nonzero = builder.ins().fcmp(FloatCC::NotEqual, right, zero);
        let one = builder.ins().f32const(Ieee32::with_bits(1.0f32.to_bits()));
        let safe_right = builder.ins().select(divisor_is_nonzero, right, one);
        let value = builder.ins().fdiv(left, safe_right);
        let result = Self::emit_pack_checked_float(builder, value);
        Self::emit_float_result(
            builder,
            left_is_float,
            right_is_float,
            result,
            Some(divisor_is_nonzero),
        )
    }

    pub fn emit_checked_float_neg(
        builder: &mut FunctionBuilder<'_>,
        value: CraneliftValue,
    ) -> EmittedValue {
        let is_float = Self::emit_is_float(builder, value);
        let value = Self::emit_unbox_float(builder, value);
        let value = builder.ins().fneg(value);
        let result = Self::emit_pack_checked_float(builder, value);
        EmittedValue {
            word: result.word(),
            is_fast: builder.ins().band(is_float, result.is_fast()),
        }
    }

    pub fn emit_checked_float_compare(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
        comparison: FloatComparison,
    ) -> EmittedValue {
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let is_fast = builder.ins().band(left_is_float, right_is_float);
        let left = Self::emit_unbox_float(builder, left);
        let right = Self::emit_unbox_float(builder, right);
        let result = builder.ins().fcmp(comparison.condition_code(), left, right);
        EmittedValue {
            word: Self::emit_bool(builder, result),
            is_fast,
        }
    }

    fn emit_float_result(
        builder: &mut FunctionBuilder<'_>,
        left_is_float: CraneliftValue,
        right_is_float: CraneliftValue,
        result: EmittedValue,
        extra_guard: Option<CraneliftValue>,
    ) -> EmittedValue {
        let is_fast = builder.ins().band(left_is_float, right_is_float);
        let is_fast = builder.ins().band(is_fast, result.is_fast());
        let is_fast = match extra_guard {
            Some(guard) => builder.ins().band(is_fast, guard),
            None => is_fast,
        };
        EmittedValue {
            word: result.word(),
            is_fast,
        }
    }

    pub fn emit_checked_numeric_add(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        Self::emit_checked_numeric_binary(builder, left, right, NumericArithmetic::Add)
    }

    pub fn emit_checked_numeric_sub(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        Self::emit_checked_numeric_binary(builder, left, right, NumericArithmetic::Subtract)
    }

    pub fn emit_checked_numeric_mul(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        Self::emit_checked_numeric_binary(builder, left, right, NumericArithmetic::Multiply)
    }

    pub fn emit_checked_numeric_div(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        Self::emit_checked_numeric_quotient(builder, left, right, NumericQuotient::Divide, None)
    }

    pub fn emit_checked_numeric_rem(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
        float_remainder_helper: FuncRef,
    ) -> EmittedValue {
        Self::emit_checked_numeric_quotient(
            builder,
            left,
            right,
            NumericQuotient::Remainder,
            Some(float_remainder_helper),
        )
    }

    fn emit_checked_numeric_binary(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
        operation: NumericArithmetic,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let both_int = builder.ins().band(left_is_int, right_is_int);

        let int_block = builder.create_block();
        let float_block = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I8);
        builder
            .ins()
            .brif(both_int, int_block, &[], float_block, &[]);

        builder.switch_to_block(int_block);
        let int_result = match operation {
            NumericArithmetic::Add => Self::emit_checked_int_add(builder, left, right),
            NumericArithmetic::Subtract => Self::emit_checked_int_sub(builder, left, right),
            NumericArithmetic::Multiply => Self::emit_checked_int_mul(builder, left, right),
        };
        builder.ins().jump(
            done,
            &[int_result.word().into(), int_result.is_fast().into()],
        );

        builder.switch_to_block(float_block);
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let left_is_numeric = builder.ins().bor(left_is_int, left_is_float);
        let right_is_numeric = builder.ins().bor(right_is_int, right_is_float);
        let operands_are_numeric = builder.ins().band(left_is_numeric, right_is_numeric);

        let left_int = Self::emit_unbox_int(builder, left);
        let right_int = Self::emit_unbox_int(builder, right);
        let left_int_float = builder.ins().fcvt_from_sint(types::F32, left_int);
        let right_int_float = builder.ins().fcvt_from_sint(types::F32, right_int);
        let left_float = Self::emit_unbox_float(builder, left);
        let right_float = Self::emit_unbox_float(builder, right);
        let left_float = builder
            .ins()
            .select(left_is_int, left_int_float, left_float);
        let right_float = builder
            .ins()
            .select(right_is_int, right_int_float, right_float);
        let float_value = match operation {
            NumericArithmetic::Add => builder.ins().fadd(left_float, right_float),
            NumericArithmetic::Subtract => builder.ins().fsub(left_float, right_float),
            NumericArithmetic::Multiply => builder.ins().fmul(left_float, right_float),
        };
        let float_result = Self::emit_pack_checked_float(builder, float_value);
        let float_is_fast = builder
            .ins()
            .band(operands_are_numeric, float_result.is_fast());
        builder
            .ins()
            .jump(done, &[float_result.word().into(), float_is_fast.into()]);

        builder.switch_to_block(done);
        let word = builder.block_params(done)[0];
        let is_fast = builder.block_params(done)[1];
        EmittedValue { word, is_fast }
    }

    fn emit_checked_numeric_quotient(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
        operation: NumericQuotient,
        float_remainder_helper: Option<FuncRef>,
    ) -> EmittedValue {
        let int_result = match operation {
            NumericQuotient::Divide => Self::emit_checked_int_div(builder, left, right),
            NumericQuotient::Remainder => Self::emit_checked_int_rem(builder, left, right),
        };
        let fallback = builder.create_block();
        let float_block = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I8);
        builder.ins().brif(
            int_result.is_fast(),
            done,
            &[int_result.word().into(), int_result.is_fast().into()],
            fallback,
            &[],
        );

        builder.switch_to_block(fallback);
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let both_int = builder.ins().band(left_is_int, right_is_int);
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let left_is_numeric = builder.ins().bor(left_is_int, left_is_float);
        let right_is_numeric = builder.ins().bor(right_is_int, right_is_float);
        let operands_are_numeric = builder.ins().band(left_is_numeric, right_is_numeric);
        let not_both_int = builder.ins().icmp_imm(IntCC::Equal, both_int, 0);
        let mixed_or_float = builder.ins().band(operands_are_numeric, not_both_int);
        let use_float = match operation {
            NumericQuotient::Divide => {
                let left_int = Self::emit_unbox_int(builder, left);
                let right_int = Self::emit_unbox_int(builder, right);
                let divisor_is_nonzero = builder.ins().icmp_imm(IntCC::NotEqual, right_int, 0);
                let one = builder.ins().iconst(types::I64, 1);
                let safe_right = builder.ins().select(divisor_is_nonzero, right_int, one);
                let remainder = builder.ins().srem(left_int, safe_right);
                let is_fractional = builder.ins().icmp_imm(IntCC::NotEqual, remainder, 0);
                let fractional_int = builder.ins().band(both_int, divisor_is_nonzero);
                let fractional_int = builder.ins().band(fractional_int, is_fractional);
                builder.ins().bor(mixed_or_float, fractional_int)
            }
            NumericQuotient::Remainder => mixed_or_float,
        };
        builder.ins().brif(
            use_float,
            float_block,
            &[],
            done,
            &[int_result.word().into(), int_result.is_fast().into()],
        );

        builder.switch_to_block(float_block);
        let left_int = Self::emit_unbox_int(builder, left);
        let right_int = Self::emit_unbox_int(builder, right);
        let left_int_float = builder.ins().fcvt_from_sint(types::F32, left_int);
        let right_int_float = builder.ins().fcvt_from_sint(types::F32, right_int);
        let left_float = Self::emit_unbox_float(builder, left);
        let right_float = Self::emit_unbox_float(builder, right);
        let left_float = builder
            .ins()
            .select(left_is_int, left_int_float, left_float);
        let right_float = builder
            .ins()
            .select(right_is_int, right_int_float, right_float);
        let zero = builder.ins().f32const(Ieee32::with_bits(0));
        let divisor_is_nonzero = builder.ins().fcmp(FloatCC::NotEqual, right_float, zero);
        let one = builder.ins().f32const(Ieee32::with_bits(1.0f32.to_bits()));
        let safe_right = builder.ins().select(divisor_is_nonzero, right_float, one);
        let float_value = match operation {
            NumericQuotient::Divide => builder.ins().fdiv(left_float, safe_right),
            NumericQuotient::Remainder => {
                let helper = float_remainder_helper.expect("remainder emission requires a helper");
                let call = builder.ins().call(helper, &[left_float, safe_right]);
                builder.inst_results(call)[0]
            }
        };
        let float_result = Self::emit_pack_checked_float(builder, float_value);
        let is_fast = builder.ins().band(operands_are_numeric, divisor_is_nonzero);
        let is_fast = builder.ins().band(is_fast, float_result.is_fast());
        builder
            .ins()
            .jump(done, &[float_result.word().into(), is_fast.into()]);

        builder.switch_to_block(done);
        let word = builder.block_params(done)[0];
        let is_fast = builder.block_params(done)[1];
        EmittedValue { word, is_fast }
    }

    pub fn emit_checked_numeric_neg(
        builder: &mut FunctionBuilder<'_>,
        value: CraneliftValue,
    ) -> EmittedValue {
        let is_int = Self::emit_is_int(builder, value);
        let int_block = builder.create_block();
        let float_block = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I8);
        builder.ins().brif(is_int, int_block, &[], float_block, &[]);

        builder.switch_to_block(int_block);
        let int_result = Self::emit_checked_int_neg(builder, value);
        builder.ins().jump(
            done,
            &[int_result.word().into(), int_result.is_fast().into()],
        );

        builder.switch_to_block(float_block);
        let float_result = Self::emit_checked_float_neg(builder, value);
        builder.ins().jump(
            done,
            &[float_result.word().into(), float_result.is_fast().into()],
        );

        builder.switch_to_block(done);
        let word = builder.block_params(done)[0];
        let is_fast = builder.block_params(done)[1];
        EmittedValue { word, is_fast }
    }

    pub fn emit_checked_int_add(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let operands_are_ints = builder.ins().band(left_is_int, right_is_int);
        let left = Self::emit_unbox_int(builder, left);
        let right = Self::emit_unbox_int(builder, right);
        let sum = builder.ins().iadd(left, right);
        let above_min = builder
            .ins()
            .icmp_imm(IntCC::SignedGreaterThanOrEqual, sum, VALUE_INT_MIN);
        let below_max = builder
            .ins()
            .icmp_imm(IntCC::SignedLessThanOrEqual, sum, VALUE_INT_MAX);
        let in_range = builder.ins().band(above_min, below_max);
        let is_fast = builder.ins().band(operands_are_ints, in_range);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_INT_TAG, sum),
            is_fast,
        }
    }

    pub fn emit_checked_int_sub(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let operands_are_ints = builder.ins().band(left_is_int, right_is_int);
        let left = Self::emit_unbox_int(builder, left);
        let right = Self::emit_unbox_int(builder, right);
        let difference = builder.ins().isub(left, right);
        let in_range = Self::emit_int_in_range(builder, difference);
        let is_fast = builder.ins().band(operands_are_ints, in_range);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_INT_TAG, difference),
            is_fast,
        }
    }

    pub fn emit_checked_int_mul(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let operands_are_ints = builder.ins().band(left_is_int, right_is_int);
        let left = Self::emit_unbox_int(builder, left);
        let right = Self::emit_unbox_int(builder, right);
        let (product, overflow) = builder.ins().smul_overflow(left, right);
        let no_overflow = builder.ins().icmp_imm(IntCC::Equal, overflow, 0);
        let in_range = Self::emit_int_in_range(builder, product);
        let is_fast = builder.ins().band(operands_are_ints, no_overflow);
        let is_fast = builder.ins().band(is_fast, in_range);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_INT_TAG, product),
            is_fast,
        }
    }

    pub fn emit_checked_int_div(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let operands_are_ints = builder.ins().band(left_is_int, right_is_int);
        let left = Self::emit_unbox_int(builder, left);
        let right = Self::emit_unbox_int(builder, right);
        let divisor_is_nonzero = builder.ins().icmp_imm(IntCC::NotEqual, right, 0);
        let one = builder.ins().iconst(types::I64, 1);
        let safe_right = builder.ins().select(divisor_is_nonzero, right, one);
        let quotient = builder.ins().sdiv(left, safe_right);
        let reconstructed = builder.ins().imul(quotient, safe_right);
        let is_exact = builder.ins().icmp(IntCC::Equal, reconstructed, left);
        let in_range = Self::emit_int_in_range(builder, quotient);
        let is_fast = builder.ins().band(operands_are_ints, divisor_is_nonzero);
        let is_fast = builder.ins().band(is_fast, is_exact);
        let is_fast = builder.ins().band(is_fast, in_range);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_INT_TAG, quotient),
            is_fast,
        }
    }

    pub fn emit_checked_int_rem(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let operands_are_ints = builder.ins().band(left_is_int, right_is_int);
        let left = Self::emit_unbox_int(builder, left);
        let right = Self::emit_unbox_int(builder, right);
        let divisor_is_nonzero = builder.ins().icmp_imm(IntCC::NotEqual, right, 0);
        let one = builder.ins().iconst(types::I64, 1);
        let safe_right = builder.ins().select(divisor_is_nonzero, right, one);
        let remainder = builder.ins().srem(left, safe_right);
        let in_range = Self::emit_int_in_range(builder, remainder);
        let is_fast = builder.ins().band(operands_are_ints, divisor_is_nonzero);
        let is_fast = builder.ins().band(is_fast, in_range);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_INT_TAG, remainder),
            is_fast,
        }
    }

    pub fn emit_checked_int_neg(
        builder: &mut FunctionBuilder<'_>,
        value: CraneliftValue,
    ) -> EmittedValue {
        let is_int = Self::emit_is_int(builder, value);
        let value = Self::emit_unbox_int(builder, value);
        let negated = builder.ins().ineg(value);
        let in_range = Self::emit_int_in_range(builder, negated);
        let is_fast = builder.ins().band(is_int, in_range);

        EmittedValue {
            word: Self::emit_pack(builder, VALUE_INT_TAG, negated),
            is_fast,
        }
    }

    fn emit_int_in_range(
        builder: &mut FunctionBuilder<'_>,
        value: CraneliftValue,
    ) -> CraneliftValue {
        let above_min =
            builder
                .ins()
                .icmp_imm(IntCC::SignedGreaterThanOrEqual, value, VALUE_INT_MIN);
        let below_max = builder
            .ins()
            .icmp_imm(IntCC::SignedLessThanOrEqual, value, VALUE_INT_MAX);
        builder.ins().band(above_min, below_max)
    }

    pub fn emit_checked_int_lt(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        Self::emit_checked_int_compare(builder, left, right, IntegerComparison::LessThan)
    }

    pub fn emit_checked_int_compare(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
        comparison: IntegerComparison,
    ) -> EmittedValue {
        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let is_fast = builder.ins().band(left_is_int, right_is_int);
        let left = Self::emit_unbox_int(builder, left);
        let right = Self::emit_unbox_int(builder, right);
        let result = builder.ins().icmp(comparison.condition_code(), left, right);

        EmittedValue {
            word: Self::emit_bool(builder, result),
            is_fast,
        }
    }

    pub fn emit_immediate_eq(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
    ) -> EmittedValue {
        Self::emit_scalar_compare(builder, left, right, ScalarComparison::Equal)
    }

    pub fn emit_scalar_compare(
        builder: &mut FunctionBuilder<'_>,
        left: CraneliftValue,
        right: CraneliftValue,
        comparison: ScalarComparison,
    ) -> EmittedValue {
        let left_is_immediate = Self::emit_is_immediate(builder, left);
        let right_is_immediate = Self::emit_is_immediate(builder, right);
        let is_fast = builder.ins().band(left_is_immediate, right_is_immediate);

        let left_is_int = Self::emit_is_int(builder, left);
        let right_is_int = Self::emit_is_int(builder, right);
        let both_int = builder.ins().band(left_is_int, right_is_int);
        let left_is_float = Self::emit_is_float(builder, left);
        let right_is_float = Self::emit_is_float(builder, right);
        let both_float = builder.ins().band(left_is_float, right_is_float);
        let left_is_numeric = builder.ins().bor(left_is_int, left_is_float);
        let right_is_numeric = builder.ins().bor(right_is_int, right_is_float);
        let both_numeric = builder.ins().band(left_is_numeric, right_is_numeric);
        let same_numeric_kind = builder.ins().bor(both_int, both_float);
        let mixed_numeric = builder.ins().bxor(both_numeric, same_numeric_kind);
        let not_mixed_numeric = builder.ins().icmp_imm(IntCC::Equal, mixed_numeric, 0);
        let is_fast = builder.ins().band(is_fast, not_mixed_numeric);

        let left_tag = Self::emit_tag(builder, left);
        let right_tag = Self::emit_tag(builder, right);
        let left_is_empty_relation =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, left_tag, i64::from(VALUE_EMPTY_RELATION_TAG));
        let right_is_empty_relation =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, right_tag, i64::from(VALUE_EMPTY_RELATION_TAG));
        let relation_order_word = builder
            .ins()
            .iconst(types::I64, i64::from(VALUE_RELATION_TAG) << VALUE_TAG_SHIFT);
        let scalar_left = builder
            .ins()
            .select(left_is_empty_relation, relation_order_word, left);
        let scalar_right =
            builder
                .ins()
                .select(right_is_empty_relation, relation_order_word, right);
        let scalar_result = builder.ins().icmp(
            comparison.unsigned_condition_code(),
            scalar_left,
            scalar_right,
        );
        let left_int = Self::emit_unbox_int(builder, left);
        let right_int = Self::emit_unbox_int(builder, right);
        let int_result =
            builder
                .ins()
                .icmp(comparison.signed_condition_code(), left_int, right_int);
        let left_float = Self::emit_unbox_float(builder, left);
        let right_float = Self::emit_unbox_float(builder, right);
        let float_result =
            builder
                .ins()
                .fcmp(comparison.float_condition_code(), left_float, right_float);
        let result = builder.ins().select(both_int, int_result, scalar_result);
        let result = builder.ins().select(both_float, float_result, result);

        EmittedValue {
            word: Self::emit_bool(builder, result),
            is_fast,
        }
    }

    pub fn emit_truthy(builder: &mut FunctionBuilder<'_>, word: CraneliftValue) -> EmittedValue {
        let tag = Self::emit_tag(builder, word);
        let payload = Self::emit_payload(builder, word);
        let is_empty_relation =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_EMPTY_RELATION_TAG));
        let is_bool = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_BOOL_TAG));
        let is_list = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_LIST_TAG));
        let is_relation = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(VALUE_RELATION_TAG));
        let bool_payload = builder.ins().icmp_imm(IntCC::NotEqual, payload, 0);
        let truth = builder.ins().iconst(types::I8, 1);
        let false_value = builder.ins().iconst(types::I8, 0);
        let truth = builder.ins().select(is_bool, bool_payload, truth);
        let truth = builder.ins().select(is_empty_relation, false_value, truth);
        let is_collection = builder.ins().bor(is_list, is_relation);
        let is_fast = builder.ins().icmp_imm(IntCC::Equal, is_collection, 0);

        EmittedValue {
            word: Self::emit_bool(builder, truth),
            is_fast,
        }
    }

    pub fn emit_not(builder: &mut FunctionBuilder<'_>, word: CraneliftValue) -> EmittedValue {
        let truth = Self::emit_truthy(builder, word);
        let truth_payload = Self::emit_payload(builder, truth.word());
        let negated = builder.ins().icmp_imm(IntCC::Equal, truth_payload, 0);
        EmittedValue {
            word: Self::emit_bool(builder, negated),
            is_fast: truth.is_fast(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EmittedValue, FloatComparison, IntegerComparison, ScalarComparison, ValueEmitter};
    use cranelift_codegen::Context;
    use cranelift_codegen::ir::{
        AbiParam, InstBuilder, MemFlagsData, Signature, Value as CraneliftValue, types,
    };
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_jit::{JITBuilder, JITModule};
    use cranelift_module::{Linkage, Module, default_libcall_names};
    use mica_var::abi::{
        VALUE_EMPTY_RELATION_TAG, VALUE_INT_MAX, VALUE_INT_MIN, borrowed_value_bits,
        from_owned_value_bits, pack_value,
    };
    use mica_var::{Symbol, Value, ValueKind};

    const STATUS_COMPLETE: u32 = 0;
    const STATUS_SIDE_EXIT: u32 = 1;

    type ProbeFunction = unsafe extern "C" fn(u64, u64, *mut u64) -> u32;

    struct Probe {
        _module: JITModule,
        function: ProbeFunction,
    }

    impl Probe {
        fn compile(
            operation: impl FnOnce(
                &mut FunctionBuilder<'_>,
                CraneliftValue,
                CraneliftValue,
            ) -> EmittedValue,
        ) -> Self {
            let builder = JITBuilder::new(default_libcall_names()).unwrap();
            let mut module = JITModule::new(builder);
            let pointer_type = module.target_config().pointer_type();
            let mut signature = Signature::new(module.isa().default_call_conv());
            signature.params.push(AbiParam::new(types::I64));
            signature.params.push(AbiParam::new(types::I64));
            signature.params.push(AbiParam::new(pointer_type));
            signature.returns.push(AbiParam::new(types::I32));

            let function_id = module
                .declare_function("value_probe", Linkage::Local, &signature)
                .unwrap();
            let mut context = Context::new();
            context.func.signature = signature;
            let mut builder_context = FunctionBuilderContext::new();
            let mut function_builder =
                FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = function_builder.create_block();
            let complete = function_builder.create_block();
            let side_exit = function_builder.create_block();
            function_builder.append_block_params_for_function_params(entry);
            function_builder.append_block_param(complete, types::I64);

            function_builder.switch_to_block(entry);
            let params = function_builder.block_params(entry).to_vec();
            let result = operation(&mut function_builder, params[0], params[1]);
            function_builder.ins().brif(
                result.is_fast(),
                complete,
                &[result.word().into()],
                side_exit,
                &[],
            );

            function_builder.switch_to_block(complete);
            let result = function_builder.block_params(complete)[0];
            function_builder
                .ins()
                .store(MemFlagsData::new(), result, params[2], 0);
            let status = function_builder
                .ins()
                .iconst(types::I32, i64::from(STATUS_COMPLETE));
            function_builder.ins().return_(&[status]);

            function_builder.switch_to_block(side_exit);
            let status = function_builder
                .ins()
                .iconst(types::I32, i64::from(STATUS_SIDE_EXIT));
            function_builder.ins().return_(&[status]);
            function_builder.seal_all_blocks();
            function_builder.finalize();

            module.define_function(function_id, &mut context).unwrap();
            module.finalize_definitions().unwrap();
            let code = module.get_finalized_function(function_id);
            let function = unsafe { std::mem::transmute::<*const u8, ProbeFunction>(code) };
            Self {
                _module: module,
                function,
            }
        }

        fn run(&self, left: &Value, right: &Value) -> Option<Value> {
            let mut output = pack_value(VALUE_EMPTY_RELATION_TAG, 0);
            let status = unsafe {
                (self.function)(
                    borrowed_value_bits(left),
                    borrowed_value_bits(right),
                    &mut output,
                )
            };
            match status {
                STATUS_COMPLETE => Some(unsafe { from_owned_value_bits(output) }),
                STATUS_SIDE_EXIT => None,
                status => panic!("value probe returned unknown status {status}"),
            }
        }
    }

    #[test]
    fn emitted_checked_integer_add_matches_value_arithmetic() {
        let probe = Probe::compile(ValueEmitter::emit_checked_int_add);
        for left in [-1_000, -1, 0, 1, 1_000] {
            for right in [-31, -1, 0, 1, 31] {
                let left = Value::int(left).unwrap();
                let right = Value::int(right).unwrap();
                assert_eq!(probe.run(&left, &right), left.checked_add(&right));
            }
        }
        assert_eq!(
            probe.run(&Value::int(VALUE_INT_MAX).unwrap(), &Value::int(1).unwrap(),),
            None,
        );
        assert_eq!(
            probe.run(&Value::float(1.0).unwrap(), &Value::int(1).unwrap()),
            None
        );
    }

    #[test]
    fn emitted_checked_integer_subtract_and_multiply_match_value_arithmetic() {
        let subtract = Probe::compile(ValueEmitter::emit_checked_int_sub);
        let multiply = Probe::compile(ValueEmitter::emit_checked_int_mul);
        for left in [-1_000, -1, 0, 1, 1_000] {
            for right in [-31, -1, 0, 1, 31] {
                let left = Value::int(left).unwrap();
                let right = Value::int(right).unwrap();
                assert_eq!(subtract.run(&left, &right), left.checked_sub(&right));
                assert_eq!(multiply.run(&left, &right), left.checked_mul(&right));
            }
        }
        let max = Value::int(VALUE_INT_MAX).unwrap();
        assert_eq!(multiply.run(&max, &Value::int(2).unwrap()), None);
        assert_eq!(
            subtract.run(&Value::int(VALUE_INT_MIN).unwrap(), &Value::int(1).unwrap(),),
            None,
        );
        assert_eq!(
            subtract.run(&Value::float(1.0).unwrap(), &Value::int(1).unwrap()),
            None,
        );
    }

    #[test]
    fn emitted_checked_integer_divide_and_remainder_match_integer_results() {
        let divide = Probe::compile(ValueEmitter::emit_checked_int_div);
        let remainder = Probe::compile(ValueEmitter::emit_checked_int_rem);
        for left in [-1_000, -31, -1, 0, 1, 31, 1_000] {
            for right in [-31, -3, -1, 0, 1, 3, 31] {
                let left = Value::int(left).unwrap();
                let right = Value::int(right).unwrap();
                let expected_division = left
                    .checked_div(&right)
                    .filter(|value| value.as_int().is_some());
                assert_eq!(divide.run(&left, &right), expected_division);
                assert_eq!(remainder.run(&left, &right), left.checked_rem(&right));
            }
        }
        let min = Value::int(VALUE_INT_MIN).unwrap();
        assert_eq!(divide.run(&min, &Value::int(-1).unwrap()), None);
        assert_eq!(
            divide.run(&Value::float(6.0).unwrap(), &Value::int(3).unwrap()),
            None,
        );
        assert_eq!(
            remainder.run(&Value::float(6.0).unwrap(), &Value::int(3).unwrap()),
            None,
        );
    }

    #[test]
    fn emitted_checked_integer_negation_matches_value_arithmetic() {
        let negate =
            Probe::compile(|builder, value, _| ValueEmitter::emit_checked_int_neg(builder, value));
        for value in [VALUE_INT_MIN, -1_000, -1, 0, 1, 1_000, VALUE_INT_MAX] {
            let value = Value::int(value).unwrap();
            assert_eq!(
                negate.run(&value, &Value::empty_relation()),
                value.checked_neg()
            );
        }
        assert_eq!(
            negate.run(&Value::float(1.0).unwrap(), &Value::empty_relation()),
            None,
        );
    }

    #[test]
    fn emitted_checked_float_arithmetic_matches_value_arithmetic() {
        let add = Probe::compile(ValueEmitter::emit_checked_float_add);
        let subtract = Probe::compile(ValueEmitter::emit_checked_float_sub);
        let multiply = Probe::compile(ValueEmitter::emit_checked_float_mul);
        let divide = Probe::compile(ValueEmitter::emit_checked_float_div);

        for left in [-31.5f32, -1.0, -0.0, 0.0, 0.1, 1.5, 31.5] {
            for right in [-3.0f32, -0.5, 0.5, 2.0, 3.0] {
                let left = Value::float(left).unwrap();
                let right = Value::float(right).unwrap();
                assert_eq!(add.run(&left, &right), left.checked_add(&right));
                assert_eq!(subtract.run(&left, &right), left.checked_sub(&right));
                assert_eq!(multiply.run(&left, &right), left.checked_mul(&right));
                assert_eq!(divide.run(&left, &right), left.checked_div(&right));
            }
        }

        let max = Value::float(f32::MAX).unwrap();
        assert_eq!(multiply.run(&max, &max), None);
        assert_eq!(
            divide.run(&Value::float(1.0).unwrap(), &Value::float(0.0).unwrap()),
            None,
        );
        assert_eq!(
            add.run(&Value::int(1).unwrap(), &Value::float(1.0).unwrap()),
            None,
        );
    }

    #[test]
    fn emitted_checked_numeric_arithmetic_matches_value_semantics() {
        let add = Probe::compile(ValueEmitter::emit_checked_numeric_add);
        let subtract = Probe::compile(ValueEmitter::emit_checked_numeric_sub);
        let multiply = Probe::compile(ValueEmitter::emit_checked_numeric_mul);
        let cases = [
            (Value::int(7).unwrap(), Value::int(3).unwrap()),
            (Value::float(7.25).unwrap(), Value::float(0.5).unwrap()),
            (Value::int(7).unwrap(), Value::float(0.5).unwrap()),
            (Value::float(-2.5).unwrap(), Value::int(4).unwrap()),
            (Value::int(16_777_217).unwrap(), Value::float(1.0).unwrap()),
        ];
        for (left, right) in cases {
            assert_eq!(add.run(&left, &right), left.checked_add(&right));
            assert_eq!(subtract.run(&left, &right), left.checked_sub(&right));
            assert_eq!(multiply.run(&left, &right), left.checked_mul(&right));
        }

        let max_int = Value::int(VALUE_INT_MAX).unwrap();
        assert_eq!(add.run(&max_int, &Value::int(1).unwrap()), None);
        assert_eq!(multiply.run(&max_int, &Value::int(2).unwrap()), None);
        assert_eq!(
            add.run(&Value::string("not numeric"), &Value::int(1).unwrap()),
            None,
        );
        assert_eq!(
            multiply.run(
                &Value::float(f32::MAX).unwrap(),
                &Value::float(2.0).unwrap(),
            ),
            None,
        );
    }

    #[test]
    fn emitted_checked_numeric_negation_matches_value_semantics() {
        let negate = Probe::compile(|builder, value, _| {
            ValueEmitter::emit_checked_numeric_neg(builder, value)
        });
        for value in [
            Value::int(-7).unwrap(),
            Value::float(3.5).unwrap(),
            Value::float(0.0).unwrap(),
        ] {
            assert_eq!(
                negate.run(&value, &Value::empty_relation()),
                value.checked_neg()
            );
        }
        assert_eq!(
            negate.run(
                &Value::int(VALUE_INT_MIN).unwrap(),
                &Value::empty_relation()
            ),
            None,
        );
        assert_eq!(
            negate.run(&Value::string("not numeric"), &Value::empty_relation()),
            None,
        );
    }

    #[test]
    fn emitted_checked_numeric_division_matches_value_semantics() {
        let divide = Probe::compile(ValueEmitter::emit_checked_numeric_div);
        for (left, right) in [
            (Value::int(8).unwrap(), Value::int(2).unwrap()),
            (Value::int(7).unwrap(), Value::int(2).unwrap()),
            (Value::int(-7).unwrap(), Value::int(2).unwrap()),
            (Value::int(7).unwrap(), Value::float(2.0).unwrap()),
            (Value::float(7.5).unwrap(), Value::int(2).unwrap()),
        ] {
            assert_eq!(divide.run(&left, &right), left.checked_div(&right));
        }

        let min = Value::int(VALUE_INT_MIN).unwrap();
        assert_eq!(divide.run(&min, &Value::int(-1).unwrap()), None);
        assert_eq!(
            divide.run(&Value::int(1).unwrap(), &Value::int(0).unwrap()),
            None
        );
        assert_eq!(
            divide.run(&Value::int(1).unwrap(), &Value::float(0.0).unwrap()),
            None,
        );
        assert_eq!(
            divide.run(&Value::string("not numeric"), &Value::int(1).unwrap()),
            None,
        );
        assert_eq!(
            divide.run(
                &Value::float(f32::MAX).unwrap(),
                &Value::float(0.5).unwrap(),
            ),
            None,
        );
    }

    #[test]
    fn emitted_checked_float_negation_canonicalizes_zero() {
        let negate = Probe::compile(|builder, value, _| {
            ValueEmitter::emit_checked_float_neg(builder, value)
        });
        for value in [-31.5f32, -0.0, 0.0, 0.1, 31.5] {
            let value = Value::float(value).unwrap();
            assert_eq!(
                negate.run(&value, &Value::empty_relation()),
                value.checked_neg()
            );
        }
        assert_eq!(
            negate
                .run(&Value::float(0.0).unwrap(), &Value::empty_relation())
                .unwrap()
                .as_float()
                .unwrap()
                .to_bits(),
            0,
        );
        assert_eq!(
            negate.run(&Value::int(1).unwrap(), &Value::empty_relation()),
            None
        );
    }

    #[test]
    fn emitted_float_comparisons_match_language_numeric_comparison() {
        let comparisons = [
            FloatComparison::Equal,
            FloatComparison::NotEqual,
            FloatComparison::LessThan,
            FloatComparison::LessThanOrEqual,
            FloatComparison::GreaterThan,
            FloatComparison::GreaterThanOrEqual,
        ];
        for comparison in comparisons {
            let probe = Probe::compile(|builder, left, right| {
                ValueEmitter::emit_checked_float_compare(builder, left, right, comparison)
            });
            for left in [-31.5f32, -0.0, 0.0, 0.1, 1.5, 31.5] {
                for right in [-3.0f32, -0.0, 0.5, 2.0, 3.0] {
                    let left = Value::float(left).unwrap();
                    let right = Value::float(right).unwrap();
                    let ordering = mica_var::language_cmp::numeric_cmp(&left, &right);
                    let expected = match comparison {
                        FloatComparison::Equal => ordering.is_eq(),
                        FloatComparison::NotEqual => !ordering.is_eq(),
                        FloatComparison::LessThan => ordering.is_lt(),
                        FloatComparison::LessThanOrEqual => ordering.is_le(),
                        FloatComparison::GreaterThan => ordering.is_gt(),
                        FloatComparison::GreaterThanOrEqual => ordering.is_ge(),
                    };
                    assert_eq!(probe.run(&left, &right), Some(Value::bool(expected)));
                }
            }
            assert_eq!(
                probe.run(&Value::int(1).unwrap(), &Value::float(1.0).unwrap()),
                None,
            );
        }
    }

    #[test]
    fn emitted_immediate_equality_matches_value_equality() {
        let probe = Probe::compile(ValueEmitter::emit_immediate_eq);
        let values = [
            Value::empty_relation(),
            Value::bool(false),
            Value::bool(true),
            Value::int(-1).unwrap(),
            Value::int(0).unwrap(),
            Value::float(1.5).unwrap(),
            Value::identity_raw(1).unwrap(),
            Value::function_raw(1).unwrap(),
        ];
        for left in &values {
            for right in &values {
                let mixed_numeric = matches!(left.kind(), ValueKind::Int | ValueKind::Float)
                    && matches!(right.kind(), ValueKind::Int | ValueKind::Float)
                    && left.kind() != right.kind();
                let expected = (!mixed_numeric).then(|| Value::bool(left == right));
                assert_eq!(probe.run(left, right), expected);
            }
        }
        assert_eq!(probe.run(&Value::string("x"), &Value::string("x")), None);
    }

    #[test]
    fn emitted_scalar_comparisons_match_language_ordering() {
        let comparisons = [
            ScalarComparison::Equal,
            ScalarComparison::NotEqual,
            ScalarComparison::LessThan,
            ScalarComparison::LessThanOrEqual,
            ScalarComparison::GreaterThan,
            ScalarComparison::GreaterThanOrEqual,
        ];
        let values = [
            Value::empty_relation(),
            Value::bool(false),
            Value::bool(true),
            Value::int(-1).unwrap(),
            Value::int(1).unwrap(),
            Value::float(-1.5).unwrap(),
            Value::float(1.5).unwrap(),
            Value::identity_raw(7).unwrap(),
            Value::symbol(Symbol::intern("alpha")),
            Value::error_code(Symbol::intern("E_SCALAR")),
            Value::capability_raw(11).unwrap(),
            Value::function_raw(13).unwrap(),
        ];

        for comparison in comparisons {
            let probe = Probe::compile(|builder, left, right| {
                ValueEmitter::emit_scalar_compare(builder, left, right, comparison)
            });
            for left in &values {
                for right in &values {
                    let mixed_numeric = matches!(left.kind(), ValueKind::Int | ValueKind::Float)
                        && matches!(right.kind(), ValueKind::Int | ValueKind::Float)
                        && left.kind() != right.kind();
                    let ordering = mica_var::language_cmp::numeric_cmp(left, right);
                    let expected = match comparison {
                        ScalarComparison::Equal => ordering.is_eq(),
                        ScalarComparison::NotEqual => !ordering.is_eq(),
                        ScalarComparison::LessThan => ordering.is_lt(),
                        ScalarComparison::LessThanOrEqual => ordering.is_le(),
                        ScalarComparison::GreaterThan => ordering.is_gt(),
                        ScalarComparison::GreaterThanOrEqual => ordering.is_ge(),
                    };
                    assert_eq!(
                        probe.run(left, right),
                        (!mixed_numeric).then(|| Value::bool(expected)),
                    );
                }
            }
            assert_eq!(probe.run(&Value::string("a"), &values[0]), None);
        }
    }

    #[test]
    fn emitted_integer_comparison_matches_value_ordering() {
        let comparisons = [
            IntegerComparison::Equal,
            IntegerComparison::NotEqual,
            IntegerComparison::LessThan,
            IntegerComparison::LessThanOrEqual,
            IntegerComparison::GreaterThan,
            IntegerComparison::GreaterThanOrEqual,
        ];
        for comparison in comparisons {
            let probe = Probe::compile(|builder, left, right| {
                ValueEmitter::emit_checked_int_compare(builder, left, right, comparison)
            });
            for left in [-1_000, -1, 0, 1, 1_000] {
                for right in [-31, -1, 0, 1, 31] {
                    let left = Value::int(left).unwrap();
                    let right = Value::int(right).unwrap();
                    let expected = match comparison {
                        IntegerComparison::Equal => left == right,
                        IntegerComparison::NotEqual => left != right,
                        IntegerComparison::LessThan => left < right,
                        IntegerComparison::LessThanOrEqual => left <= right,
                        IntegerComparison::GreaterThan => left > right,
                        IntegerComparison::GreaterThanOrEqual => left >= right,
                    };
                    assert_eq!(probe.run(&left, &right), Some(Value::bool(expected)));
                }
            }
            assert_eq!(
                probe.run(&Value::float(1.0).unwrap(), &Value::int(1).unwrap()),
                None,
            );
        }
    }

    #[test]
    fn emitted_truthiness_matches_vm_fast_cases() {
        let probe = Probe::compile(|builder, value, _| ValueEmitter::emit_truthy(builder, value));
        let cases = [
            (Value::empty_relation(), false),
            (Value::bool(false), false),
            (Value::bool(true), true),
            (Value::int(0).unwrap(), true),
            (Value::float(0.0).unwrap(), true),
            (Value::string(""), true),
        ];
        for (value, expected) in cases {
            assert_eq!(
                probe.run(&value, &Value::empty_relation()),
                Some(Value::bool(expected)),
            );
        }
        assert_eq!(probe.run(&Value::list([]), &Value::empty_relation()), None,);
        assert_eq!(
            probe.run(
                &Value::relation([Symbol::intern("value")], []).unwrap(),
                &Value::empty_relation(),
            ),
            None,
        );
    }

    #[test]
    fn emitted_not_matches_vm_fast_cases() {
        let probe = Probe::compile(|builder, value, _| ValueEmitter::emit_not(builder, value));
        let cases = [
            (Value::empty_relation(), true),
            (Value::bool(false), true),
            (Value::bool(true), false),
            (Value::int(0).unwrap(), false),
            (Value::float(0.0).unwrap(), false),
            (Value::string(""), false),
        ];
        for (value, expected) in cases {
            assert_eq!(
                probe.run(&value, &Value::empty_relation()),
                Some(Value::bool(expected)),
            );
        }
        assert_eq!(probe.run(&Value::list([]), &Value::empty_relation()), None);
        assert_eq!(
            probe.run(
                &Value::relation([Symbol::intern("value")], []).unwrap(),
                &Value::empty_relation(),
            ),
            None,
        );
    }
}
