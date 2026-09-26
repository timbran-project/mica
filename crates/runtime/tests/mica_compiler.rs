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
    (
        "emit.mica",
        include_str!("../../../apps/compiler/emit.mica"),
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

const INSTALL_EMITTED: &str = r#"
make_relation(:CompilerTestGeneration, 1)
verb compiler_test_install(source)
  let generation = len(CompilerTestGeneration(?existing))
  assert CompilerTestGeneration(generation)
  let definitions = [@source[:methods], {:selector -> :compiler_test_entry, :parameters -> [], :program -> source[:entry]}]
  for definition in definitions
    let selector = definition[:selector]
    let candidates = MethodSelector(?candidate, selector)
    let target = none
    if len(candidates) == 0
      target = make_identity(to_symbol(string_concat("compiler-test/method/", to_literal(selector))))
      assert MethodSelector(target, selector)
    else
      let exactly {:candidate -> existing} = candidates
      target = existing
    end
    let artifact = make_identity(to_symbol(string_concat("compiler-test/program/", to_literal(generation), "/", to_literal(selector))))
    assert ProgramBytes(artifact, definition[:program])
    retract MethodProgram(target, _)
    assert MethodProgram(target, artifact)
    retract Param(target, _, _, _)
    let position = 0
    for role in definition[:parameters]
      assert Param(target, role, :dispatch/unrestricted, position)
      position = position + 1
    end
  end
  return true
end
"#;

fn install_emitted(runner: &mut SourceRunner, module: Value) {
    assert_eq!(
        module.map_get(&Value::symbol(Symbol::intern("ok"))),
        Some(Value::bool(true)),
        "{module}"
    );
    let mut request = SourceRunner::root_source_request("");
    request.input = TaskInput::Invocation {
        selector: Symbol::intern("compiler_test_install"),
        roles: vec![(Symbol::intern("source"), module)],
    };
    let submitted = runner.submit_invocation(request).unwrap();
    assert!(
        matches!(submitted.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
    );
}

#[test]
fn mica_emitter_artifacts_agree_with_rust_execution() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_filein(INSTALL_EMITTED).unwrap();
        for source in [
            "return 2 + 3 * 4",
            "let uninitialized\nreturn uninitialized",
            "return begin\n let [first, last] = [3, 7]\nend",
            "return begin\n let [first, _] = [3, 7]\nend",
            "let sum = 0\nfor item: int in [1, 2]\n sum = sum + item\nend\nreturn sum",
            "let a = 1\nreturn a + (a = 2)",
            "let a = 1\nreturn [a, a = 2, a]",
            "let a = [1, 2]\nlet i = 0\na[i = 1] = (i = 0)\nreturn [a, i]",
            "let a = [1, 2]\nlet b = a\na[0] = 9\nreturn [a, b]",
            "let [a, b] = [3, 7]\nreturn a * b",
            "let m = {:a -> 2, :b -> 3}\nm[:a] = 5\nreturn m[:a] + m[:b]",
            "let total = 0\nlet i = 0\nwhile i < 10\n i = i + 1\n if i == 3\n continue\n end\n if i == 8\n break\n end\n total = total + i\nend\nreturn total",
            "let total = 0\nfor item in [1, 2, 3]\n total = total + item\nend\nreturn total",
            "let total = 0\nfor key, item in [4, 5]\n total = total + key + item\nend\nreturn total",
            "return if false\n 2\nelseif true\n 3\nelse\n 4\nend",
            "return [false && (1 / 0), true || (1 / 0), true && 7, false || 8]",
            "return string_append(\"é\", \"🦀\")",
            "return [:a, :b] {[2, 1], [1, 3]}",
        ] {
            let expected = runner.run_source(source).unwrap();
            let module = invoke(&mut runner, "emit_source", source);
            install_emitted(&mut runner, module);
            let actual = runner.run_source("return :compiler_test_entry()").unwrap();
            let TaskOutcome::Complete {
                value: expected, ..
            } = expected.outcome
            else {
                panic!("{source}: {}", expected.render());
            };
            let TaskOutcome::Complete { value: actual, .. } = actual.outcome else {
                panic!("{source}: {}", actual.render());
            };
            assert_eq!(actual, expected, "{source}");
        }
    }
}

#[test]
fn mica_emitter_installs_forward_recursive_and_typed_verbs() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_filein(INSTALL_EMITTED).unwrap();
        let source = r#"
            verb len(input)
              return 999
            end
            verb compiled_entry(n)
              return factorial(n) + typed_double(n)
            end
            verb factorial(n)
              if n <= 1
                return 1
              end
              return n * factorial(n - 1)
            end
            verb typed_double(n: int) -> int
              return n * 2
            end
            verb typed_loop(items)
              let total = 0
              for item: int in items
                total = total + item
              end
              return total
            end
            verb typed_assign(input)
              let number: int = 1
              number = input
              return number
            end
            return compiled_entry(5) + :len([])
        "#;
        let module = invoke(&mut runner, "emit_source", source);
        install_emitted(&mut runner, module);
        let report = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::int(1129).unwrap()),
            "{}",
            report.render()
        );
        for source in [
            "try\n return typed_double(\"wrong\")\ncatch E_TYPE\n return true\nend",
            "try\n return typed_assign(\"wrong\")\ncatch E_TYPE\n return true\nend",
            "try\n return typed_loop([1, \"wrong\"])\ncatch E_TYPE\n return true\nend",
        ] {
            let report = runner.run_source(source).unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
                "{}",
                report.render()
            );
        }
    }
}

#[test]
fn mica_emitter_bootstraps_its_lexer() {
    let mut runner = compiler(true).with_task_limits(TaskLimits {
        instruction_budget: 200_000_000,
        max_call_depth: 256,
        ..TaskLimits::default()
    });
    runner.run_filein(INSTALL_EMITTED).unwrap();
    let source = "let label = \"é🦀\"\r\n// comment\nreturn [label, 12.5]";
    let expected = invoke(&mut runner, "lex", source);
    let module = invoke(&mut runner, "emit_source", SOURCES[0].1);
    install_emitted(&mut runner, module);
    assert_eq!(invoke(&mut runner, "lex", source), expected);
}

#[test]
fn mica_compiler_bootstrap_preserves_artifacts_and_execution() {
    const TARGET: &str = r#"
        verb factorial(n)
          if n <= 1
            return 1
          end
          return n * factorial(n - 1)
        end
        let total = 0
        for [left, right] in [[1, 2], [3, 4]]
          total = total + left * right
        end
        return factorial(5) + total + len("é🦀")
    "#;
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only).with_task_limits(TaskLimits {
            instruction_budget: 200_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
        runner.run_filein(INSTALL_EMITTED).unwrap();
        let expected = invoke(&mut runner, "emit_source", TARGET);
        let source = SOURCES
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>()
            .join("\n");
        let compiler_module = invoke(&mut runner, "emit_source", &source);
        install_emitted(&mut runner, compiler_module);
        let actual = invoke(&mut runner, "emit_source", TARGET);
        assert_eq!(actual, expected);
        install_emitted(&mut runner, actual);
        let report = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::int(136).unwrap()),
            "{}",
            report.render()
        );
    }
}
