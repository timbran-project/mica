// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{SourceRunner, SuspendKind, TaskInput, TaskOutcome, TaskRequest};
use mica_var::Value;

const FILES: &[(&str, &str)] = &[
    (
        "sync-host",
        include_str!("../../../apps/shared/sync-host.mica"),
    ),
    ("buffers", include_str!("../../../apps/shared/buffers.mica")),
    ("schema", include_str!("../../../apps/editor/schema.mica")),
    ("windows", include_str!("../../../apps/editor/windows.mica")),
    ("buffers", include_str!("../../../apps/editor/buffers.mica")),
    ("keymaps", include_str!("../../../apps/editor/keymaps.mica")),
    ("undo", include_str!("../../../apps/editor/undo.mica")),
    (
        "commands",
        include_str!("../../../apps/editor/commands.mica"),
    ),
    ("session", include_str!("../../../apps/editor/session.mica")),
    ("picker", include_str!("../../../apps/editor/picker.mica")),
    (
        "minibuffer",
        include_str!("../../../apps/editor/minibuffer.mica"),
    ),
    ("files", include_str!("../../../apps/editor/files.mica")),
    ("ui", include_str!("../../../apps/editor/ui.mica")),
    (
        "defaults",
        include_str!("../../../apps/editor/defaults.mica"),
    ),
    ("http", include_str!("../../../apps/editor/http.mica")),
    ("tests", SCENARIOS),
];
const SCENARIOS: &str = include_str!("../../../apps/editor/tests/editor-scenarios.mica");

#[test]
fn pinned_odin_editor_scenarios() {
    for interpreter_only in [true, false] {
        let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        for (name, source) in FILES {
            let reports = runner.run_filein(source).unwrap_or_else(|error| {
                panic!(
                    "{name}: {}",
                    runner.render_source_task_error_with_source(&error, Some(name), source)
                );
            });
            for report in reports {
                assert!(
                    matches!(report.outcome, TaskOutcome::Complete { .. }),
                    "{name}: {}",
                    report.render()
                );
            }
        }
        let report = runner
            .run_source(
                r#"
            let document = editor/document()
            require string_contains(document, "<!doctype html>")
            require string_contains(document, "editor-client.js")
            require string_contains(document, "mica-editor")
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(&report.outcome, TaskOutcome::Complete { value, .. }
            if *value == Value::bool(true)),
            "{}",
            report.render()
        );
        let mut count = 0;
        for line in SCENARIOS.lines() {
            let Some(name) = line
                .strip_prefix("verb ")
                .and_then(|line| line.strip_suffix("()"))
            else {
                continue;
            };
            let mut report = runner
                .run_source(&format!("return {name}()"))
                .unwrap_or_else(|error| {
                    panic!(
                        "{name}, interpreter_only={interpreter_only}: {}",
                        runner.render_source_task_error(&error)
                    );
                });
            let mut commits = 0;
            while matches!(
                report.outcome,
                TaskOutcome::Suspended {
                    kind: SuspendKind::Commit,
                    ..
                }
            ) {
                commits += 1;
                assert!(commits < 100, "{name}: excessive commit continuations");
                let outcome = runner
                    .resume_task(TaskRequest {
                        input: TaskInput::Continuation {
                            task_id: report.task_id,
                            value: Value::unit(),
                        },
                        ..SourceRunner::root_source_request("")
                    })
                    .unwrap();
                report = runner.report_outcome(report.task_id, outcome);
            }
            assert!(
                matches!(&report.outcome, TaskOutcome::Complete { value, .. } if *value == Value::bool(true)),
                "{name}, interpreter_only={interpreter_only}: {}",
                report.render()
            );
            count += 1;
        }
        assert_eq!(count, 46);
    }
}
