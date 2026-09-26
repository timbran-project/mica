// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_relation_kernel::{
    ComputedRelation, ComputedRelationRead, KernelError, RelationId, RelationMetadata,
};
use mica_var::{Symbol, Tuple, Value};
use std::sync::Arc;

pub(crate) fn relations() -> [Arc<dyn ComputedRelation>; 3] {
    [
        Arc::new(BufferProjection::Stat),
        Arc::new(BufferProjection::Line),
        Arc::new(BufferProjection::Markers),
    ]
}

enum BufferProjection {
    Stat,
    Line,
    Markers,
}

impl ComputedRelation for BufferProjection {
    fn uses_read_authority(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Stat => "BufferStat",
            Self::Line => "BufferLine",
            Self::Markers => "BufferMarkers",
        }
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        let arity = match self {
            Self::Stat => 4,
            Self::Line => 7,
            Self::Markers => 6,
        };
        metadata.name().name() == Some(self.name()) && metadata.arity() == arity
    }

    fn required_bound_positions(&self, _: &RelationMetadata) -> &[u16] {
        match self {
            Self::Stat => &[0],
            _ => &[0, 1, 2],
        }
    }

    fn estimate(
        &self,
        _: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        match self {
            Self::Stat => Ok(1),
            Self::Line => nonnegative(metadata.id(), bindings, 2),
            Self::Markers => Ok(32),
        }
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let relation = metadata.id();
        let name = bindings[0]
            .as_ref()
            .and_then(Value::as_symbol)
            .ok_or_else(|| invalid(relation, "buffer name must be a symbol"))?;
        let bounds = match self {
            Self::Stat => None,
            _ => Some((
                nonnegative(relation, bindings, 1)?,
                nonnegative(relation, bindings, 2)?,
            )),
        };
        if matches!(self, Self::Markers) && bounds.is_some_and(|(start, stop)| stop < start) {
            return Err(invalid(relation, "marker window end precedes its start"));
        }
        let Some(view) = reader.buffer_view(name)? else {
            return Ok(Vec::new());
        };
        let name_value = Value::symbol(name);
        match self {
            Self::Stat => Ok(vec![Tuple::from([
                name_value,
                integer(relation, view.text.len())?,
                integer(relation, view.text.line_count())?,
                integer(relation, view.revision)?,
            ])]),
            Self::Line => {
                let (first, count) = bounds.unwrap();
                let end = first.saturating_add(count).min(view.text.line_count());
                let mut rows = Vec::with_capacity(end.saturating_sub(first));
                for line in first..end {
                    let span = view
                        .text
                        .line_span(line)
                        .map_err(|_| invalid(relation, "invalid line span"))?;
                    let text = view
                        .text
                        .slice(span.clone())
                        .map_err(|_| invalid(relation, "invalid line slice"))?;
                    rows.push(Tuple::from([
                        name_value.clone(),
                        bindings[1].clone().unwrap(),
                        bindings[2].clone().unwrap(),
                        integer(relation, line)?,
                        integer(relation, span.start)?,
                        integer(relation, span.end)?,
                        Value::string(text),
                    ]));
                }
                Ok(rows)
            }
            Self::Markers => {
                let (start, stop) = bounds.unwrap();
                let prepare = || marker_points(reader, name, view.revision);
                let points = match reader.preparation_cache() {
                    Some(cache) => cache.get_or_prepare(relation, &name_value, prepare)?,
                    None => Arc::new(prepare()?),
                };
                let first = points.partition_point(|point| point.position < start);
                points[first..]
                    .iter()
                    .take_while(|point| point.position < stop)
                    .map(|point| {
                        let position = integer(relation, point.position)?;
                        Ok(Tuple::from([
                            name_value.clone(),
                            bindings[1].clone().unwrap(),
                            bindings[2].clone().unwrap(),
                            point.id.clone(),
                            position.clone(),
                            position,
                        ]))
                    })
                    .collect()
            }
        }
    }
}

struct MarkerPoint {
    id: Value,
    position: usize,
}

fn marker_points(
    reader: &dyn ComputedRelationRead,
    name: Symbol,
    revision: u64,
) -> Result<Vec<MarkerPoint>, KernelError> {
    let Some(buffer) = reader.relation_id(Symbol::intern("MarkerBuffer"), 2) else {
        return Ok(Vec::new());
    };
    let Some(position) = reader.relation_id(Symbol::intern("MarkerPosition"), 2) else {
        return Ok(Vec::new());
    };
    let Some(anchor) = reader.relation_id(Symbol::intern("MarkerRevision"), 2) else {
        return Ok(Vec::new());
    };
    for relation in [buffer, position, anchor] {
        reader.require_read(relation)?;
    }
    let mut points = Vec::new();
    for marker in reader.scan_relation(buffer, &[None, Some(Value::symbol(name))])? {
        let id = &marker.values()[0];
        let positions = reader.scan_relation(position, &[Some(id.clone()), None])?;
        let revisions = reader.scan_relation(anchor, &[Some(id.clone()), None])?;
        let ([position], [anchored]) = (positions.as_slice(), revisions.as_slice()) else {
            continue;
        };
        let Some(position) = position.values()[1]
            .as_int()
            .and_then(|n| usize::try_from(n).ok())
        else {
            continue;
        };
        if anchored.values()[1]
            .as_int()
            .and_then(|n| u64::try_from(n).ok())
            == Some(revision)
        {
            points.push(MarkerPoint {
                id: id.clone(),
                position,
            });
        }
    }
    points.sort_unstable_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    Ok(points)
}

fn nonnegative(
    relation: RelationId,
    bindings: &[Option<Value>],
    index: usize,
) -> Result<usize, KernelError> {
    bindings[index]
        .as_ref()
        .and_then(Value::as_int)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| invalid(relation, "buffer bounds must be nonnegative integers"))
}

fn integer(relation: RelationId, value: impl TryInto<i64>) -> Result<Value, KernelError> {
    value
        .try_into()
        .ok()
        .and_then(|n| Value::int(n).ok())
        .ok_or_else(|| {
            invalid(
                relation,
                "buffer offset or revision exceeds the integer range",
            )
        })
}

fn invalid(relation: RelationId, message: &str) -> KernelError {
    KernelError::InvalidComputedRelation {
        relation,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceRunner, TaskInput, TaskOutcome, TaskRequest};
    use mica_relation_kernel::{ComputedRelationRead, ReadAuthority, RelationRead};
    use mica_vm::{AuthorityContext, CapabilityGrant, CapabilityOp};

    fn result(runner: &mut SourceRunner, source: &str) -> Value {
        let report = runner.run_source(source).unwrap();
        let TaskOutcome::Complete { value, .. } = &report.outcome else {
            panic!("{source}: {}", report.render());
        };
        value.clone()
    }

    fn fixture() -> SourceRunner {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                r#"
            make_relation(:BufferStat, 4)
            make_relation(:BufferLine, 7)
            make_relation(:BufferMarkers, 6)
            make_functional_relation(:MarkerBuffer, 2, [0])
            make_functional_relation(:MarkerPosition, 2, [0])
            make_functional_relation(:MarkerRevision, 2, [0])
            assert MarkerBuffer(:a, :notes)
            assert MarkerPosition(:a, 2)
            assert MarkerRevision(:a, 1)
            assert MarkerBuffer(:b, :notes)
            assert MarkerPosition(:b, 4)
            assert MarkerRevision(:b, 1)
        "#,
            )
            .unwrap();
        result(
            &mut runner,
            r#"make_buffer(:notes, :durable)
buffer_insert(:notes, 0, "aé🦀\nxy\n")"#,
        );
        runner
    }

    #[test]
    fn computed_buffer_views_observe_unicode_private_edits_and_projected_revisions() {
        let mut runner = fixture();
        assert_eq!(
            result(
                &mut runner,
                r#"
            require BufferStat(:notes, 7, 3, 1)
            require BufferLine(:notes, 0, 2, 0, 0, 3, "aé🦀")
            require BufferLine(:notes, 0, 2, 1, 4, 6, "xy")
            require !BufferLine(:notes, 0, 1, 1, _, _, _)
            require !BufferLine(:notes, 0, 2, 0, 0, 3, "wrong")
            require BufferLine(:notes, 2, 5, 2, 7, 7, "")
            buffer_insert(:notes, 1, "λ")
            require BufferStat(:notes, 8, 3, 2)
            require BufferLine(:notes, 0, 1, 0, 0, 4, "aλé🦀")
            buffer_delete(:notes, 1, 1)
            require BufferStat(:notes, 7, 3, 1)
            make_buffer(:fresh, :volatile)
            require BufferStat(:fresh, 0, 1, 0)
            return buffer_revision(:notes)
        "#
            ),
            Value::int(1).unwrap()
        );
        assert_eq!(
            result(&mut runner, "return BufferStat(:notes, 7, 3, 1)"),
            Value::bool(true)
        );
        assert_eq!(
            result(
                &mut runner,
                "kill_buffer(:notes)\nreturn BufferStat(:notes, _, _, _)"
            ),
            Value::bool(false)
        );
    }

    #[test]
    fn computed_marker_windows_filter_revision_and_invalidate_preparation_on_writes() {
        let mut runner = fixture();
        assert_eq!(
            result(
                &mut runner,
                r#"
            require BufferMarkers(:notes, 2, 4, :a, 2, 2)
            require !BufferMarkers(:notes, 2, 4, :b, _, _)
            require !BufferMarkers(:notes, 2, 4, :a, 3, _)
            retract MarkerPosition(:a, 2)
            assert MarkerPosition(:a, 3)
            require BufferMarkers(:notes, 2, 4, :a, 3, 3)
            buffer_insert(:notes, 0, "λ")
            require !BufferMarkers(:notes, 0, 10, _, _, _)
            retract MarkerRevision(:a, 1)
            assert MarkerRevision(:a, 2)
            require BufferMarkers(:notes, 0, 10, :a, 3, 3)
            require !BufferMarkers(:notes, 0, 10, :b, _, _)
            return true
        "#
            ),
            Value::bool(true)
        );
    }

    #[test]
    fn computed_buffer_authority_cannot_use_root_cached_derived_results_or_marker_grants() {
        let mut runner = fixture();
        runner.run_filein("make_relation(:NoteLine, 2)\nNoteLine(line, text) :- BufferLine(:notes, 0, 10, line, ?start, ?stop, text)").unwrap();
        assert_eq!(
            result(&mut runner, "return NoteLine(0, \"aé🦀\")"),
            Value::bool(true)
        );
        let kernel = runner.task_manager.kernel();
        let snapshot = kernel.snapshot();
        let mut authority = AuthorityContext::empty();
        for name in [
            "BufferStat",
            "BufferLine",
            "BufferMarkers",
            "NoteLine",
            "MarkerBuffer",
            "MarkerPosition",
            "MarkerRevision",
        ] {
            let id = snapshot
                .relation_metadata()
                .find(|m| m.name().name() == Some(name))
                .unwrap()
                .id();
            authority.mint(CapabilityGrant::relation(CapabilityOp::Read, id));
        }
        let notes = snapshot
            .buffer_named(Symbol::intern("notes"))
            .unwrap()
            .metadata()
            .id;
        for source in [
            "return BufferStat(:notes, _, _, _)",
            "return BufferLine(:notes, 0, 1, _, _, _, _)",
            "return BufferMarkers(:notes, 0, 10, _, _, _)",
            "return NoteLine(_, _)",
        ] {
            let report = runner.submit_source(TaskRequest {
                authority: authority.clone(),
                ..SourceRunner::root_source_request(source)
            });
            assert!(
                format!("{report:?}").contains("PermissionDenied"),
                "{source}: {report:?}"
            );
        }
        authority.mint(CapabilityGrant::relation(CapabilityOp::Read, notes));
        let report = runner
            .submit_source(TaskRequest {
                authority,
                ..SourceRunner::root_source_request("return NoteLine(0, \"aé🦀\")")
            })
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
    }

    #[test]
    fn changing_transaction_authority_clears_derived_and_marker_caches() {
        let runner = fixture();
        let kernel = runner.task_manager.kernel();
        let snapshot = kernel.snapshot();
        let relation = snapshot
            .relation_id(Symbol::intern("BufferMarkers"), 6)
            .unwrap();
        let bindings = [
            Some(Value::symbol(Symbol::intern("notes"))),
            Some(Value::int(0).unwrap()),
            Some(Value::int(10).unwrap()),
            None,
            None,
            None,
        ];
        let mut tx = kernel.begin();
        assert_eq!(tx.scan_relation(relation, &bindings).unwrap().len(), 2);
        let mut grants = ReadAuthority::empty();
        grants.grant(tx.buffer_named(Symbol::intern("notes")).unwrap());
        tx.set_read_authority(grants);
        assert!(matches!(
            tx.scan_relation(relation, &bindings),
            Err(KernelError::ReadPermissionDenied(_))
        ));
        tx.set_read_authority(ReadAuthority::All);
        assert_eq!(tx.scan_relation(relation, &bindings).unwrap().len(), 2);
        tx.set_read_authority(ReadAuthority::empty());
        assert!(matches!(
            tx.scan_relation(relation, &bindings),
            Err(KernelError::ReadPermissionDenied(_))
        ));
    }
    #[test]
    fn computed_buffer_rules_observe_private_edits_and_rollback() {
        let mut runner = fixture();
        runner.run_filein("make_relation(:NoteLine, 2)\nNoteLine(line, text) :- BufferLine(:notes, 0, 10, line, ?start, ?stop, text)").unwrap();
        assert_eq!(
            result(&mut runner, "return NoteLine(0, \"aé🦀\")"),
            Value::bool(true)
        );
        let report = runner
            .run_source(
                r#"
            require NoteLine(0, "aé🦀")
            buffer_insert(:notes, 0, "λ")
            require NoteLine(0, "λaé🦀")
            require !NoteLine(0, "aé🦀")
            raise E_ROLLBACK, "discard private edit"
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Aborted { error, .. } if error.error_code_symbol() == Some(Symbol::intern("E_ROLLBACK")))
        );
        assert_eq!(
            result(&mut runner, "return NoteLine(0, \"aé🦀\")"),
            Value::bool(true)
        );
    }

    #[test]
    fn computed_buffer_views_refresh_on_resume_and_respect_revoked_grants() {
        let mut runner = fixture();
        let report = runner.run_source("require BufferMarkers(:notes, 0, 10, :a, 2, 2)\nsuspend()\nreturn BufferMarkers(:notes, 0, 10, :a, 3, 3)").unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
        result(
            &mut runner,
            "retract MarkerPosition(:a, 2)\nassert MarkerPosition(:a, 3)",
        );
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
            matches!(outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
        runner.run_filein("make_identity(:alice)\nmake_relation(:GrantRead, 2)\nassert GrantRead(#alice, :notes)\nassert GrantRead(#alice, :BufferStat)").unwrap();
        let report = runner.run_source_as(Symbol::intern("alice"), "require BufferStat(:notes, 7, 3, 1)\nsuspend()\nreturn BufferStat(:notes, _, _, _)").unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
        result(&mut runner, "retract GrantRead(#alice, :notes)");
        let resumed = runner.resume_as(Symbol::intern("alice"), report.task_id);
        assert!(
            format!("{resumed:?}").contains("PermissionDenied"),
            "{resumed:?}"
        );
    }
}
