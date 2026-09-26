// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{
    SourceRunner, SourceTaskError, TaskError, TaskInput, TaskLimits, TaskManagerError, TaskOutcome,
};
use mica_var::{Symbol, Value};
use mica_vm::{AuthorityContext, CapabilityGrant, RuntimeError};
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
            "let make_adder = fn(base) => fn(value) => base + value\nlet add10 = make_adder(10)\nreturn add10(32)",
            "let rate = [2]\nlet charge = fn(quantity) => quantity * rate[0]\nrate[0] = 3\nreturn [charge(10), rate]",
            "fn increment(value) => value + 1\nconst saved = increment\nincrement = fn(value) => value + 10\nreturn [saved(3), increment(3)]",
            "let add = fn(value, ?extra = value + 2, @rest) => [value, extra, rest]\nreturn [add(1), add(@[1, 4, 5, 6])]",
            "let base = 9\nlet f = fn(base) => base + 1\nreturn f(2)",
            "let base = 10\nlet f = fn(value, ?extra = base) => value + extra\nbase = 20\nreturn f(1)",
            "let f = fn(value: int, ?extra: int = value + 2, @rest: list) -> list => [value, extra, rest]\nreturn [f(1), f(2, 3, 4)]",
            "let f = fn(value: int) -> int => value * 2\nreturn f(4)",
            "let f = fn(value: int) => value * 2\nlet alias = f\ntry\n return alias(\"bad\")\ncatch E_TYPE\n return true\nend",
            "let uninitialized\nreturn uninitialized",
            "return begin\n let [first, last] = [3, 7]\nend",
            "return begin\n let [first, _] = [3, 7]\nend",
            "let sum = 0\nfor item: int in [1, 2]\n sum = sum + item\nend\nreturn sum",
            "let a = 1\nreturn a + (a = 2)",
            "let a = 1\nreturn [a, a = 2, a]",
            "let a = [1, 2]\nlet i = 0\na[i = 1] = (i = 0)\nreturn [a, i]",
            "let a = [1, 2]\nlet b = a\na[0] = 9\nreturn [a, b]",
            "let [a, b] = [3, 7]\nreturn a * b",
            "let a = [1, 2]\nreturn a[0] = 9",
            "let a = [1, 2, 3]\nreturn [a[1.._], a[0..2]]",
            "let [first, ?second = first + 1, @rest] = [4]\nreturn [first, second, rest]",
            "let [first, ?second = 1 / 0, @rest] = [4, 5, 6, 7]\nreturn [first, second, rest]",
            "let [?first, ?second = 2, @rest]\nreturn [first, second, rest]",
            "let total = []\nfor [head, ?extra = head + 1, @tail] in [[1], [2, 3, 4]]\n total = [@total, [head, extra, tail]]\nend\nreturn total",
            "let m = {:a -> 2, :b -> 3}\nm[:a] = 5\nreturn m[:a] + m[:b]",
            "let total = 0\nlet i = 0\nwhile i < 10\n i = i + 1\n if i == 3\n continue\n end\n if i == 8\n break\n end\n total = total + i\nend\nreturn total",
            "let total = 0\nfor item in [1, 2, 3]\n total = total + item\nend\nreturn total",
            "let total = 0\nfor key, item in [4, 5]\n total = total + key + item\nend\nreturn total",
            "return if false\n 2\nelseif true\n 3\nelse\n 4\nend",
            "return [false && (1 / 0), true || (1 / 0), true && 7, false || 8]",
            "return string_append(\"é\", \"🦀\")",
            "return try\n 1 / 0\ncatch E_DIV\n 42\nend",
            "try\n raise E_TEST, \"é🦀\", 42\ncatch E_TEST as problem\n return [problem.code, problem.message, problem.value]\nend",
            "let changed = 0\ntry\n raise E_TEST\ncatch E_OTHER\n changed = 99\ncatch\n changed = 2\nfinally\n changed = changed + 3\nend\nreturn changed",
            "let changed = 0\nfor item in [1, 2, 3]\n try\n if item == 2\n break\n end\n finally\n changed = changed + item\n end\nend\nreturn changed",
            "try\n try\n return 1\n finally\n raise E_TEST\n end\ncatch E_TEST\n return 9\nend",
            "let changed = 0\nfor item in [1, 2, 3]\n try\n continue\n finally\n changed = changed + item\n end\nend\nreturn changed",
            "try\n try\n raise E_TEST, \"payload\", 17\n catch as inner\n raise inner\n end\ncatch E_TEST as outer\n return outer.value\nend",
            "return [:a, :b] {[2, 1], [1, 3]}",
        ] {
            let expected = runner
                .run_source(source)
                .unwrap_or_else(|error| panic!("{source}: {error:?}"));
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
        let adjust = fn(base) => fn(value, ?extra = base, @rest) => value + extra + len(rest)
        let finish = adjust(2)
        return finish(factorial(5) + total + len("é🦀"))
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
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::int(138).unwrap()),
            "{}",
            report.render()
        );
    }
}

#[test]
fn mica_emitter_queries_and_mutates_catalogue_relations() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_filein(INSTALL_EMITTED).unwrap();
        runner
            .run_filein("make_relation(:CompilerData, 2)\nassert CompilerData(1, 2)\nassert CompilerData(2, 2)\nassert CompilerData(3, 4)")
            .unwrap();
        for source in [
            "return CompilerData(?left, ?right)",
            "return CompilerData(?same, ?same)",
            "return [CompilerData(1, 2), CompilerData(1, 3), CompilerData(_, 4)]",
            "return CompilerData(@[1], ?right)",
            "assert CompilerData(8, 9)\nlet present = CompilerData(8, 9)\nretract CompilerData(8, _)\nreturn [present, CompilerData(8, 9)]",
            "assert CompilerData(@[8, 9])\nlet rows = CompilerData(8, ?right)\nretract CompilerData(@[8], _)\nreturn rows",
            "let names = []\nfor row in RelationName(?relation, :CompilerData)\n names = [@names, row[:relation]]\nend\nreturn names",
        ] {
            let expected = runner
                .run_source(source)
                .unwrap_or_else(|error| panic!("{source}: {error:?}"));
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
        for (source, operation) in [
            ("return CompilerData(?left, ?right)", "read"),
            ("assert CompilerData(8, 9)", "write"),
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            install_emitted(&mut runner, module);
            let method = runner
                .named_identity(Symbol::intern("compiler-test/method/:compiler_test_entry"))
                .unwrap();
            let mut request = SourceRunner::root_source_request("");
            request.authority = AuthorityContext::empty();
            request
                .authority
                .mint(CapabilityGrant::method(Value::identity(method)));
            request.input = TaskInput::Invocation {
                selector: Symbol::intern("compiler_test_entry"),
                roles: vec![],
            };
            assert!(matches!(
                runner.submit_invocation(request),
                Err(SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(
                    RuntimeError::PermissionDenied { operation: denied, .. }
                )))) if denied == operation
            ));
            let report = runner.run_source("return CompilerData(8, 9)").unwrap();
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. } if value == Value::bool(false)
            ));
        }
    }
}

#[test]
fn mica_emitter_rejects_invalid_function_parameters_and_preserves_arity_errors() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_filein(INSTALL_EMITTED).unwrap();
        for source in [
            "return fn(?value) => value",
            "return fn(@first, @second) => first",
            "return fn(?first = 1, second) => second",
            "return fn(@rest: int) => rest",
            "fn recur(value) => recur(value - 1)\nreturn recur(2)",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{source}"
            );
            assert!(
                module
                    .map_get(&Value::symbol(Symbol::intern("errors")))
                    .unwrap()
                    .list_len()
                    .unwrap()
                    > 0
            );
        }
        for (call, actual) in [("pick(@[])", 0), ("pick(@[1, 2, 3])", 3)] {
            let source =
                format!("let pick = fn(first, ?second = first) => [first, second]\nreturn {call}");
            let module = invoke(&mut runner, "emit_source", &source);
            install_emitted(&mut runner, module);
            assert!(
                matches!(
                    runner.run_source("return :compiler_test_entry()"),
                    Err(SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(
                        RuntimeError::InvalidCallArity { expected_min: 1, expected_max: 2, actual: count }
                    )))) if count == actual
                ),
                "{call}"
            );
        }
    }
}
