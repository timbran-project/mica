// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    BuiltinContext, BuiltinRegistry, BuiltinResultKind, RuntimeError, raised_builtin_error,
};
use mica_var::{Tuple, Value, ValueKind};

pub(super) fn install(registry: BuiltinRegistry) -> BuiltinRegistry {
    registry.with_builtin(
        "relation_from_rows",
        BuiltinResultKind::Exact(ValueKind::Relation),
        relation_from_rows,
    )
}

fn relation_from_rows(
    _: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let [heading, rows] = args else {
        return Err(raised_builtin_error(
            "E_INVARG",
            "relation_from_rows expects a heading and a list of rows",
            None,
        ));
    };
    let heading = heading
        .with_list(|columns| {
            columns
                .iter()
                .map(|column| {
                    column.as_symbol().ok_or_else(|| {
                        raised_builtin_error(
                            "E_TYPE",
                            "relation_from_rows heading must contain symbols",
                            None,
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .ok_or_else(|| {
            raised_builtin_error("E_TYPE", "relation_from_rows heading must be a list", None)
        })??;
    let rows = rows
        .with_list(|rows| {
            rows.iter()
                .map(|row| {
                    row.with_list(|cells| Tuple::new(cells.iter().cloned()))
                        .ok_or_else(|| {
                            raised_builtin_error(
                                "E_TYPE",
                                "relation_from_rows rows must be lists",
                                None,
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .ok_or_else(|| {
            raised_builtin_error("E_TYPE", "relation_from_rows rows must be a list", None)
        })??;
    Value::relation(heading, rows).map_err(|error| {
        raised_builtin_error("E_INVARG", format!("relation_from_rows: {error:?}"), None)
    })
}

#[cfg(test)]
mod tests {
    use crate::{SourceRunner, TaskOutcome};
    use mica_var::Value;

    #[test]
    fn relation_constructor_preserves_column_meaning_and_inputs() {
        let mut runner = SourceRunner::new_empty();
        let source = r#"
            let rows = [["é", 2], ["🦀", 1], ["é", 2]]
            let saved = rows
            let actual = relation_from_rows([:z, :a], rows)
            require actual == [:a, :z] {[1, "🦀"], [2, "é"]}
            rows[0] = ["changed", 9]
            require actual == [:a, :z] {[1, "🦀"], [2, "é"]}
            require saved == [["é", 2], ["🦀", 1], ["é", 2]]
            require relation_from_rows([:x], []) == [:x] {}
            require relation_from_rows([], [[]]) == [] {[]}
            return true
        "#;
        let report = runner.run_source(source).unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );
    }

    #[test]
    fn relation_constructor_reports_catchable_shape_errors() {
        let mut runner = SourceRunner::new_empty();
        for (expression, code) in [
            ("relation_from_rows()", "E_INVARG"),
            ("relation_from_rows(0, [])", "E_TYPE"),
            ("relation_from_rows([0], [])", "E_TYPE"),
            ("relation_from_rows([:x], 0)", "E_TYPE"),
            ("relation_from_rows([:x], [0])", "E_TYPE"),
            ("relation_from_rows([:x, :x], [[1, 2]])", "E_INVARG"),
            ("relation_from_rows([:x], [[1, 2]])", "E_INVARG"),
            ("relation_from_rows([:x], [[]])", "E_INVARG"),
        ] {
            let source =
                format!("try\n {expression}\n return false\ncatch {code}\n return true\nend");
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
}
