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
    AuthorityContext, BuiltinContext, BuiltinRegistry, BuiltinResultKind, CapabilityGrant,
    CapabilityOp, Effect, ErrorField, Instruction, KindCheckSite, ListItem, MapItem, Operand,
    Program, ProgramResolver, QueryBinding, Register, RelationArg, RelationRowContract,
    RelationTypeContract, RuntimeBinaryOp, RuntimeContext, RuntimeError, SpawnTarget, SuspendKind,
    Task, TaskError, TaskLimits, TaskManager, TaskManagerError, TaskOutcome, TypeContract,
    TypeLiteralContract,
};
use mica_compiler::{CompileContext, compile_source};
use mica_relation_kernel::{
    ConflictPolicy, DispatchRelations, RelationId, RelationKernel, RelationMetadata, Tuple,
};
use mica_var::abi::borrowed_value_bits;
use mica_var::{CapabilityId, FunctionId, Identity, Symbol, Value, ValueKind};
use std::sync::Arc;

fn rel(id: u64) -> RelationId {
    Identity::new(id).unwrap()
}

fn int(value: i64) -> Value {
    Value::int(value).unwrap()
}

fn sym(name: &str) -> Value {
    Value::symbol(Symbol::intern(name))
}

fn err(name: &str) -> Value {
    Value::error_code(Symbol::intern(name))
}

fn error(name: &str, message: Option<&str>, value: Option<Value>) -> Value {
    Value::error(Symbol::intern(name), message, value)
}

fn strv(value: &str) -> Value {
    Value::string(value)
}

const EFFECT_TARGET: u64 = 99;

fn ident(raw: u64) -> Value {
    Value::identity(Identity::new(raw).unwrap())
}

fn emitted(value: Value) -> crate::Emission {
    crate::Emission::new(Identity::new(EFFECT_TARGET).unwrap(), value)
}

fn reg(index: u16) -> Register {
    Register(index)
}

fn r(index: u16) -> Operand {
    Operand::Register(reg(index))
}

fn v(value: Value) -> Operand {
    Operand::Value(value)
}

fn item(value: Operand) -> ListItem {
    ListItem::Value(value)
}

fn splice(value: Operand) -> ListItem {
    ListItem::Splice(value)
}

fn map_entry(key: Operand, value: Operand) -> MapItem {
    MapItem::Entry(key, value)
}

fn map_splice(value: Operand) -> MapItem {
    MapItem::Splice(value)
}

fn representative_kind_values() -> Vec<(ValueKind, Value)> {
    vec![
        (ValueKind::Bool, Value::bool(true)),
        (ValueKind::Int, int(7)),
        (ValueKind::Float, Value::float(1.5).unwrap()),
        (ValueKind::Identity, ident(7)),
        (ValueKind::String, strv("text")),
        (ValueKind::Bytes, Value::bytes([1, 2, 3])),
        (ValueKind::Symbol, sym("symbol")),
        (ValueKind::ErrorCode, err("E_TEST")),
        (ValueKind::Error, error("E_TEST", None, Some(int(7)))),
        (
            ValueKind::Capability,
            Value::capability(CapabilityId::new(1).unwrap()),
        ),
        (
            ValueKind::Frob,
            Value::frob(Identity::new(8).unwrap(), strv("payload")),
        ),
        (
            ValueKind::Function,
            Value::function(FunctionId::new(1).unwrap()),
        ),
        (ValueKind::List, Value::list([int(1), int(2)])),
        (ValueKind::Map, Value::map([(sym("key"), int(1))])),
        (ValueKind::Range, Value::range(int(1), Some(int(3)))),
        (ValueKind::Relation, Value::nothing()),
        (
            ValueKind::Relation,
            Value::relation(
                [Symbol::intern("item")],
                [Tuple::from([strv("heap relation")])],
            )
            .unwrap(),
        ),
    ]
}

fn kernel_with_world_relations() -> RelationKernel {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(1), Symbol::intern("Portable"), 2).with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(2), Symbol::intern("LocatedIn"), 2)
                .with_index([0])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![0],
                }),
        )
        .unwrap();
    kernel
}

fn run_program(
    kernel: &RelationKernel,
    program: Program,
    limit: usize,
) -> Result<TaskOutcome, crate::TaskError> {
    let mut task = Task::new(
        1,
        kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        TaskLimits {
            instruction_budget: limit,
            max_retries: 10,
            max_call_depth: 50,
        },
    );
    task.run()
}

fn run_program_with_builtins(
    kernel: &RelationKernel,
    program: Program,
    builtins: BuiltinRegistry,
) -> Result<TaskOutcome, crate::TaskError> {
    let mut task = Task::new_with_builtins(
        1,
        kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        Arc::new(builtins),
        TaskLimits::default(),
    );
    task.run()
}

fn run_program_with_authority(
    kernel: &RelationKernel,
    program: Program,
    authority: AuthorityContext,
) -> Result<TaskOutcome, crate::TaskError> {
    let mut task = Task::new_with_authority(
        1,
        kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        Arc::new(BuiltinRegistry::new()),
        authority,
        TaskLimits::default(),
    );
    task.run()
}

fn compiled_range_loop_program() -> Arc<Program> {
    Arc::new(
        compile_source(
            "let total = 0\n\
             for number in 1..16384\n\
               total = total + number\n\
             end\n\
             return total",
            &CompileContext::new(),
        )
        .unwrap()
        .program,
    )
}

fn compiled_indexed_range_loop_program() -> Arc<Program> {
    Arc::new(
        compile_source(
            "let total = 0\n\
             for index, number in 1..16384\n\
               total = total + index + number\n\
             end\n\
             return total",
            &CompileContext::new(),
        )
        .unwrap()
        .program,
    )
}

fn integer_list_literal(iterations: usize) -> String {
    (1..=iterations)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn compiler_list_traversal_source(indexed: bool) -> String {
    let values = integer_list_literal(8_192);
    if indexed {
        format!(
            "let total = 0\n\
             for index, value in [{values}]\n\
               total = total + index + value\n\
             end\n\
             return total"
        )
    } else {
        format!(
            "let total = 0\n\
             for value in [{values}]\n\
               total = total + value\n\
             end\n\
             return total"
        )
    }
}

fn compiler_list_index_source() -> String {
    let values = integer_list_literal(8_192);
    format!(
        "let values = [{values}]\n\
         let index = 0\n\
         let total = 0\n\
         while index < 8192\n\
           total = total + values[index]\n\
           index = index + 1\n\
         end\n\
         return total"
    )
}

fn compiler_map_traversal_source(indexed: bool) -> String {
    let entries = (1..=8_192)
        .map(|value| format!(":key_{value} -> {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    if indexed {
        format!(
            "let total = 0\n\
             for key, value in {{{entries}}}\n\
               if key != :missing\n\
                 total = total + value\n\
               end\n\
             end\n\
             return total"
        )
    } else {
        format!(
            "let total = 0\n\
             for value in {{{entries}}}\n\
               total = total + value\n\
             end\n\
             return total"
        )
    }
}

fn compiler_map_index_source() -> String {
    let entries = (1..=8_192)
        .map(|value| format!(":key_{value} -> {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "let values = {{{entries}}}\n\
         let index = 0\n\
         let total = 0\n\
         while index < 8192\n\
           total = total + values[:key_4096]\n\
           index = index + 1\n\
         end\n\
         return total"
    )
}

fn assert_compiler_collection_loop_matches_interpreter(source: &str, expected: i64) {
    let kernel = RelationKernel::new();
    let program = Arc::new(
        compile_source(source, &CompileContext::new())
            .unwrap()
            .program,
    );
    let resolver = Arc::new(ProgramResolver::new());
    let limits = TaskLimits {
        instruction_budget: 200_000,
        max_retries: 0,
        max_call_depth: 50,
    };
    let mut native = Task::new(
        1,
        &kernel,
        Arc::clone(&program),
        Arc::clone(&resolver),
        limits,
    );
    let mut interpreted = Task::new(2, &kernel, Arc::clone(&program), resolver, limits);
    interpreted.vm_mut().disable_native_execution();

    let native_outcome = native.run().unwrap();
    let interpreted_outcome = interpreted.run().unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert!(matches!(
        native_outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(expected).unwrap()
    ));
    assert_eq!(
        native.vm().snapshot_state(),
        interpreted.vm().snapshot_state()
    );
}

fn emit_first_arg(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let target = args[0]
        .as_identity()
        .ok_or_else(|| RuntimeError::InvalidEffectTarget(args[0].clone()))?;
    let value = args[1].clone();
    context.emit(target, value.clone())?;
    Ok(value)
}

fn mint_read_located(
    context: &mut BuiltinContext<'_, '_>,
    _args: &[Value],
) -> Result<Value, RuntimeError> {
    Ok(context.mint_capability(CapabilityGrant::relation(CapabilityOp::Read, rel(2))))
}

#[test]
fn instruction_budget_exhaustion_reports_runtime_error() {
    let kernel = RelationKernel::new();
    let program = Program::new(0, [Instruction::Jump { target: 0 }]).unwrap();

    let error = run_program(&kernel, program, 3).unwrap_err();
    let TaskError::Runtime(RuntimeError::InstructionBudgetExceeded {
        budget,
        current_stack,
        hot_spots,
    }) = error
    else {
        panic!("unexpected error: {error:?}");
    };

    assert_eq!(budget, 3);
    assert!(current_stack.iter().any(|frame| frame.contains("Jump")));
    assert!(hot_spots.iter().any(|frame| frame.contains("Jump")));
}

#[test]
fn kind_checks_accept_every_value_kind_without_changing_the_value_word() {
    let kernel = RelationKernel::new();

    for (kind, value) in representative_kind_values() {
        let expected_bits = borrowed_value_bits(&value);
        let program = Program::new(
            1,
            [
                Instruction::Load { dst: reg(0), value },
                Instruction::CheckKind {
                    value: reg(0),
                    expected: kind,
                    site: KindCheckSite::Binding,
                    subject: Symbol::intern("item"),
                },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap();

        let TaskOutcome::Complete { value, .. } = run_program(&kernel, program, 3).unwrap() else {
            panic!("kind check did not complete for {}", kind.name());
        };
        assert_eq!(
            borrowed_value_bits(&value),
            expected_bits,
            "{}",
            kind.name()
        );
    }
}

#[test]
fn kind_check_failures_are_catchable_for_every_expected_kind() {
    let kernel = RelationKernel::new();

    for expected in representative_kind_values()
        .into_iter()
        .map(|(kind, _)| kind)
    {
        let value = if expected == ValueKind::Int {
            strv("wrong")
        } else {
            int(7)
        };
        let actual = value.kind();
        let expected_payload_bits = borrowed_value_bits(&value);
        let program = Program::new(
            2,
            [
                Instruction::Load {
                    dst: reg(0),
                    value: value.clone(),
                },
                Instruction::EnterTry {
                    catches: vec![crate::CatchHandler {
                        code: Some(err("E_TYPE")),
                        binding: Some(reg(1)),
                        target: 4,
                    }],
                    finally: None,
                    end: 5,
                },
                Instruction::CheckKind {
                    value: reg(0),
                    expected,
                    site: KindCheckSite::Binding,
                    subject: Symbol::intern("item"),
                },
                Instruction::ExitTry,
                Instruction::Return { value: r(1) },
                Instruction::Return {
                    value: v(Value::nothing()),
                },
            ],
        )
        .unwrap();

        let outcome = run_program(&kernel, program, 4).unwrap();
        let TaskOutcome::Complete {
            value: raised,
            effects,
            mailbox_sends,
            retries,
        } = outcome
        else {
            panic!("kind check did not raise for {}", expected.name());
        };
        assert_eq!(effects, vec![]);
        assert_eq!(mailbox_sends, vec![]);
        assert_eq!(retries, 0);
        let payload_bits = raised
            .with_error(|error| error.value().map(borrowed_value_bits))
            .flatten()
            .expect("E_TYPE has the offending value payload");
        assert_eq!(payload_bits, expected_payload_bits, "{}", expected.name());
        assert_eq!(
            raised,
            error(
                "E_TYPE",
                Some(&format!(
                    "binding `item` requires {}, got {}",
                    expected.name(),
                    actual.name()
                )),
                Some(value)
            ),
            "{}",
            expected.name()
        );
    }
}

fn result_contract() -> TypeContract {
    let case = Symbol::intern("case");
    let value = Symbol::intern("value");
    let row = |mut columns: Vec<(Symbol, TypeContract)>| {
        columns.sort_by_key(|(name, _)| *name);
        RelationRowContract { columns }
    };
    TypeContract::Relation(RelationTypeContract {
        alternatives: vec![
            row(vec![
                (
                    case,
                    TypeContract::Literal(TypeLiteralContract::Symbol(Symbol::intern("error"))),
                ),
                (value, TypeContract::Kind(ValueKind::Error)),
            ]),
            row(vec![
                (
                    case,
                    TypeContract::Literal(TypeLiteralContract::Symbol(Symbol::intern("ok"))),
                ),
                (value, TypeContract::Kind(ValueKind::Int)),
            ]),
        ],
        minimum_rows: 1,
        maximum_rows: Some(1),
    })
}

#[test]
fn structural_type_checks_enforce_heading_discriminator_cells_and_cardinality() {
    let kernel = RelationKernel::new();
    let valid = Value::relation(
        [Symbol::intern("case"), Symbol::intern("value")],
        [Tuple::from([sym("ok"), int(42)])],
    )
    .unwrap();
    let program = Program::new(
        1,
        [
            Instruction::Load {
                dst: reg(0),
                value: valid.clone(),
            },
            Instruction::CheckType {
                value: reg(0),
                expected: result_contract(),
                site: KindCheckSite::Binding,
                subject: Symbol::intern("result"),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    assert!(matches!(
        run_program(&kernel, program, 3).unwrap(),
        TaskOutcome::Complete { value, .. } if value == valid
    ));

    for invalid in [
        Value::relation([Symbol::intern("value")], [Tuple::from([int(42)])]).unwrap(),
        Value::relation(
            [Symbol::intern("case"), Symbol::intern("value")],
            [Tuple::from([sym("other"), int(42)])],
        )
        .unwrap(),
        Value::relation(
            [Symbol::intern("case"), Symbol::intern("value")],
            [Tuple::from([sym("ok"), strv("wrong")])],
        )
        .unwrap(),
        Value::relation([Symbol::intern("case"), Symbol::intern("value")], []).unwrap(),
    ] {
        let bits = borrowed_value_bits(&invalid);
        let program = Program::new(
            2,
            [
                Instruction::Load {
                    dst: reg(0),
                    value: invalid,
                },
                Instruction::EnterTry {
                    catches: vec![crate::CatchHandler {
                        code: Some(err("E_TYPE")),
                        binding: Some(reg(1)),
                        target: 4,
                    }],
                    finally: None,
                    end: 5,
                },
                Instruction::CheckType {
                    value: reg(0),
                    expected: result_contract(),
                    site: KindCheckSite::Binding,
                    subject: Symbol::intern("result"),
                },
                Instruction::ExitTry,
                Instruction::Return { value: r(1) },
                Instruction::Return {
                    value: v(Value::nothing()),
                },
            ],
        )
        .unwrap();
        let TaskOutcome::Complete { value: raised, .. } = run_program(&kernel, program, 4).unwrap()
        else {
            panic!("structural type check did not raise");
        };
        assert_eq!(raised.error_code_symbol(), Some(Symbol::intern("E_TYPE")));
        assert_eq!(
            raised
                .with_error(|error| error.value().map(borrowed_value_bits))
                .flatten(),
            Some(bits)
        );
    }
}

#[test]
fn kind_checks_consume_exactly_one_instruction_on_success_and_failure() {
    let kernel = RelationKernel::new();
    let success = Program::new(
        1,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(7),
            },
            Instruction::CheckKind {
                value: reg(0),
                expected: ValueKind::Int,
                site: KindCheckSite::Binding,
                subject: Symbol::intern("count"),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    assert!(matches!(
        run_program(&kernel, success.clone(), 2),
        Err(TaskError::Runtime(
            RuntimeError::InstructionBudgetExceeded { budget: 2, .. }
        ))
    ));
    assert!(matches!(
        run_program(&kernel, success, 3).unwrap(),
        TaskOutcome::Complete { value, .. } if value == int(7)
    ));

    let program = Program::new(
        2,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(7),
            },
            Instruction::EnterTry {
                catches: vec![crate::CatchHandler {
                    code: Some(err("E_TYPE")),
                    binding: Some(reg(1)),
                    target: 4,
                }],
                finally: None,
                end: 5,
            },
            Instruction::CheckKind {
                value: reg(0),
                expected: ValueKind::String,
                site: KindCheckSite::Parameter,
                subject: Symbol::intern("name"),
            },
            Instruction::ExitTry,
            Instruction::Return { value: r(1) },
            Instruction::Return {
                value: v(Value::nothing()),
            },
        ],
    )
    .unwrap();

    assert!(matches!(
        run_program(&kernel, program.clone(), 3),
        Err(TaskError::Runtime(
            RuntimeError::InstructionBudgetExceeded { budget: 3, .. }
        ))
    ));
    assert_eq!(
        run_program(&kernel, program, 4).unwrap(),
        TaskOutcome::Complete {
            value: error(
                "E_TYPE",
                Some("parameter `name` requires string, got int"),
                Some(int(7))
            ),
            effects: vec![],
            mailbox_sends: vec![],
            retries: 0,
        }
    );
}

#[test]
fn compiler_range_loop_matches_native_and_interpreted_execution() {
    let kernel = RelationKernel::new();
    let program = compiled_range_loop_program();
    let resolver = Arc::new(ProgramResolver::new());
    let limits = TaskLimits {
        instruction_budget: 200_000,
        max_retries: 0,
        max_call_depth: 50,
    };
    let mut native = Task::new(
        1,
        &kernel,
        Arc::clone(&program),
        Arc::clone(&resolver),
        limits,
    );
    let mut interpreted = Task::new(2, &kernel, program, resolver, limits);
    interpreted.vm_mut().disable_native_execution();

    let native_outcome = native.run().unwrap();
    let interpreted_outcome = interpreted.run().unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert!(matches!(
        native_outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(134_225_920).unwrap()
    ));
    assert_eq!(
        native.vm().snapshot_state(),
        interpreted.vm().snapshot_state()
    );
}

#[test]
fn compiler_range_loop_preserves_native_budget_boundaries() {
    for budget in [1, 8, 1_024, 50_000, 100_000, 131_000] {
        let kernel = RelationKernel::new();
        let program = compiled_range_loop_program();
        let resolver = Arc::new(ProgramResolver::new());
        let limits = TaskLimits {
            instruction_budget: budget,
            max_retries: 0,
            max_call_depth: 50,
        };
        let mut native = Task::new(
            1,
            &kernel,
            Arc::clone(&program),
            Arc::clone(&resolver),
            limits,
        );
        let mut interpreted = Task::new(2, &kernel, program, resolver, limits);
        interpreted.vm_mut().disable_native_execution();

        assert!(matches!(
            native.run(),
            Err(TaskError::Runtime(
                RuntimeError::InstructionBudgetExceeded { .. }
            ))
        ));
        assert!(matches!(
            interpreted.run(),
            Err(TaskError::Runtime(
                RuntimeError::InstructionBudgetExceeded { .. }
            ))
        ));
        assert_eq!(
            native.vm().snapshot_state(),
            interpreted.vm().snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn compiler_indexed_range_loop_matches_native_and_interpreted_execution() {
    let kernel = RelationKernel::new();
    let program = compiled_indexed_range_loop_program();
    let resolver = Arc::new(ProgramResolver::new());
    let limits = TaskLimits {
        instruction_budget: 163_851,
        max_retries: 0,
        max_call_depth: 50,
    };
    let mut native = Task::new(
        1,
        &kernel,
        Arc::clone(&program),
        Arc::clone(&resolver),
        limits,
    );
    let mut interpreted = Task::new(2, &kernel, program, resolver, limits);
    interpreted.vm_mut().disable_native_execution();

    let native_outcome = native.run().unwrap();
    let interpreted_outcome = interpreted.run().unwrap();
    assert_eq!(native_outcome, interpreted_outcome);
    assert!(matches!(
        native_outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(268_435_456).unwrap()
    ));
    assert_eq!(
        native.vm().snapshot_state(),
        interpreted.vm().snapshot_state()
    );
}

#[test]
fn compiler_indexed_range_loop_preserves_native_budget_boundaries() {
    for budget in [1, 8, 1_024, 50_000, 100_000, 160_000, 163_850] {
        let kernel = RelationKernel::new();
        let program = compiled_indexed_range_loop_program();
        let resolver = Arc::new(ProgramResolver::new());
        let limits = TaskLimits {
            instruction_budget: budget,
            max_retries: 0,
            max_call_depth: 50,
        };
        let mut native = Task::new(
            1,
            &kernel,
            Arc::clone(&program),
            Arc::clone(&resolver),
            limits,
        );
        let mut interpreted = Task::new(2, &kernel, program, resolver, limits);
        interpreted.vm_mut().disable_native_execution();

        assert!(matches!(
            native.run(),
            Err(TaskError::Runtime(
                RuntimeError::InstructionBudgetExceeded { .. }
            ))
        ));
        assert!(matches!(
            interpreted.run(),
            Err(TaskError::Runtime(
                RuntimeError::InstructionBudgetExceeded { .. }
            ))
        ));
        assert_eq!(
            native.vm().snapshot_state(),
            interpreted.vm().snapshot_state(),
            "budget {budget}",
        );
    }
}

#[test]
fn compiler_list_traversal_matches_native_and_interpreted_execution() {
    assert_compiler_collection_loop_matches_interpreter(
        &compiler_list_traversal_source(false),
        33_558_528,
    );
    assert_compiler_collection_loop_matches_interpreter(
        &compiler_list_traversal_source(true),
        67_108_864,
    );
}

#[test]
fn compiler_list_index_loop_matches_native_and_interpreted_execution() {
    assert_compiler_collection_loop_matches_interpreter(&compiler_list_index_source(), 33_558_528);
}

#[test]
fn compiler_map_traversal_matches_native_and_interpreted_execution() {
    assert_compiler_collection_loop_matches_interpreter(
        &compiler_map_traversal_source(false),
        33_558_528,
    );
    assert_compiler_collection_loop_matches_interpreter(
        &compiler_map_traversal_source(true),
        33_558_528,
    );
}

#[test]
fn compiler_map_index_loop_matches_native_and_interpreted_execution() {
    assert_compiler_collection_loop_matches_interpreter(&compiler_map_index_source(), 33_554_432);
}

#[test]
fn builtin_can_return_ephemeral_capability_value() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        1,
        [
            Instruction::BuiltinCall {
                dst: reg(0),
                name: Symbol::intern("mint_read_located"),
                result_kind: None,
                args: vec![],
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let builtins = BuiltinRegistry::new().with_builtin(
        "mint_read_located",
        BuiltinResultKind::Dynamic,
        mint_read_located,
    );

    let outcome = run_program_with_builtins(&kernel, program, builtins).unwrap();
    let TaskOutcome::Complete { value, .. } = outcome else {
        panic!("expected complete outcome");
    };
    assert!(value.as_capability().is_some());
    assert!(!value.is_persistable());
}

#[test]
fn authority_context_rejects_unminted_capability_values() {
    let context = AuthorityContext::empty();
    assert!(
        context
            .grant_for(Value::capability_raw(44).unwrap())
            .is_none()
    );

    let mut context = AuthorityContext::empty();
    let cap = context.mint(CapabilityGrant::relation(CapabilityOp::Read, rel(2)));
    assert!(context.grant_for(cap).is_some());
}

#[test]
fn program_artifacts_reject_capability_constants() {
    let program = Program::new(
        1,
        [Instruction::Load {
            dst: reg(0),
            value: Value::capability_raw(1).unwrap(),
        }],
    )
    .unwrap();

    assert!(matches!(
        program.to_bytes(),
        Err(RuntimeError::ProgramArtifact(message))
            if message == "capability values are not serializable"
    ));
}

#[test]
fn authority_context_gates_runtime_writes() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        0,
        [Instruction::ReplaceFunctional {
            relation: rel(2),
            values: vec![v(int(1)), v(int(2))],
        }],
    )
    .unwrap();

    assert_eq!(
        run_program_with_authority(&kernel, program, AuthorityContext::empty()).unwrap_err(),
        TaskError::Runtime(RuntimeError::PermissionDenied {
            operation: "write",
            target: Value::identity(rel(2)),
        })
    );
}

#[test]
fn authority_context_filters_dispatch_applicability() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(40), Symbol::intern("MethodSelector"), 2).with_index([1, 0]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(41), Symbol::intern("Param"), 4).with_index([0, 1]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(42), Symbol::intern("Delegates"), 3).with_index([0, 2, 1]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(43), Symbol::intern("MethodProgram"), 2).with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(
            rel(44),
            Symbol::intern("ProgramBytes"),
            2,
        ))
        .unwrap();

    let method = int(100);
    let program_id = int(900);
    let mut seed = kernel.begin();
    seed.assert(rel(40), Tuple::from([method.clone(), sym("look")]))
        .unwrap();
    seed.assert(
        rel(41),
        Tuple::from([method.clone(), sym("actor"), int(1), int(0)]),
    )
    .unwrap();
    seed.assert(rel(43), Tuple::from([method.clone(), program_id.clone()]))
        .unwrap();
    seed.commit().unwrap();

    let method_program = Program::new(
        1,
        [Instruction::Return {
            value: v(strv("ok")),
        }],
    )
    .unwrap();
    let mut resolver = ProgramResolver::new();
    resolver.insert(program_id, method_program);
    let resolver = Arc::new(resolver);
    let dispatch_program = Arc::new(
        Program::new(
            1,
            [
                Instruction::Dispatch {
                    dst: reg(0),
                    relations: mica_relation_kernel::DispatchRelations {
                        method_selector: rel(40),
                        param: rel(41),
                        delegates: rel(42),
                    },
                    program_relation: rel(43),
                    program_bytes: rel(44),
                    selector: v(sym("look")),
                    roles: vec![(sym("actor"), v(int(1)))],
                },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap(),
    );

    let mut denied = Task::new_with_authority(
        1,
        &kernel,
        dispatch_program.clone(),
        resolver.clone(),
        Arc::new(BuiltinRegistry::new()),
        AuthorityContext::empty(),
        TaskLimits::default(),
    );
    assert_eq!(
        denied.run().unwrap_err(),
        TaskError::Runtime(RuntimeError::NoApplicableMethod {
            selector: sym("look"),
        })
    );

    let mut authority = AuthorityContext::empty();
    authority.mint(CapabilityGrant::method(method));
    let mut allowed = Task::new_with_authority(
        2,
        &kernel,
        dispatch_program,
        resolver,
        Arc::new(BuiltinRegistry::new()),
        authority,
        TaskLimits::default(),
    );
    assert_eq!(
        allowed.run().unwrap(),
        TaskOutcome::Complete {
            value: strv("ok"),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn task_runs_take_like_method_transactionally() {
    let kernel = kernel_with_world_relations();
    let actor = int(100);
    let item = int(200);
    let room = int(300);

    let mut seed = kernel.begin();
    seed.assert(rel(1), Tuple::from([item.clone(), Value::bool(true)]))
        .unwrap();
    seed.replace_functional(rel(2), Tuple::from([item.clone(), room]))
        .unwrap();
    seed.commit().unwrap();

    let program = Program::new(
        4,
        [
            Instruction::Load {
                dst: reg(0),
                value: item.clone(),
            },
            Instruction::Load {
                dst: reg(1),
                value: actor.clone(),
            },
            Instruction::ScanExists {
                dst: reg(2),
                relation: rel(1),
                bindings: vec![Some(r(0)), Some(v(Value::bool(true)))],
            },
            Instruction::Branch {
                condition: reg(2),
                if_true: 4,
                if_false: 7,
            },
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(1)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("Taken.")),
            },
            Instruction::Return {
                value: v(Value::bool(true)),
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("You can't take that.")),
            },
            Instruction::Return {
                value: v(Value::bool(false)),
            },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("Taken."))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[Some(item), None]).unwrap(),
        vec![Tuple::from([int(200), actor])]
    );
}

#[test]
fn abort_rolls_back_current_transaction_and_pending_effects() {
    let kernel = kernel_with_world_relations();
    let item = int(200);
    let room = int(300);
    let actor = int(100);

    let mut seed = kernel.begin();
    seed.replace_functional(rel(2), Tuple::from([item.clone(), room.clone()]))
        .unwrap();
    seed.commit().unwrap();

    let program = Program::new(
        2,
        [
            Instruction::Load {
                dst: reg(0),
                value: item.clone(),
            },
            Instruction::Load {
                dst: reg(1),
                value: actor,
            },
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(1)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("Taken.")),
            },
            Instruction::Abort {
                error: v(sym("abort")),
            },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Aborted {
            error: sym("abort"),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[Some(item), None]).unwrap(),
        vec![Tuple::from([int(200), room])]
    );
}

#[test]
fn program_artifact_round_trips_float_constants() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        3,
        [
            Instruction::Load {
                dst: reg(0),
                value: Value::float(3.5).unwrap(),
            },
            Instruction::Load {
                dst: reg(1),
                value: Value::float(-0.0).unwrap(),
            },
            Instruction::Binary {
                dst: reg(2),
                op: RuntimeBinaryOp::Add,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Return { value: r(2) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);
    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::float(3.5).unwrap(),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn program_artifact_round_trips_kind_checks_and_rejects_stale_magic() {
    let program = Program::new(
        1,
        [
            Instruction::CheckKind {
                value: reg(0),
                expected: ValueKind::Relation,
                site: KindCheckSite::Parameter,
                subject: Symbol::intern("rows"),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let bytes = program.to_bytes().unwrap();

    assert_eq!(&bytes[..8], b"MICAPRG5");
    assert_eq!(
        program.kind_fact_after(0),
        Some((reg(0), ValueKind::Relation)),
    );
    assert_eq!(Program::from_bytes(&bytes).unwrap(), program);

    let mut invalid_facts = bytes.clone();
    let fact = invalid_facts.len() - 2;
    invalid_facts[fact] = 1;
    assert!(matches!(
        Program::from_bytes(&invalid_facts),
        Err(RuntimeError::ProgramArtifact(message))
            if message == "program kind facts do not match instructions"
    ));

    let mut stale = bytes;
    stale[..8].copy_from_slice(b"MICAPRG3");
    assert!(matches!(
        Program::from_bytes(&stale),
        Err(RuntimeError::ProgramArtifact(message))
            if message == "invalid program artifact magic"
    ));
}

#[test]
fn program_artifact_round_trips_structural_type_contracts() {
    let program = Program::new(
        1,
        [
            Instruction::CheckType {
                value: reg(0),
                expected: result_contract(),
                site: KindCheckSite::Parameter,
                subject: Symbol::intern("outcome"),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let bytes = program.to_bytes().unwrap();

    assert_eq!(&bytes[..8], b"MICAPRG5");
    assert_eq!(
        program.kind_fact_after(0),
        Some((reg(0), ValueKind::Relation))
    );
    assert_eq!(Program::from_bytes(&bytes).unwrap(), program);
}

#[test]
fn program_kind_facts_propagate_exact_instruction_results() {
    let program = Program::new(
        7,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(2),
            },
            Instruction::Move {
                dst: reg(1),
                src: reg(0),
            },
            Instruction::Binary {
                dst: reg(2),
                op: RuntimeBinaryOp::Add,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Binary {
                dst: reg(3),
                op: RuntimeBinaryOp::Div,
                left: reg(0),
                right: reg(1),
            },
            Instruction::Load {
                dst: reg(4),
                value: Value::float(0.5).unwrap(),
            },
            Instruction::Binary {
                dst: reg(5),
                op: RuntimeBinaryOp::Add,
                left: reg(2),
                right: reg(4),
            },
            Instruction::CheckKind {
                value: reg(6),
                expected: ValueKind::String,
                site: KindCheckSite::Parameter,
                subject: Symbol::intern("name"),
            },
        ],
    )
    .unwrap();

    assert_eq!(program.kind_fact_after(0), Some((reg(0), ValueKind::Int)));
    assert_eq!(program.kind_fact_after(1), Some((reg(1), ValueKind::Int)));
    assert_eq!(program.kind_fact_after(2), Some((reg(2), ValueKind::Int)));
    assert_eq!(program.kind_fact_after(3), None);
    assert_eq!(program.kind_fact_after(4), Some((reg(4), ValueKind::Float)),);
    assert_eq!(program.kind_fact_after(5), Some((reg(5), ValueKind::Float)),);
    assert_eq!(
        program.kind_fact_after(6),
        Some((reg(6), ValueKind::String)),
    );

    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    let branched = Program::new(
        3,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(1),
            },
            Instruction::Branch {
                condition: reg(2),
                if_true: 2,
                if_false: 3,
            },
            Instruction::Move {
                dst: reg(1),
                src: reg(0),
            },
            Instruction::Return { value: r(1) },
        ],
    )
    .unwrap();
    assert_eq!(branched.kind_fact_after(2), None);
}

#[test]
fn program_structural_facts_cover_literals_scans_and_control_flow_merges() {
    let value = Symbol::intern("value");
    let literal = Program::new(
        1,
        [
            Instruction::BuildRelation {
                dst: reg(0),
                heading: vec![value],
                cells: vec![v(int(1)), v(int(2))],
                row_count: 2,
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let Some((register, TypeContract::Relation(contract))) = literal.type_fact_after(0) else {
        panic!("relation literal should publish a structural fact");
    };
    assert_eq!(register, reg(0));
    assert_eq!(contract.minimum_rows, 2);
    assert_eq!(contract.maximum_rows, Some(2));
    assert_eq!(contract.alternatives[0].columns[0].0, value);
    assert_eq!(
        contract.alternatives[0].columns[0].1,
        TypeContract::Kind(ValueKind::Int)
    );

    let scan = Program::new(
        1,
        [
            Instruction::ScanBindings {
                dst: reg(0),
                relation: rel(1),
                bindings: vec![None],
                outputs: vec![QueryBinding {
                    name: value,
                    position: 0,
                }],
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let Some((_, TypeContract::Relation(contract))) = scan.type_fact_after(0) else {
        panic!("relation scan should publish a structural fact");
    };
    assert_eq!(contract.minimum_rows, 0);
    assert_eq!(contract.maximum_rows, None);
    assert_eq!(
        contract.alternatives[0].columns,
        vec![(value, TypeContract::Dynamic)]
    );

    let merge = Program::new(
        3,
        [
            Instruction::Load {
                dst: reg(0),
                value: Value::relation([value], [Tuple::from([int(1)])]).unwrap(),
            },
            Instruction::Branch {
                condition: reg(1),
                if_true: 2,
                if_false: 2,
            },
            Instruction::Move {
                dst: reg(2),
                src: reg(0),
            },
            Instruction::Return { value: r(2) },
        ],
    )
    .unwrap();
    assert_eq!(merge.type_fact_after(2), None);
}

#[test]
fn program_artifact_rejects_invalid_kind_check_fields() {
    let program = Program::new(
        1,
        [Instruction::CheckKind {
            value: reg(0),
            expected: ValueKind::Int,
            site: KindCheckSite::Binding,
            subject: Symbol::intern("count"),
        }],
    )
    .unwrap();
    let bytes = program.to_bytes().unwrap();

    // Header (16 bytes), opcode (1), register (2), then kind and site bytes.
    let mut invalid_kind = bytes.clone();
    invalid_kind[19] = u8::MAX;
    assert!(Program::from_bytes(&invalid_kind).is_err());

    let mut invalid_site = bytes;
    invalid_site[20] = u8::MAX;
    assert!(Program::from_bytes(&invalid_site).is_err());
}

#[test]
fn programs_reject_unnamed_kind_check_subjects() {
    assert!(matches!(
        Program::new(
            1,
            [Instruction::CheckKind {
                value: reg(0),
                expected: ValueKind::Int,
                site: KindCheckSite::Binding,
                subject: Symbol::from_id(u32::MAX),
            }]
        ),
        Err(RuntimeError::ProgramArtifact(message))
            if message == "kind check subject must be a named symbol"
    ));
}

#[test]
fn program_artifact_rejects_non_finite_float_bits() {
    let _kernel = kernel_with_world_relations();
    let program = Program::new(
        1,
        [Instruction::Load {
            dst: reg(0),
            value: Value::float(1.0).unwrap(),
        }],
    )
    .unwrap();
    let mut bytes = program.to_bytes().unwrap();
    // Locate this program's float record, then replace its finite payload with
    // quiet-NaN bits (0x7fc00000).
    let float_record = [3u8, 0x00, 0x00, 0x80, 0x3f];
    let pos = bytes
        .windows(float_record.len())
        .position(|window| window == float_record)
        .expect("program contains the serialized 1.0 float record");
    bytes[pos + 1..pos + 5].copy_from_slice(&[0x00, 0x00, 0xc0, 0x7f]);
    assert!(Program::from_bytes(&bytes).is_err());
}

#[test]
fn completed_task_does_not_open_another_transaction() {
    let kernel = RelationKernel::new();
    let program = Program::new(
        0,
        [Instruction::Return {
            value: v(Value::nothing()),
        }],
    )
    .unwrap();
    let mut task = Task::new(
        1,
        &kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        TaskLimits::default(),
    );

    assert!(matches!(task.run(), Ok(TaskOutcome::Complete { .. })));
    assert_eq!(task.run(), Err(TaskError::MissingTransaction));
}

#[test]
fn aborted_task_does_not_open_another_transaction() {
    let kernel = RelationKernel::new();
    let program = Program::new(
        0,
        [Instruction::Abort {
            error: v(sym("abort")),
        }],
    )
    .unwrap();
    let mut task = Task::new(
        1,
        &kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        TaskLimits::default(),
    );

    assert!(matches!(task.run(), Ok(TaskOutcome::Aborted { .. })));
    assert_eq!(task.run(), Err(TaskError::MissingTransaction));
}

#[test]
fn explicit_commit_boundary_survives_later_abort() {
    let kernel = kernel_with_world_relations();
    let item = int(200);
    let room = int(300);
    let actor = int(100);
    let box_obj = int(400);

    let mut seed = kernel.begin();
    seed.replace_functional(rel(2), Tuple::from([item.clone(), room]))
        .unwrap();
    seed.commit().unwrap();

    let program = Program::new(
        3,
        [
            Instruction::Load {
                dst: reg(0),
                value: item.clone(),
            },
            Instruction::Load {
                dst: reg(1),
                value: actor.clone(),
            },
            Instruction::Load {
                dst: reg(2),
                value: box_obj,
            },
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(1)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("Committed.")),
            },
            Instruction::Commit,
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(2)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("Rolled back.")),
            },
            Instruction::Abort {
                error: v(sym("abort")),
            },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Aborted {
            error: sym("abort"),
            effects: vec![emitted(strv("Committed."))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[Some(item), None]).unwrap(),
        vec![Tuple::from([int(200), actor])]
    );
}

#[test]
fn binary_divide_by_zero_raises_catchable_error() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        4,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(1),
            },
            Instruction::Load {
                dst: reg(1),
                value: int(0),
            },
            Instruction::EnterTry {
                catches: vec![crate::CatchHandler {
                    code: Some(err("E_DIV")),
                    binding: Some(reg(3)),
                    target: 5,
                }],
                finally: None,
                end: 6,
            },
            Instruction::Binary {
                dst: reg(2),
                op: RuntimeBinaryOp::Div,
                left: reg(0),
                right: reg(1),
            },
            Instruction::ExitTry,
            Instruction::Return { value: r(3) },
            Instruction::Return {
                value: v(Value::nothing()),
            },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: error(
                "E_DIV",
                Some("division by zero"),
                Some(Value::list([int(1), int(0)]))
            ),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn scan_bindings_returns_relation_values() {
    let kernel = kernel_with_world_relations();
    let thing = int(200);
    let room = int(300);
    let mut seed = kernel.begin();
    seed.assert(rel(2), Tuple::from([thing.clone(), room.clone()]))
        .unwrap();
    seed.commit().unwrap();
    let program = Program::new(
        1,
        [
            Instruction::ScanBindings {
                dst: reg(0),
                relation: rel(2),
                bindings: vec![Some(v(thing)), None],
                outputs: vec![QueryBinding {
                    name: Symbol::intern("room"),
                    position: 1,
                }],
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::relation([Symbol::intern("room")], [Tuple::from([room])]).unwrap(),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn one_extracts_single_query_binding_value() {
    let kernel = kernel_with_world_relations();
    let room = int(300);
    let program = Program::new(
        2,
        [
            Instruction::Load {
                dst: reg(0),
                value: Value::list([Value::map([(sym("room"), room.clone())])]),
            },
            Instruction::One {
                dst: reg(1),
                src: reg(0),
            },
            Instruction::Return { value: r(1) },
        ],
    )
    .unwrap();
    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: room,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn one_raises_on_ambiguous_query_results() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        2,
        [
            Instruction::Load {
                dst: reg(0),
                value: Value::list([
                    Value::map([(sym("room"), int(300))]),
                    Value::map([(sym("room"), int(301))]),
                ]),
            },
            Instruction::One {
                dst: reg(1),
                src: reg(0),
            },
        ],
    )
    .unwrap();

    assert!(matches!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Aborted { error, .. }
            if error.error_code_symbol() == Some(Symbol::intern("E_AMBIGUOUS"))
    ));
}

#[test]
fn suspend_commits_then_resume_continues_in_new_transaction() {
    let kernel = kernel_with_world_relations();
    let item = int(200);
    let actor = int(100);
    let box_obj = int(400);

    let program = Program::new(
        3,
        [
            Instruction::Load {
                dst: reg(0),
                value: item.clone(),
            },
            Instruction::Load {
                dst: reg(1),
                value: actor.clone(),
            },
            Instruction::Load {
                dst: reg(2),
                value: box_obj.clone(),
            },
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(1)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("phase 1")),
            },
            Instruction::Suspend {
                kind: SuspendKind::TimedMillis(10),
            },
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(2)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("phase 2")),
            },
            Instruction::Return {
                value: v(Value::bool(true)),
            },
        ],
    )
    .unwrap();

    let mut task = Task::new(
        1,
        &kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        TaskLimits::default(),
    );
    assert_eq!(
        task.run().unwrap(),
        TaskOutcome::Suspended {
            kind: SuspendKind::TimedMillis(10),
            effects: vec![emitted(strv("phase 1"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        kernel
            .snapshot()
            .scan(rel(2), &[Some(item.clone()), None])
            .unwrap(),
        vec![Tuple::from([item.clone(), actor])]
    );

    assert_eq!(
        task.run().unwrap(),
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("phase 2"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[Some(item), None]).unwrap(),
        vec![Tuple::from([int(200), box_obj])]
    );
}

#[test]
fn suspend_value_returns_supplied_resume_value() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            1,
            [
                Instruction::SuspendValue {
                    dst: reg(0),
                    duration: None,
                },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, first) = task_manager.submit(program).unwrap();
    assert_eq!(
        first,
        TaskOutcome::Suspended {
            kind: SuspendKind::Never,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );

    let second = task_manager
        .resume_with_value(task_id, AuthorityContext::root(), strv("resumed"))
        .unwrap();
    assert_eq!(
        second,
        TaskOutcome::Complete {
            value: strv("resumed"),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn commit_value_commits_and_resumes_with_nothing() {
    let kernel = kernel_with_world_relations();
    let item = int(200);
    let actor = int(100);
    let program = Arc::new(
        Program::new(
            1,
            [
                Instruction::ReplaceFunctional {
                    relation: rel(2),
                    values: vec![v(item.clone()), v(actor.clone())],
                },
                Instruction::CommitValue { dst: reg(0) },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, first) = task_manager.submit(program).unwrap();
    assert_eq!(
        first,
        TaskOutcome::Suspended {
            kind: SuspendKind::Commit,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        task_manager
            .kernel()
            .snapshot()
            .scan(rel(2), &[Some(item.clone()), None])
            .unwrap(),
        vec![Tuple::from([item, actor])]
    );

    let second = task_manager
        .resume_with_authority(task_id, AuthorityContext::root())
        .unwrap();
    assert_eq!(
        second,
        TaskOutcome::Complete {
            value: Value::nothing(),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn read_commits_effects_and_returns_supplied_input() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            1,
            [
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("prompt")),
                },
                Instruction::Read {
                    dst: reg(0),
                    metadata: Some(v(sym("line"))),
                },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, first) = task_manager.submit(program).unwrap();
    assert_eq!(
        first,
        TaskOutcome::Suspended {
            kind: SuspendKind::WaitingForInput(sym("line")),
            effects: vec![emitted(strv("prompt"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );

    let second = task_manager
        .resume_with_value(task_id, AuthorityContext::root(), strv("north"))
        .unwrap();
    assert_eq!(
        second,
        TaskOutcome::Complete {
            value: strv("north"),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn task_retries_from_last_clean_state_on_commit_conflict() {
    let kernel = kernel_with_world_relations();
    let item = int(200);
    let room = int(300);
    let other = int(500);
    let actor = int(100);

    let mut seed = kernel.begin();
    seed.replace_functional(rel(2), Tuple::from([item.clone(), room]))
        .unwrap();
    seed.commit().unwrap();

    let program = Program::new(
        2,
        [
            Instruction::Load {
                dst: reg(0),
                value: item.clone(),
            },
            Instruction::Load {
                dst: reg(1),
                value: actor.clone(),
            },
            Instruction::ReplaceFunctional {
                relation: rel(2),
                values: vec![r(0), r(1)],
            },
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("Taken.")),
            },
            Instruction::Return {
                value: v(Value::bool(true)),
            },
        ],
    )
    .unwrap();
    let mut task = Task::new(
        1,
        &kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        TaskLimits {
            instruction_budget: 100,
            max_retries: 2,
            max_call_depth: 50,
        },
    );

    let mut concurrent = kernel.begin();
    concurrent
        .replace_functional(rel(2), Tuple::from([item.clone(), other]))
        .unwrap();
    concurrent.commit().unwrap();

    assert_eq!(
        task.run().unwrap(),
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("Taken."))],
            mailbox_sends: Vec::new(),
            retries: 1,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[Some(item), None]).unwrap(),
        vec![Tuple::from([int(200), actor])]
    );
}

#[test]
fn explicit_rollback_retry_stops_at_retry_limit() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        0,
        [
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("discarded")),
            },
            Instruction::RollbackRetry,
        ],
    )
    .unwrap();
    let mut task = Task::new(
        1,
        &kernel,
        Arc::new(program),
        Arc::new(ProgramResolver::new()),
        TaskLimits {
            instruction_budget: 100,
            max_retries: 2,
            max_call_depth: 50,
        },
    );

    assert_eq!(
        task.run().unwrap_err(),
        TaskError::ConflictRetriesExceeded { retries: 2 }
    );
    assert_eq!(task.retries(), 2);
}

#[test]
fn task_manager_records_completed_task_and_delivers_effects() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("done")),
                },
                Instruction::Return {
                    value: v(Value::bool(true)),
                },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, outcome) = task_manager.submit(program).unwrap();
    assert_eq!(
        outcome,
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("done"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(task_manager.completed(task_id), Some(&outcome));
    assert!(task_manager.suspended(task_id).is_none());
    assert_eq!(
        task_manager.effects().effects(),
        &[Effect {
            task_id,
            target: rel(EFFECT_TARGET),
            value: strv("done"),
        }]
    );
}

#[test]
fn task_manager_does_not_publish_read_only_task_completion() {
    let kernel = kernel_with_world_relations();
    let version = kernel.snapshot().version();
    let program = Arc::new(Program::new(0, [Instruction::Return { value: v(int(1)) }]).unwrap());
    let mut task_manager = TaskManager::new(kernel);

    let (_, outcome) = task_manager.submit(program).unwrap();

    assert_eq!(
        outcome,
        TaskOutcome::Complete {
            value: int(1),
            effects: Vec::new(),
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(task_manager.kernel().snapshot().version(), version);
}

#[test]
fn task_manager_parks_and_resumes_suspended_task() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("before")),
                },
                Instruction::Suspend {
                    kind: SuspendKind::TimedMillis(1),
                },
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("after")),
                },
                Instruction::Return {
                    value: v(Value::bool(true)),
                },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, first) = task_manager.submit(program).unwrap();
    assert_eq!(
        first,
        TaskOutcome::Suspended {
            kind: SuspendKind::TimedMillis(1),
            effects: vec![emitted(strv("before"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(task_manager.suspended_len(), 1);
    assert_eq!(
        task_manager.suspended(task_id).map(|task| task.kind()),
        Some(&SuspendKind::TimedMillis(1))
    );
    assert_eq!(
        task_manager.effects().effects(),
        &[Effect {
            task_id,
            target: rel(EFFECT_TARGET),
            value: strv("before"),
        }]
    );

    let second = task_manager
        .resume_with_authority(task_id, AuthorityContext::root())
        .unwrap();
    assert_eq!(
        second,
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("after"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(task_manager.suspended_len(), 0);
    assert_eq!(task_manager.completed(task_id), Some(&second));
    assert_eq!(
        task_manager.effects().effects(),
        &[
            Effect {
                task_id,
                target: rel(EFFECT_TARGET),
                value: strv("before"),
            },
            Effect {
                task_id,
                target: rel(EFFECT_TARGET),
                value: strv("after"),
            },
        ]
    );
}

#[test]
fn task_manager_does_not_deliver_pending_effects_from_abort() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("discarded")),
                },
                Instruction::Abort {
                    error: v(sym("abort")),
                },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, outcome) = task_manager.submit(program).unwrap();
    assert_eq!(
        outcome,
        TaskOutcome::Aborted {
            error: sym("abort"),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(task_manager.completed(task_id), Some(&outcome));
    assert!(task_manager.effects().effects().is_empty());
}

#[test]
fn task_manager_can_refresh_authority_when_resuming_suspended_task() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Suspend {
                    kind: SuspendKind::TimedMillis(1),
                },
                Instruction::ReplaceFunctional {
                    relation: rel(2),
                    values: vec![v(int(200)), v(int(300))],
                },
                Instruction::Return {
                    value: v(Value::bool(true)),
                },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, first) = task_manager
        .submit_with_authority(program, AuthorityContext::empty())
        .unwrap();
    assert_eq!(
        first,
        TaskOutcome::Suspended {
            kind: SuspendKind::TimedMillis(1),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );

    let mut refreshed = AuthorityContext::empty();
    refreshed.mint(CapabilityGrant::relation(CapabilityOp::Write, rel(2)));
    let second = task_manager
        .resume_with_authority(task_id, refreshed)
        .unwrap();

    assert_eq!(
        second,
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(
        task_manager
            .kernel()
            .snapshot()
            .scan(rel(2), &[Some(int(200)), None])
            .unwrap(),
        vec![Tuple::from([int(200), int(300)])]
    );
}

#[test]
fn task_manager_rejects_unknown_and_completed_resume() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [Instruction::Return {
                value: v(Value::nothing()),
            }],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    assert_eq!(
        task_manager
            .resume_with_authority(999, AuthorityContext::root())
            .unwrap_err(),
        TaskManagerError::UnknownTask(999)
    );
    let (task_id, _) = task_manager.submit(program).unwrap();
    assert_eq!(
        task_manager
            .resume_with_authority(task_id, AuthorityContext::root())
            .unwrap_err(),
        TaskManagerError::TaskAlreadyCompleted(task_id)
    );
}

#[test]
fn task_manager_cancels_suspended_task() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Suspend {
                    kind: SuspendKind::Never,
                },
                Instruction::Return {
                    value: v(Value::nothing()),
                },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);
    let (task_id, outcome) = task_manager.submit(program).unwrap();
    assert!(matches!(outcome, TaskOutcome::Suspended { .. }));

    assert_eq!(
        task_manager.cancel_task(task_id).unwrap(),
        SuspendKind::Never
    );
    assert_eq!(task_manager.suspended_len(), 0);
    assert_eq!(task_manager.cancelled_len(), 1);
    assert_eq!(
        task_manager
            .resume_with_authority(task_id, AuthorityContext::root())
            .unwrap_err(),
        TaskManagerError::TaskCancelled(task_id)
    );
    assert_eq!(
        task_manager.cancel_task(task_id).unwrap_err(),
        TaskManagerError::TaskCancelled(task_id)
    );
}

#[test]
fn shared_task_manager_cancels_suspended_task() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Suspend {
                    kind: SuspendKind::Never,
                },
                Instruction::Return {
                    value: v(Value::nothing()),
                },
            ],
        )
        .unwrap(),
    );
    let task_manager = TaskManager::new(kernel).into_shared();
    let (task_id, outcome) = task_manager
        .submit_with_context(program, AuthorityContext::root(), RuntimeContext::default())
        .unwrap();
    assert!(matches!(outcome, TaskOutcome::Suspended { .. }));

    assert_eq!(
        task_manager.cancel_task(task_id).unwrap(),
        SuspendKind::Never
    );
    assert_eq!(task_manager.suspended_len(), 0);
    assert_eq!(task_manager.cancelled_len(), 1);
    assert_eq!(
        task_manager
            .resume_with_context(
                task_id,
                AuthorityContext::root(),
                Value::nothing(),
                RuntimeContext::default(),
            )
            .unwrap_err(),
        TaskManagerError::TaskCancelled(task_id)
    );
}

#[test]
fn shared_task_manager_cancels_all_suspended_tasks() {
    let kernel = kernel_with_world_relations();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Suspend {
                    kind: SuspendKind::Never,
                },
                Instruction::Return {
                    value: v(Value::nothing()),
                },
            ],
        )
        .unwrap(),
    );
    let task_manager = TaskManager::new(kernel).into_shared();
    let (first, _) = task_manager
        .submit_with_context(
            program.clone(),
            AuthorityContext::root(),
            RuntimeContext::default(),
        )
        .unwrap();
    let (second, _) = task_manager
        .submit_with_context(program, AuthorityContext::root(), RuntimeContext::default())
        .unwrap();

    assert_eq!(
        task_manager.cancel_all_tasks(),
        vec![(first, SuspendKind::Never), (second, SuspendKind::Never)]
    );
    assert_eq!(task_manager.suspended_len(), 0);
    assert_eq!(task_manager.cancelled_len(), 2);
    assert!(task_manager.cancel_all_tasks().is_empty());
}

#[test]
fn direct_program_call_returns_into_caller_register() {
    let kernel = kernel_with_world_relations();
    let callee = Arc::new(Program::new(2, [Instruction::Return { value: r(0) }]).unwrap());
    let caller = Program::new(
        2,
        [
            Instruction::Load {
                dst: reg(0),
                value: int(42),
            },
            Instruction::Call {
                dst: reg(1),
                program: callee,
                args: vec![r(0)],
            },
            Instruction::Return { value: r(1) },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, caller, 100).unwrap(),
        TaskOutcome::Complete {
            value: int(42),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn builtin_call_invokes_registered_host_function() {
    let kernel = kernel_with_world_relations();
    let builtins = BuiltinRegistry::new().with_builtin(
        "emit_first_arg",
        BuiltinResultKind::Dynamic,
        emit_first_arg,
    );
    let program = Program::new(
        1,
        [
            Instruction::BuiltinCall {
                dst: reg(0),
                name: Symbol::intern("emit_first_arg"),
                result_kind: None,
                args: vec![v(ident(EFFECT_TARGET)), v(strv("hello"))],
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program_with_builtins(&kernel, restored, builtins).unwrap(),
        TaskOutcome::Complete {
            value: strv("hello"),
            effects: vec![emitted(strv("hello"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn dynamic_builtin_call_expands_argument_splices() {
    let kernel = kernel_with_world_relations();
    let builtins = BuiltinRegistry::new().with_builtin(
        "emit_first_arg",
        BuiltinResultKind::Dynamic,
        emit_first_arg,
    );
    let program = Program::new(
        2,
        [
            Instruction::BuildList {
                dst: reg(0),
                items: vec![item(v(ident(EFFECT_TARGET))), item(v(strv("hello")))],
            },
            Instruction::BuiltinCallDynamic {
                dst: reg(1),
                name: Symbol::intern("emit_first_arg"),
                result_kind: None,
                args: vec![splice(r(0))],
            },
            Instruction::Return { value: r(1) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program_with_builtins(&kernel, restored, builtins).unwrap(),
        TaskOutcome::Complete {
            value: strv("hello"),
            effects: vec![emitted(strv("hello"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn builtin_calls_reject_results_that_violate_the_compiled_contract() {
    let kernel = kernel_with_world_relations();
    for instruction in [
        Instruction::BuiltinCall {
            dst: reg(0),
            name: Symbol::intern("wrong_result"),
            result_kind: Some(ValueKind::String),
            args: Vec::new(),
        },
        Instruction::BuiltinCallDynamic {
            dst: reg(0),
            name: Symbol::intern("wrong_result"),
            result_kind: Some(ValueKind::String),
            args: Vec::new(),
        },
    ] {
        let program = Program::new(1, [instruction, Instruction::Return { value: r(0) }]).unwrap();
        let program = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
        let builtins = BuiltinRegistry::new().with_builtin(
            "wrong_result",
            BuiltinResultKind::Exact(ValueKind::String),
            |_context: &mut BuiltinContext<'_, '_>, _args: &[Value]| Ok(int(1)),
        );

        assert_eq!(
            run_program_with_builtins(&kernel, program, builtins),
            Err(TaskError::Runtime(
                RuntimeError::BuiltinResultKindMismatch {
                    name: Symbol::intern("wrong_result"),
                    expected: ValueKind::String,
                    actual: ValueKind::Int,
                }
            ))
        );
    }
}

#[test]
fn relation_construction_round_trips_through_program_artifacts() {
    let heading = vec![Symbol::intern("thing"), Symbol::intern("count")];
    let program = Program::new(
        1,
        [
            Instruction::BuildRelation {
                dst: reg(0),
                heading: heading.clone(),
                cells: vec![v(ident(7)), v(int(2)), v(ident(7)), v(int(2))],
                row_count: 2,
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    let expected = Value::relation(heading, [Tuple::from([ident(7), int(2)])]).unwrap();
    assert_eq!(
        run_program(&kernel_with_world_relations(), restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: expected,
            effects: Vec::new(),
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn dynamic_function_value_call_expands_argument_splices() {
    let kernel = kernel_with_world_relations();
    let callee = Arc::new(
        Program::new(
            6,
            [
                Instruction::Index {
                    dst: reg(1),
                    collection: reg(0),
                    index: v(int(0)),
                },
                Instruction::Index {
                    dst: reg(2),
                    collection: reg(0),
                    index: v(int(1)),
                },
                Instruction::Index {
                    dst: reg(3),
                    collection: reg(0),
                    index: v(int(2)),
                },
                Instruction::Binary {
                    dst: reg(4),
                    op: RuntimeBinaryOp::Add,
                    left: reg(1),
                    right: reg(2),
                },
                Instruction::Binary {
                    dst: reg(5),
                    op: RuntimeBinaryOp::Add,
                    left: reg(4),
                    right: reg(3),
                },
                Instruction::Return { value: r(5) },
            ],
        )
        .unwrap(),
    );
    let program = Program::new(
        3,
        [
            Instruction::BuildList {
                dst: reg(0),
                items: vec![item(v(int(2))), item(v(int(3)))],
            },
            Instruction::LoadFunction {
                dst: reg(1),
                program: Arc::clone(&callee),
                captures: Vec::new(),
                min_arity: 3,
                max_arity: 3,
            },
            Instruction::CallValueDynamic {
                dst: reg(2),
                callee: r(1),
                args: vec![item(v(int(1))), splice(r(0))],
            },
            Instruction::Return { value: r(2) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: int(6),
            effects: Vec::new(),
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn missing_builtin_call_is_runtime_error() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        1,
        [
            Instruction::BuiltinCall {
                dst: reg(0),
                name: Symbol::intern("missing_builtin"),
                result_kind: None,
                args: vec![],
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap_err(),
        TaskError::Runtime(RuntimeError::UnknownBuiltin {
            name: Symbol::intern("missing_builtin")
        })
    );
}

#[test]
fn program_artifact_round_trips_range_slicing() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        3,
        [
            Instruction::BuildList {
                dst: reg(0),
                items: vec![
                    item(v(int(1))),
                    item(v(int(2))),
                    item(v(int(3))),
                    item(v(int(4))),
                ],
            },
            Instruction::BuildRange {
                dst: reg(1),
                start: v(int(1)),
                end: Some(v(int(2))),
            },
            Instruction::Index {
                dst: reg(2),
                collection: reg(0),
                index: r(1),
            },
            Instruction::Return { value: r(2) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::list([int(2), int(3)]),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn program_artifact_round_trips_suspend_and_read() {
    let program = Program::new(
        2,
        [
            Instruction::SuspendValue {
                dst: reg(0),
                duration: Some(v(Value::float(0.5).unwrap())),
            },
            Instruction::Read {
                dst: reg(1),
                metadata: Some(r(0)),
            },
            Instruction::CommitValue { dst: reg(0) },
            Instruction::Return { value: r(1) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();

    assert_eq!(restored, program);
}

#[test]
fn program_artifact_round_trips_positional_spawn() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        1,
        [
            Instruction::SpawnPositionalDispatch {
                dst: reg(0),
                selector: v(sym("inspect")),
                args: vec![v(ident(10)), v(ident(20))],
                delay: None,
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert!(matches!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(request),
            ..
        } if request.selector == Symbol::intern("inspect")
            && request.target == SpawnTarget::PositionalArgs(vec![ident(10), ident(20)])
            && request.delay_millis.is_none()
    ));
}

#[test]
fn program_artifact_round_trips_dynamic_positional_spawn() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        2,
        [
            Instruction::BuildList {
                dst: reg(1),
                items: vec![item(v(ident(20)))],
            },
            Instruction::SpawnPositionalDispatchDynamic {
                dst: reg(0),
                selector: v(sym("inspect")),
                args: vec![item(v(ident(10))), splice(r(1))],
                delay: Some(v(Value::float(0.5).unwrap())),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert!(matches!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(request),
            ..
        } if request.selector == Symbol::intern("inspect")
            && request.target == SpawnTarget::PositionalArgs(vec![ident(10), ident(20)])
            && request.delay_millis == Some(500)
    ));
}

#[test]
fn program_artifact_round_trips_dynamic_named_spawn() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        3,
        [
            Instruction::BuildMap {
                dst: reg(1),
                entries: vec![(v(sym("item")), v(ident(20)))],
            },
            Instruction::BuildMapDynamic {
                dst: reg(2),
                items: vec![map_entry(v(sym("actor")), v(ident(10))), map_splice(r(1))],
            },
            Instruction::SpawnDispatchDynamic {
                dst: reg(0),
                selector: v(sym("inspect")),
                roles: r(2),
                delay: Some(v(Value::float(0.5).unwrap())),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    let TaskOutcome::Suspended {
        kind: SuspendKind::Spawn(request),
        ..
    } = run_program(&kernel, restored, 100).unwrap()
    else {
        panic!("dynamic spawn should suspend");
    };
    let SpawnTarget::NamedRoles(roles) = request.target else {
        panic!("dynamic named spawn should retain named roles");
    };

    assert_eq!(request.selector, Symbol::intern("inspect"));
    assert_eq!(request.delay_millis, Some(500));
    assert_eq!(roles.len(), 2);
    assert!(roles.contains(&(Symbol::intern("actor"), ident(10))));
    assert!(roles.contains(&(Symbol::intern("item"), ident(20))));
}

#[test]
fn program_artifact_round_trips_dynamic_positional_dispatch() {
    let program = Program::new(
        2,
        [
            Instruction::PositionalDispatchDynamic {
                dst: reg(0),
                relations: DispatchRelations {
                    method_selector: rel(10),
                    param: rel(11),
                    delegates: rel(12),
                },
                program_relation: rel(13),
                program_bytes: rel(14),
                selector: v(sym("inspect")),
                args: vec![item(v(ident(20))), splice(r(1))],
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();

    assert_eq!(restored, program);
}

#[test]
fn program_artifact_round_trips_list_splices() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        3,
        [
            Instruction::BuildList {
                dst: reg(0),
                items: vec![item(v(int(2))), item(v(int(3)))],
            },
            Instruction::BuildList {
                dst: reg(1),
                items: vec![item(v(int(1))), splice(r(0)), item(v(int(4)))],
            },
            Instruction::Return { value: r(1) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::list([int(1), int(2), int(3), int(4)]),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn program_artifact_round_trips_dynamic_relation_splices() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        3,
        [
            Instruction::BuildList {
                dst: reg(0),
                items: vec![item(v(ident(10))), item(v(ident(20)))],
            },
            Instruction::AssertDynamic {
                relation: rel(2),
                args: vec![RelationArg::Splice(r(0))],
            },
            Instruction::BuildList {
                dst: reg(1),
                items: vec![item(v(ident(10)))],
            },
            Instruction::ScanDynamic {
                dst: reg(2),
                relation: rel(2),
                args: vec![
                    RelationArg::Splice(r(1)),
                    RelationArg::Query(Symbol::intern("room")),
                ],
            },
            Instruction::Return { value: r(2) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::relation([Symbol::intern("room")], [Tuple::from([ident(20)])],).unwrap(),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn program_artifact_round_trips_error_codes() {
    let kernel = kernel_with_world_relations();
    let code = err("E_NOT_PORTABLE");
    let program = Program::new(
        1,
        [
            Instruction::Load {
                dst: reg(0),
                value: code.clone(),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: code,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn program_artifact_round_trips_rich_errors() {
    let kernel = kernel_with_world_relations();
    let error = error(
        "E_NOT_PORTABLE",
        Some("That cannot be taken."),
        Some(strv("lamp")),
    );
    let program = Program::new(
        1,
        [
            Instruction::Load {
                dst: reg(0),
                value: error.clone(),
            },
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: error,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn error_field_instruction_extracts_rich_error_parts() {
    let kernel = kernel_with_world_relations();
    let error = error(
        "E_NOT_PORTABLE",
        Some("That cannot be taken."),
        Some(strv("lamp")),
    );
    let program = Program::new(
        5,
        [
            Instruction::Load {
                dst: reg(0),
                value: error,
            },
            Instruction::ErrorField {
                dst: reg(1),
                error: reg(0),
                field: ErrorField::Code,
            },
            Instruction::ErrorField {
                dst: reg(2),
                error: reg(0),
                field: ErrorField::Message,
            },
            Instruction::ErrorField {
                dst: reg(3),
                error: reg(0),
                field: ErrorField::Value,
            },
            Instruction::BuildList {
                dst: reg(4),
                items: vec![item(r(1)), item(r(2)), item(r(3))],
            },
            Instruction::Return { value: r(4) },
        ],
    )
    .unwrap();
    let restored = Program::from_bytes(&program.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, program);

    assert_eq!(
        run_program(&kernel, restored, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::list([
                err("E_NOT_PORTABLE"),
                strv("That cannot be taken."),
                strv("lamp")
            ]),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn raise_without_handler_aborts_with_rich_error() {
    let kernel = kernel_with_world_relations();
    let expected = error(
        "E_NOT_PORTABLE",
        Some("That cannot be taken."),
        Some(strv("lamp")),
    );
    let program = Program::new(
        0,
        [Instruction::Raise {
            error: v(err("E_NOT_PORTABLE")),
            message: Some(v(strv("That cannot be taken."))),
            value: Some(v(strv("lamp"))),
        }],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Aborted {
            error: expected,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn try_catches_raised_error_by_code() {
    let kernel = kernel_with_world_relations();
    let expected = error(
        "E_NOT_PORTABLE",
        Some("That cannot be taken."),
        Some(strv("lamp")),
    );
    let program = Program::new(
        2,
        [
            Instruction::EnterTry {
                catches: vec![crate::CatchHandler {
                    code: Some(err("E_NOT_PORTABLE")),
                    binding: Some(reg(1)),
                    target: 3,
                }],
                finally: None,
                end: 4,
            },
            Instruction::Raise {
                error: v(err("E_NOT_PORTABLE")),
                message: Some(v(strv("That cannot be taken."))),
                value: Some(v(strv("lamp"))),
            },
            Instruction::ExitTry,
            Instruction::Return { value: r(1) },
            Instruction::Return {
                value: v(Value::nothing()),
            },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: expected,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn raise_unwinds_across_activation_to_caller_handler() {
    let kernel = kernel_with_world_relations();
    let callee = Arc::new(
        Program::new(
            0,
            [Instruction::Raise {
                error: v(err("E_NO_EXIT")),
                message: Some(v(strv("No exit."))),
                value: None,
            }],
        )
        .unwrap(),
    );
    let expected = error("E_NO_EXIT", Some("No exit."), None);
    let caller = Program::new(
        2,
        [
            Instruction::EnterTry {
                catches: vec![crate::CatchHandler {
                    code: Some(err("E_NO_EXIT")),
                    binding: Some(reg(1)),
                    target: 3,
                }],
                finally: None,
                end: 4,
            },
            Instruction::Call {
                dst: reg(0),
                program: callee,
                args: vec![],
            },
            Instruction::ExitTry,
            Instruction::Return { value: r(1) },
            Instruction::Return {
                value: v(Value::nothing()),
            },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, caller, 100).unwrap(),
        TaskOutcome::Complete {
            value: expected,
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn finally_runs_on_return() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        1,
        [
            Instruction::EnterTry {
                catches: vec![],
                finally: Some(3),
                end: 5,
            },
            Instruction::Return { value: v(int(7)) },
            Instruction::ExitTry,
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("cleanup")),
            },
            Instruction::EndFinally,
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: int(7),
            effects: vec![emitted(strv("cleanup"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn caught_exception_runs_finally_before_continuing() {
    let kernel = kernel_with_world_relations();
    let program = Program::new(
        1,
        [
            Instruction::EnterTry {
                catches: vec![crate::CatchHandler {
                    code: Some(err("E_NOT_PORTABLE")),
                    binding: None,
                    target: 3,
                }],
                finally: Some(5),
                end: 7,
            },
            Instruction::Raise {
                error: v(err("E_NOT_PORTABLE")),
                message: None,
                value: None,
            },
            Instruction::ExitTry,
            Instruction::Load {
                dst: reg(0),
                value: Value::bool(true),
            },
            Instruction::ExitTry,
            Instruction::Emit {
                target: v(ident(EFFECT_TARGET)),
                value: v(strv("cleanup")),
            },
            Instruction::EndFinally,
            Instruction::Return { value: r(0) },
        ],
    )
    .unwrap();

    assert_eq!(
        run_program(&kernel, program, 100).unwrap(),
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("cleanup"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn call_depth_limit_is_enforced() {
    let kernel = kernel_with_world_relations();
    let leaf = Arc::new(
        Program::new(
            0,
            [Instruction::Return {
                value: v(Value::nothing()),
            }],
        )
        .unwrap(),
    );
    let caller = Arc::new(
        Program::new(
            1,
            [
                Instruction::Call {
                    dst: reg(0),
                    program: leaf,
                    args: vec![],
                },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap(),
    );
    let mut task = Task::new(
        1,
        &kernel,
        caller,
        Arc::new(ProgramResolver::new()),
        TaskLimits {
            instruction_budget: 100,
            max_retries: 1,
            max_call_depth: 1,
        },
    );

    assert_eq!(
        task.run().unwrap_err(),
        TaskError::Runtime(RuntimeError::MaxCallDepthExceeded { max_depth: 1 })
    );
}

#[test]
fn suspension_inside_callee_resumes_full_activation_stack() {
    let kernel = kernel_with_world_relations();
    let callee = Arc::new(
        Program::new(
            1,
            [
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("in callee")),
                },
                Instruction::Suspend {
                    kind: SuspendKind::TimedMillis(1),
                },
                Instruction::Return { value: r(0) },
            ],
        )
        .unwrap(),
    );
    let caller = Arc::new(
        Program::new(
            2,
            [
                Instruction::Load {
                    dst: reg(0),
                    value: int(7),
                },
                Instruction::Call {
                    dst: reg(1),
                    program: callee,
                    args: vec![r(0)],
                },
                Instruction::Return { value: r(1) },
            ],
        )
        .unwrap(),
    );
    let mut task_manager = TaskManager::new(kernel);

    let (task_id, first) = task_manager.submit(caller).unwrap();
    assert_eq!(
        first,
        TaskOutcome::Suspended {
            kind: SuspendKind::TimedMillis(1),
            effects: vec![emitted(strv("in callee"))],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
    assert_eq!(task_manager.suspended(task_id).unwrap().frame_count(), 2);

    assert_eq!(
        task_manager
            .resume_with_authority(task_id, AuthorityContext::root())
            .unwrap(),
        TaskOutcome::Complete {
            value: int(7),
            effects: vec![],
            mailbox_sends: Vec::new(),
            retries: 0,
        }
    );
}

#[test]
fn commit_conflict_retries_restore_call_stack() {
    let kernel = kernel_with_world_relations();
    let item = int(200);
    let room = int(300);
    let other = int(500);
    let actor = int(100);

    let mut seed = kernel.begin();
    seed.replace_functional(rel(2), Tuple::from([item.clone(), room]))
        .unwrap();
    seed.commit().unwrap();

    let callee = Arc::new(
        Program::new(
            2,
            [
                Instruction::ReplaceFunctional {
                    relation: rel(2),
                    values: vec![r(0), r(1)],
                },
                Instruction::Emit {
                    target: v(ident(EFFECT_TARGET)),
                    value: v(strv("moved")),
                },
                Instruction::Return {
                    value: v(Value::bool(true)),
                },
            ],
        )
        .unwrap(),
    );
    let caller = Arc::new(
        Program::new(
            3,
            [
                Instruction::Load {
                    dst: reg(0),
                    value: item.clone(),
                },
                Instruction::Load {
                    dst: reg(1),
                    value: actor.clone(),
                },
                Instruction::Call {
                    dst: reg(2),
                    program: callee,
                    args: vec![r(0), r(1)],
                },
                Instruction::Return { value: r(2) },
            ],
        )
        .unwrap(),
    );
    let mut task = Task::new(
        1,
        &kernel,
        caller,
        Arc::new(ProgramResolver::new()),
        TaskLimits {
            instruction_budget: 100,
            max_retries: 2,
            max_call_depth: 50,
        },
    );

    let mut concurrent = kernel.begin();
    concurrent
        .replace_functional(rel(2), Tuple::from([item.clone(), other]))
        .unwrap();
    concurrent.commit().unwrap();

    assert_eq!(
        task.run().unwrap(),
        TaskOutcome::Complete {
            value: Value::bool(true),
            effects: vec![emitted(strv("moved"))],
            mailbox_sends: Vec::new(),
            retries: 1,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[Some(item), None]).unwrap(),
        vec![Tuple::from([int(200), actor])]
    );
}
