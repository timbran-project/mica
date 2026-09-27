// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    BuiltinContext, BuiltinRegistry, BuiltinResultKind, RuntimeError, method_relations,
    raised_builtin_error,
};
use mica_relation_kernel::RelationId;
use mica_var::{Symbol, Value, ValueKind};
use mica_vm::{
    CatchHandler, ErrorField, Instruction, KindCheckSite, ListItem, MapItem, Operand, Program,
    Register, RelationArg, RuntimeBinaryOp, RuntimeUnaryOp,
};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub(super) fn install(registry: BuiltinRegistry) -> BuiltinRegistry {
    registry
        .with_builtin(
            "is_builtin",
            BuiltinResultKind::Exact(ValueKind::Bool),
            is_builtin,
        )
        .with_builtin(
            "assemble",
            BuiltinResultKind::Exact(ValueKind::Bytes),
            assemble,
        )
}

fn is_builtin(context: &mut BuiltinContext<'_, '_>, args: &[Value]) -> Result<Value, RuntimeError> {
    let [name] = args else {
        return Err(raised_builtin_error(
            "E_INVARG",
            "is_builtin expects one symbol",
            None,
        ));
    };
    let name = name
        .as_symbol()
        .ok_or_else(|| raised_builtin_error("E_TYPE", "is_builtin expects a symbol", None))?;
    Ok(Value::bool(context.is_builtin(name)))
}

fn assemble(_: &mut BuiltinContext<'_, '_>, args: &[Value]) -> Result<Value, RuntimeError> {
    let [description] = args else {
        return Err(raised_builtin_error(
            "E_INVARG",
            "assemble: expected one program description",
            None,
        ));
    };
    let mut remaining = 65_536;
    let result = program(description, 0, &mut remaining).and_then(|program| {
        program
            .to_bytes()
            .map_err(|error| invalid(format!("{error:?}")))
    });
    result
        .map(Value::bytes)
        .map_err(|error| raised_builtin_error("E_INVARG", format!("assemble: {error}"), None))
}

#[derive(Debug)]
struct AssemblyError(String);

impl Display for AssemblyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn invalid(message: impl Into<String>) -> AssemblyError {
    AssemblyError(message.into())
}

fn list(value: &Value) -> Result<Vec<Value>, AssemblyError> {
    value
        .with_list(<[Value]>::to_vec)
        .ok_or_else(|| invalid("expected a list"))
}

fn symbol(value: &Value) -> Result<Symbol, AssemblyError> {
    value
        .as_symbol()
        .ok_or_else(|| invalid("expected a symbol"))
}

fn integer(value: &Value) -> Result<usize, AssemblyError> {
    value
        .as_int()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value <= u16::MAX as usize)
        .ok_or_else(|| invalid("expected an integer from 0 through 65535"))
}

fn register(value: &Value) -> Result<Register, AssemblyError> {
    Ok(Register(integer(value)? as u16))
}

fn relation(value: &Value) -> Result<RelationId, AssemblyError> {
    value
        .as_identity()
        .ok_or_else(|| invalid("expected a relation identity"))
}

fn value_kind(value: &Value) -> Result<ValueKind, AssemblyError> {
    let name = symbol(value)?;
    [
        ValueKind::Bool,
        ValueKind::Int,
        ValueKind::Float,
        ValueKind::Identity,
        ValueKind::Symbol,
        ValueKind::ErrorCode,
        ValueKind::String,
        ValueKind::Bytes,
        ValueKind::List,
        ValueKind::Map,
        ValueKind::Range,
        ValueKind::Error,
        ValueKind::Capability,
        ValueKind::Frob,
        ValueKind::Function,
        ValueKind::Relation,
    ]
    .into_iter()
    .find(|kind| Some(kind.name()) == name.name())
    .ok_or_else(|| invalid("unknown value kind"))
}

fn operand(value: &Value) -> Result<Operand, AssemblyError> {
    let parts = list(value)?;
    let [tag, value] = parts.as_slice() else {
        return Err(invalid(
            "operand must be [:Register, index] or [:Constant, value]",
        ));
    };
    match symbol(tag)?.name() {
        Some("Register") => Ok(Operand::Register(register(value)?)),
        Some("Constant") => Ok(Operand::Value(value.clone())),
        _ => Err(invalid("unknown operand tag")),
    }
}

fn operands(value: &Value) -> Result<Vec<Operand>, AssemblyError> {
    list(value)?.iter().map(operand).collect()
}

fn optional_operand(value: &Value) -> Result<Option<Operand>, AssemblyError> {
    if *value == Value::option_none() {
        return Ok(None);
    }
    operand(value).map(Some)
}

fn list_items(value: &Value) -> Result<Vec<ListItem>, AssemblyError> {
    list(value)?
        .iter()
        .map(|item| {
            let parts = list(item)?;
            if let [tag, value] = parts.as_slice()
                && tag.as_symbol().and_then(Symbol::name) == Some("Splice")
            {
                return Ok(ListItem::Splice(operand(value)?));
            }
            operand(item).map(ListItem::Value)
        })
        .collect()
}

fn relation_args(value: &Value) -> Result<Vec<RelationArg>, AssemblyError> {
    list(value)?
        .iter()
        .map(|item| {
            let parts = list(item)?;
            match parts.as_slice() {
                [tag] if tag.as_symbol().and_then(Symbol::name) == Some("Hole") => {
                    Ok(RelationArg::Hole)
                }
                [tag, value] if tag.as_symbol().and_then(Symbol::name) == Some("Query") => {
                    Ok(RelationArg::Query(symbol(value)?))
                }
                [tag, value] if tag.as_symbol().and_then(Symbol::name) == Some("Splice") => {
                    Ok(RelationArg::Splice(operand(value)?))
                }
                _ => operand(item).map(RelationArg::Value),
            }
        })
        .collect()
}

fn program(value: &Value, depth: usize, remaining: &mut usize) -> Result<Program, AssemblyError> {
    if depth >= 16 {
        return Err(invalid("nested program depth exceeds 16"));
    }
    let fields = value
        .with_map(<[(Value, Value)]>::to_vec)
        .ok_or_else(|| invalid("program description must be a map"))?;
    if fields.len() != 2
        || fields.iter().any(|(key, _)| {
            !matches!(
                key.as_symbol().and_then(Symbol::name),
                Some("registers" | "code")
            )
        })
    {
        return Err(invalid("program requires exactly :registers and :code"));
    }
    let registers = integer(
        &value
            .map_get(&Value::symbol(Symbol::intern("registers")))
            .ok_or_else(|| invalid("missing :registers"))?,
    )?;
    let code = list(
        &value
            .map_get(&Value::symbol(Symbol::intern("code")))
            .ok_or_else(|| invalid("missing :code"))?,
    )?;
    if code.is_empty() || code.len() > *remaining {
        return Err(invalid(
            "program must be nonempty and total code must not exceed 65536 instructions",
        ));
    }
    *remaining -= code.len();
    let instructions = code
        .iter()
        .enumerate()
        .map(|(index, value)| {
            instruction(value, depth, remaining)
                .map_err(|error| invalid(format!("instruction {index}: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Program::new(registers, instructions).map_err(|error| invalid(format!("{error:?}")))
}

fn instruction(
    value: &Value,
    depth: usize,
    remaining: &mut usize,
) -> Result<Instruction, AssemblyError> {
    let fields = list(value)?;
    let Some((op, args)) = fields.split_first() else {
        return Err(invalid("instruction must start with an operation symbol"));
    };
    let op = symbol(op)?;
    let methods = method_relations();
    Ok(match (op.name(), args) {
        (Some("CheckKind"), [value, expected, site, subject]) => Instruction::CheckKind {
            value: register(value)?,
            expected: value_kind(expected)?,
            subject: symbol(subject)?,
            site: match symbol(site)?.name() {
                Some("Binding") => KindCheckSite::Binding,
                Some("Parameter") => KindCheckSite::Parameter,
                Some("Builtin") => KindCheckSite::Builtin,
                _ => return Err(invalid("unknown kind-check site")),
            },
        },
        (Some("Load"), [dst, value]) => Instruction::Load {
            dst: register(dst)?,
            value: value.clone(),
        },
        (Some("Move"), [dst, src]) => Instruction::Move {
            dst: register(dst)?,
            src: register(src)?,
        },
        (Some("Unary"), [dst, op, src]) => Instruction::Unary {
            dst: register(dst)?,
            src: register(src)?,
            op: match symbol(op)?.name() {
                Some("Not") => RuntimeUnaryOp::Not,
                Some("Neg") => RuntimeUnaryOp::Neg,
                _ => return Err(invalid("unknown unary operation")),
            },
        },
        (Some("Binary"), [dst, op, left, right]) => Instruction::Binary {
            dst: register(dst)?,
            left: register(left)?,
            right: register(right)?,
            op: match symbol(op)?.name() {
                Some("Eq") => RuntimeBinaryOp::Eq,
                Some("Ne") => RuntimeBinaryOp::Ne,
                Some("Lt") => RuntimeBinaryOp::Lt,
                Some("Le") => RuntimeBinaryOp::Le,
                Some("Gt") => RuntimeBinaryOp::Gt,
                Some("Ge") => RuntimeBinaryOp::Ge,
                Some("Add") => RuntimeBinaryOp::Add,
                Some("Sub") => RuntimeBinaryOp::Sub,
                Some("Mul") => RuntimeBinaryOp::Mul,
                Some("Div") => RuntimeBinaryOp::Div,
                Some("Rem") => RuntimeBinaryOp::Rem,
                _ => return Err(invalid("unknown binary operation")),
            },
        },
        (Some("BuildList"), [dst, items]) => Instruction::BuildList {
            dst: register(dst)?,
            items: list_items(items)?,
        },
        (Some("BuildMap"), [dst, items]) => Instruction::BuildMapDynamic {
            dst: register(dst)?,
            items: list(items)?
                .iter()
                .map(|item| {
                    let fields = list(item)?;
                    match fields.as_slice() {
                        [tag, value]
                            if tag.as_symbol().and_then(Symbol::name) == Some("Splice") =>
                        {
                            Ok(MapItem::Splice(operand(value)?))
                        }
                        [key, value] => Ok(MapItem::Entry(operand(key)?, operand(value)?)),
                        _ => Err(invalid(
                            "map item must contain two operands or [:Splice, operand]",
                        )),
                    }
                })
                .collect::<Result<_, _>>()?,
        },
        (Some("BuildRelation"), [dst, heading, cells, rows]) => Instruction::BuildRelation {
            dst: register(dst)?,
            heading: list(heading)?
                .iter()
                .map(symbol)
                .collect::<Result<_, _>>()?,
            cells: operands(cells)?,
            row_count: integer(rows)? as u16,
        },
        (Some("RelationPattern"), [dst, relation, heading, rows, equalities]) => {
            Instruction::RelationPattern {
                dst: register(dst)?,
                relation: register(relation)?,
                heading: list(heading)?
                    .iter()
                    .map(symbol)
                    .collect::<Result<_, _>>()?,
                row_count: integer(rows)? as u16,
                equalities: list(equalities)?
                    .iter()
                    .map(|entry| {
                        let fields = list(entry)?;
                        let [column, value] = fields.as_slice() else {
                            return Err(invalid(
                                "relation equality must contain a column and value",
                            ));
                        };
                        Ok((symbol(column)?, value.clone()))
                    })
                    .collect::<Result<_, _>>()?,
            }
        }
        (Some("RelationCell"), [dst, relation, column]) => Instruction::RelationCell {
            dst: register(dst)?,
            relation: register(relation)?,
            column: symbol(column)?,
        },
        (Some("BuildRange"), [dst, start, end]) => Instruction::BuildRange {
            dst: register(dst)?,
            start: operand(start)?,
            end: optional_operand(end)?,
        },
        (Some("Index"), [dst, collection, index]) => Instruction::Index {
            dst: register(dst)?,
            collection: register(collection)?,
            index: operand(index)?,
        },
        (Some("SetIndex"), [dst, collection, index, value]) => Instruction::SetIndex {
            dst: register(dst)?,
            collection: register(collection)?,
            index: operand(index)?,
            value: operand(value)?,
        },
        (Some("CollectionLen"), [dst, collection]) => Instruction::CollectionLen {
            dst: register(dst)?,
            collection: register(collection)?,
        },
        (Some("CollectionKeyAt"), [dst, collection, index]) => Instruction::CollectionKeyAt {
            dst: register(dst)?,
            collection: register(collection)?,
            index: register(index)?,
        },
        (Some("CollectionValueAt"), [dst, collection, index]) => Instruction::CollectionValueAt {
            dst: register(dst)?,
            collection: register(collection)?,
            index: register(index)?,
        },
        (Some("CollectionFieldAt"), [dst, collection, index, heading, column]) => {
            Instruction::CollectionFieldAt {
                dst: register(dst)?,
                collection: register(collection)?,
                index: register(index)?,
                heading: list(heading)?
                    .iter()
                    .map(symbol)
                    .collect::<Result<_, _>>()?,
                column: symbol(column)?,
            }
        }
        (Some("Branch"), [condition, if_true, if_false]) => Instruction::Branch {
            condition: register(condition)?,
            if_true: integer(if_true)?,
            if_false: integer(if_false)?,
        },
        (Some("Jump"), [target]) => Instruction::Jump {
            target: integer(target)?,
        },
        (Some("BuiltinCall"), [dst, name, args]) => Instruction::BuiltinCallDynamic {
            dst: register(dst)?,
            name: symbol(name)?,
            result_kind: None,
            args: list_items(args)?,
        },
        (Some("PositionalDispatch"), [dst, selector, args]) => {
            Instruction::PositionalDispatchDynamic {
                dst: register(dst)?,
                selector: operand(selector)?,
                args: list_items(args)?,
                relations: methods.dispatch,
                program_relation: methods.method_program,
                program_bytes: methods.program_bytes,
            }
        }
        (Some("DynamicDispatch"), [dst, selector, roles]) => Instruction::DynamicDispatch {
            dst: register(dst)?,
            selector: operand(selector)?,
            roles: operand(roles)?,
            relations: methods.dispatch,
            program_relation: methods.method_program,
            program_bytes: methods.program_bytes,
        },
        (Some("LoadFunction"), [dst, description, captures, min, max]) => {
            let program = program(description, depth + 1, remaining)?;
            let captures = operands(captures)?;
            let min_arity = integer(min)? as u16;
            let max_arity = integer(max)? as u16;
            if min_arity > max_arity || captures.len() >= program.register_count() {
                return Err(invalid(
                    "function requires ordered arity bounds and registers for captures plus the argument list",
                ));
            }
            Instruction::LoadFunction {
                dst: register(dst)?,
                program: Arc::new(program),
                captures,
                min_arity,
                max_arity,
            }
        }
        (Some("CallValue"), [dst, callee, args]) => Instruction::CallValueDynamic {
            dst: register(dst)?,
            callee: operand(callee)?,
            args: list_items(args)?,
        },
        (Some("ScanDynamic"), [dst, target, args]) => Instruction::ScanDynamic {
            dst: register(dst)?,
            relation: relation(target)?,
            args: relation_args(args)?,
        },
        (Some("AssertDynamic"), [target, args]) => Instruction::AssertDynamic {
            relation: relation(target)?,
            args: relation_args(args)?,
        },
        (Some("RetractDynamic"), [target, args]) => Instruction::RetractDynamic {
            relation: relation(target)?,
            args: relation_args(args)?,
        },
        (Some("EnterTry"), [catches, finally, end]) => Instruction::EnterTry {
            catches: list(catches)?
                .iter()
                .map(|catch| {
                    let parts = list(catch)?;
                    let [code, binding, target] = parts.as_slice() else {
                        return Err(invalid("catch must be [code, binding, target]"));
                    };
                    Ok(CatchHandler {
                        code: (*code != Value::option_none()).then(|| code.clone()),
                        binding: if *binding == Value::option_none() {
                            None
                        } else {
                            Some(register(binding)?)
                        },
                        target: integer(target)?,
                    })
                })
                .collect::<Result<_, _>>()?,
            finally: if *finally == Value::option_none() {
                None
            } else {
                Some(integer(finally)?)
            },
            end: integer(end)?,
        },
        (Some("ExitTry"), []) => Instruction::ExitTry,
        (Some("EndFinally"), []) => Instruction::EndFinally,
        (Some("Raise"), [error, message, value]) => Instruction::Raise {
            error: operand(error)?,
            message: optional_operand(message)?,
            value: optional_operand(value)?,
        },
        (Some("ErrorField"), [dst, error, field]) => Instruction::ErrorField {
            dst: register(dst)?,
            error: register(error)?,
            field: match symbol(field)?.name() {
                Some("Code") => ErrorField::Code,
                Some("Message") => ErrorField::Message,
                Some("Value") => ErrorField::Value,
                _ => return Err(invalid("unknown error field")),
            },
        },
        (Some("Return"), [value]) => Instruction::Return {
            value: operand(value)?,
        },
        (Some("Abort"), [error]) => Instruction::Abort {
            error: operand(error)?,
        },
        (Some("Emit"), [target, value]) => Instruction::Emit {
            target: operand(target)?,
            value: operand(value)?,
        },
        (Some("CommitValue"), [dst]) => Instruction::CommitValue {
            dst: register(dst)?,
        },
        (Some("SuspendValue"), [dst, duration]) => Instruction::SuspendValue {
            dst: register(dst)?,
            duration: optional_operand(duration)?,
        },
        (Some("Read"), [dst, metadata]) => Instruction::Read {
            dst: register(dst)?,
            metadata: optional_operand(metadata)?,
        },
        (Some("MailboxRecv"), [dst, receivers, timeout]) => Instruction::MailboxRecv {
            dst: register(dst)?,
            receivers: operand(receivers)?,
            timeout: optional_operand(timeout)?,
        },
        (Some("ExternalRequest"), [dst, service, payload, timeout]) => {
            Instruction::ExternalRequest {
                dst: register(dst)?,
                service: operand(service)?,
                payload: operand(payload)?,
                timeout: optional_operand(timeout)?,
            }
        }
        (Some("RollbackRetry"), []) => Instruction::RollbackRetry,
        _ => {
            return Err(invalid(format!(
                "unknown operation or invalid operands for :{}",
                op.name().unwrap_or("?")
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::program;
    use crate::{
        AuthorityContext, BuiltinContext, BuiltinRegistry, BuiltinResultKind, RuntimeError,
        SourceRunner, SuspendKind, Task, TaskError, TaskLimits, TaskOutcome,
    };
    use mica_relation_kernel::{RelationKernel, RelationMetadata};
    use mica_var::{Symbol, Value, ValueKind};
    use mica_vm::{Instruction, Operand, Program, ProgramResolver, Register};
    use std::sync::Arc;

    fn assembled(source: &str) -> Program {
        let mut runner = SourceRunner::new_empty();
        let report = runner.run_source(source).unwrap();
        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("{}", report.render());
        };
        value.with_bytes(Program::from_bytes).unwrap().unwrap()
    }

    fn execute(program: Program) -> Value {
        let kernel = RelationKernel::new();
        let mut task = Task::new(
            1,
            &kernel,
            Arc::new(program),
            Arc::new(ProgramResolver::new()),
            TaskLimits::default(),
        );
        let outcome = task.run().unwrap();
        let TaskOutcome::Complete { value, .. } = outcome else {
            panic!("{outcome:?}");
        };
        value
    }

    fn host_builtin(_: &mut BuiltinContext<'_, '_>, _: &[Value]) -> Result<Value, RuntimeError> {
        Ok(Value::bool(true))
    }

    #[test]
    fn builtin_query_uses_the_executing_registry() {
        let registry = super::install(BuiltinRegistry::new()).with_builtin(
            "host_extension",
            BuiltinResultKind::Exact(ValueKind::Bool),
            host_builtin,
        );
        let kernel = RelationKernel::new();
        for (name, expected) in [
            ("host_extension", true),
            ("assemble", true),
            ("missing", false),
            ("commit", false),
        ] {
            let program = Program::new(
                1,
                [
                    Instruction::BuiltinCall {
                        dst: Register(0),
                        name: Symbol::intern("is_builtin"),
                        result_kind: None,
                        args: vec![Operand::Value(Value::symbol(Symbol::intern(name)))],
                    },
                    Instruction::Return {
                        value: Operand::Register(Register(0)),
                    },
                ],
            )
            .unwrap();
            let mut task = Task::new_with_authority(
                1,
                &kernel,
                Arc::new(program),
                Arc::new(ProgramResolver::new()),
                Arc::new(registry.clone()),
                AuthorityContext::empty(),
                TaskLimits::default(),
            );
            assert!(
                matches!(task.run().unwrap(), TaskOutcome::Complete { value, .. } if value == Value::bool(expected)),
                "{name}"
            );
        }
    }

    #[test]
    fn assembly_artifact_runs_control_flow_and_unicode_collections() {
        let program = assembled(
            r#"return assemble({:registers -> 5, :code -> [
          [:Load, 0, "é🦀"], [:CollectionLen, 1, 0], [:Load, 2, 2],
          [:Binary, 3, :Eq, 1, 2], [:Branch, 3, 5, 8],
          [:BuildList, 4, [[:Register, 0], [:Constant, 42]]],
          [:Index, 4, 4, [:Constant, 0]], [:Return, [:Register, 4]],
          [:Return, [:Constant, false]]
        ]})"#,
        );
        assert_eq!(execute(program), Value::string("é🦀"));
    }

    #[test]
    fn assembly_artifact_catches_errors_and_calls_capturing_functions() {
        let program = assembled(
            r#"return assemble({:registers -> 4, :code -> [
          [:EnterTry, [[E_TEST, 0, 2]], none, 3],
          [:Raise, [:Constant, E_TEST], none, none],
          [:Load, 0, 40],
          [:LoadFunction, 1, {:registers -> 4, :code -> [
            [:Index, 2, 1, [:Constant, 0]], [:Binary, 3, :Add, 0, 2], [:Return, [:Register, 3]]
          ]}, [[:Register, 0]], 1, 1],
          [:CallValue, 2, [:Register, 1], [[:Constant, 2]]],
          [:Return, [:Register, 2]]
        ]})"#,
        );
        assert_eq!(execute(program), Value::int(42).unwrap());
    }

    #[test]
    fn assembly_reads_fields_of_a_caught_error() {
        let program = assembled(
            r#"return assemble({:registers -> 5, :code -> [
          [:EnterTry, [[E_TEST, 0, 2]], none, 6],
          [:Raise, [:Constant, E_TEST], [:Constant, "é🦀"], [:Constant, 42]],
          [:ErrorField, 1, 0, :Code], [:ErrorField, 2, 0, :Message],
          [:ErrorField, 3, 0, :Value],
          [:BuildList, 4, [[:Register, 1], [:Register, 2], [:Register, 3]]],
          [:Return, [:Register, 4]]
        ]})"#,
        );
        assert_eq!(
            execute(program),
            Value::list([
                Value::error_code(Symbol::intern("E_TEST")),
                Value::option_some(Value::string("é🦀")),
                Value::option_some(Value::int(42).unwrap()),
            ])
        );
    }

    #[test]
    fn assembly_matches_exact_relation_rows_and_reads_cells() {
        for (relation, expected) in [
            ("[:a, :b] {[42, \"é🦀\"]}", true),
            ("[:a, :b] {[41, \"é🦀\"]}", false),
            ("[:a, :b] {}", false),
            ("[:a, :b] {[42, \"é🦀\"], [43, \"x\"]}", false),
            ("[:a] {[42]}", false),
            ("[:a, :b, :c] {[42, \"é🦀\", 0]}", false),
            ("{:a -> 42, :b -> \"é🦀\"}", false),
        ] {
            let program = assembled(&format!(
                r#"return assemble({{:registers -> 3, :code -> [
                  [:Load, 0, {relation}],
                  [:RelationPattern, 1, 0, [:a, :b], 1, [[:a, 42]]],
                  [:Branch, 1, 3, 5],
                  [:RelationCell, 2, 0, :b], [:Return, [:Register, 2]],
                  [:Return, [:Constant, false]]
                ]}})"#,
            ));
            assert_eq!(
                execute(program),
                if expected {
                    Value::string("é🦀")
                } else {
                    Value::bool(false)
                },
                "{relation}",
            );
        }
        for relation in ["[:a] {}", "[:b] {[42]}", "42"] {
            let program = assembled(&format!(
                r#"return assemble({{:registers -> 2, :code -> [
                  [:Load, 0, {relation}], [:EnterTry, [[E_MATCH, none, 4]], none, 5],
                  [:RelationCell, 1, 0, :a], [:Return, [:Constant, false]],
                  [:Return, [:Constant, true]], [:Return, [:Constant, false]]
                ]}})"#,
            ));
            assert_eq!(execute(program), Value::bool(true), "{relation}");
        }
    }

    #[test]
    fn assembly_iteration_fields_require_exact_relation_headings() {
        for (collection, index, expected) in [
            ("[:a] {[41], [42]}", 1, 42),
            ("[{:a -> 41}, {:a -> 42, :extra -> true}]", 1, 42),
            ("[:a, :extra] {[42, true]}", 0, -1),
            ("[{:wrong -> 42}]", 0, -1),
            ("[:a] {[42]}", -1, -1),
            ("[:a] {[42]}", 1, -1),
        ] {
            let program = assembled(&format!(
                r#"return assemble({{:registers -> 3, :code -> [
                  [:Load, 0, {collection}], [:Load, 1, {index}],
                  [:EnterTry, [[E_MATCH, none, 5]], none, 6],
                  [:CollectionFieldAt, 2, 0, 1, [:a], :a], [:Return, [:Register, 2]],
                  [:Return, [:Constant, -1]], [:Return, [:Constant, false]]
                ]}})"#,
            ));
            assert_eq!(
                execute(program),
                Value::int(expected).unwrap(),
                "{collection}"
            );
        }
    }

    #[test]
    fn assembly_rejects_invalid_shapes_registers_targets_and_artifacts() {
        let mut runner = SourceRunner::new_empty();
        for description in [
            "[]",
            "{:registers -> 1, :code -> [], :constants -> []}",
            "{:registers -> 1, :code -> []}",
            "{:registers -> -1, :code -> [[:Return, [:Constant, 0]]]}",
            "{:registers -> 65536, :code -> [[:Return, [:Constant, 0]]]}",
            "{:registers -> 1, :code -> [[:Load, 1, 0]]}",
            "{:registers -> 1, :code -> [[:Jump, 2]]}",
            "{:registers -> 1, :code -> [[:Return, 0]]}",
            "{:registers -> 1, :code -> [[:Return, [:Register, -1]]]}",
            "{:registers -> 1, :code -> [[:Load, 0]]}",
            "{:registers -> 1, :code -> [[:Missing, 0]]}",
            "{:registers -> 1, :code -> [[:ErrorField, 0, 0, :Missing]]}",
            "{:registers -> 1, :code -> [[:CheckKind, 0, :missing, :Binding, :x]]}",
            "{:registers -> 1, :code -> [[:CheckKind, 0, :int, :Missing, :x]]}",
            "{:registers -> 1, :code -> [[:CheckKind, 1, :int, :Binding, :x]]}",
            "{:registers -> 1, :code -> [[:BuildRelation, 0, [:a], [], 1]]}",
            "{:registers -> 1, :code -> [[:RelationPattern, 0, 1, [:a], 1, []]]}",
            "{:registers -> 1, :code -> [[:RelationPattern, 0, 0, [1], 1, []]]}",
            "{:registers -> 1, :code -> [[:RelationPattern, 0, 0, [:a], -1, []]]}",
            "{:registers -> 1, :code -> [[:RelationPattern, 0, 0, [:a], 1, [[:a]]]]}",
            "{:registers -> 1, :code -> [[:RelationPattern, 0, 0, [:a], 1, [[1, 42]]]]}",
            "{:registers -> 1, :code -> [[:RelationPattern, 0, 0, [:a], 1, [[:a, fn() => 1]]]]}",
            "{:registers -> 1, :code -> [[:RelationCell, 0, 0, 1]]}",
            "{:registers -> 1, :code -> [[:RelationCell, 1, 0, :a]]}",
            "{:registers -> 1, :code -> [[:CollectionFieldAt, 0, 0, 1, [:a], :a]]}",
            "{:registers -> 1, :code -> [[:CollectionFieldAt, 0, 0, 0, [1], :a]]}",
            "{:registers -> 1, :code -> [[:Load, 0, fn() => 1]]}",
            "{:registers -> 1, :code -> [[:LoadFunction, 0, {:registers -> 1, :code -> [[:Return, [:Constant, true]]]}, [], 2, 1]]}",
            "{:registers -> 1, :code -> [[:LoadFunction, 0, {:registers -> 0, :code -> [[:Return, [:Constant, true]]]}, [], 0, 0]]}",
        ] {
            let source = format!(
                "try\n assemble({description})\n return false\ncatch E_INVARG\n return true\nend"
            );
            let report = runner
                .run_source(&source)
                .unwrap_or_else(|error| panic!("{source}: {error:?}"));
            assert!(
                matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
                "{source}: {}",
                report.render()
            );
        }
    }

    #[test]
    fn assembly_bounds_nested_programs_and_total_instructions() {
        let sym = |name| Value::symbol(Symbol::intern(name));
        let wrap = |code| {
            Value::map([
                (sym("registers"), Value::int(1).unwrap()),
                (sym("code"), code),
            ])
        };
        let instruction = Value::list([
            sym("Return"),
            Value::list([sym("Constant"), Value::bool(true)]),
        ]);
        let mut description = wrap(Value::list([instruction.clone()]));
        let mut budget = 0;
        assert!(program(&description, 0, &mut budget).is_err());
        for _ in 0..64 {
            description = wrap(Value::list([
                Value::list([
                    sym("LoadFunction"),
                    Value::int(0).unwrap(),
                    description,
                    Value::list([]),
                    Value::int(0).unwrap(),
                    Value::int(0).unwrap(),
                ]),
                instruction.clone(),
            ]));
        }
        assert!(program(&description, 0, &mut 65536).is_err());
    }

    #[test]
    fn assembled_writes_require_execution_authority() {
        let program = assembled(
            "let target = make_relation(:Data, 1)\nreturn assemble({:registers -> 1, :code -> [[:AssertDynamic, target, [[:Constant, 7]]], [:Return, [:Constant, true]]]})",
        );
        let Instruction::AssertDynamic {
            relation: target, ..
        } = program.instructions()[0]
        else {
            panic!("expected assertion");
        };
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(target, Symbol::intern("Data"), 1))
            .unwrap();
        let mut task = Task::new_with_authority(
            1,
            &kernel,
            Arc::new(program),
            Arc::new(ProgramResolver::new()),
            Arc::new(BuiltinRegistry::new()),
            AuthorityContext::empty(),
            TaskLimits::default(),
        );
        assert!(matches!(
            task.run(),
            Err(TaskError::Runtime(RuntimeError::PermissionDenied { .. }))
        ));
        assert!(kernel.snapshot().scan(target, &[None]).unwrap().is_empty());
    }

    #[test]
    fn assembled_program_preserves_registers_across_commit_continuations() {
        let program = assembled(
            "return assemble({:registers -> 2, :code -> [[:Load, 0, 40], [:CommitValue, 1], [:Binary, 0, :Add, 0, 1], [:Return, [:Register, 0]]]})",
        );
        let kernel = RelationKernel::new();
        let mut task = Task::new(
            1,
            &kernel,
            Arc::new(program),
            Arc::new(ProgramResolver::new()),
            TaskLimits::default(),
        );
        assert!(matches!(
            task.run().unwrap(),
            TaskOutcome::Suspended {
                kind: SuspendKind::Commit,
                ..
            }
        ));
        task.resume_with(Value::int(2).unwrap()).unwrap();
        assert!(
            matches!(task.run().unwrap(), TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap())
        );
    }
}
