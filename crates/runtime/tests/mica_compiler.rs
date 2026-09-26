// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{SourceRunner, TaskInput, TaskLimits, TaskOutcome};
use mica_var::{Symbol, Value};
use std::path::Path;

const SOURCES: &[(&str, &str)] = &[
    ("lex.mica", include_str!("../../../apps/compiler/lex.mica")),
    (
        "parse.mica",
        include_str!("../../../apps/compiler/parse.mica"),
    ),
];

fn compiler(interpreter_only: bool) -> SourceRunner {
    let mut runner = SourceRunner::new_empty()
        .with_interpreter_only(interpreter_only)
        .with_task_limits(TaskLimits {
            instruction_budget: 20_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
    for (name, source) in SOURCES {
        for report in runner.run_filein(source).unwrap_or_else(|error| {
            panic!(
                "{name}: {}",
                runner.render_source_task_error_with_source(&error, Some(name), source)
            )
        }) {
            assert!(
                matches!(report.outcome, TaskOutcome::Complete { .. }),
                "{}",
                report.render()
            );
        }
    }
    runner
}

fn invoke(runner: &mut SourceRunner, selector: &str, source: &str) -> Value {
    let mut request = SourceRunner::root_source_request("");
    request.input = TaskInput::Invocation {
        selector: Symbol::intern(selector),
        roles: vec![(Symbol::intern("source"), Value::string(source))],
    };
    let submitted = runner.submit_invocation(request).unwrap();
    let TaskOutcome::Complete { value, .. } = submitted.outcome else {
        panic!("{selector}: {:?}", submitted.outcome);
    };
    value
}

#[test]
fn mica_frontend_parses_the_shared_corpus_and_reports_malformed_input() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/parity");
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../benchmarks/parity/corpus.json")).unwrap();
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        for fixture in corpus["fixtures"].as_array().unwrap() {
            let file = fixture["file"].as_str().unwrap();
            let source = std::fs::read_to_string(root.join(file)).unwrap();
            let parsed = invoke(&mut runner, "parse_rows", &source);
            let errors = parsed
                .map_get(&Value::symbol(Symbol::intern("errors")))
                .unwrap();
            assert_eq!(errors, Value::list([]), "{file}");
        }
        for source in [
            "let = 7",
            "if true\n 1",
            "return [1,",
            "return \"unterminated",
        ] {
            let parsed = invoke(&mut runner, "parse_rows", source);
            assert!(
                parsed
                    .map_get(&Value::symbol(Symbol::intern("errors")))
                    .unwrap()
                    .list_len()
                    .unwrap()
                    > 0,
                "{source}"
            );
        }
    }
}

#[test]
fn mica_frontend_lexes_unicode_and_parses_a_program() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        let source = r#"
            let [tokens, errors] = lex("let label = \"é🦀\"\nreturn label")
            require errors == []
            require tokens[0][:kind] == :Let
            require tokens[3][:kind] == :String
            require tokens[3][:offset] == 12
            require tokens[5][:line] == 2
            let parsed = parse("let total = 2 + 3\nreturn total")
            require parsed[:errors] == []
            require len(parsed[:nodes]) > 0
            return true
        "#;
        let report = runner
            .run_source(source)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );
    }
}
