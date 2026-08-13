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

use crate::{
    AuthorityContext, Instruction, KindCheckSite, Operand, Program, Register, RegisterVm,
    RuntimeBinaryOp, RuntimeError, RuntimeUnaryOp, VmHost, VmHostResponse,
};
use mica_relation_kernel::{
    DispatchRead, KernelError, RelationId, RelationRead, RelationWorkspace, Tuple,
};
use mica_var::{Identity, Symbol, Value};
use std::sync::{Arc, Barrier};

const ITERATIONS: usize = 16_384;
const INSTRUCTION_COUNT: usize = (ITERATIONS * 3) + 4;
const NATURAL_INSTRUCTION_COUNT: usize = (ITERATIONS * 9) + 7;
const NATURAL_ARITHMETIC_INSTRUCTION_COUNT: usize = (ITERATIONS * 11) + 7;
const NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT: usize = (ITERATIONS * 19) + 6;
const NATURAL_SCALAR_INSTRUCTION_COUNT: usize = (ITERATIONS * 8) + 7;
const PREDICTABLE_BRANCH_INSTRUCTION_COUNT: usize = (ITERATIONS * 9) + 10;
const ALTERNATING_BRANCH_INSTRUCTION_COUNT: usize = (ITERATIONS / 2 * 17) + 10;
const NATURAL_RANGE_INSTRUCTION_COUNT: usize = (ITERATIONS * 8) + 8;
const NATURAL_DIV_REM_INSTRUCTION_COUNT: usize = (ITERATIONS * 9) + 9;
const NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT: usize = (ITERATIONS * 10) + 8;
const NATURAL_LIST_INDEX_INSTRUCTION_COUNT: usize = (ITERATIONS * 8) + 8;
const NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT: usize = (ITERATIONS * 6) + 8;
const NATURAL_CONSTANT_MAP_INDEX_INSTRUCTION_COUNT: usize = (ITERATIONS * 6) + 7;
const NATURAL_HEAP_COMPARISON_INSTRUCTION_COUNT: usize = (ITERATIONS * 8) + 9;
const MAX_CALL_DEPTH: usize = 8;

#[derive(Default)]
struct TestHost {
    authority: AuthorityContext,
}

impl RelationRead for TestHost {
    fn scan_relation(
        &self,
        _relation: RelationId,
        _bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        panic!("integer loop test unexpectedly scanned a relation")
    }
}

impl RelationWorkspace for TestHost {
    fn assert_tuple(&mut self, _relation: RelationId, _tuple: Tuple) -> Result<(), KernelError> {
        panic!("integer loop test unexpectedly asserted a tuple")
    }

    fn retract_tuple(&mut self, _relation: RelationId, _tuple: Tuple) -> Result<(), KernelError> {
        panic!("integer loop test unexpectedly retracted a tuple")
    }

    fn replace_functional_tuple(
        &mut self,
        _relation: RelationId,
        _tuple: Tuple,
    ) -> Result<(), KernelError> {
        panic!("integer loop test unexpectedly replaced a tuple")
    }
}

impl DispatchRead for TestHost {}

impl VmHost for TestHost {
    fn authority(&self) -> &AuthorityContext {
        &self.authority
    }

    fn authority_mut(&mut self) -> &mut AuthorityContext {
        &mut self.authority
    }

    fn emit(&mut self, _target: Identity, _value: Value) -> Result<(), RuntimeError> {
        panic!("integer loop test unexpectedly emitted a value")
    }

    fn validate_mailbox_receiver(&mut self, _receiver: &Value) -> Result<(), RuntimeError> {
        panic!("integer loop test unexpectedly validated a mailbox receiver")
    }

    fn resolve_program(
        &mut self,
        _program_bytes_relation: RelationId,
        _program_id: &Value,
    ) -> Result<Arc<Program>, RuntimeError> {
        panic!("integer loop test unexpectedly resolved a program")
    }

    fn call_builtin(&mut self, _name: Symbol, _args: &[Value]) -> Result<Value, RuntimeError> {
        panic!("integer loop test unexpectedly called a builtin")
    }
}

fn integer_loop_program(start: Value, step: Value, limit: Value) -> Arc<Program> {
    direct_loop_program(
        start,
        step,
        limit,
        RuntimeBinaryOp::Add,
        RuntimeBinaryOp::Lt,
    )
}

fn direct_loop_program(
    start: Value,
    step: Value,
    limit: Value,
    arithmetic: RuntimeBinaryOp,
    comparison: RuntimeBinaryOp,
) -> Arc<Program> {
    Arc::new(
        Program::new(
            4,
            [
                Instruction::Load {
                    dst: register(0),
                    value: start,
                },
                Instruction::Load {
                    dst: register(1),
                    value: step,
                },
                Instruction::Load {
                    dst: register(2),
                    value: limit,
                },
                Instruction::Binary {
                    dst: register(0),
                    op: arithmetic,
                    left: register(0),
                    right: register(1),
                },
                Instruction::Binary {
                    dst: register(3),
                    op: comparison,
                    left: register(0),
                    right: register(2),
                },
                Instruction::Branch {
                    condition: register(3),
                    if_true: 3,
                    if_false: 6,
                },
                Instruction::Return {
                    value: Operand::Register(register(0)),
                },
            ],
        )
        .unwrap(),
    )
}

fn canonical_program() -> Arc<Program> {
    integer_loop_program(
        Value::int(0).unwrap(),
        Value::int(1).unwrap(),
        Value::int(ITERATIONS as i64).unwrap(),
    )
}

fn natural_accumulator_program(total: Value) -> Arc<Program> {
    natural_accumulator_program_with_limit(total, ITERATIONS)
}

fn natural_accumulator_program_with_limit(total: Value, limit: usize) -> Arc<Program> {
    Arc::new(
        Program::new(
            8,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: total,
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::empty_relation(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(limit as i64).unwrap(),
                },
                Instruction::Binary {
                    dst: register(4),
                    op: RuntimeBinaryOp::Lt,
                    left: register(0),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(4),
                    if_true: 6,
                    if_false: 12,
                },
                Instruction::Load {
                    dst: register(5),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Add,
                    left: register(0),
                    right: register(5),
                },
                Instruction::Move {
                    dst: register(0),
                    src: register(6),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(0),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(7),
                },
                Instruction::Jump { target: 3 },
                Instruction::Return {
                    value: Operand::Register(register(1)),
                },
            ],
        )
        .unwrap(),
    )
}

fn checked_natural_accumulator_program() -> Arc<Program> {
    Arc::new(
        Program::new(
            8,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::empty_relation(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Unary {
                    dst: register(2),
                    op: RuntimeUnaryOp::Not,
                    src: register(2),
                },
                Instruction::CheckKind {
                    value: register(1),
                    expected: mica_var::ValueKind::Int,
                    site: KindCheckSite::Binding,
                    subject: Symbol::intern("total"),
                },
                Instruction::Binary {
                    dst: register(4),
                    op: RuntimeBinaryOp::Lt,
                    left: register(0),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(4),
                    if_true: 8,
                    if_false: 14,
                },
                Instruction::Load {
                    dst: register(5),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Add,
                    left: register(0),
                    right: register(5),
                },
                Instruction::Move {
                    dst: register(0),
                    src: register(6),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(0),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(7),
                },
                Instruction::Jump { target: 6 },
                Instruction::Return {
                    value: Operand::Register(register(1)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_collection_program(collection: Value) -> Arc<Program> {
    natural_numeric_collection_program(collection, Value::int(0).unwrap())
}

fn natural_numeric_collection_program(collection: Value, initial: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            9,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::CollectionLen {
                    dst: register(1),
                    collection: register(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: initial,
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 13,
                },
                Instruction::CollectionValueAt {
                    dst: register(6),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(6),
                },
                Instruction::Move {
                    dst: register(3),
                    src: register(7),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(8),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return {
                    value: Operand::Register(register(3)),
                },
            ],
        )
        .unwrap(),
    )
}

fn checked_float_collection_program(collection: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            9,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::CollectionLen {
                    dst: register(1),
                    collection: register(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::float(0.0).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 14,
                },
                Instruction::CollectionValueAt {
                    dst: register(6),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::CheckKind {
                    value: register(6),
                    expected: mica_var::ValueKind::Float,
                    site: KindCheckSite::Binding,
                    subject: Symbol::intern("element"),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(6),
                },
                Instruction::Move {
                    dst: register(3),
                    src: register(7),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(8),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return {
                    value: Operand::Register(register(3)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_div_rem_collection_program(
    element: Value,
    divisor: Value,
    operation: RuntimeBinaryOp,
) -> Arc<Program> {
    let collection = Value::list((0..ITERATIONS).map(|_| element.clone()));
    Arc::new(
        Program::new(
            11,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::CollectionLen {
                    dst: register(1),
                    collection: register(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::float(0.0).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Load {
                    dst: register(5),
                    value: divisor,
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(6),
                    if_true: 8,
                    if_false: 15,
                },
                Instruction::CollectionValueAt {
                    dst: register(7),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: operation,
                    left: register(7),
                    right: register(5),
                },
                Instruction::Binary {
                    dst: register(9),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(8),
                },
                Instruction::Move {
                    dst: register(3),
                    src: register(9),
                },
                Instruction::Binary {
                    dst: register(10),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(10),
                },
                Instruction::Jump { target: 6 },
                Instruction::Return {
                    value: Operand::Register(register(3)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_range_program() -> Arc<Program> {
    natural_range_program_with_iterations(ITERATIONS)
}

fn natural_range_program_with_iterations(iterations: usize) -> Arc<Program> {
    natural_collection_program(Value::range(
        Value::int(1).unwrap(),
        Some(Value::int(iterations as i64).unwrap()),
    ))
}

fn natural_indexed_collection_program(collection: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            11,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::CollectionLen {
                    dst: register(1),
                    collection: register(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 15,
                },
                Instruction::CollectionKeyAt {
                    dst: register(6),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::CollectionValueAt {
                    dst: register(7),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(6),
                },
                Instruction::Binary {
                    dst: register(9),
                    op: RuntimeBinaryOp::Add,
                    left: register(8),
                    right: register(7),
                },
                Instruction::Move {
                    dst: register(3),
                    src: register(9),
                },
                Instruction::Binary {
                    dst: register(10),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(10),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return {
                    value: Operand::Register(register(3)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_indexed_range_program() -> Arc<Program> {
    natural_indexed_collection_program(Value::range(
        Value::int(1).unwrap(),
        Some(Value::int(ITERATIONS as i64).unwrap()),
    ))
}

fn natural_index_program(collection: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            9,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(1),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 13,
                },
                Instruction::Index {
                    dst: register(6),
                    collection: register(0),
                    index: Operand::Register(register(1)),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(6),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(7),
                },
                Instruction::Binary {
                    dst: register(8),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(8),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return {
                    value: Operand::Register(register(2)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_repeated_map_index_program(map: Value, key: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            8,
            [
                Instruction::Load {
                    dst: register(0),
                    value: map,
                },
                Instruction::Load {
                    dst: register(1),
                    value: key,
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(5),
                    if_true: 7,
                    if_false: 11,
                },
                Instruction::Index {
                    dst: register(6),
                    collection: register(0),
                    index: Operand::Register(register(1)),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(2),
                    src: register(7),
                },
                Instruction::Jump { target: 5 },
                Instruction::Return {
                    value: Operand::Register(register(6)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_constant_map_index_program(map: Value, key: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            7,
            [
                Instruction::Load {
                    dst: register(0),
                    value: map,
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(4),
                    op: RuntimeBinaryOp::Lt,
                    left: register(1),
                    right: register(2),
                },
                Instruction::Branch {
                    condition: register(4),
                    if_true: 6,
                    if_false: 10,
                },
                Instruction::Index {
                    dst: register(5),
                    collection: register(0),
                    index: Operand::Value(key),
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(3),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(6),
                },
                Instruction::Jump { target: 4 },
                Instruction::Return {
                    value: Operand::Register(register(5)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_collection_count_program(collection: Value, bind_key: bool) -> Arc<Program> {
    let exit = if bind_key { 14 } else { 13 };
    let mut instructions = vec![
        Instruction::Load {
            dst: register(0),
            value: collection,
        },
        Instruction::CollectionLen {
            dst: register(1),
            collection: register(0),
        },
        Instruction::Load {
            dst: register(2),
            value: Value::int(0).unwrap(),
        },
        Instruction::Load {
            dst: register(3),
            value: Value::int(0).unwrap(),
        },
        Instruction::Load {
            dst: register(4),
            value: Value::int(1).unwrap(),
        },
        Instruction::Binary {
            dst: register(5),
            op: RuntimeBinaryOp::Lt,
            left: register(2),
            right: register(1),
        },
        Instruction::Branch {
            condition: register(5),
            if_true: 7,
            if_false: exit,
        },
    ];
    if bind_key {
        instructions.push(Instruction::CollectionKeyAt {
            dst: register(6),
            collection: register(0),
            index: register(2),
        });
    }
    instructions.extend([
        Instruction::CollectionValueAt {
            dst: register(6),
            collection: register(0),
            index: register(2),
        },
        Instruction::Binary {
            dst: register(7),
            op: RuntimeBinaryOp::Add,
            left: register(3),
            right: register(4),
        },
        Instruction::Move {
            dst: register(3),
            src: register(7),
        },
        Instruction::Binary {
            dst: register(8),
            op: RuntimeBinaryOp::Add,
            left: register(2),
            right: register(4),
        },
        Instruction::Move {
            dst: register(2),
            src: register(8),
        },
        Instruction::Jump { target: 5 },
        Instruction::Return {
            value: Operand::Register(register(3)),
        },
    ]);
    Arc::new(Program::new(9, instructions).unwrap())
}

fn natural_heap_comparison_program(
    candidate: Value,
    target: Value,
    op: RuntimeBinaryOp,
) -> Arc<Program> {
    let collection = Value::list((0..ITERATIONS).map(|_| candidate.clone()));
    Arc::new(
        Program::new(
            9,
            [
                Instruction::Load {
                    dst: register(0),
                    value: collection,
                },
                Instruction::CollectionLen {
                    dst: register(1),
                    collection: register(0),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Load {
                    dst: register(5),
                    value: target,
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Lt,
                    left: register(2),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(6),
                    if_true: 8,
                    if_false: 14,
                },
                Instruction::CollectionValueAt {
                    dst: register(7),
                    collection: register(0),
                    index: register(2),
                },
                Instruction::Binary {
                    dst: register(8),
                    op,
                    left: register(7),
                    right: register(5),
                },
                Instruction::Branch {
                    condition: register(8),
                    if_true: 11,
                    if_false: 12,
                },
                Instruction::Binary {
                    dst: register(3),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(4),
                },
                Instruction::Binary {
                    dst: register(2),
                    op: RuntimeBinaryOp::Add,
                    left: register(2),
                    right: register(4),
                },
                Instruction::Jump { target: 6 },
                Instruction::Return {
                    value: Operand::Register(register(3)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_countdown_program(iterations: usize) -> Arc<Program> {
    Arc::new(
        Program::new(
            8,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(iterations as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::empty_relation(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Binary {
                    dst: register(4),
                    op: RuntimeBinaryOp::Gt,
                    left: register(0),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(4),
                    if_true: 6,
                    if_false: 12,
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(0),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(7),
                },
                Instruction::Load {
                    dst: register(5),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Sub,
                    left: register(0),
                    right: register(5),
                },
                Instruction::Move {
                    dst: register(0),
                    src: register(6),
                },
                Instruction::Jump { target: 3 },
                Instruction::Return {
                    value: Operand::Register(register(1)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_scalar_program() -> (Arc<Program>, Value) {
    let alpha = Value::symbol(Symbol::intern("scalar_alpha"));
    let beta = Value::symbol(Symbol::intern("scalar_beta"));
    let expected = Value::bool(alpha < beta);
    let program = Program::new(
        7,
        [
            Instruction::Load {
                dst: register(0),
                value: Value::int(0).unwrap(),
            },
            Instruction::Load {
                dst: register(1),
                value: Value::int(ITERATIONS as i64).unwrap(),
            },
            Instruction::Load {
                dst: register(2),
                value: alpha,
            },
            Instruction::Load {
                dst: register(3),
                value: beta,
            },
            Instruction::Binary {
                dst: register(5),
                op: RuntimeBinaryOp::Lt,
                left: register(0),
                right: register(1),
            },
            Instruction::Branch {
                condition: register(5),
                if_true: 6,
                if_false: 12,
            },
            Instruction::Binary {
                dst: register(6),
                op: RuntimeBinaryOp::Lt,
                left: register(2),
                right: register(3),
            },
            Instruction::Unary {
                dst: register(6),
                op: RuntimeUnaryOp::Not,
                src: register(6),
            },
            Instruction::Unary {
                dst: register(6),
                op: RuntimeUnaryOp::Not,
                src: register(6),
            },
            Instruction::Load {
                dst: register(4),
                value: Value::int(1).unwrap(),
            },
            Instruction::Binary {
                dst: register(0),
                op: RuntimeBinaryOp::Add,
                left: register(0),
                right: register(4),
            },
            Instruction::Jump { target: 4 },
            Instruction::Return {
                value: Operand::Register(register(6)),
            },
        ],
    )
    .unwrap();
    (Arc::new(program), expected)
}

fn natural_branch_program(
    initial_flag: Value,
    toggle_flag: bool,
    then_increment: Value,
    else_increment: Value,
) -> Arc<Program> {
    Arc::new(
        Program::new(
            8,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: initial_flag,
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(4),
                    value: then_increment,
                },
                Instruction::Load {
                    dst: register(5),
                    value: else_increment,
                },
                Instruction::Load {
                    dst: register(7),
                    value: Value::empty_relation(),
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Lt,
                    left: register(0),
                    right: register(1),
                },
                Instruction::Branch {
                    condition: register(6),
                    if_true: 9,
                    if_false: 17,
                },
                Instruction::Branch {
                    condition: register(2),
                    if_true: 10,
                    if_false: 12,
                },
                Instruction::Binary {
                    dst: register(3),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(4),
                },
                Instruction::Jump { target: 13 },
                Instruction::Binary {
                    dst: register(3),
                    op: RuntimeBinaryOp::Add,
                    left: register(3),
                    right: register(5),
                },
                if toggle_flag {
                    Instruction::Unary {
                        dst: register(2),
                        op: RuntimeUnaryOp::Not,
                        src: register(2),
                    }
                } else {
                    Instruction::Move {
                        dst: register(2),
                        src: register(2),
                    }
                },
                Instruction::Load {
                    dst: register(7),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(0),
                    op: RuntimeBinaryOp::Add,
                    left: register(0),
                    right: register(7),
                },
                Instruction::Jump { target: 7 },
                Instruction::Return {
                    value: Operand::Register(register(3)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_scaled_countdown_program() -> Arc<Program> {
    Arc::new(
        Program::new(
            10,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::empty_relation(),
                },
                Instruction::Load {
                    dst: register(3),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Binary {
                    dst: register(4),
                    op: RuntimeBinaryOp::Gt,
                    left: register(0),
                    right: register(3),
                },
                Instruction::Branch {
                    condition: register(4),
                    if_true: 6,
                    if_false: 14,
                },
                Instruction::Load {
                    dst: register(5),
                    value: Value::int(3).unwrap(),
                },
                Instruction::Binary {
                    dst: register(6),
                    op: RuntimeBinaryOp::Mul,
                    left: register(0),
                    right: register(5),
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(6),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(7),
                },
                Instruction::Load {
                    dst: register(8),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(9),
                    op: RuntimeBinaryOp::Sub,
                    left: register(0),
                    right: register(8),
                },
                Instruction::Move {
                    dst: register(0),
                    src: register(9),
                },
                Instruction::Jump { target: 3 },
                Instruction::Return {
                    value: Operand::Register(register(1)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_integer_surface_program(divisor: Value) -> Arc<Program> {
    Arc::new(
        Program::new(
            17,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::int(0).unwrap(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(ITERATIONS as i64).unwrap(),
                },
                Instruction::Binary {
                    dst: register(3),
                    op: RuntimeBinaryOp::Lt,
                    left: register(0),
                    right: register(2),
                },
                Instruction::Branch {
                    condition: register(3),
                    if_true: 5,
                    if_false: 21,
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(6).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: RuntimeBinaryOp::Mul,
                    left: register(0),
                    right: register(4),
                },
                Instruction::Load {
                    dst: register(6),
                    value: divisor,
                },
                Instruction::Binary {
                    dst: register(7),
                    op: RuntimeBinaryOp::Div,
                    left: register(5),
                    right: register(6),
                },
                Instruction::Load {
                    dst: register(8),
                    value: Value::int(7).unwrap(),
                },
                Instruction::Binary {
                    dst: register(9),
                    op: RuntimeBinaryOp::Rem,
                    left: register(0),
                    right: register(8),
                },
                Instruction::Unary {
                    dst: register(10),
                    op: RuntimeUnaryOp::Neg,
                    src: register(9),
                },
                Instruction::Unary {
                    dst: register(11),
                    op: RuntimeUnaryOp::Not,
                    src: register(0),
                },
                Instruction::Binary {
                    dst: register(12),
                    op: RuntimeBinaryOp::Add,
                    left: register(7),
                    right: register(9),
                },
                Instruction::Binary {
                    dst: register(13),
                    op: RuntimeBinaryOp::Add,
                    left: register(12),
                    right: register(10),
                },
                Instruction::Binary {
                    dst: register(14),
                    op: RuntimeBinaryOp::Add,
                    left: register(1),
                    right: register(13),
                },
                Instruction::Move {
                    dst: register(1),
                    src: register(14),
                },
                Instruction::Load {
                    dst: register(15),
                    value: Value::int(1).unwrap(),
                },
                Instruction::Binary {
                    dst: register(16),
                    op: RuntimeBinaryOp::Add,
                    left: register(0),
                    right: register(15),
                },
                Instruction::Move {
                    dst: register(0),
                    src: register(16),
                },
                Instruction::Jump { target: 2 },
                Instruction::Return {
                    value: Operand::Register(register(1)),
                },
            ],
        )
        .unwrap(),
    )
}

fn natural_comparison_program(
    start: i64,
    limit: i64,
    comparison: RuntimeBinaryOp,
    update: RuntimeBinaryOp,
    step: i64,
) -> Arc<Program> {
    Arc::new(
        Program::new(
            6,
            [
                Instruction::Load {
                    dst: register(0),
                    value: Value::int(start).unwrap(),
                },
                Instruction::Load {
                    dst: register(1),
                    value: Value::empty_relation(),
                },
                Instruction::Load {
                    dst: register(2),
                    value: Value::int(limit).unwrap(),
                },
                Instruction::Binary {
                    dst: register(3),
                    op: comparison,
                    left: register(0),
                    right: register(2),
                },
                Instruction::Branch {
                    condition: register(3),
                    if_true: 5,
                    if_false: 9,
                },
                Instruction::Load {
                    dst: register(4),
                    value: Value::int(step).unwrap(),
                },
                Instruction::Binary {
                    dst: register(5),
                    op: update,
                    left: register(0),
                    right: register(4),
                },
                Instruction::Move {
                    dst: register(0),
                    src: register(5),
                },
                Instruction::Jump { target: 2 },
                Instruction::Return {
                    value: Operand::Register(register(0)),
                },
            ],
        )
        .unwrap(),
    )
}

fn run(vm: &mut RegisterVm, budget: usize) -> Result<VmHostResponse, RuntimeError> {
    vm.run_until_host_response(&mut TestHost::default(), budget, MAX_CALL_DEPTH)
}

fn register(index: u16) -> Register {
    Register(index)
}

#[test]
fn native_integer_loop_matches_interpreter_completion() {
    let program = canonical_program();
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let interpreted_outcome = run(&mut interpreted, INSTRUCTION_COUNT).unwrap();
    let native_outcome = run(&mut native, INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int(ITERATIONS as i64).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_float_loop_matches_interpreter_binary32_execution() {
    let program = integer_loop_program(
        Value::float(0.0).unwrap(),
        Value::float(0.5).unwrap(),
        Value::float(ITERATIONS as f32 / 2.0).unwrap(),
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let interpreted_outcome = run(&mut interpreted, INSTRUCTION_COUNT).unwrap();
    let native_outcome = run(&mut native, INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::float(ITERATIONS as f32 / 2.0).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_float_loop_does_not_compile_below_measured_break_even() {
    let iterations = 4_096;
    let program = integer_loop_program(
        Value::float(0.0).unwrap(),
        Value::float(1.0).unwrap(),
        Value::float(iterations as f32).unwrap(),
    );
    let mut vm = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut vm, (iterations * 3) + 4).unwrap(),
        VmHostResponse::Complete(Value::float(iterations as f32).unwrap()),
    );
    assert_eq!(program.native_compile_attempts(), 0);
}

#[test]
fn native_float_loop_supports_subtract_multiply_and_comparison_variants() {
    let cases = [
        (
            direct_loop_program(
                Value::float(ITERATIONS as f32 / 2.0).unwrap(),
                Value::float(0.5).unwrap(),
                Value::float(0.0).unwrap(),
                RuntimeBinaryOp::Sub,
                RuntimeBinaryOp::Gt,
            ),
            INSTRUCTION_COUNT,
        ),
        (
            direct_loop_program(
                Value::float(1.0).unwrap(),
                Value::float(1.0001).unwrap(),
                Value::float(5.0).unwrap(),
                RuntimeBinaryOp::Mul,
                RuntimeBinaryOp::Le,
            ),
            60_000,
        ),
        (
            direct_loop_program(
                Value::float(5.0).unwrap(),
                Value::float(1.0001).unwrap(),
                Value::float(1.0).unwrap(),
                RuntimeBinaryOp::Div,
                RuntimeBinaryOp::Ge,
            ),
            60_000,
        ),
        (
            direct_loop_program(
                Value::float(0.0).unwrap(),
                Value::float(1.0).unwrap(),
                Value::float(ITERATIONS as f32).unwrap(),
                RuntimeBinaryOp::Add,
                RuntimeBinaryOp::Ne,
            ),
            INSTRUCTION_COUNT,
        ),
    ];

    for (program, budget) in cases {
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));

        assert_eq!(
            run(&mut native, budget).unwrap(),
            run(&mut interpreted, budget).unwrap(),
        );
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
    }
}

#[test]
fn native_integer_loop_preserves_every_budget_boundary() {
    for budget in [
        1,
        3,
        5,
        6,
        7,
        1_024,
        INSTRUCTION_COUNT - 2,
        INSTRUCTION_COUNT - 1,
    ] {
        let program = canonical_program();
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        assert!(matches!(
            run(&mut interpreted, budget),
            Err(RuntimeError::InstructionBudgetExceeded { .. })
        ));
        assert!(matches!(
            run(&mut native, budget),
            Err(RuntimeError::InstructionBudgetExceeded { .. })
        ));
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    }
}

#[test]
fn native_integer_loop_keeps_mixed_numeric_loop_interpreted() {
    let program = integer_loop_program(
        Value::int(0).unwrap(),
        Value::int(1).unwrap(),
        Value::float(10.0).unwrap(),
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(run(&mut native, 100), run(&mut interpreted, 100));
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 0);
}

#[test]
fn native_integer_loop_does_not_compile_short_cold_loops() {
    let program = integer_loop_program(
        Value::int(0).unwrap(),
        Value::int(1).unwrap(),
        Value::int(32).unwrap(),
    );
    let mut vm = RegisterVm::new(Arc::clone(&program));
    assert_eq!(
        run(&mut vm, 100).unwrap(),
        VmHostResponse::Complete(Value::int(32).unwrap()),
    );
    assert_eq!(program.native_compile_attempts(), 0);
}

#[test]
fn native_integer_loop_keeps_short_overflow_path_interpreted() {
    let max = mica_var::abi::VALUE_INT_MAX;
    let program = integer_loop_program(
        Value::int(max - 5).unwrap(),
        Value::int(2).unwrap(),
        Value::int(max).unwrap(),
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let interpreted_outcome = run(&mut interpreted, 100).unwrap();
    let native_outcome = run(&mut native, 100).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert!(matches!(native_outcome, VmHostResponse::Abort(_)));
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 0);
}

#[test]
fn native_integer_loop_cache_is_shared_across_threads() {
    let program = canonical_program();
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let program = Arc::clone(&program);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut vm = RegisterVm::new(program);
            barrier.wait();
            run(&mut vm, INSTRUCTION_COUNT).unwrap()
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            VmHostResponse::Complete(Value::int(ITERATIONS as i64).unwrap()),
        );
    }
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_integer_loop_resumes_from_snapshotted_budget_exit() {
    let program = canonical_program();
    let mut vm = RegisterVm::new(Arc::clone(&program));
    assert!(run(&mut vm, INSTRUCTION_COUNT - 1).is_err());
    let state = vm.snapshot_state();
    let mut resumed = RegisterVm::from_state(state.clone());
    let mut interpreted = RegisterVm::new_interpreted(program);
    interpreted.restore_state(&state);

    assert_eq!(
        run(&mut resumed, 1).unwrap(),
        run(&mut interpreted, 1).unwrap()
    );
    assert_eq!(resumed.snapshot_state(), interpreted.snapshot_state());
}

#[test]
fn native_natural_loop_matches_interpreter_completion() {
    let program = natural_accumulator_program(Value::int(0).unwrap());
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let interpreted_outcome = run(&mut interpreted, NATURAL_INSTRUCTION_COUNT).unwrap();
    let native_outcome = run(&mut native, NATURAL_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int(134_225_920).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn natural_integer_accumulators_select_unboxed_region_state() {
    let integer = natural_accumulator_program(Value::int(0).unwrap());
    let integer_site = integer
        .natural_integer_loop_site(5)
        .expect("integer accumulator has a natural loop");
    assert_ne!(integer_site.plan.unboxed_integer_slots(), 0);

    let float = natural_accumulator_program(Value::float(0.0).unwrap());
    let float_site = float
        .natural_integer_loop_site(5)
        .expect("float accumulator has a natural loop");
    assert_ne!(float_site.plan.unboxed_integer_slots(), 0);
    assert_ne!(float_site.plan.unboxed_float_slots(), 0);
}

#[test]
fn dominating_kind_checks_feed_unboxed_region_planning() {
    let program = checked_natural_accumulator_program();
    let site = program
        .natural_integer_loop_site(7)
        .expect("checked accumulator has a natural loop");
    assert_ne!(site.plan.unboxed_integer_slots(), 0);

    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));
    assert_eq!(
        run(&mut native, NATURAL_INSTRUCTION_COUNT + 2).unwrap(),
        run(&mut interpreted, NATURAL_INSTRUCTION_COUNT + 2).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_range_loop_matches_interpreter_completion() {
    let program = natural_range_program();
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_range_loop_uses_its_measured_admission_threshold() {
    for (iterations, expected_compile_attempts) in [(8_191, 0), (8_192, 1)] {
        let program = natural_range_program_with_iterations(iterations);
        let mut native = RegisterVm::new(Arc::clone(&program));

        assert_eq!(
            run(&mut native, (iterations * 8) + 8).unwrap(),
            VmHostResponse::Complete(
                Value::int((iterations as i64 * (iterations as i64 + 1)) / 2).unwrap()
            ),
        );
        assert_eq!(program.native_compile_attempts(), expected_compile_attempts);
    }
}

#[test]
fn native_natural_range_loop_preserves_budget_remainders() {
    for budget in (NATURAL_RANGE_INSTRUCTION_COUNT - 10)..=NATURAL_RANGE_INSTRUCTION_COUNT {
        let program = natural_range_program();
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_natural_list_loop_matches_interpreter_completion() {
    let collection = Value::list((1..=ITERATIONS as i64).map(|value| Value::int(value).unwrap()));
    let program = natural_collection_program(collection);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_float_and_mixed_collection_sums_match_interpreter() {
    let cases = [
        (
            Value::list((0..ITERATIONS).map(|_| Value::float(0.25).unwrap())),
            Value::float(0.0).unwrap(),
            Value::float(4_096.0).unwrap(),
        ),
        (
            Value::list((0..ITERATIONS).map(|_| Value::int(2).unwrap())),
            Value::float(0.0).unwrap(),
            Value::float(32_768.0).unwrap(),
        ),
    ];
    for (collection, initial, expected) in cases {
        let program = natural_numeric_collection_program(collection, initial);
        let site = program
            .natural_integer_loop_site(6)
            .expect("numeric collection sum has a natural loop");
        assert_ne!(site.plan.unboxed_integer_slots(), 0);
        assert_ne!(site.plan.unboxed_float_slots(), 0);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));

        let interpreted_outcome = run(&mut interpreted, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap();
        let native_outcome = run(&mut native, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap();
        assert_eq!(native_outcome, interpreted_outcome);
        assert_eq!(native_outcome, VmHostResponse::Complete(expected));
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
        assert_eq!(native.native_side_exit_count(), 0);
    }
}

#[test]
fn native_natural_float_collection_sum_executes_concurrently() {
    let program = natural_numeric_collection_program(
        Value::list((0..ITERATIONS).map(|_| Value::float(0.25).unwrap())),
        Value::float(0.0).unwrap(),
    );
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let program = Arc::clone(&program);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut vm = RegisterVm::new(program);
            barrier.wait();
            let outcome = run(&mut vm, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap();
            (outcome, vm.native_side_exit_count())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (VmHostResponse::Complete(Value::float(4_096.0).unwrap()), 0,),
        );
    }
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn checked_collection_ingress_feeds_native_float_state_and_preserves_type_errors() {
    let collection = Value::list((0..ITERATIONS).map(|_| Value::string("not a float")));
    let program = checked_float_collection_program(collection);
    let site = program
        .natural_integer_loop_site(6)
        .expect("checked collection loop has a natural loop");
    assert_ne!(site.plan.unboxed_float_slots(), 0);

    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));
    let interpreted_outcome = run(&mut interpreted, NATURAL_INSTRUCTION_COUNT).unwrap();
    let native_outcome = run(&mut native, NATURAL_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert!(matches!(native_outcome, VmHostResponse::Abort(_)));
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 1);
}

#[test]
fn native_natural_division_and_remainder_match_interpreter() {
    for (element, divisor, operation) in [
        (
            Value::int(3).unwrap(),
            Value::int(2).unwrap(),
            RuntimeBinaryOp::Div,
        ),
        (
            Value::int(3).unwrap(),
            Value::float(2.0).unwrap(),
            RuntimeBinaryOp::Div,
        ),
        (
            Value::float(5.5).unwrap(),
            Value::float(2.0).unwrap(),
            RuntimeBinaryOp::Rem,
        ),
    ] {
        let program = natural_div_rem_collection_program(element, divisor, operation);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));
        let expected = VmHostResponse::Complete(Value::float(24_576.0).unwrap());

        assert_eq!(
            run(&mut interpreted, NATURAL_DIV_REM_INSTRUCTION_COUNT).unwrap(),
            expected,
        );
        assert_eq!(
            run(&mut native, NATURAL_DIV_REM_INSTRUCTION_COUNT).unwrap(),
            expected,
        );
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
        assert_eq!(native.native_side_exit_count(), 0);
    }
}

#[test]
fn native_natural_float_remainder_executes_concurrently() {
    let program = natural_div_rem_collection_program(
        Value::float(5.5).unwrap(),
        Value::float(2.0).unwrap(),
        RuntimeBinaryOp::Rem,
    );
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let program = Arc::clone(&program);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut vm = RegisterVm::new(program);
            barrier.wait();
            let outcome = run(&mut vm, NATURAL_DIV_REM_INSTRUCTION_COUNT).unwrap();
            (outcome, vm.native_side_exit_count())
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            (VmHostResponse::Complete(Value::float(24_576.0).unwrap()), 0,),
        );
    }
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_indexed_range_loop_matches_interpreter_completion() {
    let program = natural_indexed_range_program();
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state(),);
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_indexed_range_loop_preserves_budget_remainders() {
    for budget in
        (NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT - 12)..=NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT
    {
        let program = natural_indexed_range_program();
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_natural_indexed_list_loop_matches_interpreter_completion() {
    let collection = Value::list((1..=ITERATIONS as i64).map(|value| Value::int(value).unwrap()));
    let program = natural_indexed_collection_program(collection);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state(),);
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_list_index_loop_matches_interpreter_completion() {
    let collection = Value::list((1..=ITERATIONS as i64).map(|value| Value::int(value).unwrap()));
    let program = natural_index_program(collection);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_LIST_INDEX_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_LIST_INDEX_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_list_index_loop_preserves_budget_remainders() {
    for budget in (NATURAL_LIST_INDEX_INSTRUCTION_COUNT - 10)..=NATURAL_LIST_INDEX_INSTRUCTION_COUNT
    {
        let collection =
            Value::list((1..=ITERATIONS as i64).map(|value| Value::int(value).unwrap()));
        let program = natural_index_program(collection);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_natural_map_value_loop_matches_interpreter_completion() {
    let collection = Value::map(
        (0..ITERATIONS as i64)
            .map(|index| (Value::int(index).unwrap(), Value::int(index + 1).unwrap())),
    );
    let program = natural_collection_program(collection);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_RANGE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_indexed_map_loop_matches_interpreter_completion() {
    let collection = Value::map(
        (0..ITERATIONS as i64)
            .map(|index| (Value::int(index).unwrap(), Value::int(index + 1).unwrap())),
    );
    let program = natural_indexed_collection_program(collection);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_INDEXED_RANGE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_collection_loops_commit_owned_clones_of_heap_bindings() {
    let cases =
        [
            (
                natural_collection_count_program(
                    Value::list((0..ITERATIONS).map(|_| Value::string("value"))),
                    false,
                ),
                NATURAL_RANGE_INSTRUCTION_COUNT,
            ),
            (
                natural_collection_count_program(
                    Value::map((0..ITERATIONS as i64).map(|index| {
                        (Value::int(index).unwrap(), Value::string(index.to_string()))
                    })),
                    false,
                ),
                NATURAL_RANGE_INSTRUCTION_COUNT,
            ),
        ];

    for (program, instruction_count) in cases {
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));
        assert_eq!(
            run(&mut native, instruction_count).unwrap(),
            run(&mut interpreted, instruction_count).unwrap(),
        );
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
        assert_eq!(native.native_side_exit_count(), 0);
    }
}

#[test]
fn native_heap_collection_binding_preserves_budget_remainders() {
    for budget in [65_600, NATURAL_RANGE_INSTRUCTION_COUNT - 1] {
        let collection = Value::list((0..ITERATIONS).map(|_| Value::string("value")));
        let program = natural_collection_count_program(collection, false);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));

        assert!(matches!(
            run(&mut native, budget),
            Err(RuntimeError::InstructionBudgetExceeded { .. })
        ));
        assert!(matches!(
            run(&mut interpreted, budget),
            Err(RuntimeError::InstructionBudgetExceeded { .. })
        ));
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
        assert_eq!(program.native_compile_attempts(), 1);
    }
}

#[test]
fn native_natural_map_index_loop_matches_interpreter_completion() {
    let collection = Value::map(
        (0..ITERATIONS as i64)
            .map(|index| (Value::int(index).unwrap(), Value::int(index + 1).unwrap())),
    );
    let program = natural_index_program(collection);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_LIST_INDEX_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_LIST_INDEX_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_map_index_loop_preserves_budget_remainders() {
    for budget in (NATURAL_LIST_INDEX_INSTRUCTION_COUNT - 10)..=NATURAL_LIST_INDEX_INSTRUCTION_COUNT
    {
        let collection = Value::map(
            (0..ITERATIONS as i64)
                .map(|index| (Value::int(index).unwrap(), Value::int(index + 1).unwrap())),
        );
        let program = natural_index_program(collection);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_map_index_preserves_canonical_keys_and_heap_outputs() {
    let map = Value::map([
        (Value::int(1).unwrap(), Value::int(10).unwrap()),
        (Value::float(1.0).unwrap(), Value::int(20).unwrap()),
        (
            Value::symbol(Symbol::intern("map_index_heap_value")),
            Value::string("heap output"),
        ),
    ]);
    let cases = [
        (Value::int(1).unwrap(), Value::int(10).unwrap()),
        (Value::float(1.0).unwrap(), Value::int(20).unwrap()),
        (
            Value::symbol(Symbol::intern("map_index_heap_value")),
            Value::string("heap output"),
        ),
    ];

    for (key, expected) in cases {
        let program = natural_repeated_map_index_program(map.clone(), key);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));
        let native_outcome =
            run(&mut native, NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT).unwrap();

        assert_eq!(
            native_outcome,
            run(
                &mut interpreted,
                NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT,
            )
            .unwrap(),
        );
        assert_eq!(native_outcome, VmHostResponse::Complete(expected));
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
        assert_eq!(native.native_side_exit_count(), 0);
    }
}

#[test]
fn native_missing_map_index_side_exits_to_the_interpreter_error() {
    let map = Value::map([(Value::int(1).unwrap(), Value::int(10).unwrap())]);
    let program = natural_repeated_map_index_program(map, Value::int(2).unwrap());
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT).unwrap();
    assert_eq!(
        native_outcome,
        run(
            &mut interpreted,
            NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT,
        )
        .unwrap(),
    );
    assert!(matches!(
        native_outcome,
        VmHostResponse::Abort(error)
            if error.error_code_symbol() == Some(Symbol::intern("E_INDEX"))
    ));
    assert!(program.native_compile_attempts() > 0);
    assert!(native.native_side_exit_count() > 0);
}

#[test]
fn native_map_index_accepts_immediate_constant_operands() {
    let key = Value::symbol(Symbol::intern("constant_map_index"));
    let program = natural_constant_map_index_program(
        Value::map([(key.clone(), Value::int(17).unwrap())]),
        key,
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));
    let native_outcome = run(&mut native, NATURAL_CONSTANT_MAP_INDEX_INSTRUCTION_COUNT).unwrap();

    assert_eq!(
        native_outcome,
        run(
            &mut interpreted,
            NATURAL_CONSTANT_MAP_INDEX_INSTRUCTION_COUNT,
        )
        .unwrap(),
    );
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int(17).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_map_index_heap_key_uses_helper_without_side_exit() {
    let key = Value::string("heap key");
    let program = natural_repeated_map_index_program(
        Value::map([(key.clone(), Value::int(7).unwrap())]),
        key,
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT,).unwrap(),
        run(
            &mut interpreted,
            NATURAL_REPEATED_MAP_INDEX_INSTRUCTION_COUNT,
        )
        .unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_string_equality_loop_calls_helper_without_side_exit() {
    let value = Value::string("compiled equality");
    let program = natural_heap_comparison_program(value.clone(), value, RuntimeBinaryOp::Eq);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let expected = VmHostResponse::Complete(Value::int(ITERATIONS as i64).unwrap());
    assert_eq!(
        run(&mut interpreted, NATURAL_HEAP_COMPARISON_INSTRUCTION_COUNT).unwrap(),
        expected,
    );
    assert_eq!(
        run(&mut native, NATURAL_HEAP_COMPARISON_INSTRUCTION_COUNT).unwrap(),
        expected,
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_language_ordering_loops_call_helper_without_side_exit() {
    let cases = [
        (
            Value::string("alpha"),
            Value::string("beta"),
            RuntimeBinaryOp::Lt,
        ),
        (
            Value::list([Value::int(1).unwrap()]),
            Value::list([Value::int(1).unwrap()]),
            RuntimeBinaryOp::Le,
        ),
        (
            Value::map([(Value::string("key"), Value::int(2).unwrap())]),
            Value::map([(Value::string("key"), Value::int(1).unwrap())]),
            RuntimeBinaryOp::Gt,
        ),
        (
            Value::int(16_777_217).unwrap(),
            Value::float(16_777_216.0).unwrap(),
            RuntimeBinaryOp::Ge,
        ),
    ];

    for (candidate, target, op) in cases {
        let program = natural_heap_comparison_program(candidate, target, op);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));
        let expected = VmHostResponse::Complete(Value::int(ITERATIONS as i64).unwrap());

        assert_eq!(
            run(&mut interpreted, NATURAL_HEAP_COMPARISON_INSTRUCTION_COUNT).unwrap(),
            expected,
        );
        assert_eq!(
            run(&mut native, NATURAL_HEAP_COMPARISON_INSTRUCTION_COUNT).unwrap(),
            expected,
        );
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
        assert_eq!(native.native_side_exit_count(), 0);
    }
}

#[test]
fn native_natural_loop_preserves_budget_remainders() {
    for budget in (NATURAL_INSTRUCTION_COUNT - 12)..=NATURAL_INSTRUCTION_COUNT {
        let program = natural_accumulator_program(Value::int(0).unwrap());
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_natural_loop_executes_float_accumulation_without_side_exits() {
    let program = natural_accumulator_program(Value::float(0.0).unwrap());
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let interpreted_outcome = run(&mut interpreted, NATURAL_INSTRUCTION_COUNT).unwrap();
    let native_outcome = run(&mut native, NATURAL_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_loop_does_not_compile_short_cold_loops() {
    let program = natural_accumulator_program_with_limit(Value::int(0).unwrap(), 32);
    let mut vm = RegisterVm::new(Arc::clone(&program));
    assert_eq!(
        run(&mut vm, (32 * 9) + 7).unwrap(),
        VmHostResponse::Complete(Value::int(528).unwrap()),
    );
    assert_eq!(program.native_compile_attempts(), 0);
}

#[test]
fn native_natural_countdown_loop_matches_interpreter() {
    let program = natural_countdown_program(ITERATIONS);
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_loop_executes_boolean_and_symbol_operations() {
    let (program, expected) = natural_scalar_program();
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, NATURAL_SCALAR_INSTRUCTION_COUNT).unwrap();
    let interpreted_outcome = run(&mut interpreted, NATURAL_SCALAR_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(native_outcome, VmHostResponse::Complete(expected));
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_loop_executes_predictable_internal_branch_and_join() {
    let program = natural_branch_program(
        Value::bool(true),
        false,
        Value::int(1).unwrap(),
        Value::int(2).unwrap(),
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, PREDICTABLE_BRANCH_INSTRUCTION_COUNT).unwrap();
    let interpreted_outcome = run(&mut interpreted, PREDICTABLE_BRANCH_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int(ITERATIONS as i64).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_loop_executes_alternating_internal_branches() {
    let program = natural_branch_program(
        Value::bool(true),
        true,
        Value::int(1).unwrap(),
        Value::int(2).unwrap(),
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, ALTERNATING_BRANCH_INSTRUCTION_COUNT).unwrap();
    let interpreted_outcome = run(&mut interpreted, ALTERNATING_BRANCH_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int((ITERATIONS / 2 * 3) as i64).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_branch_loop_preserves_unequal_path_budget_boundaries() {
    let program = natural_branch_program(
        Value::bool(true),
        true,
        Value::int(1).unwrap(),
        Value::int(2).unwrap(),
    );
    let mut warm = RegisterVm::new(Arc::clone(&program));
    assert!(run(&mut warm, ALTERNATING_BRANCH_INSTRUCTION_COUNT).is_ok());
    assert_eq!(program.native_compile_attempts(), 1);

    for budget in (20..=200)
        .chain((ALTERNATING_BRANCH_INSTRUCTION_COUNT - 12)..=ALTERNATING_BRANCH_INSTRUCTION_COUNT)
    {
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));
        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_branch_loop_executes_float_arithmetic_in_either_arm() {
    for (initial_flag, then_increment, else_increment) in [
        (
            Value::bool(true),
            Value::float(1.0).unwrap(),
            Value::int(2).unwrap(),
        ),
        (
            Value::bool(true),
            Value::int(1).unwrap(),
            Value::float(2.0).unwrap(),
        ),
    ] {
        let program = natural_branch_program(initial_flag, true, then_increment, else_increment);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));

        assert_eq!(
            run(&mut native, ALTERNATING_BRANCH_INSTRUCTION_COUNT).unwrap(),
            run(&mut interpreted, ALTERNATING_BRANCH_INSTRUCTION_COUNT).unwrap(),
        );
        assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
        assert_eq!(program.native_compile_attempts(), 1);
        assert_eq!(native.native_side_exit_count(), 0);
    }
}

#[test]
fn native_branch_loop_side_exit_from_condition_is_atomic() {
    let program = natural_branch_program(
        Value::list([]),
        false,
        Value::int(1).unwrap(),
        Value::int(2).unwrap(),
    );
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, ALTERNATING_BRANCH_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, ALTERNATING_BRANCH_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 1);
}

#[test]
fn native_natural_scaled_countdown_loop_matches_interpreter() {
    let program = natural_scaled_countdown_program();
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, NATURAL_ARITHMETIC_INSTRUCTION_COUNT).unwrap();
    let interpreted_outcome = run(&mut interpreted, NATURAL_ARITHMETIC_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int(402_677_760).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_loop_covers_the_integer_operation_surface() {
    let program = natural_integer_surface_program(Value::int(3).unwrap());
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap();
    let interpreted_outcome =
        run(&mut interpreted, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert_eq!(
        native_outcome,
        VmHostResponse::Complete(Value::int(268_419_072).unwrap()),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_integer_surface_preserves_budget_remainders() {
    for budget in
        (NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT - 20)..=NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT
    {
        let program = natural_integer_surface_program(Value::int(3).unwrap());
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(program);

        let interpreted_outcome = run(&mut interpreted, budget);
        let native_outcome = run(&mut native, budget);
        match (native_outcome, interpreted_outcome) {
            (Ok(native), Ok(interpreted)) => assert_eq!(native, interpreted, "budget {budget}"),
            (
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
                Err(RuntimeError::InstructionBudgetExceeded { .. }),
            ) => {}
            (native, interpreted) => panic!(
                "budget {budget} produced different outcomes: native={native:?} interpreted={interpreted:?}",
            ),
        }
        assert_eq!(
            native.snapshot_state(),
            interpreted.snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn native_natural_fractional_division_executes_without_side_exits() {
    let program = natural_integer_surface_program(Value::int(4).unwrap());
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    assert_eq!(
        run(&mut native, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap(),
        run(&mut interpreted, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap(),
    );
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 0);
}

#[test]
fn native_natural_zero_division_side_exits_without_trapping() {
    let program = natural_integer_surface_program(Value::int(0).unwrap());
    let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
    let mut native = RegisterVm::new(Arc::clone(&program));

    let native_outcome = run(&mut native, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap();
    let interpreted_outcome =
        run(&mut interpreted, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert!(matches!(native_outcome, VmHostResponse::Abort(_)));
    assert_eq!(native.snapshot_state(), interpreted.snapshot_state());
    assert_eq!(program.native_compile_attempts(), 1);
    assert_eq!(native.native_side_exit_count(), 1);
}

#[test]
fn native_natural_countdown_loop_does_not_compile_when_short() {
    let program = natural_countdown_program(32);
    let mut vm = RegisterVm::new(Arc::clone(&program));
    assert_eq!(
        run(&mut vm, (32 * 9) + 7).unwrap(),
        VmHostResponse::Complete(Value::int(528).unwrap()),
    );
    assert_eq!(program.native_compile_attempts(), 0);
}

#[test]
fn native_natural_loop_supports_inclusive_and_inequality_conditions() {
    let cases = [
        (
            0,
            4_095,
            RuntimeBinaryOp::Le,
            RuntimeBinaryOp::Add,
            1,
            4_096,
        ),
        (4_095, 0, RuntimeBinaryOp::Ge, RuntimeBinaryOp::Sub, 1, -1),
        (
            0,
            4_096,
            RuntimeBinaryOp::Ne,
            RuntimeBinaryOp::Add,
            1,
            4_096,
        ),
    ];
    let instruction_count = (4_096 * 7) + 6;
    for (start, limit, comparison, update, step, expected) in cases {
        let program = natural_comparison_program(start, limit, comparison, update, step);
        let mut interpreted = RegisterVm::new_interpreted(Arc::clone(&program));
        let mut native = RegisterVm::new(Arc::clone(&program));
        assert_eq!(
            run(&mut native, instruction_count).unwrap(),
            run(&mut interpreted, instruction_count).unwrap(),
        );
        assert_eq!(
            run(&mut RegisterVm::new(program.clone()), instruction_count).unwrap(),
            VmHostResponse::Complete(Value::int(expected).unwrap()),
        );
        assert_eq!(program.native_compile_attempts(), 1);
    }
}

#[test]
fn native_natural_loop_cache_is_shared_across_threads() {
    let program = natural_accumulator_program(Value::int(0).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let program = Arc::clone(&program);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut vm = RegisterVm::new(program);
            barrier.wait();
            run(&mut vm, NATURAL_INSTRUCTION_COUNT).unwrap()
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            VmHostResponse::Complete(Value::int(134_225_920).unwrap()),
        );
    }
    assert_eq!(program.native_compile_attempts(), 1);
}

#[test]
fn native_natural_integer_surface_cache_is_shared_across_threads() {
    let program = natural_integer_surface_program(Value::int(3).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let program = Arc::clone(&program);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let mut vm = RegisterVm::new(program);
            barrier.wait();
            run(&mut vm, NATURAL_INTEGER_SURFACE_INSTRUCTION_COUNT).unwrap()
        }));
    }
    for thread in threads {
        assert_eq!(
            thread.join().unwrap(),
            VmHostResponse::Complete(Value::int(268_419_072).unwrap()),
        );
    }
    assert_eq!(program.native_compile_attempts(), 1);
}
