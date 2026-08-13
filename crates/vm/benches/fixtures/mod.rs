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

use mica_relation_kernel::{
    DispatchRead, KernelError, RelationId, RelationRead, RelationWorkspace, Tuple,
};
use mica_var::{Identity, Symbol, Value};
use mica_vm::{
    AuthorityContext, Instruction, Operand, Program, Register, RuntimeBinaryOp, RuntimeError,
    RuntimeUnaryOp, VmHost,
};
use std::sync::Arc;

pub const INTEGER_LOOP_ITERATIONS: usize = 16_384;
pub const INTEGER_LOOP_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 3) + 4;
pub const NATURAL_INTEGER_ACCUMULATOR_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 7) + 7;
pub const SCALAR_LOOP_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 8) + 7;
pub const PREDICTABLE_BRANCH_LOOP_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 9) + 10;
pub const ALTERNATING_BRANCH_LOOP_INSTRUCTIONS: u64 =
    (INTEGER_LOOP_ITERATIONS as u64 / 2 * 17) + 10;
pub const STATIC_CALL_COUNT: usize = 8_192;
pub const STATIC_CALL_INSTRUCTIONS: u64 = (STATIC_CALL_COUNT as u64 * 2) + 1;
pub const BUILTIN_CALL_COUNT: usize = 16_384;
pub const BUILTIN_CALL_INSTRUCTIONS: u64 = BUILTIN_CALL_COUNT as u64 + 1;
pub const NATURAL_FLOAT_SUM_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 7) + 8;
pub const NATURAL_FLOAT_TRANSFORM_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 10) + 9;
pub const NATURAL_MIXED_SCALE_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 9) + 9;
pub const NATURAL_NUMERIC_DIV_REM_INSTRUCTIONS: u64 = (INTEGER_LOOP_ITERATIONS as u64 * 9) + 9;
pub const MAX_CALL_DEPTH: usize = 64;

pub struct ProgramFixture {
    pub program: Arc<Program>,
    pub instruction_count: u64,
}

pub fn integer_loop_fixture() -> ProgramFixture {
    direct_loop_fixture(
        int(0),
        int(1),
        int(INTEGER_LOOP_ITERATIONS as i64),
        RuntimeBinaryOp::Add,
        RuntimeBinaryOp::Lt,
        INTEGER_LOOP_ITERATIONS as u64,
    )
}

pub fn natural_integer_accumulator_fixture() -> ProgramFixture {
    let program = Program::new(
        7,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(1),
                value: int(INTEGER_LOOP_ITERATIONS as i64),
            },
            Instruction::Load {
                dst: reg(2),
                value: int(1),
            },
            Instruction::Load {
                dst: reg(3),
                value: int(0),
            },
            Instruction::Binary {
                dst: reg(4),
                op: RuntimeBinaryOp::Lt,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(4),
                if_true: 6,
                if_false: 11,
            },
            Instruction::Binary {
                dst: reg(5),
                op: RuntimeBinaryOp::Add,
                left: reg(3),
                right: reg(0),
            },
            Instruction::Move {
                dst: reg(3),
                src: reg(5),
            },
            Instruction::Binary {
                dst: reg(6),
                op: RuntimeBinaryOp::Add,
                left: reg(0),
                right: reg(2),
            },
            Instruction::Move {
                dst: reg(0),
                src: reg(6),
            },
            Instruction::Jump { target: 4 },
            Instruction::Return { value: r(3) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: NATURAL_INTEGER_ACCUMULATOR_INSTRUCTIONS,
    }
}

pub fn float_add_loop_fixture() -> ProgramFixture {
    direct_loop_fixture(
        float(0.0),
        float(0.5),
        float(INTEGER_LOOP_ITERATIONS as f32 / 2.0),
        RuntimeBinaryOp::Add,
        RuntimeBinaryOp::Lt,
        INTEGER_LOOP_ITERATIONS as u64,
    )
}

pub fn float_multiply_loop_fixture() -> ProgramFixture {
    let factor = 1.0001_f32;
    let limit = 5.0_f32;
    let mut current = 1.0_f32;
    let mut iterations = 0_u64;
    loop {
        current *= factor;
        iterations += 1;
        if current >= limit {
            break;
        }
    }
    direct_loop_fixture(
        float(1.0),
        float(factor),
        float(limit),
        RuntimeBinaryOp::Mul,
        RuntimeBinaryOp::Lt,
        iterations,
    )
}

pub fn natural_float_sum_fixture() -> ProgramFixture {
    let collection = Value::list((0..INTEGER_LOOP_ITERATIONS).map(|_| float(0.25)));
    let program = Program::new(
        8,
        [
            Instruction::Load {
                dst: reg(0),
                value: collection,
            },
            Instruction::CollectionLen {
                dst: reg(1),
                collection: reg(0),
            },
            Instruction::Load {
                dst: reg(2),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(3),
                value: float(0.0),
            },
            Instruction::Load {
                dst: reg(4),
                value: int(1),
            },
            Instruction::Binary {
                dst: reg(5),
                op: RuntimeBinaryOp::Lt,
                left: reg(2),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(5),
                if_true: 7,
                if_false: 12,
            },
            Instruction::CollectionValueAt {
                dst: reg(6),
                collection: reg(0),
                index: reg(2),
            },
            Instruction::Binary {
                dst: reg(7),
                op: RuntimeBinaryOp::Add,
                left: reg(3),
                right: reg(6),
            },
            Instruction::Move {
                dst: reg(3),
                src: reg(7),
            },
            Instruction::Binary {
                dst: reg(2),
                op: RuntimeBinaryOp::Add,
                left: reg(2),
                right: reg(4),
            },
            Instruction::Jump { target: 5 },
            Instruction::Return { value: r(3) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: NATURAL_FLOAT_SUM_INSTRUCTIONS,
    }
}

pub fn natural_float_transform_fixture() -> ProgramFixture {
    let collection = Value::list((0..INTEGER_LOOP_ITERATIONS).map(|_| float(2.0)));
    let program = Program::new(
        12,
        [
            Instruction::Load {
                dst: reg(0),
                value: collection,
            },
            Instruction::CollectionLen {
                dst: reg(1),
                collection: reg(0),
            },
            Instruction::Load {
                dst: reg(2),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(3),
                value: float(0.0),
            },
            Instruction::Load {
                dst: reg(4),
                value: int(1),
            },
            Instruction::Load {
                dst: reg(5),
                value: float(0.5),
            },
            Instruction::Binary {
                dst: reg(6),
                op: RuntimeBinaryOp::Lt,
                left: reg(2),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(6),
                if_true: 8,
                if_false: 16,
            },
            Instruction::CollectionValueAt {
                dst: reg(7),
                collection: reg(0),
                index: reg(2),
            },
            Instruction::Unary {
                dst: reg(8),
                op: RuntimeUnaryOp::Neg,
                src: reg(7),
            },
            Instruction::Binary {
                dst: reg(9),
                op: RuntimeBinaryOp::Mul,
                left: reg(8),
                right: reg(5),
            },
            Instruction::Binary {
                dst: reg(10),
                op: RuntimeBinaryOp::Sub,
                left: reg(3),
                right: reg(9),
            },
            Instruction::Move {
                dst: reg(3),
                src: reg(10),
            },
            Instruction::Binary {
                dst: reg(11),
                op: RuntimeBinaryOp::Add,
                left: reg(2),
                right: reg(4),
            },
            Instruction::Move {
                dst: reg(2),
                src: reg(11),
            },
            Instruction::Jump { target: 6 },
            Instruction::Return { value: r(3) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: NATURAL_FLOAT_TRANSFORM_INSTRUCTIONS,
    }
}

pub fn natural_mixed_scale_fixture() -> ProgramFixture {
    let collection = Value::list((0..INTEGER_LOOP_ITERATIONS).map(|_| int(2)));
    let program = Program::new(
        11,
        [
            Instruction::Load {
                dst: reg(0),
                value: collection,
            },
            Instruction::CollectionLen {
                dst: reg(1),
                collection: reg(0),
            },
            Instruction::Load {
                dst: reg(2),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(3),
                value: float(0.0),
            },
            Instruction::Load {
                dst: reg(4),
                value: int(1),
            },
            Instruction::Load {
                dst: reg(5),
                value: float(0.5),
            },
            Instruction::Binary {
                dst: reg(6),
                op: RuntimeBinaryOp::Lt,
                left: reg(2),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(6),
                if_true: 8,
                if_false: 15,
            },
            Instruction::CollectionValueAt {
                dst: reg(7),
                collection: reg(0),
                index: reg(2),
            },
            Instruction::Binary {
                dst: reg(8),
                op: RuntimeBinaryOp::Mul,
                left: reg(7),
                right: reg(5),
            },
            Instruction::Binary {
                dst: reg(9),
                op: RuntimeBinaryOp::Add,
                left: reg(3),
                right: reg(8),
            },
            Instruction::Move {
                dst: reg(3),
                src: reg(9),
            },
            Instruction::Binary {
                dst: reg(10),
                op: RuntimeBinaryOp::Add,
                left: reg(2),
                right: reg(4),
            },
            Instruction::Move {
                dst: reg(2),
                src: reg(10),
            },
            Instruction::Jump { target: 6 },
            Instruction::Return { value: r(3) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: NATURAL_MIXED_SCALE_INSTRUCTIONS,
    }
}

pub fn natural_exact_integer_division_fixture() -> ProgramFixture {
    natural_numeric_div_rem_fixture(int(4), int(2), int(0), RuntimeBinaryOp::Div)
}

pub fn natural_fractional_integer_division_fixture() -> ProgramFixture {
    natural_numeric_div_rem_fixture(int(3), int(2), float(0.0), RuntimeBinaryOp::Div)
}

pub fn natural_mixed_division_fixture() -> ProgramFixture {
    natural_numeric_div_rem_fixture(int(3), float(2.0), float(0.0), RuntimeBinaryOp::Div)
}

pub fn natural_float_remainder_fixture() -> ProgramFixture {
    natural_numeric_div_rem_fixture(float(5.5), float(2.0), float(0.0), RuntimeBinaryOp::Rem)
}

fn natural_numeric_div_rem_fixture(
    element: Value,
    divisor: Value,
    initial: Value,
    operation: RuntimeBinaryOp,
) -> ProgramFixture {
    let collection = Value::list((0..INTEGER_LOOP_ITERATIONS).map(|_| element.clone()));
    let program = Program::new(
        11,
        [
            Instruction::Load {
                dst: reg(0),
                value: collection,
            },
            Instruction::CollectionLen {
                dst: reg(1),
                collection: reg(0),
            },
            Instruction::Load {
                dst: reg(2),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(3),
                value: initial,
            },
            Instruction::Load {
                dst: reg(4),
                value: int(1),
            },
            Instruction::Load {
                dst: reg(5),
                value: divisor,
            },
            Instruction::Binary {
                dst: reg(6),
                op: RuntimeBinaryOp::Lt,
                left: reg(2),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(6),
                if_true: 8,
                if_false: 15,
            },
            Instruction::CollectionValueAt {
                dst: reg(7),
                collection: reg(0),
                index: reg(2),
            },
            Instruction::Binary {
                dst: reg(8),
                op: operation,
                left: reg(7),
                right: reg(5),
            },
            Instruction::Binary {
                dst: reg(9),
                op: RuntimeBinaryOp::Add,
                left: reg(3),
                right: reg(8),
            },
            Instruction::Move {
                dst: reg(3),
                src: reg(9),
            },
            Instruction::Binary {
                dst: reg(10),
                op: RuntimeBinaryOp::Add,
                left: reg(2),
                right: reg(4),
            },
            Instruction::Move {
                dst: reg(2),
                src: reg(10),
            },
            Instruction::Jump { target: 6 },
            Instruction::Return { value: r(3) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: NATURAL_NUMERIC_DIV_REM_INSTRUCTIONS,
    }
}

pub fn scalar_symbol_loop_fixture() -> ProgramFixture {
    let alpha = Value::symbol(Symbol::intern("benchmark_scalar_alpha"));
    let beta = Value::symbol(Symbol::intern("benchmark_scalar_beta"));
    let program = Program::new(
        7,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(1),
                value: int(INTEGER_LOOP_ITERATIONS as i64),
            },
            Instruction::Load {
                dst: reg(2),
                value: alpha,
            },
            Instruction::Load {
                dst: reg(3),
                value: beta,
            },
            Instruction::Binary {
                dst: reg(5),
                op: RuntimeBinaryOp::Lt,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(5),
                if_true: 6,
                if_false: 12,
            },
            Instruction::Binary {
                dst: reg(6),
                op: RuntimeBinaryOp::Lt,
                left: reg(2),
                right: reg(3),
            },
            Instruction::Unary {
                dst: reg(6),
                op: RuntimeUnaryOp::Not,
                src: reg(6),
            },
            Instruction::Unary {
                dst: reg(6),
                op: RuntimeUnaryOp::Not,
                src: reg(6),
            },
            Instruction::Load {
                dst: reg(4),
                value: int(1),
            },
            Instruction::Binary {
                dst: reg(0),
                op: RuntimeBinaryOp::Add,
                left: reg(0),
                right: reg(4),
            },
            Instruction::Jump { target: 4 },
            Instruction::Return { value: r(6) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: SCALAR_LOOP_INSTRUCTIONS,
    }
}

pub fn predictable_branch_loop_fixture() -> ProgramFixture {
    branch_loop_fixture(false, PREDICTABLE_BRANCH_LOOP_INSTRUCTIONS)
}

pub fn alternating_branch_loop_fixture() -> ProgramFixture {
    branch_loop_fixture(true, ALTERNATING_BRANCH_LOOP_INSTRUCTIONS)
}

fn branch_loop_fixture(toggle_flag: bool, instruction_count: u64) -> ProgramFixture {
    let flag_update = if toggle_flag {
        Instruction::Unary {
            dst: reg(2),
            op: RuntimeUnaryOp::Not,
            src: reg(2),
        }
    } else {
        Instruction::Move {
            dst: reg(2),
            src: reg(2),
        }
    };
    let program = Program::new(
        8,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(1),
                value: int(INTEGER_LOOP_ITERATIONS as i64),
            },
            Instruction::Load {
                dst: reg(2),
                value: Value::bool(true),
            },
            Instruction::Load {
                dst: reg(3),
                value: int(0),
            },
            Instruction::Load {
                dst: reg(4),
                value: int(1),
            },
            Instruction::Load {
                dst: reg(5),
                value: int(2),
            },
            Instruction::Load {
                dst: reg(7),
                value: Value::empty_relation(),
            },
            Instruction::Binary {
                dst: reg(6),
                op: RuntimeBinaryOp::Lt,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Branch {
                condition: reg(6),
                if_true: 9,
                if_false: 17,
            },
            Instruction::Branch {
                condition: reg(2),
                if_true: 10,
                if_false: 12,
            },
            Instruction::Binary {
                dst: reg(3),
                op: RuntimeBinaryOp::Add,
                left: reg(3),
                right: reg(4),
            },
            Instruction::Jump { target: 13 },
            Instruction::Binary {
                dst: reg(3),
                op: RuntimeBinaryOp::Add,
                left: reg(3),
                right: reg(5),
            },
            flag_update,
            Instruction::Load {
                dst: reg(7),
                value: int(1),
            },
            Instruction::Binary {
                dst: reg(0),
                op: RuntimeBinaryOp::Add,
                left: reg(0),
                right: reg(7),
            },
            Instruction::Jump { target: 7 },
            Instruction::Return { value: r(3) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count,
    }
}

fn direct_loop_fixture(
    start: Value,
    step: Value,
    limit: Value,
    arithmetic: RuntimeBinaryOp,
    comparison: RuntimeBinaryOp,
    iterations: u64,
) -> ProgramFixture {
    let program = Program::new(
        4,
        [
            Instruction::Load {
                dst: reg(0),
                value: start,
            },
            Instruction::Load {
                dst: reg(1),
                value: step,
            },
            Instruction::Load {
                dst: reg(2),
                value: limit,
            },
            Instruction::Binary {
                dst: reg(0),
                op: arithmetic,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Binary {
                dst: reg(3),
                op: comparison,
                left: reg(0),
                right: reg(2),
            },
            Instruction::Branch {
                condition: reg(3),
                if_true: 3,
                if_false: 6,
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: (iterations * 3) + 4,
    }
}

pub fn static_call_fixture() -> ProgramFixture {
    let callee = Arc::new(Program::new(1, [Instruction::Return { value: r(0) }]).unwrap());
    let mut instructions = Vec::with_capacity(STATIC_CALL_COUNT + 1);
    for index in 0..STATIC_CALL_COUNT {
        instructions.push(Instruction::Call {
            dst: reg(0),
            program: Arc::clone(&callee),
            args: vec![Operand::Value(int(index as i64))],
        });
    }
    instructions.push(Instruction::Return { value: r(0) });
    let program = Program::new(1, instructions).unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: STATIC_CALL_INSTRUCTIONS,
    }
}

pub fn builtin_call_fixture() -> ProgramFixture {
    let name = Symbol::intern("benchmark_noop");
    let mut instructions = Vec::with_capacity(BUILTIN_CALL_COUNT + 1);
    for _ in 0..BUILTIN_CALL_COUNT {
        instructions.push(Instruction::BuiltinCall {
            dst: reg(0),
            name,
            result_kind: None,
            args: Vec::new(),
        });
    }
    instructions.push(Instruction::Return { value: r(0) });
    let program = Program::new(1, instructions).unwrap();
    ProgramFixture {
        program: Arc::new(program),
        instruction_count: BUILTIN_CALL_INSTRUCTIONS,
    }
}

#[derive(Default)]
pub struct BenchmarkHost {
    authority: AuthorityContext,
    builtin_calls: u64,
}

impl BenchmarkHost {
    pub fn builtin_calls(&self) -> u64 {
        self.builtin_calls
    }
}

impl RelationRead for BenchmarkHost {
    fn scan_relation(
        &self,
        _relation: RelationId,
        _bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        panic!("VM core benchmark unexpectedly scanned a relation")
    }
}

impl RelationWorkspace for BenchmarkHost {
    fn assert_tuple(&mut self, _relation: RelationId, _tuple: Tuple) -> Result<(), KernelError> {
        panic!("VM core benchmark unexpectedly asserted a tuple")
    }

    fn retract_tuple(&mut self, _relation: RelationId, _tuple: Tuple) -> Result<(), KernelError> {
        panic!("VM core benchmark unexpectedly retracted a tuple")
    }

    fn replace_functional_tuple(
        &mut self,
        _relation: RelationId,
        _tuple: Tuple,
    ) -> Result<(), KernelError> {
        panic!("VM core benchmark unexpectedly replaced a tuple")
    }
}

impl DispatchRead for BenchmarkHost {}

impl VmHost for BenchmarkHost {
    fn authority(&self) -> &AuthorityContext {
        &self.authority
    }

    fn authority_mut(&mut self) -> &mut AuthorityContext {
        &mut self.authority
    }

    fn emit(&mut self, _target: Identity, _value: Value) -> Result<(), RuntimeError> {
        panic!("VM core benchmark unexpectedly emitted a value")
    }

    fn validate_mailbox_receiver(&mut self, _receiver: &Value) -> Result<(), RuntimeError> {
        panic!("VM core benchmark unexpectedly validated a mailbox receiver")
    }

    fn resolve_program(
        &mut self,
        _program_bytes_relation: RelationId,
        _program_id: &Value,
    ) -> Result<Arc<Program>, RuntimeError> {
        panic!("VM core benchmark unexpectedly resolved a program")
    }

    fn call_builtin(&mut self, name: Symbol, args: &[Value]) -> Result<Value, RuntimeError> {
        debug_assert_eq!(name, Symbol::intern("benchmark_noop"));
        debug_assert!(args.is_empty());
        self.builtin_calls += 1;
        Ok(Value::empty_relation())
    }
}

fn reg(index: u16) -> Register {
    Register(index)
}

fn r(index: u16) -> Operand {
    Operand::Register(reg(index))
}

fn int(value: i64) -> Value {
    Value::int(value).unwrap()
}

fn float(value: f32) -> Value {
    Value::float(value).unwrap()
}
