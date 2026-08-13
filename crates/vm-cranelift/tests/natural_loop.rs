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

use mica_var::abi::{VALUE_INT_MAX, VALUE_INT_MIN, borrowed_value_bits, from_owned_value_bits};
use mica_var::{Value, ValueRef};
use mica_vm_cranelift::{
    CompiledNaturalLoop, NaturalLoopCollectionView, NaturalLoopInstruction, NaturalLoopOutcome,
    NaturalLoopPlan, ScalarComparison,
};
use std::sync::{Arc, Barrier};

const CURRENT: u16 = 0;
const TOTAL: u16 = 1;
const LIMIT: u16 = 2;
const CONDITION: u16 = 3;
const STEP: u16 = 4;
const NEXT: u16 = 5;
const NEXT_TOTAL: u16 = 6;
const ELEMENT: u16 = 7;

fn bits(value: Value) -> u64 {
    borrowed_value_bits(&value)
}

fn int_bits(value: i64) -> u64 {
    bits(Value::int(value).unwrap())
}

fn plan(limit: i64) -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        7,
        0,
        3,
        [
            NaturalLoopInstruction::Load {
                dst: LIMIT,
                value: int_bits(limit),
            },
            NaturalLoopInstruction::Compare {
                dst: CONDITION,
                comparison: ScalarComparison::LessThan,
                left: CURRENT,
                right: LIMIT,
            },
            NaturalLoopInstruction::Branch {
                condition: CONDITION,
                if_true: 3,
                if_false: 9,
            },
            NaturalLoopInstruction::Load {
                dst: STEP,
                value: int_bits(1),
            },
            NaturalLoopInstruction::Add {
                dst: NEXT,
                left: CURRENT,
                right: STEP,
            },
            NaturalLoopInstruction::Move {
                dst: CURRENT,
                src: NEXT,
            },
            NaturalLoopInstruction::Add {
                dst: NEXT_TOTAL,
                left: TOTAL,
                right: CURRENT,
            },
            NaturalLoopInstruction::Move {
                dst: TOTAL,
                src: NEXT_TOTAL,
            },
            NaturalLoopInstruction::Jump { target: 0 },
        ],
    )
    .unwrap()
}

fn unboxed_integer_plan(limit: i64) -> NaturalLoopPlan {
    let integer_slots = (1_u32 << CURRENT)
        | (1_u32 << TOTAL)
        | (1_u32 << LIMIT)
        | (1_u32 << STEP)
        | (1_u32 << NEXT)
        | (1_u32 << NEXT_TOTAL);
    let entry_slots = (1_u32 << CURRENT) | (1_u32 << TOTAL);
    plan(limit)
        .with_unboxed_slots(integer_slots, entry_slots, 0, 0)
        .unwrap()
}

fn scratch(limit: i64) -> [u64; 7] {
    [
        int_bits(0),
        int_bits(0),
        int_bits(limit),
        bits(Value::bool(true)),
        bits(Value::empty_relation()),
        bits(Value::empty_relation()),
        bits(Value::empty_relation()),
    ]
}

fn float_collection_plan(unboxed: bool) -> NaturalLoopPlan {
    let plan = NaturalLoopPlan::new(
        8,
        1,
        2,
        [
            NaturalLoopInstruction::Compare {
                dst: CONDITION,
                comparison: ScalarComparison::LessThan,
                left: CURRENT,
                right: LIMIT,
            },
            NaturalLoopInstruction::Branch {
                condition: CONDITION,
                if_true: 2,
                if_false: 9,
            },
            NaturalLoopInstruction::CollectionValueAt {
                dst: ELEMENT,
                view: 0,
                index: CURRENT,
            },
            NaturalLoopInstruction::CheckKind {
                value: ELEMENT,
                expected: mica_var::ValueKind::Float,
            },
            NaturalLoopInstruction::Add {
                dst: NEXT_TOTAL,
                left: TOTAL,
                right: ELEMENT,
            },
            NaturalLoopInstruction::Move {
                dst: TOTAL,
                src: NEXT_TOTAL,
            },
            NaturalLoopInstruction::Add {
                dst: NEXT,
                left: CURRENT,
                right: STEP,
            },
            NaturalLoopInstruction::Move {
                dst: CURRENT,
                src: NEXT,
            },
            NaturalLoopInstruction::Jump { target: 0 },
        ],
    )
    .unwrap();
    if !unboxed {
        return plan;
    }
    let integer_slots = (1_u32 << CURRENT) | (1_u32 << LIMIT) | (1_u32 << STEP) | (1_u32 << NEXT);
    let entry_integer_slots = (1_u32 << CURRENT) | (1_u32 << LIMIT) | (1_u32 << STEP);
    let float_slots = (1_u32 << TOTAL) | (1_u32 << NEXT_TOTAL) | (1_u32 << ELEMENT);
    plan.with_unboxed_slots(
        integer_slots,
        entry_integer_slots,
        float_slots,
        1_u32 << TOTAL,
    )
    .unwrap()
}

fn float_collection_scratch(limit: usize) -> [u64; 8] {
    [
        int_bits(0),
        bits(Value::float(0.0).unwrap()),
        int_bits(limit as i64),
        bits(Value::bool(true)),
        int_bits(1),
        bits(Value::empty_relation()),
        bits(Value::empty_relation()),
        bits(Value::empty_relation()),
    ]
}

fn float_arithmetic_plan(unboxed: bool, factor: f32) -> NaturalLoopPlan {
    let plan = NaturalLoopPlan::new(
        8,
        0,
        2,
        [
            NaturalLoopInstruction::Compare {
                dst: CONDITION,
                comparison: ScalarComparison::LessThan,
                left: CURRENT,
                right: LIMIT,
            },
            NaturalLoopInstruction::Branch {
                condition: CONDITION,
                if_true: 2,
                if_false: 11,
            },
            NaturalLoopInstruction::Load {
                dst: ELEMENT,
                value: bits(Value::float(factor).unwrap()),
            },
            NaturalLoopInstruction::Multiply {
                dst: NEXT_TOTAL,
                left: TOTAL,
                right: ELEMENT,
            },
            NaturalLoopInstruction::Negate {
                dst: NEXT_TOTAL,
                src: NEXT_TOTAL,
            },
            NaturalLoopInstruction::Subtract {
                dst: NEXT_TOTAL,
                left: NEXT_TOTAL,
                right: ELEMENT,
            },
            NaturalLoopInstruction::Divide {
                dst: NEXT_TOTAL,
                left: NEXT_TOTAL,
                right: ELEMENT,
            },
            NaturalLoopInstruction::Move {
                dst: TOTAL,
                src: NEXT_TOTAL,
            },
            NaturalLoopInstruction::Add {
                dst: NEXT,
                left: CURRENT,
                right: STEP,
            },
            NaturalLoopInstruction::Move {
                dst: CURRENT,
                src: NEXT,
            },
            NaturalLoopInstruction::Jump { target: 0 },
        ],
    )
    .unwrap();
    if !unboxed {
        return plan;
    }
    let integer_slots = (1_u32 << CURRENT) | (1_u32 << LIMIT) | (1_u32 << STEP) | (1_u32 << NEXT);
    let entry_integer_slots = (1_u32 << CURRENT) | (1_u32 << LIMIT) | (1_u32 << STEP);
    let float_slots = (1_u32 << TOTAL) | (1_u32 << NEXT_TOTAL) | (1_u32 << ELEMENT);
    plan.with_unboxed_slots(
        integer_slots,
        entry_integer_slots,
        float_slots,
        1_u32 << TOTAL,
    )
    .unwrap()
}

fn float_arithmetic_scratch(limit: i64) -> [u64; 8] {
    [
        int_bits(0),
        bits(Value::float(2.0).unwrap()),
        int_bits(limit),
        bits(Value::bool(true)),
        int_bits(1),
        bits(Value::empty_relation()),
        bits(Value::empty_relation()),
        bits(Value::empty_relation()),
    ]
}

fn value(bits: u64) -> Value {
    unsafe { from_owned_value_bits(bits) }
}

fn collection_value_plan() -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        2,
        1,
        0,
        [NaturalLoopInstruction::CollectionValueAt {
            dst: 0,
            view: 0,
            index: 1,
        }],
    )
    .unwrap()
}

fn collection_key_plan() -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        2,
        1,
        0,
        [NaturalLoopInstruction::CollectionKeyAt {
            dst: 0,
            view: 0,
            index: 1,
        }],
    )
    .unwrap()
}

fn index_plan() -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        2,
        1,
        0,
        [NaturalLoopInstruction::IndexValue {
            dst: 0,
            view: 0,
            index: 1,
        }],
    )
    .unwrap()
}

fn immediate_index_plan(index: Value) -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        1,
        1,
        0,
        [NaturalLoopInstruction::IndexValueImmediate {
            dst: 0,
            view: 0,
            index: borrowed_value_bits(&index),
        }],
    )
    .unwrap()
}

fn comparison_plan(comparison: ScalarComparison) -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        3,
        0,
        0,
        [NaturalLoopInstruction::Compare {
            dst: 2,
            comparison,
            left: 0,
            right: 1,
        }],
    )
    .unwrap()
}

fn numeric_arithmetic_plan() -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        6,
        0,
        0,
        [
            NaturalLoopInstruction::Negate { dst: 2, src: 0 },
            NaturalLoopInstruction::Add {
                dst: 3,
                left: 0,
                right: 1,
            },
            NaturalLoopInstruction::Subtract {
                dst: 4,
                left: 0,
                right: 1,
            },
            NaturalLoopInstruction::Multiply {
                dst: 5,
                left: 0,
                right: 1,
            },
        ],
    )
    .unwrap()
}

fn numeric_division_plan() -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        3,
        0,
        0,
        [NaturalLoopInstruction::Divide {
            dst: 2,
            left: 0,
            right: 1,
        }],
    )
    .unwrap()
}

fn numeric_remainder_plan() -> NaturalLoopPlan {
    NaturalLoopPlan::new(
        3,
        0,
        0,
        [NaturalLoopInstruction::Remainder {
            dst: 2,
            left: 0,
            right: 1,
        }],
    )
    .unwrap()
}

#[test]
fn unboxed_integer_accumulator_matches_tagged_execution_without_helpers() {
    let limit = 4_096;
    let tagged = CompiledNaturalLoop::compile(&plan(limit)).unwrap();
    let unboxed_plan = unboxed_integer_plan(limit);
    assert_eq!(unboxed_plan.unboxed_boolean_slots(), 1_u32 << CONDITION);
    let unboxed = CompiledNaturalLoop::compile(&unboxed_plan).unwrap();
    let mut tagged_scratch = scratch(limit);
    let mut unboxed_scratch = tagged_scratch;
    let budget = u64::try_from(limit * 9).unwrap();

    assert_eq!(
        unboxed.run(&mut unboxed_scratch, &[], budget),
        tagged.run(&mut tagged_scratch, &[], budget),
    );
    assert_eq!(unboxed_scratch, tagged_scratch);
    assert_eq!(
        value(unboxed_scratch[1]).as_int(),
        Some((limit * (limit + 1)) / 2),
    );
    assert_eq!(unboxed.imported_helper_count(), 0);
    assert!(unboxed.code_size() < tagged.code_size());
}

#[test]
fn unboxed_integer_plan_rejects_overlapping_integer_and_boolean_state() {
    let integer_slots = unboxed_integer_plan(4_096).unboxed_integer_slots();
    let error = plan(4_096)
        .with_unboxed_slots(integer_slots | (1_u32 << CONDITION), 1_u32 << CURRENT, 0, 0)
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "natural loop has overlapping unboxed slot kinds",
    );
}

#[test]
fn unboxed_integer_entry_guards_side_exit_atomically() {
    let compiled = CompiledNaturalLoop::compile(&unboxed_integer_plan(4_096)).unwrap();
    let mut values = scratch(4_096);
    values[TOTAL as usize] = bits(Value::float(0.0).unwrap());
    let original = values;

    assert_eq!(
        compiled.run(&mut values, &[], 4_096 * 9),
        NaturalLoopOutcome::SideExit,
    );
    assert_eq!(values, original);
}

#[test]
fn unboxed_integer_overflow_side_exits_atomically() {
    let compiled = CompiledNaturalLoop::compile(&unboxed_integer_plan(4_096)).unwrap();
    let mut values = scratch(4_096);
    values[TOTAL as usize] = int_bits(VALUE_INT_MAX);
    let original = values;

    assert_eq!(
        compiled.run(&mut values, &[], 4_096 * 9),
        NaturalLoopOutcome::SideExit,
    );
    assert_eq!(values, original);
}

#[test]
fn unboxed_float_collection_state_matches_tagged_execution() {
    let values = (0..4_096)
        .map(|_| Value::float(0.25).unwrap())
        .collect::<Vec<_>>();
    let views = [NaturalLoopCollectionView::list(&values)];
    let tagged = CompiledNaturalLoop::compile(&float_collection_plan(false)).unwrap();
    let unboxed_plan = float_collection_plan(true);
    assert_ne!(unboxed_plan.unboxed_integer_slots(), 0);
    assert_ne!(unboxed_plan.unboxed_float_slots(), 0);
    assert_eq!(unboxed_plan.unboxed_boolean_slots(), 1_u32 << CONDITION);
    let unboxed = CompiledNaturalLoop::compile(&unboxed_plan).unwrap();
    let mut tagged_scratch = float_collection_scratch(values.len());
    let mut unboxed_scratch = tagged_scratch;
    let budget = (values.len() as u64 * 9) + 2;

    assert_eq!(
        unboxed.run(&mut unboxed_scratch, &views, budget),
        tagged.run(&mut tagged_scratch, &views, budget),
    );
    assert_eq!(unboxed_scratch, tagged_scratch);
    assert_eq!(
        value(unboxed_scratch[TOTAL as usize]).as_float(),
        Some(1_024.0)
    );
    assert_eq!(unboxed.imported_helper_count(), 0);
    assert!(unboxed.code_size() < tagged.code_size());
}

#[test]
fn unboxed_float_collection_state_preserves_every_budget_boundary() {
    let values = (0..3)
        .map(|_| Value::float(0.25).unwrap())
        .collect::<Vec<_>>();
    let views = [NaturalLoopCollectionView::list(&values)];
    let tagged = CompiledNaturalLoop::compile(&float_collection_plan(false)).unwrap();
    let unboxed = CompiledNaturalLoop::compile(&float_collection_plan(true)).unwrap();

    for budget in 0..=(values.len() as u64 * 9) + 2 {
        let mut tagged_scratch = float_collection_scratch(values.len());
        let mut unboxed_scratch = tagged_scratch;
        let tagged_outcome = tagged.run(&mut tagged_scratch, &views, budget);
        let unboxed_outcome = unboxed.run(&mut unboxed_scratch, &views, budget);
        assert_eq!(unboxed_outcome, tagged_outcome, "budget {budget}",);
        let modified_slots = match tagged_outcome {
            NaturalLoopOutcome::Complete { modified_slots, .. }
            | NaturalLoopOutcome::BudgetExhausted { modified_slots, .. } => modified_slots,
            NaturalLoopOutcome::SideExit => unreachable!("uniform float input stays native"),
        };
        for slot in 0..unboxed_scratch.len() {
            if modified_slots & (1_u32 << slot) != 0 {
                assert_eq!(
                    unboxed_scratch[slot], tagged_scratch[slot],
                    "budget {budget}, slot {slot}",
                );
            }
        }
    }
}

#[test]
fn unboxed_float_collection_guards_and_arithmetic_side_exit_atomically() {
    let cases = [
        vec![Value::float(0.25).unwrap(), Value::int(1).unwrap()],
        vec![
            Value::float(f32::MAX).unwrap(),
            Value::float(f32::MAX).unwrap(),
        ],
    ];
    let compiled = CompiledNaturalLoop::compile(&float_collection_plan(true)).unwrap();

    for values in cases {
        let views = [NaturalLoopCollectionView::list(&values)];
        let mut scratch = float_collection_scratch(values.len());
        let original = scratch;
        assert_eq!(
            compiled.run(&mut scratch, &views, u64::MAX),
            NaturalLoopOutcome::SideExit,
        );
        assert_eq!(scratch, original);
    }
}

#[test]
fn unboxed_float_state_supports_negate_subtract_multiply_and_divide() {
    let tagged = CompiledNaturalLoop::compile(&float_arithmetic_plan(false, 1.5)).unwrap();
    let unboxed = CompiledNaturalLoop::compile(&float_arithmetic_plan(true, 1.5)).unwrap();
    let mut tagged_scratch = float_arithmetic_scratch(3);
    let mut unboxed_scratch = tagged_scratch;

    assert_eq!(
        unboxed.run(&mut unboxed_scratch, &[], u64::MAX),
        tagged.run(&mut tagged_scratch, &[], u64::MAX),
    );
    assert_eq!(unboxed_scratch, tagged_scratch);
    assert_eq!(
        value(unboxed_scratch[TOTAL as usize]).as_float(),
        Some(-3.0)
    );
    assert_eq!(unboxed.imported_helper_count(), 0);
}

#[test]
fn unboxed_float_division_by_zero_side_exits_atomically() {
    let compiled = CompiledNaturalLoop::compile(&float_arithmetic_plan(true, 0.0)).unwrap();
    let mut scratch = float_arithmetic_scratch(1);
    let original = scratch;

    assert_eq!(
        compiled.run(&mut scratch, &[], u64::MAX),
        NaturalLoopOutcome::SideExit,
    );
    assert_eq!(scratch, original);
}

#[test]
fn generated_range_value_at_emits_checked_integer_values_without_helpers() {
    let compiled = CompiledNaturalLoop::compile(&collection_value_plan()).unwrap();
    let range = [NaturalLoopCollectionView::range(-5, 5).unwrap()];
    let mut scratch = [bits(Value::empty_relation()), int_bits(7)];

    assert_eq!(
        compiled.run(&mut scratch, &range, 1),
        NaturalLoopOutcome::Complete {
            instructions: 1,
            modified_slots: 1,
        },
    );
    assert_eq!(value(scratch[0]).as_int(), Some(2));
    assert_eq!(compiled.imported_helper_count(), 0);
}

#[test]
fn generated_range_value_at_side_exits_on_invalid_indices_and_bounds() {
    let compiled = CompiledNaturalLoop::compile(&collection_value_plan()).unwrap();
    let cases = [
        (NaturalLoopCollectionView::range(5, 4).unwrap(), int_bits(0)),
        (
            NaturalLoopCollectionView::range(0, 5).unwrap(),
            int_bits(-1),
        ),
        (NaturalLoopCollectionView::range(0, 5).unwrap(), int_bits(6)),
        (
            NaturalLoopCollectionView::range(0, 5).unwrap(),
            bits(Value::float(1.0).unwrap()),
        ),
    ];

    for (range, index) in cases {
        let mut scratch = [bits(Value::empty_relation()), index];
        assert_eq!(
            compiled.run(&mut scratch, &[range], 1),
            NaturalLoopOutcome::SideExit,
        );
    }
}

#[test]
fn generated_range_key_at_emits_checked_zero_based_ordinals_without_helpers() {
    let compiled = CompiledNaturalLoop::compile(&collection_key_plan()).unwrap();
    let range = [NaturalLoopCollectionView::range(10, 20).unwrap()];
    let mut scratch = [bits(Value::empty_relation()), int_bits(7)];

    assert_eq!(
        compiled.run(&mut scratch, &range, 1),
        NaturalLoopOutcome::Complete {
            instructions: 1,
            modified_slots: 1,
        },
    );
    assert_eq!(value(scratch[0]).as_int(), Some(7));
    assert_eq!(compiled.imported_helper_count(), 0);
}

#[test]
fn generated_range_key_at_side_exits_on_invalid_ordinals() {
    let compiled = CompiledNaturalLoop::compile(&collection_key_plan()).unwrap();
    let range = [NaturalLoopCollectionView::range(10, 20).unwrap()];
    for index in [int_bits(-1), bits(Value::float(1.0).unwrap())] {
        let mut scratch = [bits(Value::empty_relation()), index];
        assert_eq!(
            compiled.run(&mut scratch, &range, 1),
            NaturalLoopOutcome::SideExit,
        );
    }
}

#[test]
fn generated_list_access_emits_immediate_values_and_ordinals_without_helpers() {
    let values = [
        Value::int(10).unwrap(),
        Value::float(2.5).unwrap(),
        Value::bool(true),
    ];
    let view = [NaturalLoopCollectionView::list(&values)];
    let mut value_scratch = [bits(Value::empty_relation()), int_bits(1)];
    let value_compiled = CompiledNaturalLoop::compile(&collection_value_plan()).unwrap();

    assert!(matches!(
        value_compiled.run(&mut value_scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(value(value_scratch[0]).as_float(), Some(2.5));
    assert_eq!(value_compiled.imported_helper_count(), 0);

    let mut key_scratch = [bits(Value::empty_relation()), int_bits(2)];
    let key_compiled = CompiledNaturalLoop::compile(&collection_key_plan()).unwrap();
    assert!(matches!(
        key_compiled.run(&mut key_scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(value(key_scratch[0]).as_int(), Some(2));
    assert_eq!(key_compiled.imported_helper_count(), 0);
}

#[test]
fn generated_list_index_emits_checked_values_without_calling_map_helper() {
    let values = [Value::int(4).unwrap(), Value::int(9).unwrap()];
    let view = [NaturalLoopCollectionView::list(&values)];
    let compiled = CompiledNaturalLoop::compile(&index_plan()).unwrap();
    let mut scratch = [bits(Value::empty_relation()), int_bits(1)];

    assert!(matches!(
        compiled.run(&mut scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(value(scratch[0]).as_int(), Some(9));
    // Dynamic indexing imports the map comparison helper, but list lookup does
    // not call it.
    assert_eq!(compiled.imported_helper_count(), 1);

    for index in [int_bits(-1), int_bits(2), bits(Value::float(0.0).unwrap())] {
        let mut scratch = [bits(Value::empty_relation()), index];
        assert_eq!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::SideExit,
        );
    }
}

#[test]
fn generated_map_index_uses_native_canonical_order_for_immediate_keys() {
    let heap_value = Value::string("heap value");
    let entries = [
        (Value::int(-9).unwrap(), Value::int(1).unwrap()),
        (Value::int(1).unwrap(), Value::int(2).unwrap()),
        (Value::float(-3.5).unwrap(), Value::int(3).unwrap()),
        (Value::float(1.0).unwrap(), heap_value.clone()),
        (
            Value::symbol(mica_var::Symbol::intern("map_index_key")),
            Value::int(5).unwrap(),
        ),
    ];
    let view = [NaturalLoopCollectionView::map(&entries)];
    let compiled = CompiledNaturalLoop::compile(&index_plan()).unwrap();

    for (key, expected) in [
        (Value::int(-9).unwrap(), borrowed_value_bits(&entries[0].1)),
        (Value::int(1).unwrap(), borrowed_value_bits(&entries[1].1)),
        (
            Value::float(-3.5).unwrap(),
            borrowed_value_bits(&entries[2].1),
        ),
        (Value::float(1.0).unwrap(), borrowed_value_bits(&heap_value)),
        (
            Value::symbol(mica_var::Symbol::intern("map_index_key")),
            borrowed_value_bits(&entries[4].1),
        ),
    ] {
        let mut scratch = [bits(Value::empty_relation()), borrowed_value_bits(&key)];
        assert_eq!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::Complete {
                instructions: 1,
                modified_slots: 1,
            },
        );
        assert_eq!(scratch[0], expected);
    }

    for missing in [Value::int(0).unwrap(), Value::float(0.0).unwrap()] {
        let mut scratch = [bits(Value::bool(true)), borrowed_value_bits(&missing)];
        assert_eq!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::SideExit,
        );
    }
    assert_eq!(compiled.imported_helper_count(), 1);
}

#[test]
fn generated_map_index_uses_canonical_helper_for_heap_keys() {
    let keys = [
        Value::string("alpha"),
        Value::string("omega"),
        Value::list([Value::int(1).unwrap(), Value::string("nested")]),
        Value::map([(Value::string("nested"), Value::bool(true))]),
    ];
    let map = Value::map(
        keys.iter()
            .cloned()
            .enumerate()
            .map(|(index, key)| (key, Value::int(index as i64).unwrap())),
    );
    let ValueRef::Map(entries) = map.as_value_ref() else {
        panic!("expected map value");
    };
    let view = [NaturalLoopCollectionView::map(entries)];
    let compiled = CompiledNaturalLoop::compile(&index_plan()).unwrap();

    for key in &keys {
        let expected = map.map_get(key).unwrap();
        let mut scratch = [bits(Value::empty_relation()), borrowed_value_bits(key)];
        assert!(matches!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::Complete { .. }
        ));
        assert_eq!(scratch[0], borrowed_value_bits(&expected), "key {key:?}");
    }
    for missing in [Value::string("missing"), Value::list([])] {
        let mut scratch = [bits(Value::empty_relation()), borrowed_value_bits(&missing)];
        assert_eq!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::SideExit,
        );
    }
    assert_eq!(compiled.imported_helper_count(), 1);
}

#[test]
fn generated_map_index_matches_value_lookup_across_immediate_kinds() {
    let keys = [
        Value::empty_relation(),
        Value::bool(false),
        Value::bool(true),
        Value::int(-7).unwrap(),
        Value::int(3).unwrap(),
        Value::float(-8.5).unwrap(),
        Value::float(2.25).unwrap(),
        Value::identity_raw(42).unwrap(),
        Value::symbol(mica_var::Symbol::intern("map_index_symbol")),
        Value::error_code(mica_var::Symbol::intern("E_MAP_INDEX")),
        Value::string("heap key between immediate kinds"),
        Value::capability_raw(7).unwrap(),
        Value::function_raw(9).unwrap(),
    ];
    let map = Value::map(
        keys.iter()
            .cloned()
            .enumerate()
            .map(|(index, key)| (key, Value::int(index as i64).unwrap())),
    );
    let ValueRef::Map(entries) = map.as_value_ref() else {
        panic!("expected map value");
    };
    let view = [NaturalLoopCollectionView::map(entries)];
    let compiled = CompiledNaturalLoop::compile(&index_plan()).unwrap();

    for key in keys.iter().filter(|key| key.is_immediate()) {
        let expected = map.map_get(key).unwrap();
        let mut scratch = [bits(Value::empty_relation()), borrowed_value_bits(key)];
        assert!(matches!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::Complete { .. }
        ));
        assert_eq!(scratch[0], borrowed_value_bits(&expected), "key {key:?}");
    }
    for missing in [Value::int(99).unwrap(), Value::float(0.5).unwrap()] {
        let mut scratch = [bits(Value::empty_relation()), borrowed_value_bits(&missing)];
        assert_eq!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::SideExit,
        );
    }
}

#[test]
fn generated_map_index_accepts_immediate_operands() {
    let entries = [(
        Value::symbol(mica_var::Symbol::intern("direct")),
        Value::int(7).unwrap(),
    )];
    let view = [NaturalLoopCollectionView::map(&entries)];
    let compiled = CompiledNaturalLoop::compile(&immediate_index_plan(Value::symbol(
        mica_var::Symbol::intern("direct"),
    )))
    .unwrap();
    let mut scratch = [bits(Value::empty_relation())];

    assert!(matches!(
        compiled.run(&mut scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(value(scratch[0]).as_int(), Some(7));
    assert_eq!(compiled.imported_helper_count(), 0);
}

#[test]
fn generated_heap_map_index_helper_executes_concurrently() {
    let compiled = Arc::new(CompiledNaturalLoop::compile(&index_plan()).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let compiled = Arc::clone(&compiled);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let key = Value::string("concurrent key");
            let map = Value::map([
                (Value::string("alpha"), Value::int(1).unwrap()),
                (key.clone(), Value::int(7).unwrap()),
                (Value::string("omega"), Value::int(9).unwrap()),
            ]);
            let ValueRef::Map(entries) = map.as_value_ref() else {
                panic!("expected map value");
            };
            let view = [NaturalLoopCollectionView::map(entries)];
            let mut scratch = [bits(Value::empty_relation()), borrowed_value_bits(&key)];
            barrier.wait();
            let outcome = compiled.run(&mut scratch, &view, 1);
            (outcome, scratch[0])
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (
                NaturalLoopOutcome::Complete {
                    instructions: 1,
                    modified_slots: 1,
                },
                int_bits(7),
            ),
        );
    }
}

#[test]
fn generated_collection_access_preserves_heap_words_and_checks_ordinals() {
    let values = [Value::string("heap")];
    let view = [NaturalLoopCollectionView::list(&values)];
    let compiled = CompiledNaturalLoop::compile(&collection_value_plan()).unwrap();

    let mut scratch = [bits(Value::empty_relation()), int_bits(0)];
    assert!(matches!(
        compiled.run(&mut scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(scratch[0], borrowed_value_bits(&values[0]));

    for index in [int_bits(-1), int_bits(1)] {
        let mut scratch = [bits(Value::empty_relation()), index];
        assert_eq!(
            compiled.run(&mut scratch, &view, 1),
            NaturalLoopOutcome::SideExit,
        );
    }
}

#[test]
fn generated_map_access_emits_immediate_keys_and_values_without_helpers() {
    let entries = [
        (Value::int(3).unwrap(), Value::float(4.5).unwrap()),
        (
            Value::symbol(mica_var::Symbol::intern("key")),
            Value::int(7).unwrap(),
        ),
    ];
    let view = [NaturalLoopCollectionView::map(&entries)];
    let mut key_scratch = [bits(Value::empty_relation()), int_bits(1)];
    let key_compiled = CompiledNaturalLoop::compile(&collection_key_plan()).unwrap();
    assert!(matches!(
        key_compiled.run(&mut key_scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(
        value(key_scratch[0]).as_symbol(),
        Some(mica_var::Symbol::intern("key"))
    );

    let mut value_scratch = [bits(Value::empty_relation()), int_bits(0)];
    let value_compiled = CompiledNaturalLoop::compile(&collection_value_plan()).unwrap();
    assert!(matches!(
        value_compiled.run(&mut value_scratch, &view, 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(value(value_scratch[0]).as_float(), Some(4.5));
    assert_eq!(key_compiled.imported_helper_count(), 0);
    assert_eq!(value_compiled.imported_helper_count(), 0);
}

#[test]
fn generated_natural_loop_completes_compiler_shaped_accumulation() {
    let compiled = CompiledNaturalLoop::compile(&plan(16_384)).unwrap();
    let mut scratch = scratch(16_384);
    assert_eq!(
        compiled.run(&mut scratch, &[], 16_384 * 9),
        NaturalLoopOutcome::Complete {
            instructions: 16_384 * 9,
            modified_slots: 0x7f,
        },
    );
    assert_eq!(value(scratch[CURRENT as usize]).as_int(), Some(16_384));
    assert_eq!(value(scratch[TOTAL as usize]).as_int(), Some(134_225_920),);
    assert_eq!(value(scratch[CONDITION as usize]).as_bool(), Some(false),);
    // The plan imports ordering fallback support, although this integer loop
    // remains entirely on the generated comparison path.
    assert_eq!(compiled.imported_helper_count(), 1);
    assert!(compiled.code_size() > 0);
}

#[test]
fn generated_natural_loop_stops_at_an_exact_instruction_budget() {
    let compiled = CompiledNaturalLoop::compile(&plan(16_384)).unwrap();
    let mut scratch = scratch(16_384);
    assert_eq!(
        compiled.run(&mut scratch, &[], 90),
        NaturalLoopOutcome::BudgetExhausted {
            instructions: 90,
            resume: 3,
            modified_slots: 0x7f,
        },
    );
    assert_eq!(value(scratch[CURRENT as usize]).as_int(), Some(10));
    assert_eq!(value(scratch[TOTAL as usize]).as_int(), Some(55));
    assert_eq!(value(scratch[CONDITION as usize]).as_bool(), Some(true),);
}

#[test]
fn generated_numeric_arithmetic_matches_float_and_mixed_value_semantics() {
    let compiled = CompiledNaturalLoop::compile(&numeric_arithmetic_plan()).unwrap();
    for (left, right) in [
        (Value::float(2.0).unwrap(), Value::float(0.5).unwrap()),
        (Value::int(2).unwrap(), Value::float(0.5).unwrap()),
        (Value::float(-2.5).unwrap(), Value::int(4).unwrap()),
    ] {
        let expected = [
            left.checked_neg().unwrap(),
            left.checked_add(&right).unwrap(),
            left.checked_sub(&right).unwrap(),
            left.checked_mul(&right).unwrap(),
        ];
        let mut scratch = [
            borrowed_value_bits(&left),
            borrowed_value_bits(&right),
            bits(Value::empty_relation()),
            bits(Value::empty_relation()),
            bits(Value::empty_relation()),
            bits(Value::empty_relation()),
        ];
        assert_eq!(
            compiled.run(&mut scratch, &[], 4),
            NaturalLoopOutcome::Complete {
                instructions: 4,
                modified_slots: 0x3c,
            },
        );
        for (slot, expected) in (2..6).zip(expected) {
            assert_eq!(value(scratch[slot]), expected);
        }
    }
}

#[test]
fn generated_numeric_arithmetic_side_exits_on_invalid_or_non_finite_results() {
    let compiled = CompiledNaturalLoop::compile(&numeric_arithmetic_plan()).unwrap();
    let cases = [
        (Value::string("not numeric"), Value::int(1).unwrap()),
        (Value::int(VALUE_INT_MAX).unwrap(), Value::int(1).unwrap()),
        (Value::float(f32::MAX).unwrap(), Value::float(2.0).unwrap()),
    ];
    for (left, right) in cases {
        let mut scratch = [
            borrowed_value_bits(&left),
            borrowed_value_bits(&right),
            bits(Value::empty_relation()),
            bits(Value::empty_relation()),
            bits(Value::empty_relation()),
            bits(Value::empty_relation()),
        ];
        assert_eq!(
            compiled.run(&mut scratch, &[], 4),
            NaturalLoopOutcome::SideExit,
        );
    }
}

#[test]
fn generated_numeric_division_matches_value_semantics() {
    let compiled = CompiledNaturalLoop::compile(&numeric_division_plan()).unwrap();
    assert_eq!(compiled.imported_helper_count(), 0);
    for (left, right) in [
        (Value::int(8).unwrap(), Value::int(2).unwrap()),
        (Value::int(7).unwrap(), Value::int(2).unwrap()),
        (Value::int(-7).unwrap(), Value::int(2).unwrap()),
        (Value::int(7).unwrap(), Value::float(2.0).unwrap()),
        (Value::float(7.5).unwrap(), Value::int(2).unwrap()),
    ] {
        let mut scratch = [
            borrowed_value_bits(&left),
            borrowed_value_bits(&right),
            bits(Value::empty_relation()),
        ];
        assert_eq!(
            compiled.run(&mut scratch, &[], 1),
            NaturalLoopOutcome::Complete {
                instructions: 1,
                modified_slots: 0x4,
            },
        );
        assert_eq!(value(scratch[2]), left.checked_div(&right).unwrap());
    }
}

#[test]
fn generated_numeric_remainder_matches_value_semantics() {
    let compiled = CompiledNaturalLoop::compile(&numeric_remainder_plan()).unwrap();
    assert_eq!(compiled.imported_helper_count(), 1);
    for (left, right) in [
        (Value::int(7).unwrap(), Value::int(2).unwrap()),
        (Value::int(-7).unwrap(), Value::int(2).unwrap()),
        (Value::int(7).unwrap(), Value::float(2.0).unwrap()),
        (Value::float(7.5).unwrap(), Value::int(2).unwrap()),
        (Value::float(f32::MAX).unwrap(), Value::float(0.5).unwrap()),
    ] {
        let mut scratch = [
            borrowed_value_bits(&left),
            borrowed_value_bits(&right),
            bits(Value::empty_relation()),
        ];
        assert_eq!(
            compiled.run(&mut scratch, &[], 1),
            NaturalLoopOutcome::Complete {
                instructions: 1,
                modified_slots: 0x4,
            },
        );
        assert_eq!(value(scratch[2]), left.checked_rem(&right).unwrap());
    }
}

#[test]
fn generated_numeric_division_and_remainder_side_exit_on_errors() {
    let divide = CompiledNaturalLoop::compile(&numeric_division_plan()).unwrap();
    let remainder = CompiledNaturalLoop::compile(&numeric_remainder_plan()).unwrap();
    let cases = [
        (Value::int(1).unwrap(), Value::int(0).unwrap()),
        (Value::int(1).unwrap(), Value::float(0.0).unwrap()),
        (Value::string("not numeric"), Value::int(1).unwrap()),
    ];
    for compiled in [&divide, &remainder] {
        for (left, right) in &cases {
            let mut scratch = [
                borrowed_value_bits(left),
                borrowed_value_bits(right),
                bits(Value::empty_relation()),
            ];
            assert_eq!(
                compiled.run(&mut scratch, &[], 1),
                NaturalLoopOutcome::SideExit,
            );
        }
    }

    let mut scratch = [
        bits(Value::int(VALUE_INT_MIN).unwrap()),
        bits(Value::int(-1).unwrap()),
        bits(Value::empty_relation()),
    ];
    assert_eq!(
        divide.run(&mut scratch, &[], 1),
        NaturalLoopOutcome::SideExit,
    );
    let mut scratch = [
        bits(Value::float(f32::MAX).unwrap()),
        bits(Value::float(0.5).unwrap()),
        bits(Value::empty_relation()),
    ];
    assert_eq!(
        divide.run(&mut scratch, &[], 1),
        NaturalLoopOutcome::SideExit,
    );
}

#[test]
fn generated_equality_calls_one_helper_for_heap_and_mixed_numeric_values() {
    let compiled = CompiledNaturalLoop::compile(&comparison_plan(ScalarComparison::Equal)).unwrap();
    let cases = [
        (Value::string("same"), Value::string("same"), true),
        (Value::string("same"), Value::string("different"), false),
        (
            Value::list([Value::int(1).unwrap(), Value::string("nested")]),
            Value::list([Value::int(1).unwrap(), Value::string("nested")]),
            true,
        ),
        (
            Value::map([(Value::string("key"), Value::int(7).unwrap())]),
            Value::map([(Value::string("key"), Value::int(8).unwrap())]),
            false,
        ),
        (
            Value::int(16_777_216).unwrap(),
            Value::float(16_777_216.0).unwrap(),
            true,
        ),
        (
            Value::int(16_777_217).unwrap(),
            Value::float(16_777_216.0).unwrap(),
            false,
        ),
    ];

    for (left, right, expected) in cases {
        let mut scratch = [
            borrowed_value_bits(&left),
            borrowed_value_bits(&right),
            bits(Value::empty_relation()),
        ];
        assert_eq!(
            compiled.run(&mut scratch, &[], 1),
            NaturalLoopOutcome::Complete {
                instructions: 1,
                modified_slots: 4,
            },
        );
        assert_eq!(value(scratch[2]).as_bool(), Some(expected));
    }
    assert_eq!(compiled.imported_helper_count(), 1);
}

#[test]
fn generated_language_ordering_calls_one_helper_for_heap_and_mixed_numeric_values() {
    let not_equal =
        CompiledNaturalLoop::compile(&comparison_plan(ScalarComparison::NotEqual)).unwrap();
    let left = Value::string("alpha");
    let right = Value::string("beta");
    let mut scratch = [
        borrowed_value_bits(&left),
        borrowed_value_bits(&right),
        bits(Value::empty_relation()),
    ];
    assert!(matches!(
        not_equal.run(&mut scratch, &[], 1),
        NaturalLoopOutcome::Complete { .. }
    ));
    assert_eq!(value(scratch[2]).as_bool(), Some(true));
    assert_eq!(not_equal.imported_helper_count(), 1);

    let cases = [
        (Value::string("alpha"), Value::string("beta")),
        (
            Value::list([Value::int(1).unwrap()]),
            Value::list([Value::int(2).unwrap()]),
        ),
        (
            Value::map([(Value::string("key"), Value::int(1).unwrap())]),
            Value::map([(Value::string("key"), Value::int(2).unwrap())]),
        ),
        (
            Value::int(16_777_217).unwrap(),
            Value::float(16_777_216.0).unwrap(),
        ),
        (Value::int(3).unwrap(), Value::float(3.5).unwrap()),
        (
            Value::float(2_f32.powi(55)).unwrap(),
            Value::int((1_i64 << 55) - 1).unwrap(),
        ),
    ];
    for comparison in [
        ScalarComparison::LessThan,
        ScalarComparison::LessThanOrEqual,
        ScalarComparison::GreaterThan,
        ScalarComparison::GreaterThanOrEqual,
    ] {
        let compiled = CompiledNaturalLoop::compile(&comparison_plan(comparison)).unwrap();
        assert_eq!(compiled.imported_helper_count(), 1);
        for (left, right) in &cases {
            let ordering = mica_var::language_cmp::numeric_cmp(left, right);
            let expected = match comparison {
                ScalarComparison::LessThan => ordering.is_lt(),
                ScalarComparison::LessThanOrEqual => ordering.is_le(),
                ScalarComparison::GreaterThan => ordering.is_gt(),
                ScalarComparison::GreaterThanOrEqual => ordering.is_ge(),
                ScalarComparison::Equal | ScalarComparison::NotEqual => unreachable!(),
            };
            let mut scratch = [
                borrowed_value_bits(left),
                borrowed_value_bits(right),
                bits(Value::empty_relation()),
            ];
            assert_eq!(
                compiled.run(&mut scratch, &[], 1),
                NaturalLoopOutcome::Complete {
                    instructions: 1,
                    modified_slots: 4,
                },
            );
            assert_eq!(value(scratch[2]).as_bool(), Some(expected));
        }
    }
}

#[test]
fn generated_heap_equality_helper_executes_concurrently() {
    let compiled =
        Arc::new(CompiledNaturalLoop::compile(&comparison_plan(ScalarComparison::Equal)).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let compiled = Arc::clone(&compiled);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let left = Value::string("concurrent equality");
            let right = Value::string("concurrent equality");
            let mut scratch = [
                borrowed_value_bits(&left),
                borrowed_value_bits(&right),
                bits(Value::empty_relation()),
            ];
            barrier.wait();
            let outcome = compiled.run(&mut scratch, &[], 1);
            (outcome, value(scratch[2]).as_bool())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (
                NaturalLoopOutcome::Complete {
                    instructions: 1,
                    modified_slots: 4,
                },
                Some(true),
            ),
        );
    }
}

#[test]
fn generated_language_ordering_helper_executes_concurrently() {
    let compiled = Arc::new(
        CompiledNaturalLoop::compile(&comparison_plan(ScalarComparison::LessThan)).unwrap(),
    );
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let compiled = Arc::clone(&compiled);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let left = Value::list([Value::string("alpha")]);
            let right = Value::list([Value::string("beta")]);
            let mut scratch = [
                borrowed_value_bits(&left),
                borrowed_value_bits(&right),
                bits(Value::empty_relation()),
            ];
            barrier.wait();
            let outcome = compiled.run(&mut scratch, &[], 1);
            (outcome, value(scratch[2]).as_bool())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (
                NaturalLoopOutcome::Complete {
                    instructions: 1,
                    modified_slots: 4,
                },
                Some(true),
            ),
        );
    }
}

#[test]
fn generated_mixed_numeric_arithmetic_executes_concurrently() {
    let compiled = Arc::new(CompiledNaturalLoop::compile(&numeric_arithmetic_plan()).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let compiled = Arc::clone(&compiled);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let left = Value::int(3).unwrap();
            let right = Value::float(0.5).unwrap();
            let mut scratch = [
                borrowed_value_bits(&left),
                borrowed_value_bits(&right),
                bits(Value::empty_relation()),
                bits(Value::empty_relation()),
                bits(Value::empty_relation()),
                bits(Value::empty_relation()),
            ];
            barrier.wait();
            let outcome = compiled.run(&mut scratch, &[], 4);
            (outcome, value(scratch[5]).as_float())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (
                NaturalLoopOutcome::Complete {
                    instructions: 4,
                    modified_slots: 0x3c,
                },
                Some(1.5),
            ),
        );
    }
}

#[test]
fn generated_float_remainder_helper_executes_concurrently() {
    let compiled = Arc::new(CompiledNaturalLoop::compile(&numeric_remainder_plan()).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let compiled = Arc::clone(&compiled);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let left = Value::float(7.5).unwrap();
            let right = Value::float(2.0).unwrap();
            let mut scratch = [
                borrowed_value_bits(&left),
                borrowed_value_bits(&right),
                bits(Value::empty_relation()),
            ];
            barrier.wait();
            let outcome = compiled.run(&mut scratch, &[], 1);
            (outcome, value(scratch[2]).as_float())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (
                NaturalLoopOutcome::Complete {
                    instructions: 1,
                    modified_slots: 0x4,
                },
                Some(1.5),
            ),
        );
    }
}

#[test]
fn generated_natural_loop_executes_concurrently() {
    let compiled = Arc::new(CompiledNaturalLoop::compile(&plan(16_384)).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let compiled = Arc::clone(&compiled);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut scratch = scratch(16_384);
            barrier.wait();
            let outcome = compiled.run(&mut scratch, &[], 16_384 * 9);
            (outcome, value(scratch[TOTAL as usize]).as_int())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (
                NaturalLoopOutcome::Complete {
                    instructions: 16_384 * 9,
                    modified_slots: 0x7f,
                },
                Some(134_225_920),
            ),
        );
    }
}
