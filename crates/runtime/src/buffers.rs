// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{EPHEMERAL_HOST_IDENTITY_START, GENERATED_RELATION_ID_START};
use mica_relation_kernel::buffer::{
    BufferApplyOutcome, BufferApplyStatus, BufferConflictPolicy, BufferError, BufferMetadata,
    BufferRevertStatus, Delta, InsertionAffinity, Replacement, TextError,
};
use mica_relation_kernel::{KernelError, RelationDurability};
use mica_var::{Identity, Symbol, Tuple, Value, ValueKind};
use mica_vm::{
    Builtin, BuiltinContext, BuiltinRegistry, BuiltinResultKind, CapabilityGrant, CapabilityOp,
    RuntimeError,
};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn install(mut registry: BuiltinRegistry) -> BuiltinRegistry {
    registry = registry.with_builtin(
        "make_buffer",
        BuiltinResultKind::Exact(ValueKind::Bool),
        MakeBuffer {
            next_id: AtomicU64::new(GENERATED_RELATION_ID_START),
        },
    );
    registry = registry
        .with_builtin(
            "buffer_compact",
            BuiltinResultKind::Exact(ValueKind::Bool),
            compact_builtin,
        )
        .with_builtin(
            "buffer_revert",
            BuiltinResultKind::Exact(ValueKind::Symbol),
            revert_builtin,
        )
        .with_builtin(
            "buffer_apply",
            BuiltinResultKind::Exact(ValueKind::Symbol),
            apply_builtin,
        )
        .with_builtin(
            "buffer_apply_result",
            BuiltinResultKind::Dynamic,
            apply_result_builtin,
        )
        .with_builtin(
            "buffer_marker_rebase",
            BuiltinResultKind::Exact(ValueKind::Int),
            marker_rebase_builtin,
        );
    for (name, operation, count, kind) in [
        ("buffer_len", Read::Len, 1, Some(ValueKind::Int)),
        (
            "buffer_line_count",
            Read::LineCount,
            1,
            Some(ValueKind::Int),
        ),
        ("buffer_revision", Read::Revision, 1, Some(ValueKind::Int)),
        ("buffer_text", Read::Text, 1, Some(ValueKind::String)),
        ("buffer_slice", Read::Slice, 3, Some(ValueKind::String)),
        ("buffer_find", Read::Find, 4, None),
        ("buffer_lines", Read::Lines, 3, Some(ValueKind::Relation)),
        (
            "buffer_viewport",
            Read::Viewport,
            4,
            Some(ValueKind::Relation),
        ),
        ("buffer_line_span", Read::LineSpan, 2, None),
        (
            "buffer_position_line_column",
            Read::PositionLineColumn,
            2,
            Some(ValueKind::Map),
        ),
        (
            "buffer_line_column_offset",
            Read::LineColumnOffset,
            3,
            Some(ValueKind::Int),
        ),
    ] {
        registry = registry.with_builtin(
            name,
            kind.map_or(BuiltinResultKind::Dynamic, BuiltinResultKind::Exact),
            ReadBuffer {
                name,
                operation,
                count,
            },
        );
    }
    for (name, operation, count) in [
        ("buffer_insert", Write::Insert, 3),
        ("buffer_delete", Write::Delete, 3),
        ("buffer_replace", Write::Replace, 4),
        ("kill_buffer", Write::Kill, 1),
    ] {
        registry = registry.with_builtin(
            name,
            BuiltinResultKind::Exact(ValueKind::Bool),
            WriteBuffer {
                name,
                operation,
                count,
            },
        );
    }
    registry
}

fn fault(code: &str, message: impl AsRef<str>) -> RuntimeError {
    RuntimeError::Raised(Value::error(
        Symbol::intern(code),
        Some(message.as_ref()),
        None,
    ))
}

fn kernel_error(error: KernelError) -> RuntimeError {
    match error {
        KernelError::Buffer {
            error: BufferError::Deleted,
            ..
        } => fault("E_KILLED", "the buffer is retired"),
        KernelError::Buffer {
            error: BufferError::Unknown,
            ..
        } => fault("E_INVARG", "unknown buffer"),
        KernelError::Buffer {
            error: BufferError::Text(_),
            ..
        } => fault("E_INDEX", "buffer range is outside the text"),
        KernelError::Buffer {
            error: BufferError::AlreadyApplied,
            ..
        } => fault(
            "E_STATE",
            "this buffer was already modified or applied in this transaction",
        ),
        KernelError::Buffer {
            error: BufferError::TokenInUse,
            ..
        } => fault("E_STATE", "the buffer apply token is already in use"),
        KernelError::Buffer {
            error: BufferError::ResultsFull,
            ..
        } => fault("E_STATE", "too many buffer apply results are pending"),
        error => RuntimeError::Kernel(error),
    }
}

fn arity(name: &str, args: &[Value], count: usize) -> Result<(), RuntimeError> {
    if args.len() != count {
        return Err(fault(
            "E_INVARG",
            format!("{name} expects {count} arguments"),
        ));
    }
    Ok(())
}

fn symbol(value: &Value) -> Result<Symbol, RuntimeError> {
    value
        .as_symbol()
        .ok_or_else(|| fault("E_TYPE", "expected a symbol"))
}

fn offset(value: &Value) -> Result<usize, RuntimeError> {
    usize::try_from(nonnegative(value)?).map_err(|_| fault("E_RANGE", "buffer offset is too large"))
}

fn nonnegative(value: &Value) -> Result<u64, RuntimeError> {
    value
        .as_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| fault("E_TYPE", "expected a non-negative integer"))
}

fn integer(value: impl TryInto<i64>) -> Result<Value, RuntimeError> {
    value
        .try_into()
        .ok()
        .and_then(|value| Value::int(value).ok())
        .ok_or_else(|| fault("E_RANGE", "buffer position exceeds the integer range"))
}

fn resolve(
    context: &mut BuiltinContext<'_, '_>,
    value: &Value,
    write: bool,
) -> Result<Identity, RuntimeError> {
    let name = symbol(value)?;
    let id = context
        .tx()
        .buffer_named(name)
        .ok_or_else(|| fault("E_INVARG", "unknown buffer"))?;
    context.tx().buffer_metadata(id).map_err(kernel_error)?;
    let allowed = if write {
        context.authority().can_write_relation(id)
    } else {
        context.authority().can_read_relation(id)
    };
    if !allowed {
        return Err(fault(
            "E_PERMISSION",
            if write {
                "buffer write denied"
            } else {
                "buffer read denied"
            },
        ));
    }
    Ok(id)
}

struct MakeBuffer {
    next_id: AtomicU64,
}

impl Builtin for MakeBuffer {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        if !(2..=3).contains(&args.len()) {
            return Err(fault(
                "E_INVARG",
                "make_buffer expects a name, durability, and optional conflict policy",
            ));
        }
        if !context.authority().can_grant()
            && !context
                .authority()
                .can_invoke_builtin(Symbol::intern("make_buffer"))
        {
            return Err(fault(
                "E_PERMISSION",
                "buffer creation requires a make_buffer invoke grant",
            ));
        }
        let name = symbol(&args[0])?;
        let durability = match symbol(&args[1])?.name() {
            Some("durable") => RelationDurability::Durable,
            Some("volatile") => RelationDurability::Volatile,
            _ => {
                return Err(fault(
                    "E_INVARG",
                    "buffer durability must be :durable or :volatile",
                ));
            }
        };
        let conflict = match args.get(2).map(symbol).transpose()?.and_then(Symbol::name) {
            None | Some("reject") => BufferConflictPolicy::Reject,
            Some("span") => BufferConflictPolicy::Span,
            Some("whole") => BufferConflictPolicy::Whole,
            _ => {
                return Err(fault(
                    "E_INVARG",
                    "buffer conflict policy must be :reject, :span, or :whole",
                ));
            }
        };
        if let Some(id) = context.tx().buffer_named(name) {
            let metadata = context.tx().buffer_metadata(id).map_err(kernel_error)?;
            if metadata.durability != durability || metadata.conflict != conflict {
                return Err(fault(
                    "E_INVARG",
                    "buffer name exists with different metadata",
                ));
            }
            return Ok(Value::bool(true));
        }
        if context
            .kernel()
            .snapshot()
            .relation_metadata_named(name)
            .is_some()
        {
            return Err(fault(
                "E_INVARG",
                "buffer name already refers to a relation",
            ));
        }
        loop {
            let raw = self.next_id.fetch_add(1, Ordering::Relaxed);
            let id = (raw < EPHEMERAL_HOST_IDENTITY_START)
                .then(|| Identity::new(raw))
                .flatten()
                .ok_or_else(|| fault("E_RANGE", "generated buffer identity exhausted"))?;
            match context.tx().create_buffer(BufferMetadata {
                id,
                name,
                durability,
                conflict,
            }) {
                Ok(()) => {
                    if !context.authority().can_read_relation(id) {
                        context.mint_capability(CapabilityGrant::relation(CapabilityOp::Read, id));
                    }
                    if !context.authority().can_write_relation(id) {
                        context.mint_capability(CapabilityGrant::relation(CapabilityOp::Write, id));
                    }
                    return Ok(Value::bool(true));
                }
                Err(KernelError::Buffer {
                    error: BufferError::AlreadyExists,
                    ..
                }) => continue,
                Err(error) => return Err(kernel_error(error)),
            }
        }
    }
}

enum Read {
    Len,
    LineCount,
    Revision,
    Text,
    Slice,
    Find,
    Lines,
    Viewport,
    LineSpan,
    PositionLineColumn,
    LineColumnOffset,
}
struct ReadBuffer {
    name: &'static str,
    operation: Read,
    count: usize,
}

impl Builtin for ReadBuffer {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        arity(self.name, args, self.count)?;
        let id = resolve(context, &args[0], false)?;
        if matches!(self.operation, Read::Revision) {
            let revision = context.tx().buffer_revision(id).map_err(kernel_error)?;
            return integer(revision);
        }
        let text = context.tx().buffer_text(id).map_err(kernel_error)?;
        match self.operation {
            Read::Len => integer(text.len()),
            Read::LineCount => integer(text.line_count()),
            Read::Revision => unreachable!(),
            Read::Text => Ok(Value::string(text.to_text())),
            Read::Slice => text
                .slice(offset(&args[1])?..offset(&args[2])?)
                .map(Value::string)
                .map_err(|_| fault("E_INDEX", "buffer slice is outside the text")),
            Read::Find => {
                let from = offset(&args[2])?;
                let limit = offset(&args[3])?;
                let position = args[1]
                    .with_str(|pattern| text.find(pattern, from, limit))
                    .ok_or_else(|| fault("E_TYPE", "buffer search pattern must be a string"))?;
                position.map_or(Ok(Value::option_none()), integer)
            }
            Read::LineSpan => match text.line_span(offset(&args[1])?) {
                Ok(range) => Ok(Value::map([
                    (
                        Value::symbol(Symbol::intern("start")),
                        integer(range.start)?,
                    ),
                    (Value::symbol(Symbol::intern("stop")), integer(range.end)?),
                ])),
                Err(TextError::InvalidLine) => Ok(Value::option_none()),
                Err(_) => Err(fault("E_INDEX", "invalid buffer line")),
            },
            Read::PositionLineColumn => {
                let (line, column) = text.line_column(offset(&args[1])?.min(text.len())).unwrap();
                Ok(Value::map([
                    (Value::symbol(Symbol::intern("line")), integer(line)?),
                    (Value::symbol(Symbol::intern("column")), integer(column)?),
                ]))
            }
            Read::LineColumnOffset => {
                let line = offset(&args[1])?;
                let column = offset(&args[2])?;
                if line >= text.line_count() {
                    return integer(text.len());
                }
                let range = text.line_span(line).unwrap();
                integer(range.start.saturating_add(column).min(range.end))
            }
            Read::Lines | Read::Viewport => {
                let first = offset(&args[1])?;
                let count = offset(&args[2])?;
                let viewport = matches!(self.operation, Read::Viewport);
                let mut budget = if viewport {
                    offset(&args[3])?
                } else {
                    usize::MAX
                };
                let mut rows = Vec::new();
                for line in first..first.saturating_add(count).min(text.line_count()) {
                    let range = text.line_span(line).unwrap();
                    let complete = range.len() <= budget;
                    let stop = range.start + range.len().min(budget);
                    let mut row = vec![
                        args[0].clone(),
                        integer(line)?,
                        integer(range.start)?,
                        integer(stop)?,
                        Value::string(text.slice(range.start..stop).unwrap()),
                    ];
                    if viewport {
                        row.push(Value::bool(complete));
                    }
                    rows.push(Tuple::new(row));
                    budget -= stop - range.start;
                    if !complete {
                        break;
                    }
                }
                let heading = if viewport {
                    &["buffer", "line", "start", "stop", "text", "complete"][..]
                } else {
                    &["buffer", "line", "start", "stop", "text"][..]
                };
                Value::relation(heading.iter().map(|column| Symbol::intern(column)), rows).map_err(
                    |error| {
                        fault(
                            "E_INVARG",
                            format!("invalid buffer line relation: {error:?}"),
                        )
                    },
                )
            }
        }
    }
}

enum Write {
    Insert,
    Delete,
    Replace,
    Kill,
}
struct WriteBuffer {
    name: &'static str,
    operation: Write,
    count: usize,
}

impl Builtin for WriteBuffer {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        arity(self.name, args, self.count)?;
        let id = resolve(context, &args[0], true)?;
        if matches!(self.operation, Write::Kill) {
            context.tx().delete_buffer(id).map_err(kernel_error)?;
            return Ok(Value::bool(true));
        }
        let start = offset(&args[1])?;
        let end = match self.operation {
            Write::Insert => start,
            Write::Delete => start
                .checked_add(offset(&args[2])?)
                .ok_or_else(|| fault("E_INDEX", "buffer range overflows"))?,
            Write::Replace => offset(&args[2])?,
            Write::Kill => unreachable!(),
        };
        if matches!(self.operation, Write::Delete) {
            context
                .tx()
                .replace_buffer(id, start..end, "")
                .map_err(kernel_error)?;
        } else {
            let index = if matches!(self.operation, Write::Insert) {
                2
            } else {
                3
            };
            args[index]
                .with_str(|text| context.tx().replace_buffer(id, start..end, text))
                .ok_or_else(|| fault("E_TYPE", "buffer replacement must be a string"))?
                .map_err(kernel_error)?;
        }
        Ok(Value::bool(true))
    }
}

fn compact_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    arity("buffer_compact", args, 1)?;
    let id = resolve(context, &args[0], true)?;
    context.tx().compact_buffer(id).map_err(kernel_error)?;
    Ok(Value::bool(true))
}

fn revert_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    arity("buffer_revert", args, 3)?;
    let id = resolve(context, &args[0], true)?;
    if !context
        .authority()
        .can_invoke_builtin(Symbol::intern("buffer_revert"))
    {
        return Err(fault(
            "E_PERMISSION",
            "buffer reversion requires an invoke grant",
        ));
    }
    let revision = nonnegative(&args[1])?;
    let expected = nonnegative(&args[2])?;
    let status = context
        .tx()
        .revert_buffer(id, revision, expected)
        .map_err(kernel_error)?;
    Ok(Value::symbol(Symbol::intern(match status {
        BufferRevertStatus::Staged => "staged",
        BufferRevertStatus::Stale => "stale",
        BufferRevertStatus::Unknown => "unknown",
    })))
}

fn parse_edits(value: &Value) -> Result<Vec<Replacement>, RuntimeError> {
    let at = Value::symbol(Symbol::intern("at"));
    let remove = Value::symbol(Symbol::intern("remove"));
    let text = Value::symbol(Symbol::intern("text"));
    value
        .with_list(|values| {
            values
                .iter()
                .map(|value| {
                    if value.with_map(|_| ()).is_none() {
                        return Err(fault("E_TYPE", "each buffer edit must be a map"));
                    }
                    let start = value
                        .map_get(&at)
                        .as_ref()
                        .map(offset)
                        .transpose()?
                        .unwrap_or(0);
                    let count = value
                        .map_get(&remove)
                        .as_ref()
                        .map(offset)
                        .transpose()?
                        .unwrap_or(0);
                    let end = start
                        .checked_add(count)
                        .ok_or_else(|| fault("E_INDEX", "buffer edit range overflows"))?;
                    let text = value
                        .map_get(&text)
                        .map(|value| {
                            value
                                .with_str(str::to_owned)
                                .ok_or_else(|| fault("E_TYPE", "buffer edit text must be a string"))
                        })
                        .transpose()?
                        .unwrap_or_default();
                    Ok(Replacement {
                        range: start..end,
                        text,
                    })
                })
                .collect::<Result<Vec<_>, RuntimeError>>()
        })
        .ok_or_else(|| fault("E_TYPE", "buffer edits must be a list"))?
}

fn apply_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if !(3..=4).contains(&args.len()) {
        return Err(fault(
            "E_INVARG",
            "buffer_apply expects a buffer, revision, edits, and optional token",
        ));
    }
    let id = resolve(context, &args[0], true)?;
    let expected = nonnegative(&args[1])?;
    let edits = parse_edits(&args[2])?;
    let token = args
        .get(3)
        .map(nonnegative)
        .transpose()?
        .and_then(NonZeroU64::new);
    let status = context
        .tx()
        .apply_buffer(id, expected, &edits, token)
        .map_err(kernel_error)?;
    Ok(Value::symbol(Symbol::intern(match status {
        BufferApplyStatus::Staged => "staged",
        BufferApplyStatus::Stale => "stale",
    })))
}

fn apply_result_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    arity("buffer_apply_result", args, 1)?;
    let result = NonZeroU64::new(nonnegative(&args[0])?)
        .and_then(|token| context.kernel().buffer_apply_result(token));
    let Some(result) = result else {
        return Ok(Value::symbol(Symbol::intern("pending")));
    };
    if !context.authority().can_read_relation(result.buffer) {
        return Err(fault("E_PERMISSION", "buffer apply result read denied"));
    }
    let status = match &result.outcome {
        BufferApplyOutcome::Pending => return Ok(Value::symbol(Symbol::intern("pending"))),
        BufferApplyOutcome::Ok { .. } => "ok",
        BufferApplyOutcome::Resync => "resync",
        BufferApplyOutcome::Conflict => "conflict",
        BufferApplyOutcome::Aborted => "aborted",
    };
    let mut fields = vec![
        (
            Value::symbol(Symbol::intern("status")),
            Value::symbol(Symbol::intern(status)),
        ),
        (
            Value::symbol(Symbol::intern("revision")),
            integer(result.revision)?,
        ),
    ];
    if let BufferApplyOutcome::Ok { applied } = &result.outcome {
        let edits = applied
            .replacements()
            .iter()
            .map(|replacement| {
                Ok(Value::map([
                    (
                        Value::symbol(Symbol::intern("at")),
                        integer(replacement.range.start)?,
                    ),
                    (
                        Value::symbol(Symbol::intern("remove")),
                        integer(replacement.range.len())?,
                    ),
                    (
                        Value::symbol(Symbol::intern("text")),
                        Value::string(&replacement.text),
                    ),
                ]))
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        fields.push((Value::symbol(Symbol::intern("applied")), Value::list(edits)));
    }
    Ok(Value::map(fields))
}

fn marker_rebase_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    arity("buffer_marker_rebase", args, 3)?;
    let edits = parse_edits(&args[0])?;
    let position = offset(&args[1])?;
    let affinity = match symbol(&args[2])?.name() {
        Some("stick_before") => InsertionAffinity::Before,
        Some("stick_after") => InsertionAffinity::After,
        _ => {
            return Err(fault(
                "E_INVARG",
                "marker insertion type must be :stick_before or :stick_after",
            ));
        }
    };
    Delta::new(edits)
        .and_then(|delta| delta.rebase_marker(position, affinity))
        .map_err(|_| {
            fault(
                "E_INVARG",
                "marker delta must have sorted, non-overlapping ranges",
            )
        })
        .and_then(integer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Instruction, Operand, Program, ProgramResolver, Register, SourceRunner, Task, TaskError,
        TaskInput, TaskLimits, TaskOutcome, TaskRequest,
    };
    use mica_relation_kernel::RelationKernel;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn result(runner: &mut SourceRunner, source: &str) -> Value {
        let report = runner
            .run_source(source)
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("{source}: {}", report.render());
        };
        value
    }

    #[test]
    fn buffer_builtins_edit_unicode_and_read_bounded_views() {
        for interpreter_only in [true, false] {
            let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
            let value = result(
                &mut runner,
                r#"
                make_buffer(:notes, :durable, :span)
                buffer_insert(:notes, 0, "aé🦀\nxy\n")
                let before = buffer_text(:notes)
                buffer_replace(:notes, 1, 2, "λ")
                buffer_delete(:notes, 4, 1)
                return [before, buffer_text(:notes), buffer_len(:notes), buffer_line_count(:notes),
                  buffer_slice(:notes, 1, 3), buffer_find(:notes, "🦀", 0, 0), buffer_revision(:notes)]
            "#,
            );
            assert_eq!(
                value,
                crate::value_from_json_text(r#"["aé🦀\nxy\n","aλ🦀\ny\n",6,3,"λ🦀",2,0]"#).unwrap()
            );
            let value = result(
                &mut runner,
                r#"
                return [buffer_revision(:notes), buffer_line_span(:notes, 0),
                  buffer_line_span(:notes, 2), buffer_line_span(:notes, 99) == none,
                  buffer_position_line_column(:notes, 999), buffer_line_column_offset(:notes, 1, 999),
                  buffer_line_column_offset(:notes, 999, 0)]
            "#,
            );
            assert_eq!(
                value,
                Value::list([
                    integer(1).unwrap(),
                    Value::map([
                        (Value::symbol(Symbol::intern("start")), integer(0).unwrap()),
                        (Value::symbol(Symbol::intern("stop")), integer(3).unwrap())
                    ]),
                    Value::map([
                        (Value::symbol(Symbol::intern("start")), integer(6).unwrap()),
                        (Value::symbol(Symbol::intern("stop")), integer(6).unwrap())
                    ]),
                    Value::bool(true),
                    Value::map([
                        (Value::symbol(Symbol::intern("line")), integer(2).unwrap()),
                        (Value::symbol(Symbol::intern("column")), integer(0).unwrap())
                    ]),
                    integer(5).unwrap(),
                    integer(6).unwrap(),
                ])
            );
            assert_eq!(
                result(
                    &mut runner,
                    r#"
                let rows = buffer_viewport(:notes, 0, 99, 2)
                return [rows == [:buffer, :line, :start, :stop, :text, :complete] { [:notes, 0, 0, 2, "aλ", false] },
                  buffer_lines(:notes, 1, 99) == [:buffer, :line, :start, :stop, :text] { [:notes, 1, 4, 5, "y"], [:notes, 2, 6, 6, ""] },
                  buffer_find(:notes, "", 0, 0) == none, buffer_find(:notes, "🦀", 0, 2) == none]
            "#
                ),
                Value::list(vec![Value::bool(true); 4])
            );
        }
    }

    #[test]
    fn buffer_errors_are_catchable_and_aborted_tasks_publish_nothing() {
        let mut runner = SourceRunner::new_empty();
        result(
            &mut runner,
            "make_buffer(:notes, :durable)\nbuffer_insert(:notes, 0, \"text\")",
        );
        assert_eq!(
            result(
                &mut runner,
                r#"
            let errors = []
            try
              buffer_slice(:notes, 0, 999)
            catch E_INDEX
              errors = [@errors, :index]
            end
            try
              buffer_insert(:notes, -1, "x")
            catch E_TYPE
              errors = [@errors, :type]
            end
            try
              make_buffer(:notes, :volatile)
            catch E_INVARG
              errors = [@errors, :metadata]
            end
            return errors == [:index, :type, :metadata]
        "#
            ),
            Value::bool(true)
        );
        let report = runner
            .run_source(
                r#"
            make_buffer(:abandoned, :durable)
            buffer_insert(:abandoned, 0, "private")
            buffer_replace(:notes, 0, 4, "private")
            raise E_ABORT
        "#,
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
        assert_eq!(
            result(
                &mut runner,
                r#"
            let absent = false
            try
              buffer_text(:abandoned)
            catch E_INVARG
              absent = true
            end
            return [absent, buffer_text(:notes), buffer_revision(:notes)]
        "#
            ),
            Value::list([
                Value::bool(true),
                Value::string("text"),
                integer(1).unwrap()
            ])
        );
        assert_eq!(
            result(
                &mut runner,
                r#"
            kill_buffer(:notes)
            let retired = 0
            try
              buffer_text(:notes)
            catch E_KILLED
              retired = retired + 1
            end
            try
              make_buffer(:notes, :durable)
            catch E_KILLED
              retired = retired + 1
            end
            return retired
        "#
            ),
            integer(2).unwrap()
        );
    }

    #[test]
    fn buffer_creation_grants_only_new_buffer_access_until_the_next_task_boundary() {
        for interpreter_only in [true, false] {
            let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
            runner
                .run_filein(
                    r#"
                make_identity(:alice)
                make_identity(:bob)
                make_relation(:CanInvoke, 2)
                make_relation(:CanRead, 2)
                make_relation(:CanWrite, 2)
                make_relation(:BufferStat, 4)
                make_buffer(:private, :durable)
                buffer_insert(:private, 0, "secret")
                assert CanInvoke(#alice, :make_buffer)
                assert CanRead(#alice, :BufferStat)
            "#,
                )
                .unwrap();
            let report = runner
                .run_source_as(
                    Symbol::intern("alice"),
                    r#"
                make_buffer(:mine, :durable)
                buffer_insert(:mine, 0, "owned")
                require BufferStat(:mine, 5, 1, 1)
                require buffer_text(:mine) == "owned"
                make_buffer(:private, :durable)
                let denied = 0
                try
                  buffer_text(:private)
                catch E_PERMISSION
                  denied = denied + 1
                end
                try
                  buffer_insert(:private, 0, "denied")
                catch E_PERMISSION
                  denied = denied + 1
                end
                return denied == 2
            "#,
                )
                .unwrap();
            assert!(
                matches!(&report.outcome, TaskOutcome::Complete { value, .. }
                if *value == Value::bool(true)),
                "{}",
                report.render()
            );
            for (actor, source) in [
                ("alice", "buffer_text(:mine)"),
                ("alice", "buffer_insert(:mine, 0, \"denied\")"),
                ("bob", "make_buffer(:denied, :durable)"),
            ] {
                let report = runner
                    .run_source_as(
                        Symbol::intern(actor),
                        &format!(
                            "try\n{source}\ncatch E_PERMISSION\nreturn true\nend\nreturn false"
                        ),
                    )
                    .unwrap();
                assert!(
                    matches!(&report.outcome, TaskOutcome::Complete { value, .. }
                    if *value == Value::bool(true)),
                    "{}",
                    report.render()
                );
            }
            result(
                &mut runner,
                "assert CanRead(#alice, :mine)\nassert CanWrite(#alice, :mine)",
            );
            let report = runner
                .run_source_as(
                    Symbol::intern("alice"),
                    "buffer_insert(:mine, 5, \"!\")\nreturn buffer_text(:mine)",
                )
                .unwrap();
            assert!(
                matches!(&report.outcome, TaskOutcome::Complete { value, .. }
                if *value == Value::string("owned!")),
                "{}",
                report.render()
            );
            let report = runner
                .run_source_as(
                    Symbol::intern("alice"),
                    r#"
                make_buffer(:temporary, :volatile)
                buffer_insert(:temporary, 0, "before")
                suspend()
                try
                  buffer_text(:temporary)
                catch E_PERMISSION
                  return true
                end
                return false
            "#,
                )
                .unwrap();
            assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
            let report = runner
                .resume_as(Symbol::intern("alice"), report.task_id)
                .unwrap();
            assert!(
                matches!(&report.outcome, TaskOutcome::Complete { value, .. }
                if *value == Value::bool(true)),
                "{}",
                report.render()
            );
        }
    }

    #[test]
    fn buffer_reads_and_writes_use_fresh_actor_authority() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                r#"
            make_identity(:alice)
            make_relation(:GrantRead, 2)
            make_relation(:GrantWrite, 2)
            make_buffer(:notes, :durable)
            buffer_insert(:notes, 0, "text")
            assert GrantRead(#alice, :notes)
        "#,
            )
            .unwrap();
        let report = runner
            .run_source_as(Symbol::intern("alice"), "return buffer_text(:notes)")
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::string("text"))
        );
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                r#"
            try
              buffer_insert(:notes, 0, "denied")
            catch E_PERMISSION
              return true
            end
            return false
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
        result(&mut runner, "assert GrantWrite(#alice, :notes)");
        let report = runner
            .run_source_as(Symbol::intern("alice"), "buffer_insert(:notes, 4, \"!\")")
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Complete { .. }));
        result(&mut runner, "retract GrantRead(#alice, :notes)");
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                r#"
            try
              buffer_text(:notes)
            catch E_PERMISSION
              return true
            end
            return false
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
        assert_eq!(
            result(&mut runner, "return buffer_text(:notes)"),
            Value::string("text!")
        );
    }

    #[test]
    fn buffer_edits_commit_at_suspension_and_resume_with_a_fresh_view() {
        let mut runner = SourceRunner::new_empty();
        let report = runner
            .run_source(
                r#"
            make_buffer(:notes, :durable)
            buffer_insert(:notes, 0, "before")
            suspend()
            buffer_insert(:notes, buffer_len(:notes), " after")
            return buffer_text(:notes)
        "#,
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
        assert_eq!(
            result(&mut runner, "return buffer_text(:notes)"),
            Value::string("before")
        );
        result(&mut runner, "buffer_insert(:notes, 6, \" concurrent\")");
        let outcome = runner
            .resume_task(TaskRequest {
                input: TaskInput::Continuation {
                    task_id: report.task_id,
                    value: Value::unit(),
                },
                ..SourceRunner::root_source_request("")
            })
            .unwrap();
        assert!(
            matches!(outcome, TaskOutcome::Complete { value, .. } if value == Value::string("before concurrent after"))
        );
    }

    #[test]
    fn buffer_catalogue_allocation_does_not_collide_with_tuple_relations() {
        let mut runner = SourceRunner::new_empty();
        assert_eq!(
            result(
                &mut runner,
                r#"
            make_buffer(:first, :durable)
            make_relation(:Other, 1)
            make_buffer(:second, :volatile)
            make_functional_relation(:Functional, 2, [0])
            buffer_insert(:first, 0, "first")
            buffer_insert(:second, 0, "second")
            return [buffer_text(:first), buffer_text(:second)]
        "#
            ),
            Value::list([Value::string("first"), Value::string("second")])
        );
        for source in [
            "make_relation(:first, 1)",
            "make_functional_relation(:first, 2, [0])",
        ] {
            assert!(runner.run_source(source).is_err());
        }
        result(&mut runner, "kill_buffer(:first)");
        assert!(runner.run_filein("make_relation(:first, 1)").is_err());
        assert_eq!(
            result(
                &mut runner,
                r#"
            try
              make_buffer(:Other, :durable)
            catch E_INVARG
              return true
            end
            return false
        "#
            ),
            Value::bool(true)
        );
    }

    #[test]
    fn buffer_authority_is_refreshed_after_suspension() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                r#"
            make_identity(:alice)
            make_relation(:GrantWrite, 2)
            make_buffer(:notes, :durable)
            assert GrantWrite(#alice, :notes)
        "#,
            )
            .unwrap();
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                r#"
            buffer_insert(:notes, 0, "before")
            suspend()
            try
              buffer_insert(:notes, 6, " denied")
            catch E_PERMISSION
              return true
            end
            return false
        "#,
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
        result(&mut runner, "retract GrantWrite(#alice, :notes)");
        let report = runner
            .resume_as(Symbol::intern("alice"), report.task_id)
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
        assert_eq!(
            result(&mut runner, "return buffer_text(:notes)"),
            Value::string("before")
        );
    }

    #[test]
    fn read_only_queries_allow_buffer_reads_and_reject_buffer_writes() {
        let runner = SourceRunner::new_empty();
        for (source, expected) in [
            ("return buffer_slice(:notes, 0, 2)", true),
            ("return buffer_viewport(:notes, 0, 3, 128)", true),
            ("return buffer_insert(:notes, 0, \"text\")", false),
            ("return make_buffer(:notes, :durable)", false),
            ("return kill_buffer(:notes)", false),
        ] {
            let semantic = mica_compiler::parse_semantic_with_context(source, &runner.context);
            assert_eq!(
                crate::validate_read_only_source_query(&semantic).is_ok(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn tagged_buffer_applies_settle_only_after_publication() {
        let mut runner = SourceRunner::new_empty();
        assert_eq!(
            result(
                &mut runner,
                r#"
            make_buffer(:notes, :durable)
            let stale = buffer_apply(:notes, 99, [{:at -> 999, :text -> "stale"}], 1)
            let staged = buffer_apply(:notes, 0, [{:text -> "aé"}, {:at -> 2, :text -> "🦀"}], 42)
            let blocked = false
            try
              buffer_insert(:notes, 0, "later")
            catch E_STATE
              blocked = true
            end
            return [stale, staged, buffer_apply_result(42), buffer_text(:notes), blocked]
        "#
            ),
            Value::list([
                Value::symbol(Symbol::intern("stale")),
                Value::symbol(Symbol::intern("staged")),
                Value::symbol(Symbol::intern("pending")),
                Value::string("aé🦀"),
                Value::bool(true)
            ])
        );
        assert_eq!(
            result(
                &mut runner,
                r#"
            let settled = buffer_apply_result(42)
            return [settled[:status] == :ok, settled[:revision],
              settled[:applied] == [{:at -> 0, :remove -> 0, :text -> "aé🦀"}],
              buffer_apply_result(1) == :pending]
        "#
            ),
            Value::list([
                Value::bool(true),
                integer(1).unwrap(),
                Value::bool(true),
                Value::bool(true)
            ])
        );
        let report = runner
            .run_source(
                r#"
            buffer_apply(:notes, 1, [{:at -> 0, :remove -> 3, :text -> "private"}], 43)
            raise E_ABORT
        "#,
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
        assert_eq!(
            result(
                &mut runner,
                "return [buffer_apply_result(43)[:status] == :aborted, buffer_text(:notes)]"
            ),
            Value::list([Value::bool(true), Value::string("aé🦀")])
        );
    }

    #[test]
    fn buffer_result_reads_require_buffer_authority() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein("make_identity(:alice)\nmake_relation(:GrantRead, 2)")
            .unwrap();
        result(
            &mut runner,
            "make_buffer(:notes, :durable)\nbuffer_apply(:notes, 0, [{:text -> \"secret\"}], 42)",
        );
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                r#"
            try
              buffer_apply_result(42)
            catch E_PERMISSION
              return true
            end
            return false
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
        result(&mut runner, "assert GrantRead(#alice, :notes)");
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                "return buffer_apply_result(42)[:status] == :ok",
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
    }

    #[test]
    fn marker_rebasing_counts_unicode_scalars_and_validates_deltas() {
        let mut runner = SourceRunner::new_empty();
        assert_eq!(
            result(
                &mut runner,
                r#"
            let insertion = [{:at -> 2, :text -> "é🦀"}]
            let removal = [{:at -> 2, :remove -> 3, :text -> "x"}]
            let rejected = false
            try
              buffer_marker_rebase([{:at -> 5, :remove -> 2}, {:at -> 3, :text -> "x"}], 4, :stick_after)
            catch E_INVARG
              rejected = true
            end
            return [buffer_marker_rebase(insertion, 2, :stick_before),
              buffer_marker_rebase(insertion, 2, :stick_after),
              buffer_marker_rebase(removal, 3, :stick_after),
              buffer_marker_rebase(removal, 5, :stick_after), rejected]
        "#
            ),
            Value::list([
                integer(2).unwrap(),
                integer(4).unwrap(),
                integer(2).unwrap(),
                integer(3).unwrap(),
                Value::bool(true)
            ])
        );
    }

    #[test]
    fn tagged_client_conflicts_do_not_reexecute_the_submission() {
        let kernel = RelationKernel::new();
        let id = Identity::new(10).unwrap();
        let token = NonZeroU64::new(42).unwrap();
        let mut tx = kernel.begin();
        tx.create_buffer(BufferMetadata {
            id,
            name: Symbol::intern("notes"),
            durability: RelationDurability::Durable,
            conflict: BufferConflictPolicy::Span,
        })
        .unwrap();
        tx.replace_buffer(id, 0..0, "base").unwrap();
        tx.commit().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let builtins = BuiltinRegistry::new().with_builtin(
            "race",
            BuiltinResultKind::Dynamic,
            move |context: &mut BuiltinContext<'_, '_>, _: &[Value]| {
                observed.fetch_add(1, Ordering::Relaxed);
                context.tx().apply_buffer(
                    id,
                    1,
                    &[Replacement {
                        range: 0..1,
                        text: "client".to_owned(),
                    }],
                    Some(token),
                )?;
                let mut concurrent = context.kernel().begin();
                concurrent.replace_buffer(id, 0..1, "winner")?;
                concurrent.commit()?;
                Ok(Value::bool(true))
            },
        );
        let program = Program::new(
            1,
            [
                Instruction::BuiltinCall {
                    dst: Register(0),
                    name: Symbol::intern("race"),
                    result_kind: None,
                    args: vec![],
                },
                Instruction::Return {
                    value: Operand::Register(Register(0)),
                },
            ],
        )
        .unwrap();
        let mut task = Task::new_with_builtins(
            1,
            &kernel,
            Arc::new(program),
            Arc::new(ProgramResolver::new()),
            Arc::new(builtins),
            TaskLimits::default(),
        );
        assert!(matches!(
            task.run(),
            Err(TaskError::Runtime(RuntimeError::Kernel(
                KernelError::Buffer {
                    error: BufferError::Conflict { .. },
                    ..
                }
            )))
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            kernel.buffer_apply_result(token).unwrap().outcome,
            BufferApplyOutcome::Conflict
        ));
        assert_eq!(
            kernel.snapshot().buffer(id).unwrap().text().to_text(),
            "winnerase"
        );
    }

    #[test]
    fn buffer_reversion_and_compaction_keep_their_revision_and_authority_contracts() {
        let mut runner = SourceRunner::new_empty();
        result(
            &mut runner,
            "make_buffer(:notes, :durable)\nbuffer_insert(:notes, 0, \"one\")",
        );
        result(&mut runner, "buffer_replace(:notes, 0, 3, \"two\")");
        assert_eq!(
            result(
                &mut runner,
                r#"
            let stale = buffer_revert(:notes, 1, 1)
            let unknown = buffer_revert(:notes, 99, 2)
            let staged = buffer_revert(:notes, 1, 2)
            let blocked = false
            try
              buffer_insert(:notes, 0, "later")
            catch E_STATE
              blocked = true
            end
            return [stale, unknown, staged, buffer_text(:notes), buffer_revision(:notes), blocked]
        "#
            ),
            Value::list([
                Value::symbol(Symbol::intern("stale")),
                Value::symbol(Symbol::intern("unknown")),
                Value::symbol(Symbol::intern("staged")),
                Value::string("one"),
                integer(2).unwrap(),
                Value::bool(true)
            ])
        );
        assert_eq!(
            result(
                &mut runner,
                r#"
            buffer_compact(:notes)
            let blocked = false
            try
              buffer_replace(:notes, 0, 3, "later")
            catch E_STATE
              blocked = true
            end
            return [buffer_revision(:notes), buffer_text(:notes), blocked]
        "#
            ),
            Value::list([integer(3).unwrap(), Value::string("one"), Value::bool(true)])
        );
        assert_eq!(
            result(&mut runner, "return buffer_revision(:notes)"),
            integer(3).unwrap()
        );
        runner
            .run_filein(
                r#"
            make_identity(:alice)
            make_relation(:GrantWrite, 2)
            make_relation(:GrantInvoke, 2)
            assert GrantWrite(#alice, :notes)
        "#,
            )
            .unwrap();
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                r#"
            try
              buffer_revert(:notes, 2, 3)
            catch E_PERMISSION
              return true
            end
            return false
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
        result(&mut runner, "assert GrantInvoke(#alice, :buffer_revert)");
        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                "return buffer_revert(:notes, 2, 3)",
            )
            .unwrap();
        assert!(
            matches!(&report.outcome, TaskOutcome::Complete { value, .. } if *value == Value::symbol(Symbol::intern("staged"))),
            "{}",
            report.render()
        );
        assert_eq!(
            result(
                &mut runner,
                "return [buffer_text(:notes), buffer_revision(:notes)]"
            ),
            Value::list([Value::string("two"), integer(4).unwrap()])
        );
    }
}
