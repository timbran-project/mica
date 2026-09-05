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

use crate::{ScalarComparison, ValueEmitter};
use cranelift_codegen::Context;
use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature,
    condcodes::{FloatCC, IntCC},
    immediates::Ieee32,
    types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use mica_var::abi::{
    VALUE_ABI_VERSION, VALUE_EMPTY_RELATION_TAG, VALUE_FLOAT_TAG, VALUE_INT_MAX, VALUE_INT_MIN,
    VALUE_INT_TAG, VALUE_RELATION_TAG, borrowed_value_cmp, borrowed_value_numeric_cmp,
    borrowed_value_numeric_eq, value_is_immediate, value_tag,
};
use mica_var::{Value, ValueKind};
use std::cmp::Ordering;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::mem::{offset_of, size_of};
use std::ptr;
use std::sync::Mutex;

const STATUS_COMPLETE: u32 = 0;
const STATUS_BUDGET_EXHAUSTED: u32 = 1;
const STATUS_SIDE_EXIT: u32 = 2;
const NUMERIC_EQUAL_HELPER: &str = "mica_borrowed_value_numeric_equal";
const NUMERIC_COMPARE_HELPER: &str = "mica_borrowed_value_numeric_compare";
const CANONICAL_COMPARE_HELPER: &str = "mica_borrowed_value_canonical_compare";
const FLOAT_REMAINDER_HELPER: &str = "mica_f32_remainder";

unsafe extern "C" fn borrowed_value_numeric_equal(left: u64, right: u64) -> u64 {
    u64::from(unsafe { borrowed_value_numeric_eq(left, right) })
}

unsafe extern "C" fn borrowed_value_numeric_compare(left: u64, right: u64) -> i64 {
    ordering_code(unsafe { borrowed_value_numeric_cmp(left, right) })
}

unsafe extern "C" fn borrowed_value_canonical_compare(left: u64, right: u64) -> i64 {
    ordering_code(unsafe { borrowed_value_cmp(left, right) })
}

unsafe extern "C" fn f32_remainder(left: f32, right: f32) -> f32 {
    left % right
}

fn ordering_code(ordering: Ordering) -> i64 {
    match ordering {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

type NaturalLoopFunction = unsafe extern "C" fn(
    *mut u64,
    *const NaturalLoopCollectionView<'static>,
    u64,
    *mut u64,
    *mut u32,
    *mut u32,
) -> u32;

#[derive(Clone, Copy)]
struct ImportedHelpers {
    numeric_equal: Option<cranelift_codegen::ir::FuncRef>,
    numeric_compare: Option<cranelift_codegen::ir::FuncRef>,
    canonical_compare: Option<cranelift_codegen::ir::FuncRef>,
    float_remainder: Option<cranelift_codegen::ir::FuncRef>,
}

const COLLECTION_RANGE: u64 = 0;
const COLLECTION_LIST: u64 = 1;
const COLLECTION_MAP: u64 = 2;
static EMPTY_COLLECTION_WORD: u64 = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NaturalLoopCollectionView<'a> {
    kind: u64,
    len: u64,
    start: i64,
    key_base: *const u8,
    value_base: *const u8,
    stride: u64,
    owner: PhantomData<&'a Value>,
}

impl<'a> NaturalLoopCollectionView<'a> {
    pub fn range(start: i64, end: i64) -> Option<Self> {
        if !(VALUE_INT_MIN..=VALUE_INT_MAX).contains(&start)
            || !(VALUE_INT_MIN..=VALUE_INT_MAX).contains(&end)
        {
            return None;
        }
        let len = if end < start {
            0
        } else {
            u64::try_from(end.checked_sub(start)?.checked_add(1)?).ok()?
        };
        Some(Self {
            kind: COLLECTION_RANGE,
            len,
            start,
            ..Self::default()
        })
    }

    pub fn list(values: &'a [Value]) -> Self {
        let value_base = values.first().map_or_else(
            || ptr::from_ref(&EMPTY_COLLECTION_WORD).cast(),
            |value| ptr::from_ref(value).cast(),
        );
        Self {
            kind: COLLECTION_LIST,
            len: values.len() as u64,
            value_base,
            stride: size_of::<Value>() as u64,
            ..Self::default()
        }
    }

    pub fn map(entries: &'a [(Value, Value)]) -> Self {
        let key_base = entries.first().map_or_else(
            || ptr::from_ref(&EMPTY_COLLECTION_WORD).cast(),
            |entry| ptr::from_ref(&entry.0).cast(),
        );
        let value_base = entries.first().map_or_else(
            || ptr::from_ref(&EMPTY_COLLECTION_WORD).cast(),
            |entry| ptr::from_ref(&entry.1).cast(),
        );
        Self {
            kind: COLLECTION_MAP,
            len: entries.len() as u64,
            key_base,
            value_base,
            stride: size_of::<(Value, Value)>() as u64,
            ..Self::default()
        }
    }
}

impl Default for NaturalLoopCollectionView<'_> {
    fn default() -> Self {
        let empty = ptr::from_ref(&EMPTY_COLLECTION_WORD).cast();
        Self {
            kind: COLLECTION_RANGE,
            len: 0,
            start: 0,
            key_base: empty,
            value_base: empty,
            stride: 0,
            owner: PhantomData,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NaturalLoopInstruction {
    Load {
        dst: u16,
        value: u64,
    },
    Move {
        dst: u16,
        src: u16,
    },
    Negate {
        dst: u16,
        src: u16,
    },
    Not {
        dst: u16,
        src: u16,
    },
    Add {
        dst: u16,
        left: u16,
        right: u16,
    },
    Subtract {
        dst: u16,
        left: u16,
        right: u16,
    },
    Multiply {
        dst: u16,
        left: u16,
        right: u16,
    },
    Divide {
        dst: u16,
        left: u16,
        right: u16,
    },
    Remainder {
        dst: u16,
        left: u16,
        right: u16,
    },
    CheckKind {
        value: u16,
        expected: ValueKind,
    },
    Compare {
        dst: u16,
        comparison: ScalarComparison,
        left: u16,
        right: u16,
    },
    CollectionValueAt {
        dst: u16,
        view: u16,
        index: u16,
    },
    CollectionKeyAt {
        dst: u16,
        view: u16,
        index: u16,
    },
    IndexValue {
        dst: u16,
        view: u16,
        index: u16,
    },
    IndexValueImmediate {
        dst: u16,
        view: u16,
        index: u64,
    },
    Branch {
        condition: u16,
        if_true: u16,
        if_false: u16,
    },
    Jump {
        target: u16,
    },
}

impl NaturalLoopInstruction {
    fn slots(self) -> [Option<u16>; 3] {
        match self {
            Self::Load { dst, .. } => [Some(dst), None, None],
            Self::Move { dst, src } | Self::Negate { dst, src } | Self::Not { dst, src } => {
                [Some(dst), Some(src), None]
            }
            Self::Add { dst, left, right }
            | Self::Subtract { dst, left, right }
            | Self::Multiply { dst, left, right }
            | Self::Divide { dst, left, right }
            | Self::Remainder { dst, left, right }
            | Self::Compare {
                dst, left, right, ..
            } => [Some(dst), Some(left), Some(right)],
            Self::CheckKind { value, .. } => [Some(value), None, None],
            Self::CollectionValueAt { dst, index, .. }
            | Self::CollectionKeyAt { dst, index, .. }
            | Self::IndexValue { dst, index, .. } => [Some(dst), Some(index), None],
            Self::IndexValueImmediate { dst, .. } => [Some(dst), None, None],
            Self::Branch { condition, .. } => [Some(condition), None, None],
            Self::Jump { .. } => [None, None, None],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NaturalLoopPlan {
    slot_count: u16,
    collection_view_count: u16,
    entry: u16,
    instructions: Box<[NaturalLoopInstruction]>,
    unboxed_integer_slots: u32,
    unboxed_float_slots: u32,
    unboxed_boolean_slots: u32,
    entry_integer_slots: u32,
    entry_float_slots: u32,
}

impl NaturalLoopPlan {
    pub fn new(
        slot_count: u16,
        collection_view_count: u16,
        entry: u16,
        instructions: impl Into<Box<[NaturalLoopInstruction]>>,
    ) -> Result<Self, NaturalLoopError> {
        let instructions = instructions.into();
        if slot_count == 0 || slot_count > 32 || usize::from(entry) >= instructions.len() {
            return Err(NaturalLoopError(
                "natural loop has an invalid slot layout".to_owned(),
            ));
        }
        if instructions.is_empty() || instructions.len() > usize::from(u16::MAX) {
            return Err(NaturalLoopError(
                "natural loop instruction layout is invalid".to_owned(),
            ));
        }

        let exit = u16::try_from(instructions.len()).expect("instruction length checked above");
        for instruction in &instructions {
            for slot in instruction.slots().into_iter().flatten() {
                if slot >= slot_count {
                    return Err(NaturalLoopError(
                        "natural loop instruction references an invalid slot".to_owned(),
                    ));
                }
            }
            match *instruction {
                NaturalLoopInstruction::Load { value, .. } if !value_is_immediate(value) => {
                    return Err(NaturalLoopError(
                        "natural loop constants must use immediate values".to_owned(),
                    ));
                }
                NaturalLoopInstruction::Branch {
                    if_true, if_false, ..
                } if if_true > exit || if_false > exit => {
                    return Err(NaturalLoopError(
                        "natural loop branch target is outside the compiled region".to_owned(),
                    ));
                }
                NaturalLoopInstruction::Jump { target } if target > exit => {
                    return Err(NaturalLoopError(
                        "natural loop jump target is outside the compiled region".to_owned(),
                    ));
                }
                NaturalLoopInstruction::CollectionValueAt { view, .. }
                | NaturalLoopInstruction::CollectionKeyAt { view, .. }
                | NaturalLoopInstruction::IndexValue { view, .. }
                | NaturalLoopInstruction::IndexValueImmediate { view, .. }
                    if view >= collection_view_count =>
                {
                    return Err(NaturalLoopError(
                        "natural loop collection instruction references an invalid view".to_owned(),
                    ));
                }
                NaturalLoopInstruction::IndexValueImmediate { index, .. }
                    if !value_is_immediate(index) =>
                {
                    return Err(NaturalLoopError(
                        "natural loop immediate index must be an immediate value".to_owned(),
                    ));
                }
                NaturalLoopInstruction::CheckKind { expected, .. }
                    if !matches!(expected, ValueKind::Int | ValueKind::Float) =>
                {
                    return Err(NaturalLoopError(
                        "natural loop kind checks support only native numeric kinds".to_owned(),
                    ));
                }
                _ => {}
            }
        }
        Ok(Self {
            slot_count,
            collection_view_count,
            entry,
            instructions,
            unboxed_integer_slots: 0,
            unboxed_float_slots: 0,
            unboxed_boolean_slots: 0,
            entry_integer_slots: 0,
            entry_float_slots: 0,
        })
    }

    /// Keeps selected numeric slots as native Cranelift block state.
    ///
    /// This deliberately accepts only a single-header, single-backedge loop
    /// whose instructions have one stable numeric representation. Other plans
    /// retain the general tagged representation.
    pub fn with_unboxed_slots(
        mut self,
        integer_slots: u32,
        entry_integer_slots: u32,
        float_slots: u32,
        entry_float_slots: u32,
    ) -> Result<Self, NaturalLoopError> {
        let valid_slots = if self.slot_count == 32 {
            u32::MAX
        } else {
            (1_u32 << self.slot_count) - 1
        };
        let numeric_slots = integer_slots | float_slots;
        if numeric_slots == 0
            || numeric_slots & !valid_slots != 0
            || integer_slots & float_slots != 0
            || entry_integer_slots & !integer_slots != 0
            || entry_float_slots & !float_slots != 0
        {
            return Err(NaturalLoopError(
                "natural loop has an invalid unboxed slot layout".to_owned(),
            ));
        }
        let boolean_slots = self.validate_unboxed_instructions(
            integer_slots,
            entry_integer_slots,
            float_slots,
            entry_float_slots,
        )?;
        self.unboxed_integer_slots = integer_slots;
        self.unboxed_float_slots = float_slots;
        self.unboxed_boolean_slots = boolean_slots;
        self.entry_integer_slots = entry_integer_slots;
        self.entry_float_slots = entry_float_slots;
        Ok(self)
    }

    fn validate_unboxed_instructions(
        &self,
        integer_slots: u32,
        entry_integer_slots: u32,
        float_slots: u32,
        entry_float_slots: u32,
    ) -> Result<u32, NaturalLoopError> {
        let exit = u16::try_from(self.instructions.len()).expect("plan length is validated");
        let boolean_slots = self
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                NaturalLoopInstruction::Compare { dst, .. } => Some(1_u32 << dst),
                _ => None,
            })
            .fold(0_u32, |slots, slot| slots | slot);
        if boolean_slots & (integer_slots | float_slots) != 0 {
            return Err(NaturalLoopError(
                "natural loop has overlapping unboxed slot kinds".to_owned(),
            ));
        }
        let is_integer = |slot: u16| integer_slots & (1_u32 << slot) != 0;
        let is_float = |slot: u16| float_slots & (1_u32 << slot) != 0;
        let is_numeric = |slot: u16| is_integer(slot) || is_float(slot);
        let same_numeric_kind = |left: u16, right: u16| {
            (is_integer(left) && is_integer(right)) || (is_float(left) && is_float(right))
        };
        for (index, instruction) in self.instructions.iter().enumerate() {
            let valid = match *instruction {
                NaturalLoopInstruction::Load { dst, value } => match value_tag(value) {
                    VALUE_INT_TAG => is_integer(dst),
                    VALUE_FLOAT_TAG => is_float(dst),
                    _ => false,
                },
                NaturalLoopInstruction::Move { dst, src }
                | NaturalLoopInstruction::Negate { dst, src } => same_numeric_kind(dst, src),
                NaturalLoopInstruction::Add { dst, left, right }
                | NaturalLoopInstruction::Subtract { dst, left, right }
                | NaturalLoopInstruction::Multiply { dst, left, right } => {
                    is_numeric(left)
                        && is_numeric(right)
                        && if is_integer(left) && is_integer(right) {
                            is_integer(dst)
                        } else {
                            is_float(dst)
                        }
                }
                NaturalLoopInstruction::Divide { dst, left, right } => {
                    is_numeric(left)
                        && is_numeric(right)
                        && (is_float(left) || is_float(right))
                        && is_float(dst)
                }
                NaturalLoopInstruction::Compare {
                    dst, left, right, ..
                } => boolean_slots & (1_u32 << dst) != 0 && same_numeric_kind(left, right),
                NaturalLoopInstruction::CheckKind { value, expected } => match expected {
                    ValueKind::Int => is_integer(value),
                    ValueKind::Float => is_float(value),
                    _ => false,
                },
                NaturalLoopInstruction::CollectionValueAt { dst, index, .. }
                | NaturalLoopInstruction::CollectionKeyAt { dst, index, .. } => {
                    is_numeric(dst) && is_integer(index)
                }
                NaturalLoopInstruction::Branch {
                    condition,
                    if_true,
                    if_false,
                } => {
                    index.checked_add(1) == Some(usize::from(self.entry))
                        && if_true == self.entry
                        && if_false == exit
                        && boolean_slots & (1_u32 << condition) != 0
                }
                NaturalLoopInstruction::Jump { target } => {
                    index.checked_add(1) == Some(self.instructions.len()) && target == 0
                }
                _ => false,
            };
            if !valid {
                return Err(NaturalLoopError(
                    "natural loop cannot use the unboxed representation".to_owned(),
                ));
            }
        }

        let mut defined = entry_integer_slots | entry_float_slots;
        for index in
            (usize::from(self.entry)..self.instructions.len()).chain(0..usize::from(self.entry))
        {
            let instruction = self.instructions[index];
            let reads = match instruction {
                NaturalLoopInstruction::Move { src, .. }
                | NaturalLoopInstruction::Negate { src, .. } => 1_u32 << src,
                NaturalLoopInstruction::Add { left, right, .. }
                | NaturalLoopInstruction::Subtract { left, right, .. }
                | NaturalLoopInstruction::Multiply { left, right, .. }
                | NaturalLoopInstruction::Divide { left, right, .. }
                | NaturalLoopInstruction::Compare { left, right, .. } => {
                    (1_u32 << left) | (1_u32 << right)
                }
                NaturalLoopInstruction::CheckKind { value, .. } => 1_u32 << value,
                NaturalLoopInstruction::CollectionValueAt { index, .. }
                | NaturalLoopInstruction::CollectionKeyAt { index, .. } => 1_u32 << index,
                NaturalLoopInstruction::Branch { condition, .. } => 1_u32 << condition,
                _ => 0,
            };
            if reads & !defined != 0 {
                return Err(NaturalLoopError(
                    "unboxed slot is read before it is defined".to_owned(),
                ));
            }
            let write = match instruction {
                NaturalLoopInstruction::Load { dst, .. }
                | NaturalLoopInstruction::Move { dst, .. }
                | NaturalLoopInstruction::Negate { dst, .. }
                | NaturalLoopInstruction::Add { dst, .. }
                | NaturalLoopInstruction::Subtract { dst, .. }
                | NaturalLoopInstruction::Multiply { dst, .. }
                | NaturalLoopInstruction::Divide { dst, .. }
                | NaturalLoopInstruction::Compare { dst, .. } => 1_u32 << dst,
                NaturalLoopInstruction::CollectionValueAt { dst, .. }
                | NaturalLoopInstruction::CollectionKeyAt { dst, .. } => 1_u32 << dst,
                _ => 0,
            };
            defined |= write;
        }
        Ok(boolean_slots)
    }

    pub const fn slot_count(&self) -> u16 {
        self.slot_count
    }

    pub fn instructions(&self) -> &[NaturalLoopInstruction] {
        &self.instructions
    }

    pub const fn unboxed_integer_slots(&self) -> u32 {
        self.unboxed_integer_slots
    }

    pub const fn unboxed_float_slots(&self) -> u32 {
        self.unboxed_float_slots
    }

    pub const fn unboxed_boolean_slots(&self) -> u32 {
        self.unboxed_boolean_slots
    }

    fn requires_numeric_equal_helper(&self) -> bool {
        if self.unboxed_integer_slots | self.unboxed_float_slots != 0 {
            return false;
        }
        self.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                NaturalLoopInstruction::Compare {
                    comparison: ScalarComparison::Equal | ScalarComparison::NotEqual,
                    ..
                }
            )
        })
    }

    fn requires_numeric_compare_helper(&self) -> bool {
        if self.unboxed_integer_slots | self.unboxed_float_slots != 0 {
            return false;
        }
        self.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                NaturalLoopInstruction::Compare {
                    comparison: ScalarComparison::LessThan
                        | ScalarComparison::LessThanOrEqual
                        | ScalarComparison::GreaterThan
                        | ScalarComparison::GreaterThanOrEqual,
                    ..
                }
            )
        })
    }

    fn requires_canonical_compare_helper(&self) -> bool {
        self.instructions
            .iter()
            .any(|instruction| matches!(instruction, NaturalLoopInstruction::IndexValue { .. }))
    }

    fn requires_float_remainder_helper(&self) -> bool {
        self.instructions
            .iter()
            .any(|instruction| matches!(instruction, NaturalLoopInstruction::Remainder { .. }))
    }
}

#[derive(Debug)]
pub struct NaturalLoopError(String);

impl Display for NaturalLoopError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for NaturalLoopError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NaturalLoopOutcome {
    Complete {
        instructions: u64,
        modified_slots: u32,
    },
    BudgetExhausted {
        instructions: u64,
        resume: u16,
        modified_slots: u32,
    },
    SideExit,
}

#[derive(Clone, Copy)]
struct UnboxedSlots<'a> {
    integers: &'a [u16],
    floats: &'a [u16],
    booleans: &'a [u16],
}

#[derive(Clone, Copy)]
struct UnboxedState<'a> {
    integers: &'a [cranelift_codegen::ir::Value],
    floats: &'a [cranelift_codegen::ir::Value],
    booleans: &'a [cranelift_codegen::ir::Value],
}

/// Generated execution for a compiler-shaped natural loop.
///
/// The caller supplies scratch value words. Generated code may mutate that
/// scratch before a side exit, but never touches VM-owned values directly.
pub struct CompiledNaturalLoop {
    _module: Mutex<JITModule>,
    function: NaturalLoopFunction,
    slot_count: usize,
    collection_view_count: usize,
    code_size: usize,
    imported_helper_count: usize,
    value_abi_version: u32,
}

impl CompiledNaturalLoop {
    pub fn compile(plan: &NaturalLoopPlan) -> Result<Self, NaturalLoopError> {
        let mut jit_builder =
            JITBuilder::with_flags(&[("opt_level", "speed")], default_libcall_names()).map_err(
                |error| NaturalLoopError(format!("could not initialize Cranelift: {error}")),
            )?;
        if plan.requires_numeric_equal_helper() {
            jit_builder.symbol(
                NUMERIC_EQUAL_HELPER,
                borrowed_value_numeric_equal as *const u8,
            );
        }
        if plan.requires_numeric_compare_helper() {
            jit_builder.symbol(
                NUMERIC_COMPARE_HELPER,
                borrowed_value_numeric_compare as *const u8,
            );
        }
        if plan.requires_canonical_compare_helper() {
            jit_builder.symbol(
                CANONICAL_COMPARE_HELPER,
                borrowed_value_canonical_compare as *const u8,
            );
        }
        if plan.requires_float_remainder_helper() {
            jit_builder.symbol(FLOAT_REMAINDER_HELPER, f32_remainder as *const u8);
        }
        let mut module = JITModule::new(jit_builder);
        let pointer_type = module.target_config().pointer_type();
        let mut signature = Signature::new(module.isa().default_call_conv());
        signature.params.push(AbiParam::new(pointer_type));
        signature.params.push(AbiParam::new(pointer_type));
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(pointer_type));
        signature.params.push(AbiParam::new(pointer_type));
        signature.params.push(AbiParam::new(pointer_type));
        signature.returns.push(AbiParam::new(types::I32));

        let function_id = module
            .declare_function("mica_natural_loop", Linkage::Local, &signature)
            .map_err(|error| {
                NaturalLoopError(format!("could not declare natural loop: {error}"))
            })?;
        let numeric_equal_helper = if plan.requires_numeric_equal_helper() {
            let mut helper_signature = Signature::new(module.isa().default_call_conv());
            helper_signature.params.push(AbiParam::new(types::I64));
            helper_signature.params.push(AbiParam::new(types::I64));
            helper_signature.returns.push(AbiParam::new(types::I64));
            Some(
                module
                    .declare_function(NUMERIC_EQUAL_HELPER, Linkage::Import, &helper_signature)
                    .map_err(|error| {
                        NaturalLoopError(format!(
                            "could not declare numeric equality helper: {error}"
                        ))
                    })?,
            )
        } else {
            None
        };
        let numeric_compare_helper = if plan.requires_numeric_compare_helper() {
            let mut helper_signature = Signature::new(module.isa().default_call_conv());
            helper_signature.params.push(AbiParam::new(types::I64));
            helper_signature.params.push(AbiParam::new(types::I64));
            helper_signature.returns.push(AbiParam::new(types::I64));
            Some(
                module
                    .declare_function(NUMERIC_COMPARE_HELPER, Linkage::Import, &helper_signature)
                    .map_err(|error| {
                        NaturalLoopError(format!(
                            "could not declare numeric comparison helper: {error}"
                        ))
                    })?,
            )
        } else {
            None
        };
        let canonical_compare_helper = if plan.requires_canonical_compare_helper() {
            let mut helper_signature = Signature::new(module.isa().default_call_conv());
            helper_signature.params.push(AbiParam::new(types::I64));
            helper_signature.params.push(AbiParam::new(types::I64));
            helper_signature.returns.push(AbiParam::new(types::I64));
            Some(
                module
                    .declare_function(CANONICAL_COMPARE_HELPER, Linkage::Import, &helper_signature)
                    .map_err(|error| {
                        NaturalLoopError(format!(
                            "could not declare canonical comparison helper: {error}"
                        ))
                    })?,
            )
        } else {
            None
        };
        let float_remainder_helper = if plan.requires_float_remainder_helper() {
            let mut helper_signature = Signature::new(module.isa().default_call_conv());
            helper_signature.params.push(AbiParam::new(types::F32));
            helper_signature.params.push(AbiParam::new(types::F32));
            helper_signature.returns.push(AbiParam::new(types::F32));
            Some(
                module
                    .declare_function(FLOAT_REMAINDER_HELPER, Linkage::Import, &helper_signature)
                    .map_err(|error| {
                        NaturalLoopError(format!(
                            "could not declare float remainder helper: {error}"
                        ))
                    })?,
            )
        } else {
            None
        };
        let mut context = Context::new();
        context.func.signature = signature;
        let numeric_equal_helper = numeric_equal_helper
            .map(|helper| module.declare_func_in_func(helper, &mut context.func));
        let numeric_compare_helper = numeric_compare_helper
            .map(|helper| module.declare_func_in_func(helper, &mut context.func));
        let canonical_compare_helper = canonical_compare_helper
            .map(|helper| module.declare_func_in_func(helper, &mut context.func));
        let float_remainder_helper = float_remainder_helper
            .map(|helper| module.declare_func_in_func(helper, &mut context.func));
        let mut builder_context = FunctionBuilderContext::new();
        let helpers = ImportedHelpers {
            numeric_equal: numeric_equal_helper,
            numeric_compare: numeric_compare_helper,
            canonical_compare: canonical_compare_helper,
            float_remainder: float_remainder_helper,
        };
        Self::build_function(plan, &mut context, &mut builder_context, helpers);
        let imported_helper_count = context.func.dfg.ext_funcs.len();
        module
            .define_function(function_id, &mut context)
            .map_err(|error| {
                NaturalLoopError(format!("could not compile natural loop: {error:?}"))
            })?;
        let code_size = context
            .compiled_code()
            .map(|code| code.code_info().total_size as usize)
            .ok_or_else(|| NaturalLoopError("Cranelift did not retain compiled code".to_owned()))?;
        module.finalize_definitions().map_err(|error| {
            NaturalLoopError(format!("could not finalize natural loop: {error}"))
        })?;
        let code = module.get_finalized_function(function_id);
        let function = unsafe { std::mem::transmute::<*const u8, NaturalLoopFunction>(code) };
        Ok(Self {
            _module: Mutex::new(module),
            function,
            slot_count: usize::from(plan.slot_count),
            collection_view_count: usize::from(plan.collection_view_count),
            code_size,
            imported_helper_count,
            value_abi_version: VALUE_ABI_VERSION,
        })
    }

    fn build_function(
        plan: &NaturalLoopPlan,
        context: &mut Context,
        builder_context: &mut FunctionBuilderContext,
        helpers: ImportedHelpers,
    ) {
        if plan.unboxed_integer_slots | plan.unboxed_float_slots != 0 {
            Self::build_unboxed_function(plan, context, builder_context);
            return;
        }
        let mut builder = FunctionBuilder::new(&mut context.func, builder_context);
        let entry = builder.create_block();
        let complete = builder.create_block();
        let budget_exhausted = builder.create_block();
        let side_exit = builder.create_block();
        let instruction_blocks = plan
            .instructions
            .iter()
            .map(|_| builder.create_block())
            .collect::<Vec<_>>();
        let execute_blocks = plan
            .instructions
            .iter()
            .map(|_| builder.create_block())
            .collect::<Vec<_>>();
        let branch_dispatch = plan
            .instructions
            .iter()
            .map(|instruction| {
                matches!(instruction, NaturalLoopInstruction::Branch { .. })
                    .then(|| builder.create_block())
            })
            .collect::<Vec<_>>();

        builder.append_block_params_for_function_params(entry);
        builder.append_block_param(complete, types::I64);
        builder.append_block_param(complete, types::I32);
        builder.append_block_param(budget_exhausted, types::I64);
        builder.append_block_param(budget_exhausted, types::I32);
        builder.append_block_param(budget_exhausted, types::I32);
        for block in instruction_blocks.iter().chain(execute_blocks.iter()) {
            builder.append_block_param(*block, types::I64);
            builder.append_block_param(*block, types::I32);
        }
        for block in branch_dispatch.iter().flatten() {
            builder.append_block_param(*block, types::I64);
            builder.append_block_param(*block, types::I32);
            builder.append_block_param(*block, types::I8);
        }

        builder.switch_to_block(entry);
        let params = builder.block_params(entry).to_vec();
        let scratch = params[0];
        let collection_views = params[1];
        let instruction_budget = params[2];
        let instructions_out = params[3];
        let resume_out = params[4];
        let modified_slots_out = params[5];
        let zero_instructions = builder.ins().iconst(types::I64, 0);
        let zero_slots = builder.ins().iconst(types::I32, 0);
        builder.ins().jump(
            instruction_blocks[usize::from(plan.entry)],
            &[zero_instructions.into(), zero_slots.into()],
        );

        for (index, instruction) in plan.instructions.iter().copied().enumerate() {
            let instruction_block = instruction_blocks[index];
            let execute_block = execute_blocks[index];
            builder.switch_to_block(instruction_block);
            let instructions = builder.block_params(instruction_block)[0];
            let modified_slots = builder.block_params(instruction_block)[1];
            let has_budget =
                builder
                    .ins()
                    .icmp(IntCC::UnsignedLessThan, instructions, instruction_budget);
            let resume = builder.ins().iconst(types::I32, index as i64);
            builder.ins().brif(
                has_budget,
                execute_block,
                &[instructions.into(), modified_slots.into()],
                budget_exhausted,
                &[instructions.into(), resume.into(), modified_slots.into()],
            );

            builder.switch_to_block(execute_block);
            let instructions = builder.block_params(execute_block)[0];
            let modified_slots = builder.block_params(execute_block)[1];
            let one = builder.ins().iconst(types::I64, 1);
            let instructions = builder.ins().iadd(instructions, one);

            match instruction {
                NaturalLoopInstruction::Branch {
                    condition,
                    if_true,
                    if_false,
                } => {
                    let condition = Self::load_slot(&mut builder, scratch, condition);
                    let truth = ValueEmitter::emit_truthy(&mut builder, condition);
                    let truth_payload = ValueEmitter::emit_payload(&mut builder, truth.word());
                    let truth_value = builder.ins().icmp_imm(IntCC::NotEqual, truth_payload, 0);
                    let dispatch = branch_dispatch[index].expect("branch dispatch block");
                    builder.ins().brif(
                        truth.is_fast(),
                        dispatch,
                        &[
                            instructions.into(),
                            modified_slots.into(),
                            truth_value.into(),
                        ],
                        side_exit,
                        &[],
                    );

                    builder.switch_to_block(dispatch);
                    let instructions = builder.block_params(dispatch)[0];
                    let modified_slots = builder.block_params(dispatch)[1];
                    let truth_value = builder.block_params(dispatch)[2];
                    let if_true = Self::target_block(&instruction_blocks, complete, if_true);
                    let if_false = Self::target_block(&instruction_blocks, complete, if_false);
                    builder.ins().brif(
                        truth_value,
                        if_true,
                        &[instructions.into(), modified_slots.into()],
                        if_false,
                        &[instructions.into(), modified_slots.into()],
                    );
                }
                NaturalLoopInstruction::Jump { target } => {
                    let target = Self::target_block(&instruction_blocks, complete, target);
                    builder
                        .ins()
                        .jump(target, &[instructions.into(), modified_slots.into()]);
                }
                NaturalLoopInstruction::CheckKind { value, expected } => {
                    let word = Self::load_slot(&mut builder, scratch, value);
                    let is_fast = match expected {
                        ValueKind::Int => ValueEmitter::emit_is_int(&mut builder, word),
                        ValueKind::Float => ValueEmitter::emit_is_float(&mut builder, word),
                        _ => unreachable!("natural loop plan validates numeric checks"),
                    };
                    let next = Self::target_block(
                        &instruction_blocks,
                        complete,
                        u16::try_from(index + 1).expect("plan length validated"),
                    );
                    builder.ins().brif(
                        is_fast,
                        next,
                        &[instructions.into(), modified_slots.into()],
                        side_exit,
                        &[],
                    );
                }
                instruction => {
                    let (is_fast, dst) = Self::emit_instruction(
                        &mut builder,
                        scratch,
                        collection_views,
                        instruction,
                        helpers,
                    );
                    let slot_bit = 1_u32 << dst;
                    let slot_bit = builder.ins().iconst(types::I32, i64::from(slot_bit));
                    let modified_slots = builder.ins().bor(modified_slots, slot_bit);
                    let next = Self::target_block(
                        &instruction_blocks,
                        complete,
                        u16::try_from(index + 1).expect("plan length validated"),
                    );
                    builder.ins().brif(
                        is_fast,
                        next,
                        &[instructions.into(), modified_slots.into()],
                        side_exit,
                        &[],
                    );
                }
            }
        }

        builder.switch_to_block(complete);
        let instructions = builder.block_params(complete)[0];
        let modified_slots = builder.block_params(complete)[1];
        builder
            .ins()
            .store(MemFlagsData::new(), instructions, instructions_out, 0);
        builder
            .ins()
            .store(MemFlagsData::new(), modified_slots, modified_slots_out, 0);
        let status = builder.ins().iconst(types::I32, i64::from(STATUS_COMPLETE));
        builder.ins().return_(&[status]);

        builder.switch_to_block(budget_exhausted);
        let instructions = builder.block_params(budget_exhausted)[0];
        let resume = builder.block_params(budget_exhausted)[1];
        let modified_slots = builder.block_params(budget_exhausted)[2];
        builder
            .ins()
            .store(MemFlagsData::new(), instructions, instructions_out, 0);
        builder
            .ins()
            .store(MemFlagsData::new(), resume, resume_out, 0);
        builder
            .ins()
            .store(MemFlagsData::new(), modified_slots, modified_slots_out, 0);
        let status = builder
            .ins()
            .iconst(types::I32, i64::from(STATUS_BUDGET_EXHAUSTED));
        builder.ins().return_(&[status]);

        builder.switch_to_block(side_exit);
        let status = builder
            .ins()
            .iconst(types::I32, i64::from(STATUS_SIDE_EXIT));
        builder.ins().return_(&[status]);

        builder.seal_all_blocks();
        builder.finalize();
    }

    fn build_unboxed_function(
        plan: &NaturalLoopPlan,
        context: &mut Context,
        builder_context: &mut FunctionBuilderContext,
    ) {
        // The scratch ABI remains tagged. Entry values are guarded and
        // unboxed once, native state is threaded through Cranelift block
        // parameters, and modified slots are repacked only on a committed
        // completion or budget exit. Side exits discard this state atomically.
        let mut builder = FunctionBuilder::new(&mut context.func, builder_context);
        let entry = builder.create_block();
        let complete = builder.create_block();
        let budget_exhausted = builder.create_block();
        let side_exit = builder.create_block();
        let instruction_blocks = plan
            .instructions
            .iter()
            .map(|_| builder.create_block())
            .collect::<Vec<_>>();
        let execute_blocks = plan
            .instructions
            .iter()
            .map(|_| builder.create_block())
            .collect::<Vec<_>>();
        let integer_slots = (0..plan.slot_count)
            .filter(|slot| plan.unboxed_integer_slots & (1_u32 << slot) != 0)
            .collect::<Vec<_>>();
        let float_slots = (0..plan.slot_count)
            .filter(|slot| plan.unboxed_float_slots & (1_u32 << slot) != 0)
            .collect::<Vec<_>>();
        let boolean_slots = (0..plan.slot_count)
            .filter(|slot| plan.unboxed_boolean_slots & (1_u32 << slot) != 0)
            .collect::<Vec<_>>();
        let integer_index = |slot: u16| {
            integer_slots
                .iter()
                .position(|candidate| *candidate == slot)
                .expect("unboxed instruction references an integer slot")
        };
        let float_index = |slot: u16| {
            float_slots
                .iter()
                .position(|candidate| *candidate == slot)
                .expect("unboxed instruction references a float slot")
        };
        let boolean_index = |slot: u16| {
            boolean_slots
                .iter()
                .position(|candidate| *candidate == slot)
                .expect("unboxed instruction references a boolean slot")
        };

        builder.append_block_params_for_function_params(entry);
        for block in instruction_blocks.iter().chain(execute_blocks.iter()) {
            builder.append_block_param(*block, types::I64);
            builder.append_block_param(*block, types::I32);
            for _ in &integer_slots {
                builder.append_block_param(*block, types::I64);
            }
            for _ in &float_slots {
                builder.append_block_param(*block, types::F32);
            }
            for _ in &boolean_slots {
                builder.append_block_param(*block, types::I8);
            }
        }
        builder.append_block_param(complete, types::I64);
        builder.append_block_param(complete, types::I32);
        for _ in &integer_slots {
            builder.append_block_param(complete, types::I64);
        }
        for _ in &float_slots {
            builder.append_block_param(complete, types::F32);
        }
        for _ in &boolean_slots {
            builder.append_block_param(complete, types::I8);
        }
        builder.append_block_param(budget_exhausted, types::I64);
        builder.append_block_param(budget_exhausted, types::I32);
        builder.append_block_param(budget_exhausted, types::I32);
        for _ in &integer_slots {
            builder.append_block_param(budget_exhausted, types::I64);
        }
        for _ in &float_slots {
            builder.append_block_param(budget_exhausted, types::F32);
        }
        for _ in &boolean_slots {
            builder.append_block_param(budget_exhausted, types::I8);
        }

        builder.switch_to_block(entry);
        let params = builder.block_params(entry).to_vec();
        let scratch = params[0];
        let collection_views = params[1];
        let instruction_budget = params[2];
        let instructions_out = params[3];
        let resume_out = params[4];
        let modified_slots_out = params[5];
        let mut entry_is_fast = builder.ins().iconst(types::I8, 1);
        let integer_state = integer_slots
            .iter()
            .map(|slot| {
                if plan.entry_integer_slots & (1_u32 << slot) == 0 {
                    return builder.ins().iconst(types::I64, 0);
                }
                let word = Self::load_slot(&mut builder, scratch, *slot);
                let is_int = ValueEmitter::emit_is_int(&mut builder, word);
                entry_is_fast = builder.ins().band(entry_is_fast, is_int);
                ValueEmitter::emit_unbox_int(&mut builder, word)
            })
            .collect::<Vec<_>>();
        let float_state = float_slots
            .iter()
            .map(|slot| {
                if plan.entry_float_slots & (1_u32 << slot) == 0 {
                    return builder.ins().f32const(Ieee32::with_bits(0));
                }
                let word = Self::load_slot(&mut builder, scratch, *slot);
                let is_float = ValueEmitter::emit_is_float(&mut builder, word);
                entry_is_fast = builder.ins().band(entry_is_fast, is_float);
                ValueEmitter::emit_unbox_float(&mut builder, word)
            })
            .collect::<Vec<_>>();
        let boolean_state = boolean_slots
            .iter()
            .map(|_| builder.ins().iconst(types::I8, 0))
            .collect::<Vec<_>>();
        let zero_instructions = builder.ins().iconst(types::I64, 0);
        let zero_slots = builder.ins().iconst(types::I32, 0);
        let mut entry_args = vec![zero_instructions.into(), zero_slots.into()];
        Self::append_unboxed_state_args(
            &mut entry_args,
            &integer_state,
            &float_state,
            &boolean_state,
        );
        builder.ins().brif(
            entry_is_fast,
            instruction_blocks[usize::from(plan.entry)],
            &entry_args,
            side_exit,
            &[],
        );

        for (index, instruction) in plan.instructions.iter().copied().enumerate() {
            let instruction_block = instruction_blocks[index];
            let execute_block = execute_blocks[index];
            builder.switch_to_block(instruction_block);
            let block_params = builder.block_params(instruction_block).to_vec();
            let instructions = block_params[0];
            let modified_slots = block_params[1];
            let integer_end = 2 + integer_slots.len();
            let integer_state = block_params[2..integer_end].to_vec();
            let float_end = integer_end + float_slots.len();
            let float_state = block_params[integer_end..float_end].to_vec();
            let boolean_state = block_params[float_end..].to_vec();
            let has_budget =
                builder
                    .ins()
                    .icmp(IntCC::UnsignedLessThan, instructions, instruction_budget);
            let resume = builder.ins().iconst(types::I32, index as i64);
            let mut execute_args = vec![instructions.into(), modified_slots.into()];
            Self::append_unboxed_state_args(
                &mut execute_args,
                &integer_state,
                &float_state,
                &boolean_state,
            );
            let mut budget_args = vec![instructions.into(), resume.into(), modified_slots.into()];
            Self::append_unboxed_state_args(
                &mut budget_args,
                &integer_state,
                &float_state,
                &boolean_state,
            );
            builder.ins().brif(
                has_budget,
                execute_block,
                &execute_args,
                budget_exhausted,
                &budget_args,
            );

            builder.switch_to_block(execute_block);
            let block_params = builder.block_params(execute_block).to_vec();
            let instructions = block_params[0];
            let modified_slots = block_params[1];
            let integer_end = 2 + integer_slots.len();
            let mut integer_state = block_params[2..integer_end].to_vec();
            let float_end = integer_end + float_slots.len();
            let mut float_state = block_params[integer_end..float_end].to_vec();
            let mut boolean_state = block_params[float_end..].to_vec();
            let one = builder.ins().iconst(types::I64, 1);
            let instructions = builder.ins().iadd(instructions, one);

            match instruction {
                NaturalLoopInstruction::Branch {
                    condition,
                    if_true,
                    if_false,
                } => {
                    let mut target_args = vec![instructions.into(), modified_slots.into()];
                    Self::append_unboxed_state_args(
                        &mut target_args,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                    );
                    let if_true = Self::target_block(&instruction_blocks, complete, if_true);
                    let if_false = Self::target_block(&instruction_blocks, complete, if_false);
                    builder.ins().brif(
                        boolean_state[boolean_index(condition)],
                        if_true,
                        &target_args,
                        if_false,
                        &target_args,
                    );
                }
                NaturalLoopInstruction::Jump { target } => {
                    let target = Self::target_block(&instruction_blocks, complete, target);
                    let mut args = vec![instructions.into(), modified_slots.into()];
                    Self::append_unboxed_state_args(
                        &mut args,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                    );
                    builder.ins().jump(target, &args);
                }
                NaturalLoopInstruction::Load { dst, value } => {
                    let word = builder.ins().iconst(types::I64, value as i64);
                    if plan.unboxed_integer_slots & (1_u32 << dst) != 0 {
                        integer_state[integer_index(dst)] =
                            ValueEmitter::emit_unbox_int(&mut builder, word);
                    } else {
                        float_state[float_index(dst)] =
                            ValueEmitter::emit_unbox_float(&mut builder, word);
                    }
                    let is_fast = builder.ins().iconst(types::I8, 1);
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        Some(dst),
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                NaturalLoopInstruction::Move { dst, src } => {
                    if plan.unboxed_integer_slots & (1_u32 << dst) != 0 {
                        integer_state[integer_index(dst)] = integer_state[integer_index(src)];
                    } else {
                        float_state[float_index(dst)] = float_state[float_index(src)];
                    }
                    let is_fast = builder.ins().iconst(types::I8, 1);
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        Some(dst),
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                NaturalLoopInstruction::Negate { dst, src } => {
                    let is_fast = if plan.unboxed_integer_slots & (1_u32 << dst) != 0 {
                        let value = builder.ins().ineg(integer_state[integer_index(src)]);
                        integer_state[integer_index(dst)] = value;
                        Self::emit_unboxed_int_in_range(&mut builder, value)
                    } else {
                        let value = builder.ins().fneg(float_state[float_index(src)]);
                        let (value, _, is_finite) =
                            ValueEmitter::emit_canonical_float(&mut builder, value);
                        float_state[float_index(dst)] = value;
                        is_finite
                    };
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        Some(dst),
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                instruction @ (NaturalLoopInstruction::Add { .. }
                | NaturalLoopInstruction::Subtract { .. }
                | NaturalLoopInstruction::Multiply { .. }
                | NaturalLoopInstruction::Divide { .. }) => {
                    let (dst, left, right) = match instruction {
                        NaturalLoopInstruction::Add { dst, left, right }
                        | NaturalLoopInstruction::Subtract { dst, left, right }
                        | NaturalLoopInstruction::Multiply { dst, left, right }
                        | NaturalLoopInstruction::Divide { dst, left, right } => (dst, left, right),
                        _ => unreachable!(),
                    };
                    let is_fast = if plan.unboxed_integer_slots & (1_u32 << dst) != 0 {
                        let left = integer_state[integer_index(left)];
                        let right = integer_state[integer_index(right)];
                        let (value, no_overflow) = match instruction {
                            NaturalLoopInstruction::Add { .. } => {
                                (builder.ins().iadd(left, right), None)
                            }
                            NaturalLoopInstruction::Subtract { .. } => {
                                (builder.ins().isub(left, right), None)
                            }
                            NaturalLoopInstruction::Multiply { .. } => {
                                let (value, overflow) = builder.ins().smul_overflow(left, right);
                                let no_overflow = builder.ins().icmp_imm(IntCC::Equal, overflow, 0);
                                (value, Some(no_overflow))
                            }
                            _ => unreachable!("integer division is not an exact native kind"),
                        };
                        integer_state[integer_index(dst)] = value;
                        let in_range = Self::emit_unboxed_int_in_range(&mut builder, value);
                        no_overflow
                            .map(|guard| builder.ins().band(guard, in_range))
                            .unwrap_or(in_range)
                    } else {
                        let slots = UnboxedSlots {
                            integers: &integer_slots,
                            floats: &float_slots,
                            booleans: &boolean_slots,
                        };
                        let state = UnboxedState {
                            integers: &integer_state,
                            floats: &float_state,
                            booleans: &boolean_state,
                        };
                        let left =
                            Self::emit_unboxed_numeric_as_float(&mut builder, left, slots, state);
                        let right =
                            Self::emit_unboxed_numeric_as_float(&mut builder, right, slots, state);
                        let (value, divisor_is_nonzero) = match instruction {
                            NaturalLoopInstruction::Add { .. } => {
                                (builder.ins().fadd(left, right), None)
                            }
                            NaturalLoopInstruction::Subtract { .. } => {
                                (builder.ins().fsub(left, right), None)
                            }
                            NaturalLoopInstruction::Multiply { .. } => {
                                (builder.ins().fmul(left, right), None)
                            }
                            NaturalLoopInstruction::Divide { .. } => {
                                let zero = builder.ins().f32const(Ieee32::with_bits(0));
                                let nonzero = builder.ins().fcmp(FloatCC::NotEqual, right, zero);
                                let one =
                                    builder.ins().f32const(Ieee32::with_bits(1.0_f32.to_bits()));
                                let safe_right = builder.ins().select(nonzero, right, one);
                                (builder.ins().fdiv(left, safe_right), Some(nonzero))
                            }
                            _ => unreachable!(),
                        };
                        let (value, _, is_finite) =
                            ValueEmitter::emit_canonical_float(&mut builder, value);
                        float_state[float_index(dst)] = value;
                        divisor_is_nonzero
                            .map(|guard| builder.ins().band(guard, is_finite))
                            .unwrap_or(is_finite)
                    };
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        Some(dst),
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                NaturalLoopInstruction::Compare {
                    dst,
                    comparison,
                    left,
                    right,
                } => {
                    let predicate = if plan.unboxed_integer_slots & (1_u32 << left) != 0 {
                        builder.ins().icmp(
                            match comparison {
                                ScalarComparison::Equal => IntCC::Equal,
                                ScalarComparison::NotEqual => IntCC::NotEqual,
                                ScalarComparison::LessThan => IntCC::SignedLessThan,
                                ScalarComparison::LessThanOrEqual => IntCC::SignedLessThanOrEqual,
                                ScalarComparison::GreaterThan => IntCC::SignedGreaterThan,
                                ScalarComparison::GreaterThanOrEqual => {
                                    IntCC::SignedGreaterThanOrEqual
                                }
                            },
                            integer_state[integer_index(left)],
                            integer_state[integer_index(right)],
                        )
                    } else {
                        builder.ins().fcmp(
                            match comparison {
                                ScalarComparison::Equal => FloatCC::Equal,
                                ScalarComparison::NotEqual => FloatCC::NotEqual,
                                ScalarComparison::LessThan => FloatCC::LessThan,
                                ScalarComparison::LessThanOrEqual => FloatCC::LessThanOrEqual,
                                ScalarComparison::GreaterThan => FloatCC::GreaterThan,
                                ScalarComparison::GreaterThanOrEqual => FloatCC::GreaterThanOrEqual,
                            },
                            float_state[float_index(left)],
                            float_state[float_index(right)],
                        )
                    };
                    boolean_state[boolean_index(dst)] = predicate;
                    let is_fast = builder.ins().iconst(types::I8, 1);
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        Some(dst),
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                NaturalLoopInstruction::CheckKind { .. } => {
                    let is_fast = builder.ins().iconst(types::I8, 1);
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        None,
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                NaturalLoopInstruction::CollectionValueAt {
                    dst,
                    view,
                    index: ordinal,
                }
                | NaturalLoopInstruction::CollectionKeyAt {
                    dst,
                    view,
                    index: ordinal,
                } => {
                    let ordinal = integer_state[integer_index(ordinal)];
                    let key = matches!(instruction, NaturalLoopInstruction::CollectionKeyAt { .. });
                    let (word, collection_is_fast) = Self::emit_unboxed_collection_at(
                        &mut builder,
                        collection_views,
                        view,
                        ordinal,
                        key,
                    );
                    let kind_is_fast = if plan.unboxed_integer_slots & (1_u32 << dst) != 0 {
                        let is_int = ValueEmitter::emit_is_int(&mut builder, word);
                        integer_state[integer_index(dst)] =
                            ValueEmitter::emit_unbox_int(&mut builder, word);
                        is_int
                    } else {
                        let is_float = ValueEmitter::emit_is_float(&mut builder, word);
                        float_state[float_index(dst)] =
                            ValueEmitter::emit_unbox_float(&mut builder, word);
                        is_float
                    };
                    let is_fast = builder.ins().band(collection_is_fast, kind_is_fast);
                    Self::finish_unboxed_instruction(
                        &mut builder,
                        &instruction_blocks,
                        complete,
                        index,
                        Some(dst),
                        instructions,
                        modified_slots,
                        &integer_state,
                        &float_state,
                        &boolean_state,
                        is_fast,
                        side_exit,
                    );
                }
                _ => unreachable!("unboxed plans contain only supported instructions"),
            }
        }

        builder.switch_to_block(complete);
        let instructions = builder.block_params(complete)[0];
        let modified_slots = builder.block_params(complete)[1];
        let complete_params = builder.block_params(complete).to_vec();
        let integer_end = 2 + integer_slots.len();
        let integer_state = &complete_params[2..integer_end];
        let float_end = integer_end + float_slots.len();
        let float_state = &complete_params[integer_end..float_end];
        let boolean_state = &complete_params[float_end..];
        Self::flush_unboxed_state(
            &mut builder,
            scratch,
            UnboxedSlots {
                integers: &integer_slots,
                floats: &float_slots,
                booleans: &boolean_slots,
            },
            UnboxedState {
                integers: integer_state,
                floats: float_state,
                booleans: boolean_state,
            },
        );
        builder
            .ins()
            .store(MemFlagsData::new(), instructions, instructions_out, 0);
        builder
            .ins()
            .store(MemFlagsData::new(), modified_slots, modified_slots_out, 0);
        let status = builder.ins().iconst(types::I32, i64::from(STATUS_COMPLETE));
        builder.ins().return_(&[status]);

        builder.switch_to_block(budget_exhausted);
        let block_params = builder.block_params(budget_exhausted).to_vec();
        let instructions = block_params[0];
        let resume = block_params[1];
        let modified_slots = block_params[2];
        let integer_end = 3 + integer_slots.len();
        let integer_state = &block_params[3..integer_end];
        let float_end = integer_end + float_slots.len();
        let float_state = &block_params[integer_end..float_end];
        let boolean_state = &block_params[float_end..];
        Self::flush_unboxed_state(
            &mut builder,
            scratch,
            UnboxedSlots {
                integers: &integer_slots,
                floats: &float_slots,
                booleans: &boolean_slots,
            },
            UnboxedState {
                integers: integer_state,
                floats: float_state,
                booleans: boolean_state,
            },
        );
        builder
            .ins()
            .store(MemFlagsData::new(), instructions, instructions_out, 0);
        builder
            .ins()
            .store(MemFlagsData::new(), resume, resume_out, 0);
        builder
            .ins()
            .store(MemFlagsData::new(), modified_slots, modified_slots_out, 0);
        let status = builder
            .ins()
            .iconst(types::I32, i64::from(STATUS_BUDGET_EXHAUSTED));
        builder.ins().return_(&[status]);

        builder.switch_to_block(side_exit);
        let status = builder
            .ins()
            .iconst(types::I32, i64::from(STATUS_SIDE_EXIT));
        builder.ins().return_(&[status]);

        builder.seal_all_blocks();
        builder.finalize();
    }

    fn emit_unboxed_int_in_range(
        builder: &mut FunctionBuilder<'_>,
        value: cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value {
        let above_min =
            builder
                .ins()
                .icmp_imm(IntCC::SignedGreaterThanOrEqual, value, VALUE_INT_MIN);
        let below_max = builder
            .ins()
            .icmp_imm(IntCC::SignedLessThanOrEqual, value, VALUE_INT_MAX);
        builder.ins().band(above_min, below_max)
    }

    fn emit_unboxed_numeric_as_float(
        builder: &mut FunctionBuilder<'_>,
        slot: u16,
        slots: UnboxedSlots<'_>,
        state: UnboxedState<'_>,
    ) -> cranelift_codegen::ir::Value {
        if let Some(index) = slots
            .integers
            .iter()
            .position(|candidate| *candidate == slot)
        {
            return builder
                .ins()
                .fcvt_from_sint(types::F32, state.integers[index]);
        }
        state.floats[Self::unboxed_state_index(slots.floats, slot)]
    }

    fn unboxed_state_index(slots: &[u16], slot: u16) -> usize {
        slots
            .iter()
            .position(|candidate| *candidate == slot)
            .expect("validated unboxed instruction references typed state")
    }

    fn emit_unboxed_collection_at(
        builder: &mut FunctionBuilder<'_>,
        collection_views: cranelift_codegen::ir::Value,
        view: u16,
        index: cranelift_codegen::ir::Value,
        key: bool,
    ) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::Value) {
        let (kind, len, start, key_base, value_base, stride) =
            Self::load_collection_view(builder, collection_views, view);
        let index_is_nonnegative =
            builder
                .ins()
                .icmp_imm(IntCC::SignedGreaterThanOrEqual, index, 0);
        let index_is_bounded = builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let index_is_valid = builder.ins().band(index_is_nonnegative, index_is_bounded);
        let zero = builder.ins().iconst(types::I64, 0);
        let safe_index = builder.ins().select(index_is_valid, index, zero);
        let is_range = builder
            .ins()
            .icmp_imm(IntCC::Equal, kind, COLLECTION_RANGE as i64);
        let is_list = builder
            .ins()
            .icmp_imm(IntCC::Equal, kind, COLLECTION_LIST as i64);
        let is_map = builder
            .ins()
            .icmp_imm(IntCC::Equal, kind, COLLECTION_MAP as i64);
        let ordinal_kind = builder.ins().bor(is_range, is_list);
        let kind_is_valid = builder.ins().bor(ordinal_kind, is_map);
        let value = if key {
            let address_offset = builder.ins().imul(safe_index, stride);
            let address = builder.ins().iadd(key_base, address_offset);
            let map_key =
                builder
                    .ins()
                    .load(types::I64, MemFlagsData::new().with_readonly(), address, 0);
            let ordinal = ValueEmitter::emit_pack(builder, VALUE_INT_TAG, safe_index);
            builder.ins().select(is_map, map_key, ordinal)
        } else {
            let range_value = builder.ins().iadd(start, safe_index);
            let range_value = ValueEmitter::emit_pack(builder, VALUE_INT_TAG, range_value);
            let address_offset = builder.ins().imul(safe_index, stride);
            let address = builder.ins().iadd(value_base, address_offset);
            let collection_value =
                builder
                    .ins()
                    .load(types::I64, MemFlagsData::new().with_readonly(), address, 0);
            builder
                .ins()
                .select(is_range, range_value, collection_value)
        };
        let is_fast = builder.ins().band(index_is_valid, kind_is_valid);
        (value, is_fast)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_unboxed_instruction(
        builder: &mut FunctionBuilder<'_>,
        instruction_blocks: &[cranelift_codegen::ir::Block],
        complete: cranelift_codegen::ir::Block,
        index: usize,
        modified_slot: Option<u16>,
        instructions: cranelift_codegen::ir::Value,
        modified_slots: cranelift_codegen::ir::Value,
        integer_state: &[cranelift_codegen::ir::Value],
        float_state: &[cranelift_codegen::ir::Value],
        boolean_state: &[cranelift_codegen::ir::Value],
        is_fast: cranelift_codegen::ir::Value,
        side_exit: cranelift_codegen::ir::Block,
    ) {
        let modified_slots = modified_slot.map_or(modified_slots, |slot| {
            let slot_bit = builder.ins().iconst(types::I32, i64::from(1_u32 << slot));
            builder.ins().bor(modified_slots, slot_bit)
        });
        let next = Self::target_block(
            instruction_blocks,
            complete,
            u16::try_from(index + 1).expect("plan length validated"),
        );
        let mut args = vec![instructions.into(), modified_slots.into()];
        Self::append_unboxed_state_args(&mut args, integer_state, float_state, boolean_state);
        builder.ins().brif(is_fast, next, &args, side_exit, &[]);
    }

    fn append_unboxed_state_args(
        args: &mut Vec<cranelift_codegen::ir::BlockArg>,
        integer_state: &[cranelift_codegen::ir::Value],
        float_state: &[cranelift_codegen::ir::Value],
        boolean_state: &[cranelift_codegen::ir::Value],
    ) {
        args.extend(
            integer_state
                .iter()
                .chain(float_state)
                .chain(boolean_state)
                .copied()
                .map(cranelift_codegen::ir::BlockArg::from),
        );
    }

    fn flush_unboxed_state(
        builder: &mut FunctionBuilder<'_>,
        scratch: cranelift_codegen::ir::Value,
        slots: UnboxedSlots<'_>,
        state: UnboxedState<'_>,
    ) {
        for (slot, value) in slots
            .integers
            .iter()
            .copied()
            .zip(state.integers.iter().copied())
        {
            let value = ValueEmitter::emit_pack(builder, VALUE_INT_TAG, value);
            Self::store_slot(builder, scratch, slot, value);
        }
        for (slot, value) in slots
            .floats
            .iter()
            .copied()
            .zip(state.floats.iter().copied())
        {
            let value = ValueEmitter::emit_pack_checked_float(builder, value).word();
            Self::store_slot(builder, scratch, slot, value);
        }
        for (slot, value) in slots
            .booleans
            .iter()
            .copied()
            .zip(state.booleans.iter().copied())
        {
            let value = ValueEmitter::emit_bool(builder, value);
            Self::store_slot(builder, scratch, slot, value);
        }
    }

    fn target_block(
        instruction_blocks: &[cranelift_codegen::ir::Block],
        complete: cranelift_codegen::ir::Block,
        target: u16,
    ) -> cranelift_codegen::ir::Block {
        instruction_blocks
            .get(usize::from(target))
            .copied()
            .unwrap_or(complete)
    }

    fn emit_instruction(
        builder: &mut FunctionBuilder<'_>,
        scratch: cranelift_codegen::ir::Value,
        collection_views: cranelift_codegen::ir::Value,
        instruction: NaturalLoopInstruction,
        helpers: ImportedHelpers,
    ) -> (cranelift_codegen::ir::Value, u16) {
        match instruction {
            NaturalLoopInstruction::Load { dst, value } => {
                let value = builder.ins().iconst(types::I64, value as i64);
                Self::store_slot(builder, scratch, dst, value);
                let is_fast = builder.ins().iconst(types::I8, 1);
                (is_fast, dst)
            }
            NaturalLoopInstruction::Move { dst, src } => {
                let value = Self::load_slot(builder, scratch, src);
                Self::store_slot(builder, scratch, dst, value);
                (ValueEmitter::emit_is_immediate(builder, value), dst)
            }
            NaturalLoopInstruction::Negate { dst, src } => {
                let value = Self::load_slot(builder, scratch, src);
                let result = ValueEmitter::emit_checked_numeric_neg(builder, value);
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Not { dst, src } => {
                let value = Self::load_slot(builder, scratch, src);
                let result = ValueEmitter::emit_not(builder, value);
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Add { dst, left, right } => {
                let left = Self::load_slot(builder, scratch, left);
                let right = Self::load_slot(builder, scratch, right);
                let result = ValueEmitter::emit_checked_numeric_add(builder, left, right);
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Subtract { dst, left, right } => {
                let left = Self::load_slot(builder, scratch, left);
                let right = Self::load_slot(builder, scratch, right);
                let result = ValueEmitter::emit_checked_numeric_sub(builder, left, right);
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Multiply { dst, left, right } => {
                let left = Self::load_slot(builder, scratch, left);
                let right = Self::load_slot(builder, scratch, right);
                let result = ValueEmitter::emit_checked_numeric_mul(builder, left, right);
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Divide { dst, left, right } => {
                let left = Self::load_slot(builder, scratch, left);
                let right = Self::load_slot(builder, scratch, right);
                let result = ValueEmitter::emit_checked_numeric_div(builder, left, right);
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Remainder { dst, left, right } => {
                let left = Self::load_slot(builder, scratch, left);
                let right = Self::load_slot(builder, scratch, right);
                let result = ValueEmitter::emit_checked_numeric_rem(
                    builder,
                    left,
                    right,
                    helpers
                        .float_remainder
                        .expect("remainder plans import their helper"),
                );
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::Compare {
                dst,
                comparison,
                left,
                right,
            } => {
                let left = Self::load_slot(builder, scratch, left);
                let right = Self::load_slot(builder, scratch, right);
                let result = ValueEmitter::emit_scalar_compare(builder, left, right, comparison);
                let result = Self::emit_numeric_equal_fallback(
                    builder,
                    left,
                    right,
                    comparison,
                    result,
                    helpers.numeric_equal,
                );
                let result = Self::emit_numeric_compare_fallback(
                    builder,
                    left,
                    right,
                    comparison,
                    result,
                    helpers.numeric_compare,
                );
                Self::store_slot(builder, scratch, dst, result.word());
                (result.is_fast(), dst)
            }
            NaturalLoopInstruction::CollectionValueAt { dst, view, index } => {
                let index_word = Self::load_slot(builder, scratch, index);
                let index_is_int = ValueEmitter::emit_is_int(builder, index_word);
                let index = ValueEmitter::emit_unbox_int(builder, index_word);
                let (kind, len, start, _, value_base, stride) =
                    Self::load_collection_view(builder, collection_views, view);
                let index_is_nonnegative =
                    builder
                        .ins()
                        .icmp_imm(IntCC::SignedGreaterThanOrEqual, index, 0);
                let index_is_bounded = builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
                let index_is_valid = builder.ins().band(index_is_int, index_is_nonnegative);
                let index_is_valid = builder.ins().band(index_is_valid, index_is_bounded);
                let zero = builder.ins().iconst(types::I64, 0);
                let safe_index = builder.ins().select(index_is_valid, index, zero);

                let range_value = builder.ins().iadd(start, safe_index);
                let range_value = ValueEmitter::emit_pack(builder, VALUE_INT_TAG, range_value);
                let address_offset = builder.ins().imul(safe_index, stride);
                let address = builder.ins().iadd(value_base, address_offset);
                let collection_value =
                    builder
                        .ins()
                        .load(types::I64, MemFlagsData::new().with_readonly(), address, 0);
                let is_range = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, COLLECTION_RANGE as i64);
                let is_list = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, COLLECTION_LIST as i64);
                let is_map = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, COLLECTION_MAP as i64);
                let kind_is_valid = builder.ins().bor(is_range, is_list);
                let kind_is_valid = builder.ins().bor(kind_is_valid, is_map);
                let value = builder
                    .ins()
                    .select(is_range, range_value, collection_value);
                let is_fast = builder.ins().band(index_is_valid, kind_is_valid);
                Self::store_slot(builder, scratch, dst, value);
                (is_fast, dst)
            }
            NaturalLoopInstruction::CollectionKeyAt { dst, view, index } => {
                let index_word = Self::load_slot(builder, scratch, index);
                let index_is_int = ValueEmitter::emit_is_int(builder, index_word);
                let index = ValueEmitter::emit_unbox_int(builder, index_word);
                let (kind, len, _, key_base, _, stride) =
                    Self::load_collection_view(builder, collection_views, view);
                let index_is_nonnegative =
                    builder
                        .ins()
                        .icmp_imm(IntCC::SignedGreaterThanOrEqual, index, 0);
                let index_is_bounded = builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
                let index_is_valid = builder.ins().band(index_is_int, index_is_nonnegative);
                let index_is_valid = builder.ins().band(index_is_valid, index_is_bounded);
                let zero = builder.ins().iconst(types::I64, 0);
                let safe_index = builder.ins().select(index_is_valid, index, zero);
                let address_offset = builder.ins().imul(safe_index, stride);
                let address = builder.ins().iadd(key_base, address_offset);
                let map_key =
                    builder
                        .ins()
                        .load(types::I64, MemFlagsData::new().with_readonly(), address, 0);
                let is_range = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, COLLECTION_RANGE as i64);
                let is_list = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, COLLECTION_LIST as i64);
                let is_map = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, COLLECTION_MAP as i64);
                let ordinal_kind = builder.ins().bor(is_range, is_list);
                let kind_is_valid = builder.ins().bor(ordinal_kind, is_map);
                let value = builder.ins().select(is_map, map_key, index_word);
                let is_fast = builder.ins().band(index_is_valid, kind_is_valid);
                Self::store_slot(builder, scratch, dst, value);
                (is_fast, dst)
            }
            NaturalLoopInstruction::IndexValue { dst, view, index } => {
                let index = Self::load_slot(builder, scratch, index);
                let view = Self::load_collection_view(builder, collection_views, view);
                let (value, is_fast) =
                    Self::emit_index_value(builder, index, view, helpers.canonical_compare);
                Self::store_slot(builder, scratch, dst, value);
                (is_fast, dst)
            }
            NaturalLoopInstruction::IndexValueImmediate { dst, view, index } => {
                let index = builder.ins().iconst(types::I64, index as i64);
                let view = Self::load_collection_view(builder, collection_views, view);
                let (value, is_fast) = Self::emit_index_value(builder, index, view, None);
                Self::store_slot(builder, scratch, dst, value);
                (is_fast, dst)
            }
            NaturalLoopInstruction::CheckKind { .. }
            | NaturalLoopInstruction::Branch { .. }
            | NaturalLoopInstruction::Jump { .. } => {
                unreachable!("control-flow instructions are emitted by build_function")
            }
        }
    }

    fn emit_numeric_equal_fallback(
        builder: &mut FunctionBuilder<'_>,
        left: cranelift_codegen::ir::Value,
        right: cranelift_codegen::ir::Value,
        comparison: ScalarComparison,
        immediate: crate::EmittedValue,
        numeric_equal_helper: Option<cranelift_codegen::ir::FuncRef>,
    ) -> crate::EmittedValue {
        if !matches!(
            comparison,
            ScalarComparison::Equal | ScalarComparison::NotEqual
        ) {
            return immediate;
        }

        let helper = numeric_equal_helper.expect("equality plans import their helper");
        let call_helper = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.ins().brif(
            immediate.is_fast(),
            done,
            &[immediate.word().into()],
            call_helper,
            &[],
        );

        builder.switch_to_block(call_helper);
        let call = builder.ins().call(helper, &[left, right]);
        let equal = builder.inst_results(call)[0];
        let result = match comparison {
            ScalarComparison::Equal => builder.ins().icmp_imm(IntCC::NotEqual, equal, 0),
            ScalarComparison::NotEqual => builder.ins().icmp_imm(IntCC::Equal, equal, 0),
            _ => unreachable!("only equality comparisons use the helper"),
        };
        let result = ValueEmitter::emit_bool(builder, result);
        builder.ins().jump(done, &[result.into()]);

        builder.switch_to_block(done);
        let word = builder.block_params(done)[0];
        crate::EmittedValue::always_fast(builder, word)
    }

    fn emit_numeric_compare_fallback(
        builder: &mut FunctionBuilder<'_>,
        left: cranelift_codegen::ir::Value,
        right: cranelift_codegen::ir::Value,
        comparison: ScalarComparison,
        immediate: crate::EmittedValue,
        numeric_compare_helper: Option<cranelift_codegen::ir::FuncRef>,
    ) -> crate::EmittedValue {
        if matches!(
            comparison,
            ScalarComparison::Equal | ScalarComparison::NotEqual
        ) {
            return immediate;
        }

        let helper = numeric_compare_helper.expect("ordering plans import their helper");
        let call_helper = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.ins().brif(
            immediate.is_fast(),
            done,
            &[immediate.word().into()],
            call_helper,
            &[],
        );

        builder.switch_to_block(call_helper);
        let call = builder.ins().call(helper, &[left, right]);
        let ordering = builder.inst_results(call)[0];
        let result = match comparison {
            ScalarComparison::LessThan => {
                builder.ins().icmp_imm(IntCC::SignedLessThan, ordering, 0)
            }
            ScalarComparison::LessThanOrEqual => {
                builder
                    .ins()
                    .icmp_imm(IntCC::SignedLessThanOrEqual, ordering, 0)
            }
            ScalarComparison::GreaterThan => {
                builder
                    .ins()
                    .icmp_imm(IntCC::SignedGreaterThan, ordering, 0)
            }
            ScalarComparison::GreaterThanOrEqual => {
                builder
                    .ins()
                    .icmp_imm(IntCC::SignedGreaterThanOrEqual, ordering, 0)
            }
            ScalarComparison::Equal | ScalarComparison::NotEqual => {
                unreachable!("equality comparisons use their equality helper")
            }
        };
        let result = ValueEmitter::emit_bool(builder, result);
        builder.ins().jump(done, &[result.into()]);

        builder.switch_to_block(done);
        let word = builder.block_params(done)[0];
        crate::EmittedValue::always_fast(builder, word)
    }

    fn load_collection_view(
        builder: &mut FunctionBuilder<'_>,
        collection_views: cranelift_codegen::ir::Value,
        view: u16,
    ) -> (
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) {
        let view_offset = i32::from(view) * size_of::<NaturalLoopCollectionView>() as i32;
        let load = |builder: &mut FunctionBuilder<'_>, field_offset: usize| {
            builder.ins().load(
                types::I64,
                MemFlagsData::new().with_readonly(),
                collection_views,
                view_offset + field_offset as i32,
            )
        };
        (
            load(builder, offset_of!(NaturalLoopCollectionView, kind)),
            load(builder, offset_of!(NaturalLoopCollectionView, len)),
            load(builder, offset_of!(NaturalLoopCollectionView, start)),
            load(builder, offset_of!(NaturalLoopCollectionView, key_base)),
            load(builder, offset_of!(NaturalLoopCollectionView, value_base)),
            load(builder, offset_of!(NaturalLoopCollectionView, stride)),
        )
    }

    /// Emits list ordinal lookup or canonical map-key binary search.
    ///
    /// Immediate map keys follow `Value::cmp` entirely in generated code: kinds
    /// order by tag, integers compare signed, finite canonical binary32 values
    /// compare numerically, and the remaining immediate kinds compare by
    /// payload. Dynamic heap keys use the borrowed canonical comparison helper
    /// at each binary-search probe.
    fn emit_index_value(
        builder: &mut FunctionBuilder<'_>,
        index_word: cranelift_codegen::ir::Value,
        view: (
            cranelift_codegen::ir::Value,
            cranelift_codegen::ir::Value,
            cranelift_codegen::ir::Value,
            cranelift_codegen::ir::Value,
            cranelift_codegen::ir::Value,
            cranelift_codegen::ir::Value,
        ),
        canonical_compare_helper: Option<cranelift_codegen::ir::FuncRef>,
    ) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::Value) {
        let list = builder.create_block();
        let map_check = builder.create_block();
        let map_loop = builder.create_block();
        let map_probe = builder.create_block();
        let map_helper_compare = canonical_compare_helper.map(|_| builder.create_block());
        let map_compare_done = canonical_compare_helper.map(|_| builder.create_block());
        let map_update = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(map_loop, types::I64);
        builder.append_block_param(map_loop, types::I64);
        builder.append_block_param(map_probe, types::I64);
        builder.append_block_param(map_probe, types::I64);
        if let Some(map_compare_done) = map_compare_done {
            builder.append_block_param(map_compare_done, types::I8);
            builder.append_block_param(map_compare_done, types::I8);
        }
        builder.append_block_param(map_update, types::I64);
        builder.append_block_param(map_update, types::I64);
        builder.append_block_param(map_update, types::I64);
        builder.append_block_param(map_update, types::I8);
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I8);

        let (kind, len, _, key_base, value_base, stride) = view;
        let zero = builder.ins().iconst(types::I64, 0);
        let false_value = builder.ins().iconst(types::I8, 0);
        let true_value = builder.ins().iconst(types::I8, 1);
        let is_list = builder
            .ins()
            .icmp_imm(IntCC::Equal, kind, COLLECTION_LIST as i64);
        builder.ins().brif(is_list, list, &[], map_check, &[]);

        builder.switch_to_block(list);
        let index_is_int = ValueEmitter::emit_is_int(builder, index_word);
        let index = ValueEmitter::emit_unbox_int(builder, index_word);
        let index_is_nonnegative =
            builder
                .ins()
                .icmp_imm(IntCC::SignedGreaterThanOrEqual, index, 0);
        let index_is_bounded = builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let index_is_valid = builder.ins().band(index_is_int, index_is_nonnegative);
        let index_is_valid = builder.ins().band(index_is_valid, index_is_bounded);
        let safe_index = builder.ins().select(index_is_valid, index, zero);
        let address_offset = builder.ins().imul(safe_index, stride);
        let address = builder.ins().iadd(value_base, address_offset);
        let value = builder
            .ins()
            .load(types::I64, MemFlagsData::new().with_readonly(), address, 0);
        builder
            .ins()
            .jump(done, &[value.into(), index_is_valid.into()]);

        builder.switch_to_block(map_check);
        let is_map = builder
            .ins()
            .icmp_imm(IntCC::Equal, kind, COLLECTION_MAP as i64);
        let index_is_immediate = ValueEmitter::emit_is_immediate(builder, index_word);
        let can_search = if canonical_compare_helper.is_some() {
            is_map
        } else {
            builder.ins().band(is_map, index_is_immediate)
        };
        builder.ins().brif(
            can_search,
            map_loop,
            &[zero.into(), len.into()],
            done,
            &[zero.into(), false_value.into()],
        );

        builder.switch_to_block(map_loop);
        let lower = builder.block_params(map_loop)[0];
        let upper = builder.block_params(map_loop)[1];
        let has_candidate = builder.ins().icmp(IntCC::UnsignedLessThan, lower, upper);
        builder.ins().brif(
            has_candidate,
            map_probe,
            &[lower.into(), upper.into()],
            done,
            &[zero.into(), false_value.into()],
        );

        builder.switch_to_block(map_probe);
        let lower = builder.block_params(map_probe)[0];
        let upper = builder.block_params(map_probe)[1];
        let width = builder.ins().isub(upper, lower);
        let half_width = builder.ins().ushr_imm(width, 1);
        let middle = builder.ins().iadd(lower, half_width);
        let address_offset = builder.ins().imul(middle, stride);
        let key_address = builder.ins().iadd(key_base, address_offset);
        let value_address = builder.ins().iadd(value_base, address_offset);
        let entry_key = builder.ins().load(
            types::I64,
            MemFlagsData::new().with_readonly(),
            key_address,
            0,
        );
        let entry_value = builder.ins().load(
            types::I64,
            MemFlagsData::new().with_readonly(),
            value_address,
            0,
        );
        let entry_tag = ValueEmitter::emit_tag(builder, entry_key);
        let index_tag = ValueEmitter::emit_tag(builder, index_word);
        let entry_is_empty_relation =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, entry_tag, i64::from(VALUE_EMPTY_RELATION_TAG));
        let index_is_empty_relation =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, index_tag, i64::from(VALUE_EMPTY_RELATION_TAG));
        let relation_tag = builder
            .ins()
            .iconst(types::I64, i64::from(VALUE_RELATION_TAG));
        let entry_order_tag =
            builder
                .ins()
                .select(entry_is_empty_relation, relation_tag, entry_tag);
        let index_order_tag =
            builder
                .ins()
                .select(index_is_empty_relation, relation_tag, index_tag);
        let same_kind = builder
            .ins()
            .icmp(IntCC::Equal, entry_order_tag, index_order_tag);
        let equal = builder.ins().icmp(IntCC::Equal, entry_key, index_word);
        let kind_less =
            builder
                .ins()
                .icmp(IntCC::UnsignedLessThan, entry_order_tag, index_order_tag);

        let entry_payload = ValueEmitter::emit_payload(builder, entry_key);
        let index_payload = ValueEmitter::emit_payload(builder, index_word);
        let payload_less =
            builder
                .ins()
                .icmp(IntCC::UnsignedLessThan, entry_payload, index_payload);
        let entry_int = ValueEmitter::emit_unbox_int(builder, entry_key);
        let index_int = ValueEmitter::emit_unbox_int(builder, index_word);
        let int_less = builder
            .ins()
            .icmp(IntCC::SignedLessThan, entry_int, index_int);
        let entry_float = ValueEmitter::emit_unbox_float(builder, entry_key);
        let index_float = ValueEmitter::emit_unbox_float(builder, index_word);
        let float_less = builder
            .ins()
            .fcmp(FloatCC::LessThan, entry_float, index_float);
        let is_int = builder
            .ins()
            .icmp_imm(IntCC::Equal, index_tag, i64::from(VALUE_INT_TAG));
        let is_float = builder
            .ins()
            .icmp_imm(IntCC::Equal, index_tag, i64::from(VALUE_FLOAT_TAG));
        let same_kind_less = builder.ins().select(is_int, int_less, payload_less);
        let same_kind_less = builder.ins().select(is_float, float_less, same_kind_less);
        let empty_relation_mismatch = builder
            .ins()
            .bxor(entry_is_empty_relation, index_is_empty_relation);
        let same_kind_less = builder.ins().select(
            empty_relation_mismatch,
            entry_is_empty_relation,
            same_kind_less,
        );
        let entry_less = builder.ins().select(same_kind, same_kind_less, kind_less);
        if let (Some(helper), Some(map_helper_compare), Some(map_compare_done)) = (
            canonical_compare_helper,
            map_helper_compare,
            map_compare_done,
        ) {
            builder.ins().brif(
                index_is_immediate,
                map_compare_done,
                &[equal.into(), entry_less.into()],
                map_helper_compare,
                &[],
            );

            builder.switch_to_block(map_helper_compare);
            let call = builder.ins().call(helper, &[entry_key, index_word]);
            let comparison = builder.inst_results(call)[0];
            let equal = builder.ins().icmp_imm(IntCC::Equal, comparison, 0);
            let entry_less = builder.ins().icmp_imm(IntCC::SignedLessThan, comparison, 0);
            builder
                .ins()
                .jump(map_compare_done, &[equal.into(), entry_less.into()]);

            builder.switch_to_block(map_compare_done);
            let equal = builder.block_params(map_compare_done)[0];
            let entry_less = builder.block_params(map_compare_done)[1];
            builder.ins().brif(
                equal,
                done,
                &[entry_value.into(), true_value.into()],
                map_update,
                &[lower.into(), upper.into(), middle.into(), entry_less.into()],
            );
        } else {
            builder.ins().brif(
                equal,
                done,
                &[entry_value.into(), true_value.into()],
                map_update,
                &[lower.into(), upper.into(), middle.into(), entry_less.into()],
            );
        }

        builder.switch_to_block(map_update);
        let lower = builder.block_params(map_update)[0];
        let upper = builder.block_params(map_update)[1];
        let middle = builder.block_params(map_update)[2];
        let entry_less = builder.block_params(map_update)[3];
        let one = builder.ins().iconst(types::I64, 1);
        let after_middle = builder.ins().iadd(middle, one);
        let lower = builder.ins().select(entry_less, after_middle, lower);
        let upper = builder.ins().select(entry_less, upper, middle);
        builder.ins().jump(map_loop, &[lower.into(), upper.into()]);

        builder.switch_to_block(done);
        (builder.block_params(done)[0], builder.block_params(done)[1])
    }

    fn load_slot(
        builder: &mut FunctionBuilder<'_>,
        scratch: cranelift_codegen::ir::Value,
        slot: u16,
    ) -> cranelift_codegen::ir::Value {
        builder.ins().load(
            types::I64,
            MemFlagsData::new(),
            scratch,
            i32::from(slot) * 8,
        )
    }

    fn store_slot(
        builder: &mut FunctionBuilder<'_>,
        scratch: cranelift_codegen::ir::Value,
        slot: u16,
        value: cranelift_codegen::ir::Value,
    ) {
        builder
            .ins()
            .store(MemFlagsData::new(), value, scratch, i32::from(slot) * 8);
    }

    pub const fn code_size(&self) -> usize {
        self.code_size
    }

    pub const fn imported_helper_count(&self) -> usize {
        self.imported_helper_count
    }

    /// Executes against borrowed value words without changing their ownership.
    ///
    /// # Safety
    ///
    /// Every word in the used portion of `scratch`, and every constant in the
    /// compiled plan, must represent a valid Mica value. The owners of any heap
    /// values must remain alive throughout the call. Returned words may borrow
    /// from those owners or `collection_views`;
    /// callers must retain them while inspecting the words or clone the values
    /// before releasing their owners. A side exit may leave scratch overwritten
    /// with intermediate words, which must be discarded.
    ///
    /// ```compile_fail
    /// use mica_vm_cranelift::CompiledNaturalLoop;
    ///
    /// fn execute(compiled: &CompiledNaturalLoop, words: &mut [u64]) {
    ///     compiled.run(words, &[], 1);
    /// }
    /// ```
    pub unsafe fn run(
        &self,
        scratch: &mut [u64],
        collection_views: &[NaturalLoopCollectionView<'_>],
        instruction_budget: u64,
    ) -> NaturalLoopOutcome {
        if self.value_abi_version != VALUE_ABI_VERSION
            || scratch.len() < self.slot_count
            || collection_views.len() < self.collection_view_count
        {
            return NaturalLoopOutcome::SideExit;
        }
        let mut instructions = 0;
        let mut resume = 0;
        let mut modified_slots = 0;
        let status = unsafe {
            (self.function)(
                scratch.as_mut_ptr(),
                collection_views.as_ptr().cast(),
                instruction_budget,
                &mut instructions,
                &mut resume,
                &mut modified_slots,
            )
        };
        match status {
            STATUS_COMPLETE => NaturalLoopOutcome::Complete {
                instructions,
                modified_slots,
            },
            STATUS_BUDGET_EXHAUSTED => NaturalLoopOutcome::BudgetExhausted {
                instructions,
                resume: u16::try_from(resume).expect("compiled resume index fits u16"),
                modified_slots,
            },
            STATUS_SIDE_EXIT => NaturalLoopOutcome::SideExit,
            status => panic!("generated natural loop returned unknown status {status}"),
        }
    }
}
