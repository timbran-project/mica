// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{
    SourceRunner, SourceTaskError, SuspendKind, TaskError, TaskInput, TaskLimits, TaskManagerError,
    TaskOutcome, TaskRequest,
};
use mica_var::{Symbol, Value};
use mica_vm::{AuthorityContext, CapabilityGrant, RuntimeError};

const SOURCES: &[(&str, &str)] = &[
    ("lex.mica", include_str!("../../../apps/compiler/lex.mica")),
    (
        "parse.mica",
        include_str!("../../../apps/compiler/parse.mica"),
    ),
    ("ast.mica", include_str!("../../../apps/compiler/ast.mica")),
    (
        "emit.mica",
        include_str!("../../../apps/compiler/emit.mica"),
    ),
    (
        "install.mica",
        include_str!("../../../apps/compiler/install.mica"),
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
fn mica_frontend_parses_language_forms_and_reports_malformed_input() {
    let sources = [
        "let values = [3, 1, 2]\nreturn [n * 2 for n in values if n > 1 sort -n]",
        "verb choose(value, ?fallback = 0, @rest)\n return match value\n case some(x)\n x\n case _\n fallback\n end\nend",
        "make_relation(:Edge, 2)\nReach(x, y) :- Edge(x, y)\nReach(x, z) :- Reach(x, y), Edge(y, z)",
        "let total = 0\nfor [a, b] in [[1, 2], [3, 4]]\n total = total + a + b\nend\nreturn total",
        "try\n return 1 / 0\ncatch E_DIV as problem\n return problem.message\nfinally\n let done = true\nend",
    ];
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        for source in sources {
            let parsed = invoke(&mut runner, "parse_rows", source);
            let errors = parsed
                .map_get(&Value::symbol(Symbol::intern("errors")))
                .unwrap();
            assert_eq!(errors, Value::list([]), "{source}");
        }
        for source in [
            "let = 7",
            "if true\n 1",
            "return [1,",
            "Reach(x) :-",
            "Reach(x) :- Edge(x),",
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

fn install_emitted(runner: &mut SourceRunner, module: Value) -> Value {
    assert_eq!(
        module.map_get(&Value::symbol(Symbol::intern("ok"))),
        Some(Value::bool(true)),
        "{module}"
    );
    let mut request = SourceRunner::root_source_request("");
    request.input = TaskInput::Invocation {
        selector: Symbol::intern("compiler/install"),
        roles: vec![
            (Symbol::intern("module"), module),
            (
                Symbol::intern("name"),
                Value::symbol(Symbol::intern("compiler_test_entry")),
            ),
        ],
    };
    let submitted = runner.submit_invocation(request).unwrap();
    let TaskOutcome::Complete { value, .. } = submitted.outcome else {
        panic!("{:?}", submitted.outcome);
    };
    assert_eq!(
        value.map_get(&Value::symbol(Symbol::intern("ok"))),
        Some(Value::bool(true)),
        "{value}"
    );
    let methods = value
        .map_get(&Value::symbol(Symbol::intern("methods")))
        .unwrap();
    methods.list_get(methods.list_len().unwrap() - 1).unwrap()
}

fn retire_bootstrap_methods(runner: &mut SourceRunner) {
    let report = runner
        .run_source(
            "for owned in compiler/Method(:compiler_test_entry, ?owned_method)\n\
           for selected in MethodSelector(owned[:owned_method], ?selector)\n\
             for previous in MethodSelector(?candidate, selected[:selector])\n\
               if previous[:candidate] != owned[:owned_method]\n\
                 retract MethodSelector(previous[:candidate], selected[:selector])\n\
               end\n\
             end\n\
           end\n\
         end",
        )
        .unwrap();
    assert!(matches!(report.outcome, TaskOutcome::Complete { .. }));
}

fn assert_emitted_agrees(runner: &mut SourceRunner, source: &str) {
    let expected = runner
        .run_source(source)
        .unwrap_or_else(|error| panic!("{source}: {error:?}"));
    let module = invoke(runner, "emit_source", source);
    assert_eq!(
        module.map_get(&Value::symbol(Symbol::intern("ok"))),
        Some(Value::bool(true)),
        "{source}: {module}"
    );
    install_emitted(runner, module);
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

#[test]
fn mica_emitter_rows_and_comprehensions_agree_with_rust_execution() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        for source in [
            "let exactly {:path/field -> x, :\"é🦀\" -> y} = [:path/field, :\"é🦀\"] {[1, 2]}\nreturn [x, y]",
            "let exactly {b, :a -> first} = [:a, :b] {[2, 3]}\nreturn first + b",
            "let exactly {a} = [:a] {[1]}\na = 3\nreturn a",
            "return begin\n let exactly {a} = [:a] {[7]}\nend",
            "let exactly {} = [] {[]}\nreturn true",
            "let total = 0\nfor {:b -> second, a} in [:a, :b] {[1, 2], [3, 4]}\n total = total + a + second\nend\nreturn total",
            "let total = 0\nfor {a} in [{:a -> 1}, {:a -> 2, :extra -> 3}]\n total = total + a\nend\nreturn total",
            "try\n let exactly {a} = [:a] {}\ncatch E_CARDINALITY as problem\n return [problem.code, problem.message, problem.value]\nend",
            "try\n let exactly {a} = [:a] {[1], [2]}\ncatch E_CARDINALITY\n return true\nend",
            "try\n let exactly {a} = [:a, :extra] {[1, 2]}\ncatch E_CARDINALITY\n return true\nend",
            "try\n let exactly {a} = {:a -> 1}\ncatch E_CARDINALITY\n return true\nend",
            "try\n for {a} in [:a, :extra] {[1, 2]}\n end\ncatch E_MATCH as problem\n return [problem.message, problem.value]\nend",
            "try\n for {a} in [{:wrong -> 1}]\n end\ncatch E_MATCH\n return true\nend",
            "return [x * 2 for x in [3, 1, 2] if x != 1]",
            "return [x for x in [] sort 0]",
            "return [x for x in [] sort]",
            "fn sort(items) => [99]\nreturn [x for x in [3, 1, 2] sort]",
            "let items = [1, 2]\nreturn [(items = [9])[0] + x for x in items]",
            "return [x for x in [3, 1, 2] sort]",
            "return [x for x in [3, 1, 2] sort -x]",
            "return [x for x in [3, 1, 2] sort 0]",
            "return [[key, item] for key: int, item: string in [\"é\", \"🦀\"]]",
            "return [[key, item] for key, item in {:b -> 2, :a -> 1}]",
            "return [ch for ch in \"é🦀\"]",
            "return [x for x in 1..4]",
            "return [a + b for {:b -> b, a} in [:a, :b] {[1, 2], [3, 4]}]",
            "return [a for {a} in [{:a -> 2}, {:a -> 1, :extra -> true}] sort]",
            "return [[a, b, rest] for [a, ?b = a + 1, @rest] in [[1], [2, 3, 4]]]",
            "return [42 for _, _ in [1, 2]]",
            "let total = 0\nfor _, value in [1, 2]\n total = total + value\nend\nreturn total",
            "let x = 10\nlet items = [[x + y for y in [1, 2]] for x in [3, 4]]\nreturn [items, x]",
            "let calls = 0\nlet items = [(calls = calls + 1) for x in [1, 2] if false sort (calls = calls + 10)]\nreturn [items, calls]",
            "let calls = 0\nlet items = [(calls = calls + 1) for x in [1, 2] sort (calls = calls + 10)]\nreturn [items, calls]",
            "let callbacks = [fn() => a for {a} in [{:a -> 1}, {:a -> 2}]]\nreturn [callbacks[0](), callbacks[1]()]",
            "let items = [begin\n if x == 2\n continue\n end\n if x == 4\n break\n end\n x\nend for x in [1, 2, 3, 4, 5]]\nreturn items",
            "let total = 0\nlet items = [try\n if x == 2\n continue\n end\n if x == 4\n break\n end\n x\nfinally\n total = total + x\nend for x in [1, 2, 3, 4, 5]]\nreturn [items, total]",
            "return [x for x in [3, 1, 2]\nif x > 1\nsort\n]",
            "return [x for x in [1, 2, 3] if begin\n if x == 2\n continue\n end\n true\nend]",
            "return [x for x in [1, 2, 3] sort begin\n if x == 2\n break\n end\n x\nend]",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
        for source in [
            "let exactly {a}",
            "let exactly {a, :a -> b} = [:a] {[1]}",
            "const exactly {a} = [:a] {[1]}\na = 3",
            "return [a for {a, :a -> b} in [:a] {[1]}]",
            "return [x for x, y, z in [1]]",
            "return [x for x in [1]]\nreturn x",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{source}: {module}"
            );
        }
    }
}

#[test]
fn mica_emitter_comprehension_captures_and_accumulator_survive_suspension() {
    const SOURCE: &str = "let callbacks = [begin\n let callback = fn() => a\n if a == 2\n suspend()\n end\n callback\nend for {a} in [{:a -> 3}, {:a -> 2}, {:a -> 1}] sort a]\nreturn [f() for f in callbacks]";
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        let module = invoke(&mut runner, "emit_source", SOURCE);
        install_emitted(&mut runner, module);
        for source in [SOURCE, "return :compiler_test_entry()"] {
            let report = runner.run_source(source).unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Suspended { .. }),
                "{}",
                report.render()
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
            assert!(matches!(outcome, TaskOutcome::Complete { value, .. }
                if value == Value::list([Value::int(1).unwrap(), Value::int(2).unwrap(), Value::int(3).unwrap()])));
        }
    }
}

#[test]
fn mica_emitter_compiles_many_locals_into_an_executable_artifact() {
    let mut source = (0..100)
        .map(|index| format!("let value{index} = \"é🦀\"\n"))
        .collect::<String>();
    source.push_str("return [n * 2 for n in [3, 1, 2] if n > 1 sort -n]\n");
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        assert_emitted_agrees(&mut runner, &source);
        let result = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(matches!(result.outcome, TaskOutcome::Complete { value, .. }
            if value == Value::list([Value::int(6).unwrap(), Value::int(4).unwrap()])));
    }
}

#[test]
fn mica_emitter_matches_donor_patterns_with_independent_expected_results() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        for (source, expected) in [
            (
                "return match 2\ncase 1\n0\ncase 2\n42\ncase _\n9\nend",
                "42",
            ),
            ("return match \"é🦀\"\ncase text\ntext\nend", "\"é🦀\""),
            (
                "return match 9\ncase []\n0\ncase {}\n1\ncase some(x)\nx\ncase _\n42\nend",
                "42",
            ),
            (
                "let subject = [1, 2, 3, 4]\nreturn match subject\ncase [1, @middle, 4]\nmiddle\ncase _\n[]\nend",
                "[2, 3]",
            ),
            (
                "let subject = [1, 4]\nreturn match subject\ncase [1, @middle, 4]\nmiddle\ncase _\n[99]\nend",
                "[]",
            ),
            (
                "let subject = [1, 2, 3]\nreturn match subject\ncase [@prefix, last]\n[prefix, last]\ncase _\n[]\nend",
                "[[1, 2], 3]",
            ),
            (
                "let subject = []\nreturn match subject\ncase [@all]\nall\ncase _\n[99]\nend",
                "[]",
            ),
            (
                "let subject = [1, 2, 3, 4, 5]\nreturn match subject\ncase [1, @middle, 4, 5]\nmiddle\ncase _\n[]\nend",
                "[2, 3]",
            ),
            (
                "let subject = [1, 2]\nreturn match subject\ncase [1]\n0\ncase [1, 2, 3]\n1\ncase [first, @rest]\n[first, rest]\nend",
                "[1, [2]]",
            ),
            (
                "return match {:present -> none}\ncase {:absent -> x}\n0\ncase {:present -> none}\n7\ncase _\n9\nend",
                "7",
            ),
            (
                "return match {:items -> [7, 2], :extra -> true}\ncase {:items -> [x, 2]}\nx\ncase _\n0\nend",
                "7",
            ),
            (
                "let subject = [:a, :b] {[1, 2]}\nreturn match subject\ncase {a}\n0\ncase {:b -> y, a}\na + y\ncase _\n9\nend",
                "3",
            ),
            (
                "let calls = 0\nlet value = match begin\ncalls = 1\ncalls\nend\ncase 1 if begin\ncalls = calls * 10 + 2\nfalse\nend\n0\ncase x\nx\nend\nreturn [value, calls]",
                "[1, 12]",
            ),
            (
                "let subject = [7]\nlet callback = match subject\ncase [x]\nfn() => x\ncase _\nfn() => 0\nend\nreturn callback()",
                "7",
            ),
            (
                "let subject = [7]\nreturn match subject\ncase [x] if match x\ncase 7\ntrue\ncase _\nfalse\nend\nx\ncase _\n0\nend",
                "7",
            ),
            (
                "let subject = [1]\ntry\nmatch subject\ncase [2]\n0\nend\ncatch E_MATCH as problem\nreturn problem.value\nend",
                "some([1])",
            ),
            (
                "try\nreturn err(7)\ncatch E_TYPE as problem\nreturn problem.code\nend",
                "E_TYPE",
            ),
            (
                "let total = 0\nfor x in [1, 2, 3]\ntry\nmatch x\ncase 2\ncontinue\ncase n\ntotal = total + n\nend\nfinally\ntotal = total + 10\nend\nend\nreturn total",
                "34",
            ),
        ] {
            let expected = runner.run_source(&format!("return {expected}")).unwrap();
            let module = invoke(&mut runner, "emit_source", source);
            install_emitted(&mut runner, module);
            let actual = runner.run_source("return :compiler_test_entry()").unwrap();
            let TaskOutcome::Complete {
                value: expected, ..
            } = expected.outcome
            else {
                panic!("expected literal did not complete");
            };
            assert!(
                matches!(&actual.outcome, TaskOutcome::Complete { value, .. } if *value == expected),
                "{source}: {}",
                actual.render()
            );
        }
        for source in [
            "return match none\ncase none\n42\ncase _\n0\nend",
            "return match some(7)\ncase some(x) if x == 7\nx\ncase _\n0\nend",
            "return match ok(7)\ncase ok(x)\nx\ncase _\n0\nend",
            "return match err(error(E_TEST))\ncase err(problem)\nproblem.code\ncase _\nE_OTHER\nend",
            "let some = fn(x) => x + 1\nreturn some(7)",
            "let subject = [:a, :b] {[1, 2]}\nreturn match subject\ncase {a, b}\na + b\ncase _\n0\nend",
            "let subject = [:a] {}\nreturn match subject\ncase {a}\na\ncase _\n0\nend",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
        for source in [
            "let subject = []\nreturn match subject\nend",
            "let subject = [1, 2]\nreturn match subject\ncase [x, x]\nx\nend",
            "let subject = []\nreturn match subject\ncase [@x, @y]\nx\nend",
            "return match 1\ncase unknown(x)\nx\nend",
            "let subject = [:a] {[1]}\nreturn match subject\ncase {a, a}\na\nend",
            "match 1\ncase x\nx\nend\nreturn x",
            "return some()",
            "return ok(1, 2)",
            "return some(@[1])",
            "return some(value: 1)",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{source}: {module}"
            );
        }
        let source = "let subject = [7]\nreturn match subject\ncase [x] if begin\nsuspend(0)\ntrue\nend\nx + 1\ncase _\n0\nend";
        let module = invoke(&mut runner, "emit_source", source);
        install_emitted(&mut runner, module);
        let report = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
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
            matches!(outcome, TaskOutcome::Complete { value, .. } if value == Value::int(8).unwrap())
        );
    }
}

#[test]
fn mica_emitter_functional_fields_preserve_visibility_cardinality_and_authority() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_filein(
            "make_functional_relation(:CompilerLabel, 2, [0])\nmake_functional_relation(:compiler/Label, 2, [0])\nmake_relation(:CompilerPlain, 2)\nmake_functional_relation(:CompilerWide, 3, [0])\nmake_functional_relation(:CompilerWrongKey, 2, [1])"
        ).unwrap();
        for source in [
            "(1).compilerLabel = \"é🦀\"\nlet first = (1).compilerLabel\n(1).compilerLabel = \"second\"\nreturn [first, (1).compilerLabel, CompilerLabel(1, ?value)]",
            "(1).compiler/label = [7, 8]\nreturn (1).compiler/label",
            "try\n return (99).compilerLabel\ncatch E_CARDINALITY as problem\n return [problem.code, problem.message, problem.value]\nend",
            "let order = 0\nlet assigned = (order = order * 10 + 2).compilerLabel = (order = order * 10 + 1)\nreturn [assigned, order, (12).compilerLabel]",
            "let value = fn() => (1).compilerLabel\nreturn value()",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
        for source in [
            "return (1).compilerPlain",
            "(1).compilerPlain = 2",
            "return (1).compilerWide",
            "return (1).compilerWrongKey",
            "return (1).undeclaredField",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{source}: {module}"
            );
        }
        for (source, operation) in [
            ("return (1).compilerLabel", "read"),
            ("(1).compilerLabel = 9", "write"),
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            let entry = install_emitted(&mut runner, module);
            let mut request = SourceRunner::root_source_request("");
            request.authority = AuthorityContext::empty();
            request.authority.mint(CapabilityGrant::method(entry));
            request.input = TaskInput::Invocation {
                selector: Symbol::intern("compiler_test_entry"),
                roles: vec![],
            };
            assert!(
                matches!(runner.submit_invocation(request), Err(SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(RuntimeError::PermissionDenied { operation: denied, .. })))) if denied == operation)
            );
        }
    }
}

#[test]
fn mica_emitter_dom_and_structural_literals_agree_with_rust_execution() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_source("make_identity(:compiler_tag)").unwrap();
        for source in [
            "return dom_html(dom <button disabled title=\"é🦀\">Send go</button>)",
            "let label = \"Send & go\"\nlet extra = [dom <span class=\"note\">!</span>]\nreturn dom_html(dom <button id=\"send\">{label}{@extra}</button>)",
            "return dom_html(dom <div> before <br/> after </div>)",
            "return dom_html(dom <span aria-label=\"é\">ok</span>)",
            "let order = 0\nlet node = dom <p data-a={(order = order * 10 + 1)} data-b={(order = order * 10 + 2)}>{to_literal(order = order * 10 + 3)}</p>\nreturn [dom_html(node), order]",
            "return [frob_value(#compiler_tag<[]>), frob_value(#compiler_tag<[7]>), frob_value(#compiler_tag<[7, 8]>)]",
            "let rest = [2, 3]\nreturn frob_value(#compiler_tag<[1, @rest]>)",
            "return [frob_value(#compiler_tag<7>), frob_value(#compiler_tag<\"é🦀\">)]",
            "return frob_value(#compiler_tag<{:label -> \"é🦀\", :number -> 7}>)",
            "let order = 0\nlet value = #compiler_tag<{:a -> (order = order * 10 + 1), :b -> (order = order * 10 + 2)}>\nreturn [frob_value(value), order]",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
    }
}

#[test]
fn mica_emitter_invoke_and_mailbox_receive_use_runtime_instructions() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner
            .run_filein("verb compiler_pair(left, right)\nreturn [left, right]\nend")
            .unwrap();
        for source in [
            "return invoke(:compiler_pair, {:left -> 1, :right -> 2})",
            "let args = [:compiler_pair, {:left -> 1, :right -> 2}]\nreturn invoke(@args)",
            "let order = 0\nlet value = invoke(begin\norder = 1\n:compiler_pair\nend, {:left -> (order = order * 10 + 2), :right -> (order = order * 10 + 3)})\nreturn [value, order]",
            "let invoke = fn(value) => value + 1\nreturn invoke(7)",
            "let mailbox_recv = fn(value) => value + 2\nreturn mailbox_recv(7)",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
        for call in [
            "mailbox_recv([caps[0]])",
            "mailbox_recv([caps[0]], 0.25)",
            "mailbox_recv(@[[caps[0]]])",
            "mailbox_recv(@[[caps[0]], 0.25])",
        ] {
            let source = format!(
                "let saved = [7, 8]\nlet caps = mailbox()\nlet result = {call}\nreturn [saved, result]"
            );
            let module = invoke(&mut runner, "emit_source", &source);
            install_emitted(&mut runner, module);
            for input in [source.as_str(), "return :compiler_test_entry()"] {
                let report = runner.run_source(input).unwrap();
                let TaskOutcome::Suspended {
                    kind: SuspendKind::MailboxRecv(request),
                    ..
                } = report.outcome
                else {
                    panic!("{source}: {}", report.render());
                };
                assert_eq!(request.receivers.len(), 1);
                assert_eq!(request.timeout_millis, call.contains("0.25").then_some(250));
                runner.mailbox_for_receiver(&request.receivers[0]).unwrap();
                let outcome = runner
                    .resume_task(TaskRequest {
                        input: TaskInput::Continuation {
                            task_id: report.task_id,
                            value: Value::string("received"),
                        },
                        ..SourceRunner::root_source_request("")
                    })
                    .unwrap();
                assert!(matches!(outcome, TaskOutcome::Complete { value, .. }
                if value == Value::list([
                    Value::list([Value::int(7).unwrap(), Value::int(8).unwrap()]),
                    Value::string("received"),
                ])));
            }
        }
        for source in [
            "return invoke(:compiler_pair)",
            "return invoke(:compiler_pair, {}, 3)",
            "return mailbox_recv()",
            "return mailbox_recv([], 1, 2)",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{source}: {module}"
            );
        }
        for source in [
            "return invoke(@[])",
            "return invoke(@[:compiler_pair, {}, 3])",
            "return mailbox_recv(@[])",
            "return mailbox_recv(@[[], 1, 2])",
        ] {
            let expected = runner.run_source(source).unwrap();
            let module = invoke(&mut runner, "emit_source", source);
            install_emitted(&mut runner, module);
            let actual = runner.run_source("return :compiler_test_entry()").unwrap();
            let TaskOutcome::Aborted {
                error: expected, ..
            } = expected.outcome
            else {
                panic!("{source}: {}", expected.render());
            };
            assert!(
                matches!(actual.outcome, TaskOutcome::Aborted { error, .. } if error == expected)
            );
        }
    }
}

#[test]
fn mica_emitter_dispatch_preserves_receivers_roles_and_evaluation_order() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner.run_filein("verb compiler_pair(receiver, argument)\n return [receiver, argument]\nend\nverb compiler/pair(receiver, argument)\n return [receiver, argument]\nend").unwrap();
        for source in [
            "return (1):compiler_pair(2)",
            "return (1):compiler_pair(argument: 2)",
            "return (1):compiler/pair(@[2])",
            "return :\"compiler/pair\"(receiver: 1, argument: 2)",
            "return :compiler_pair(receiver: 1, @{:argument -> 2})",
            "return (1):compiler_pair(argument: 2, @{:receiver -> 3})",
            "return :compiler_pair(receiver: 1, argument: 2, @{:argument -> 3})",
            "return :compiler_pair(receiver: 1, receiver: 9, argument: 2)",
            "return (1):compiler_pair(receiver: 9, argument: 2)",
            "let selector = :compiler_pair\nreturn :(selector)(@{:receiver -> 1, :argument -> 2})",
            "let selector = :compiler_pair\nreturn :(selector)(receiver: 1, @{:argument -> 2})",
            "let selector = :compiler_pair\nreturn (1):(selector)(argument: 2, @{})",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
        for call in [
            "(order = order * 10 + 1):(begin\norder = order * 10 + 2\n:compiler_pair\nend)(order = order * 10 + 3)",
            "(order = order * 10 + 1):(begin\norder = order * 10 + 2\n:compiler_pair\nend)(@[order = order * 10 + 3])",
            "(order = order * 10 + 1):(begin\norder = order * 10 + 2\n:compiler_pair\nend)(argument: order = order * 10 + 3, @{})",
        ] {
            assert_emitted_agrees(
                &mut runner,
                &format!("let order = 0\nlet result = {call}\nreturn [result, order]"),
            );
        }
        for source in [
            "return compiler_pair(receiver: 1, @{:argument -> 2})",
            "return :compiler_pair(1, 2)",
            "return (1):compiler_pair(2, argument: 3)",
            "return :compiler_pair(receiver: @[1], argument: 2)",
            "spawn compiler_pair(1, 2)",
            "spawn (fn() => 1)()",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{source}: {module}"
            );
        }
        for source in [
            "return (1):compiler_pair(2)",
            "return (1):compiler_pair(argument: 2, @{})",
            "return invoke(:compiler_pair, {:receiver -> 1, :argument -> 2})",
            "return invoke(@[:compiler_pair, {:receiver -> 1, :argument -> 2}])",
        ] {
            let module = invoke(&mut runner, "emit_source", source);
            let entry = install_emitted(&mut runner, module);
            let mut request = SourceRunner::root_source_request(source);
            request.authority = AuthorityContext::empty();
            request.authority.mint(CapabilityGrant::method(entry));
            let native_denied = runner.submit_source(request.clone());
            request.input = TaskInput::Invocation {
                selector: Symbol::intern("compiler_test_entry"),
                roles: vec![],
            };
            let denied = runner.submit_invocation(request);
            for result in [native_denied, denied] {
                assert!(
                    matches!(
                        &result,
                        Err(SourceTaskError::TaskManager(TaskManagerError::Task(
                            TaskError::Runtime(RuntimeError::NoApplicableMethod { selector })
                        ))) if *selector == Value::symbol(Symbol::intern("compiler_pair"))
                    ),
                    "{source}: {result:?}"
                );
            }
        }
    }
}

#[test]
fn mica_emitter_spawn_preserves_requests_order_and_parent_continuations() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        for target in [
            ":work(7, @[8, 9])",
            ":work(value: 7)",
            ":work(value: 7, value: 8)",
            ":work(value: 7, @{:extra -> 8})",
            "(7):work(@[8])",
            "(7):work(value: 8)",
            "(7):work(value: 8, @{:receiver -> 9})",
            ":(begin\norder = order * 10 + 1\n:work\nend)(order = order * 10 + 2)",
            "(order = order * 10 + 1):(begin\norder = order * 10 + 2\n:work\nend)(@[order = order * 10 + 3])",
            "(order = order * 10 + 1):(begin\norder = order * 10 + 2\n:work\nend)(value: order = order * 10 + 3, @{})",
        ] {
            for delay in [
                "",
                " after (order = order * 10 + 4)",
                " after 0.125 + 0.125",
            ] {
                let source = format!(
                    "let order = 0\nlet child = spawn {target}{delay}\nreturn [child, order]"
                );
                let module = invoke(&mut runner, "emit_source", &source);
                assert_eq!(
                    module.map_get(&Value::symbol(Symbol::intern("ok"))),
                    Some(Value::bool(true)),
                    "{source}: {module}"
                );
                install_emitted(&mut runner, module);
                let mut requests = Vec::new();
                let mut results = Vec::new();
                for input in [source.as_str(), "return :compiler_test_entry()"] {
                    let report = runner
                        .run_source(input)
                        .unwrap_or_else(|error| panic!("{source}: {error:?}"));
                    let TaskOutcome::Suspended {
                        kind: SuspendKind::Spawn(request),
                        ..
                    } = report.outcome
                    else {
                        panic!("{source}: {}", report.render());
                    };
                    requests.push(request);
                    let resumed = runner
                        .resume_task(TaskRequest {
                            input: TaskInput::Continuation {
                                task_id: report.task_id,
                                value: Value::int(42).unwrap(),
                            },
                            ..SourceRunner::root_source_request("")
                        })
                        .unwrap();
                    let TaskOutcome::Complete { value, .. } = resumed else {
                        panic!("{source}: {resumed:?}");
                    };
                    results.push(value);
                }
                assert_eq!(requests[0], requests[1], "{source}");
                assert_eq!(results[0], results[1], "{source}");
            }
        }
    }
}

#[test]
fn mica_emitter_artifacts_agree_with_rust_execution() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
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
            assert_emitted_agrees(&mut runner, source);
        }
    }
}

#[test]
fn mica_emitter_named_recursion_preserves_self_captures_and_argument_binding() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        for source in [
            "fn factorial(n: int)\nif n <= 1\nreturn 1\nend\nreturn n * factorial(n - 1)\nend\nrequire factorial(5) == 120\nreturn factorial(5)",
            "let step = [2]\nfn sum(n)\nif n == 0\nreturn 0\nend\nreturn step[0] + sum(n - 1)\nend\nstep[0] = 9\nrequire sum(3) == 6\nreturn sum(3)",
            "fn identity() => identity\nlet alias = identity\nidentity = fn() => 0\nrequire alias() == alias\nreturn alias() == alias",
            "fn count(n)\nif n == 0\nreturn 0\nend\nreturn 1 + count(n - 1)\nend\nlet saved = count\ncount = fn(n) => 99\nrequire saved(4) == 4\nreturn [saved(4), count(4)]",
            "fn sum(n, ?total = 0, @rest)\nif n == 0\nreturn [total, rest]\nend\nreturn sum(@[n - 1, total + n, @rest])\nend\nrequire sum(3, 10, 7, 8) == [16, [7, 8]]\nreturn sum(3)",
            "fn build(n)\nif n == 0\nreturn fn() => build(2)\nend\nreturn n\nend\nlet escaped = build(0)\nbuild = fn(n) => 99\nrequire escaped() == 2\nreturn escaped()",
            "fn descend(n)\nif n == 0\nraise E_RECURSION_TEST\nend\nreturn descend(n - 1)\nend\ntry\ndescend(3)\ncatch E_RECURSION_TEST as problem\nreturn problem.code\nend",
            "let shadow = fn(n) => 99\nbegin\nfn shadow(n)\nif n == 0\nreturn 0\nend\nreturn 1 + shadow(n - 1)\nend\nrequire shadow(3) == 3\nend\nreturn shadow(3)",
        ] {
            assert_emitted_agrees(&mut runner, source);
        }
    }
}

#[test]
fn mica_emitter_named_recursion_survives_suspension_and_enforces_depth() {
    let source = "let step = 2\nfn climb(n)\nif n == 0\nsuspend(0)\nreturn 0\nend\nreturn step + climb(n - 1)\nend\nreturn climb(4)";
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        let module = invoke(&mut runner, "emit_source", source);
        install_emitted(&mut runner, module);
        for entry in [source, "return :compiler_test_entry()"] {
            let report = runner.run_source(entry).unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Suspended { .. }),
                "{}",
                report.render()
            );
            let resumed = runner
                .resume_task(TaskRequest {
                    input: TaskInput::Continuation {
                        task_id: report.task_id,
                        value: Value::unit(),
                    },
                    ..SourceRunner::root_source_request("")
                })
                .unwrap();
            assert!(
                matches!(&resumed, TaskOutcome::Complete { value, .. } if *value == Value::int(8).unwrap()),
                "{resumed:?}"
            );
        }
        let source = "fn recurse(n) => recurse(n + 1)\nreturn recurse(0)";
        let module = invoke(&mut runner, "emit_source", source);
        install_emitted(&mut runner, module);
        let mut runner = runner.with_task_limits(TaskLimits {
            max_call_depth: 16,
            ..TaskLimits::default()
        });
        for entry in [source, "return :compiler_test_entry()"] {
            assert!(matches!(
                runner.run_source(entry),
                Err(SourceTaskError::TaskManager(TaskManagerError::Task(
                    TaskError::Runtime(RuntimeError::MaxCallDepthExceeded { max_depth: 16 })
                )))
            ));
        }
    }
}

#[test]
fn mica_compiler_installs_overloads_and_replaces_only_owned_methods() {
    const INITIAL: &str = r#"
        verb compiler_pick(value)
          return :fallback
        end
        verb compiler_pick(value @ #integer)
          return value + 10
        end
        verb compiler_pick(value @ #string)
          return string_append(value, "!")
        end
        verb compiler_pick(value @ #compiler_parent)
          return :wrapped
        end
        verb compiler_member(value @ #compiler_parent)
          return :member
        end
        return [compiler_pick(2), :compiler_pick(value: "é"), invoke(:compiler_pick, {:value -> true}), compiler_pick(#compiler_parent<7>), compiler_member(#compiler_child)]
    "#;
    const REPLACEMENT: &str =
        "verb compiler_pick(value)\nreturn :updated\nend\nreturn compiler_pick(2)";
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner
            .run_filein(
                "make_identity(:compiler_parent)\nmake_identity(:compiler_child)\n\
             assert Delegates(#compiler_child, #compiler_parent, 0)\n\
             verb compiler_pick(value @ #float)\nreturn :independent\nend",
            )
            .unwrap();
        let source = format!(
            r#"
            let installed = compiler/install(emit_source({initial:?}), :compiler_installed)
            if !(installed[:ok])
              raise E_INSTALL_TEST, "initial compilation", installed
            end
            if !(invoke(installed[:entry], {{}}) == [12, "é!", :fallback, :wrapped, :member])
              raise E_INSTALL_TEST, "overload selection", installed
            end
            if !(len(MethodSelector(?candidate, :compiler_pick)) == 5)
              raise E_INSTALL_TEST, "overload count", installed
            end
            let fallback = installed[:methods][0]
            let replaced = compiler/install(emit_source({replacement:?}), :compiler_installed)
            if !(replaced[:ok])
              raise E_INSTALL_TEST, "replacement compilation", installed
            end
            if !(replaced[:methods][0] == fallback)
              raise E_INSTALL_TEST, "stable method identity", installed
            end
            if !(:compiler_installed() == :updated)
              raise E_INSTALL_TEST, "replacement execution", installed
            end
            if !(:compiler_pick(value: 1.5) == :independent)
              raise E_INSTALL_TEST, "independent overload preserved", installed
            end
            if !(len(MethodSelector(?candidate, :compiler_pick)) == 2)
              raise E_INSTALL_TEST, "obsolete overloads removed", installed
            end
            if !(len(MethodSelector(?candidate, :compiler_member)) == 0)
              raise E_INSTALL_TEST, "obsolete selector removed", installed
            end
            if !(len(compiler/Method(:compiler_installed, ?owned_method)) == 2)
              raise E_INSTALL_TEST, "module ownership updated", installed
            end
            let generation = compiler/Generation(:compiler_installed, ?number)
            let rejected = compiler/install(emit_source("verb compiler_installed()\nreturn 0\nend"), :compiler_installed)
            if !(!rejected[:ok])
              raise E_INSTALL_TEST, "entry selector collision rejected", installed
            end
            if !(compiler/Generation(:compiler_installed, ?number) == generation)
              raise E_INSTALL_TEST, "rejected installation leaves generation unchanged", installed
            end
            if !(:compiler_installed() == :updated)
              raise E_INSTALL_TEST, "rejected installation leaves entry unchanged", installed
            end
            return true
            "#,
            initial = Value::string(INITIAL),
            replacement = Value::string(REPLACEMENT),
        );
        let report = runner.run_source(&source).unwrap();
        assert!(
            matches!(&report.outcome, TaskOutcome::Complete { value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );
        for invalid in [
            "verb duplicate(x)\nx\nend\nverb duplicate(x)\nx + 1\nend",
            "verb bad(x @ 7)\nx\nend",
            "verb bad(x @ #absent_compiler_prototype)\nx\nend",
            "verb bad(x @ #compiler_parent<7>)\nx\nend",
        ] {
            let module = invoke(&mut runner, "emit_source", invalid);
            assert_eq!(
                module.map_get(&Value::symbol(Symbol::intern("ok"))),
                Some(Value::bool(false)),
                "{invalid}: {module}"
            );
        }
    }
}

#[test]
fn mica_compiler_run_rolls_back_installation_with_failed_entry_effects() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        runner
            .run_filein("make_relation(:CompilerInstallFact, 1)")
            .unwrap();
        let installed = runner
            .run_source(r#"return compiler/run("return 17", :compiler_atomic)"#)
            .unwrap();
        assert!(matches!(
            installed.outcome,
            TaskOutcome::Complete { value, .. }
                if value.map_get(&Value::symbol(Symbol::intern("value"))) == Some(Value::int(17).unwrap())
        ));
        let failed = runner.run_source(
            r#"return compiler/run("assert CompilerInstallFact(7)\nraise E_INSTALL_TEST", :compiler_atomic)"#,
        ).unwrap();
        assert!(
            matches!(failed.outcome, TaskOutcome::Aborted { .. }),
            "{}",
            failed.render()
        );
        let verified = runner
            .run_source(
                "require len(CompilerInstallFact(?item)) == 0\n\
             require compiler/Generation(:compiler_atomic, 0)\n\
             require :compiler_atomic() == 17\n\
             let invalid = compiler/run(\"let = 7\", :compiler_atomic)\n\
             require !invalid[:ok]\n\
             require compiler/Generation(:compiler_atomic, 0)\n\
             return true",
            )
            .unwrap();
        assert!(
            matches!(&verified.outcome, TaskOutcome::Complete { value, .. } if *value == Value::bool(true)),
            "{}",
            verified.render()
        );
    }
}

#[test]
fn mica_compiler_reinstallation_preserves_suspended_programs_and_checks_authority() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
        let module = invoke(
            &mut runner,
            "emit_source",
            "verb compiled_wait(?value = [17], @tail)\nlet saved = fn() => value[0] + len(tail)\nsuspend(0)\nreturn saved()\nend\nreturn compiled_wait()",
        );
        install_emitted(&mut runner, module);
        let suspended = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(matches!(suspended.outcome, TaskOutcome::Suspended { .. }));
        let replacement = invoke(
            &mut runner,
            "emit_source",
            "verb compiled_wait(?value = [99], @tail)\nreturn value[0] + len(tail)\nend\nreturn compiled_wait()",
        );
        install_emitted(&mut runner, replacement);
        let current = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(
            matches!(current.outcome, TaskOutcome::Complete { value, .. } if value == Value::int(99).unwrap())
        );
        let resumed = runner
            .resume_task(TaskRequest {
                input: TaskInput::Continuation {
                    task_id: suspended.task_id,
                    value: Value::unit(),
                },
                ..SourceRunner::root_source_request("")
            })
            .unwrap();
        assert!(
            matches!(&resumed, TaskOutcome::Complete { value, .. } if *value == Value::int(17).unwrap()),
            "{resumed:?}"
        );

        let installer = runner.run_source(
            "let exactly {candidate} = MethodSelector(?candidate, :compiler/install)\nreturn candidate",
        ).unwrap();
        let TaskOutcome::Complete {
            value: installer, ..
        } = installer.outcome
        else {
            panic!("{}", installer.render());
        };
        let module = invoke(&mut runner, "emit_source", "return 0");
        let mut request = SourceRunner::root_source_request("");
        request.authority = AuthorityContext::empty();
        request.authority.mint(CapabilityGrant::method(installer));
        request.input = TaskInput::Invocation {
            selector: Symbol::intern("compiler/install"),
            roles: vec![
                (Symbol::intern("module"), module),
                (
                    Symbol::intern("name"),
                    Value::symbol(Symbol::intern("compiler_denied")),
                ),
            ],
        };
        assert!(matches!(
            runner.submit_invocation(request),
            Err(SourceTaskError::TaskManager(TaskManagerError::Task(
                TaskError::Runtime(RuntimeError::PermissionDenied { .. })
            )))
        ));
        let verified = runner
            .run_source("return len(compiler/Method(:compiler_denied, ?owned_method)) == 0")
            .unwrap();
        assert!(
            matches!(&verified.outcome, TaskOutcome::Complete { value, .. } if *value == Value::bool(true))
        );
    }
}

#[test]
fn mica_emitter_installs_forward_recursive_and_typed_verbs() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
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
            return compiled_entry(5) + :len(input: [])
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
    let source = "let label = \"é🦀\"\r\n// comment\nreturn [label, 12.5]";
    let expected = invoke(&mut runner, "lex", source);
    let module = invoke(&mut runner, "emit_source", SOURCES[0].1);
    install_emitted(&mut runner, module);
    retire_bootstrap_methods(&mut runner);
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
        verb bootstrap_defaults(first, ?extra = 3, @tail)
          return first + extra + len(tail)
        end
        require bootstrap_defaults(1) == 4
        require bootstrap_defaults(1, 2, 3, 4) == 5
        let total = 0
        for [left, right] in [[1, 2], [3, 4]]
          total = total + left * right
        end
        let adjust = fn(base) => fn(value, ?extra = base, @rest) => value + extra + len(rest)
        let finish = adjust(2)
        fn local_sum(n)
          if n == 0
            return 0
          end
          return n + local_sum(n - 1)
        end
        require local_sum(3) == 6
        let exactly {adjustment} = [:adjustment] {[2]}
        let values = [a for {a} in [{:a -> 3}, {:a -> 1}] sort -a]
        (1).compilerBootstrap = values[0]
        let subject = [some((1).compilerBootstrap), 2]
        let matched = match subject
          case [some(value), @tail] if len(tail) == 1
            value
          case _
            0
        end
        return finish(factorial(5) + total + len("é🦀") + adjustment + matched)
    "#;
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only).with_task_limits(TaskLimits {
            instruction_budget: 200_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
        runner
            .run_filein("make_functional_relation(:CompilerBootstrap, 2, [0])")
            .unwrap();
        let expected = invoke(&mut runner, "emit_source", TARGET);
        let source = SOURCES
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>()
            .join("\n");
        let compiler_module = invoke(&mut runner, "emit_source", &source);
        install_emitted(&mut runner, compiler_module);
        retire_bootstrap_methods(&mut runner);
        let actual = invoke(&mut runner, "emit_source", TARGET);
        assert_eq!(actual, expected);
        install_emitted(&mut runner, actual);
        let report = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::int(143).unwrap()),
            "{}",
            report.render()
        );
    }
}

#[test]
fn mica_emitter_queries_and_mutates_catalogue_relations() {
    for interpreter_only in [true, false] {
        let mut runner = compiler(interpreter_only);
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
            let method = install_emitted(&mut runner, module);
            let mut request = SourceRunner::root_source_request("");
            request.authority = AuthorityContext::empty();
            request.authority.mint(CapabilityGrant::method(method));
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
        for source in [
            "return fn(?value) => value",
            "return fn(@first, @second) => first",
            "return fn(?first = 1, second) => second",
            "return fn(@rest: int) => rest",
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

#[test]
fn verb_defaults_and_rest_bind_in_native_and_mica_compilers() {
    const SOURCE: &str = r#"
        verb collect(first, ?extra = {:items -> [2, some(3), ok(:yes), E_TYPE]}, @tail)
          let saved = extra
          let changed = extra
          changed[:items] = [99]
          require saved == extra
          return [first, saved, tail]
        end
        verb restricted(?value @ #string: string = "ok", @tail @ #list: list)
          return [value, tail]
        end
        verb invalid_default(?value @ #string = 7)
          return value
        end
        verb absent(?value)
          return value
        end
        verb literals(?value = [-7, -1.5, "é🦀", b"YQ==", true, #string, none])
          return value
        end
        verb choose(value)
          return :fixed
        end
        verb choose(value, ?extra = 2)
          return :optional
        end
        verb choose(value, @tail)
          return :rest
        end
        verb choose_named(?value = 1)
          return :optional
        end
        verb choose_named(value)
          return :required
        end
        begin
        let expected = {:items -> [2, some(3), ok(:yes), E_TYPE]}
        require collect(1) == [1, expected, []]
        require collect(1) == [1, expected, []]
        require collect(@[1, {:items -> [4]}, 5, 6]) == [1, {:items -> [4]}, [5, 6]]
        require :collect(first: 1, tail: [5, 6]) == [1, expected, [5, 6]]
        require invoke(:collect, {:first -> 1}) == [1, expected, []]
        require restricted() == ["ok", []]
        require restricted("text", 1, 2) == ["text", [1, 2]]
        require :restricted(tail: [1]) == ["ok", [1]]
        require absent() == none
        require :absent() == none
        require literals() == [-7, -1.5, "é🦀", b"YQ==", true, #string, none]
        require choose(1) == :fixed
        require choose(1, 2) == :optional
        require choose(1, 2, 3) == :rest
        require :choose_named(value: 1) == :required
        require :choose_named() == :optional
        return true
        end
    "#;
    for interpreter_only in [true, false] {
        let mut native = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        let report = native
            .run_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );
        let mut runner = compiler(interpreter_only);
        let module = invoke(&mut runner, "emit_source", SOURCE);
        install_emitted(&mut runner, module);
        let report = runner.run_source("return :compiler_test_entry()").unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );
        for source in [
            "return restricted(7)",
            "return invalid_default()",
            "return :invalid_default()",
            "return collect()",
            "return :collect(first: 1, tail: 7)",
            "return literals(1, 2)",
        ] {
            assert!(
                matches!(
                    runner.run_source(source),
                    Err(SourceTaskError::TaskManager(TaskManagerError::Task(
                        TaskError::Runtime(RuntimeError::NoApplicableMethod { .. })
                    )))
                ),
                "{source}"
            );
        }
    }
}

#[test]
fn verb_defaults_reject_expressions_and_invalid_parameter_order() {
    let mut runner = compiler(true);
    for params in [
        "?value = err(E_TYPE)",
        "?value = 1 + 2",
        "?value = len([])",
        "?value = [@[1]]",
        "value = 1",
        "?first = 1, second",
        "@tail, ?last = 1",
        "@tail, last",
        "@first, @second",
        "@tail = []",
    ] {
        let source = format!("verb invalid({params})\n return true\nend");
        let module = invoke(&mut runner, "emit_source", &source);
        assert_eq!(
            module.map_get(&Value::symbol(Symbol::intern("ok"))),
            Some(Value::bool(false)),
            "{source}: {module}"
        );
        let mut native = SourceRunner::new_empty();
        assert!(native.run_source(&source).is_err(), "{source}");
    }
}
