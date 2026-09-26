// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{SourceRunner, TaskOutcome};
use mica_var::{Symbol, Value};

const LIBRARY: &str = include_str!("../../../apps/shared/buffers.mica");
const BUFFER_SCENARIOS: &str = include_str!("../../../apps/buffers/tests/buffer-scenarios.mica");
const MARKER_SCENARIOS: &str = include_str!("../../../apps/buffers/tests/marker-scenarios.mica");

fn filein(runner: &mut SourceRunner, source: &str) {
    let reports = runner.run_filein(source).unwrap_or_else(|error| {
        panic!("{}", runner.render_source_task_error(&error));
    });
    for report in reports {
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { .. }),
            "{}",
            report.render()
        );
    }
}

fn run_scenarios(source: &str, library: Option<&str>) {
    for interpreter_only in [true, false] {
        let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        if let Some(library) = library {
            filein(&mut runner, library);
        }
        filein(&mut runner, source);
        let mut count = 0;
        for line in source.lines() {
            let Some(name) = line
                .strip_prefix("verb ")
                .and_then(|line| line.strip_suffix("()"))
            else {
                continue;
            };
            let invocation = format!("return {name}()");
            let report = runner.run_source(&invocation).unwrap_or_else(|error| {
                panic!(
                    "{name}, interpreter_only={interpreter_only}: {}",
                    runner.render_source_task_error(&error)
                );
            });
            let symbol = |name| Value::symbol(Symbol::intern(name));
            let expected = if name == "test/buffer_revert_reports_status" {
                Value::map([
                    (symbol("stale"), symbol("stale")),
                    (symbol("unknown"), symbol("unknown")),
                    (symbol("noop"), symbol("staged")),
                    (symbol("text"), Value::string("one")),
                    (symbol("revision"), Value::int(4).unwrap()),
                ])
            } else {
                Value::bool(true)
            };
            assert!(
                matches!(&report.outcome, TaskOutcome::Complete { value, .. } if *value == expected),
                "{name}, interpreter_only={interpreter_only}: {}",
                report.render()
            );
            count += 1;
        }
        assert!(count >= 10, "the corpus must execute its scenarios");
    }
}

#[test]
fn pinned_odin_buffer_scenarios() {
    run_scenarios(BUFFER_SCENARIOS, None);
}

#[test]
fn pinned_odin_marker_and_annotation_scenarios() {
    run_scenarios(MARKER_SCENARIOS, Some(LIBRARY));
}
