// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mica_runtime::{SourceRunner, TaskInput, TaskLimits, TaskOutcome};
use mica_var::{Symbol, Value, ValueKind};

const SOURCES: &[&str] = &[
    include_str!("../../../apps/native/ir.mica"),
    include_str!("../../../apps/native/builders.mica"),
    include_str!("../../../apps/native/generation.mica"),
    include_str!("../../../apps/native/sequences.mica"),
    include_str!("../../../apps/native/types.mica"),
    include_str!("../../../apps/native/numeric.mica"),
    include_str!("../../../apps/native/layout.mica"),
    include_str!("../../../apps/native/storage.mica"),
    include_str!("../../../apps/native/contracts.mica"),
    include_str!("../../../apps/native/execution.mica"),
    include_str!("../../../apps/native/check.mica"),
    include_str!("../../../apps/native/flow.mica"),
    include_str!("../../../apps/native/c_syntax.mica"),
    include_str!("../../../apps/native/c_scopes.mica"),
    include_str!("../../../apps/native/c_flow.mica"),
    include_str!("../../../apps/native/c.mica"),
    include_str!("../../../apps/native/examples/scalars.mica"),
    include_str!("../../../apps/native/examples/values.mica"),
];

fn runner(interpreter_only: bool) -> SourceRunner {
    let mut runner = SourceRunner::new_empty()
        .with_interpreter_only(interpreter_only)
        .with_task_limits(TaskLimits {
            instruction_budget: 100_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
    for source in SOURCES {
        for report in runner.run_filein(source).unwrap_or_else(|error| {
            panic!("{}", runner.render_source_task_error(&error));
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

fn callable_runner(interpreter_only: bool) -> SourceRunner {
    let mut runner = runner(interpreter_only);
    runner
        .run_filein(include_str!("../../../apps/native/tests/callables.mica"))
        .unwrap_or_else(|error| panic!("{}", runner.render_source_task_error(&error)));
    runner
}

#[test]
fn typed_callables_execute_and_preserve_aggregate_layout() {
    let mut previous = None;
    for interpreter_only in [true, false] {
        let mut runner = callable_runner(interpreter_only);
        let generated = eval(
            &mut runner,
            r#"
let state = native/callable_example()
let expected = native/emit_c(state)
for key, rows in state
  if is_kind(rows, :list)
    let reversed = []
    for row in rows
      reversed = [row, @reversed]
    end
    state[key] = reversed
  end
end
native/ensure(native/emit_c(state) == expected, "callable emission depends on table order")
return expected
"#,
        )
        .with_str(str::to_owned)
        .unwrap();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        assert!(generated.contains("typedef uint64_t (*mica_function_type_Unary)"));
        assert!(
            generated.find("uint64_t mica_z_increment(").unwrap()
                < generated
                    .find("mica_function_type_Unary mica_a_factory(")
                    .unwrap()
        );
        let scratch = Scratch::new();
        fs::write(
            scratch.0.join("callables.c"),
            format!(
                "{generated}\n{}",
                include_str!("../../../apps/native/tests/callables.c")
            ),
        )
        .unwrap();
        let mut compilers = vec![std::env::var("CC").unwrap_or_else(|_| "cc".to_owned())];
        if compilers[0] != "clang"
            && Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        {
            compilers.push("clang".to_owned());
        }
        for (index, compiler) in compilers.iter().enumerate() {
            let binary = scratch.0.join(format!("callables-{index}"));
            let output = Command::new(compiler)
                .current_dir(&scratch.0)
                .args([
                    "-std=c11",
                    "-O2",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-pedantic",
                    "-fsanitize=address,undefined",
                    "-fno-omit-frame-pointer",
                    "callables.c",
                    "-o",
                ])
                .arg(&binary)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{compiler}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = Command::new(&binary).output().unwrap();
            assert!(output.status.success(), "{compiler}: {output:?}");
            assert!(output.stderr.is_empty(), "{compiler}: {output:?}");
            let output = Command::new(&binary).arg("null").output().unwrap();
            #[cfg(unix)]
            assert_eq!(output.status.signal(), Some(6), "{compiler}: {output:?}");
            assert!(!output.status.success(), "{compiler}: {output:?}");
        }
        previous = Some(generated);
    }
}

#[test]
fn typed_callables_check_effects_types_and_borrow_regions() {
    let cases = [
        (
            r#"state[:function_types] = [@state[:function_types], state[:function_types][0]]"#,
            "duplicate function pointer type",
        ),
        (
            r#"state = native/add_local(state, state[:functions][0][0], "unknown", [:FunctionPointer, "Missing"])[1]"#,
            "unknown function pointer type",
        ),
        (
            r#"state = native/function_pointer_type(state, "Cycle", [:FunctionPointer, "Cycle"], [], [], :Value, "")[1]"#,
            "function pointer cycle",
        ),
        (
            r#"let record = [:Record, "Cycle"]
state = native/record_type(state, "Cycle", [["child", record]])[1]
state = native/function_pointer_type(state, "Cycle", record, [], [], :Value, "")[1]"#,
            "record contains a by-value cycle",
        ),
        (
            r#"state = native/function_pointer_type(state, "MissingOwner", [:Pointer, :U8, :Mutable], [], [], :Borrow, "missing")[1]"#,
            "function pointer result requires borrowed owner",
        ),
        (
            r#"let pointer = [:Pointer, :U8, :Mutable]
state = native/function_pointer_type(state, "CycleRegion", :Void, [["x", pointer, [:Borrow, "y"]], ["y", pointer, [:Borrow, "x"]]], [], :Value, "")[1]"#,
            "function pointer borrow region requires root Borrow parameter",
        ),
        (
            r#"let rows = []
for row in state[:function_types]
  if row[0] == "Reader"
    row[3] = []
  end
  rows = [@rows, row]
end
state[:function_types] = rows"#,
            "function address effect exceeds callable contract",
        ),
        (
            r#"let target = [row[0] for row in state[:functions] if row[1] == "memory_dispatch"][0]
let rows = []
for row in state[:contracts]
  if row[0] == target
    row[1] = []
  end
  rows = [@rows, row]
end
state[:contracts] = rows"#,
            "exceeds contract of memory_dispatch",
        ),
        (
            r#"let rows = []
for row in state[:function_types]
  if row[0] == "Unary"
    row[1] = :U8
  end
  rows = [@rows, row]
end
state[:function_types] = rows"#,
            "function address signature mismatch",
        ),
        (
            r#"let context = native/module_context(state, {})
context = native/declare_function(context, "bad", :U64, [["f", [:FunctionPointer, "Unary"], :Value]], [], [], :Value, "")
context = native/define_function(context, "bad", [native/return_value(native/indirect_call_expression("f", []))])
state = context[:state]"#,
            "indirect call arity mismatch",
        ),
        (
            r#"let context = native/module_context(state, {})
context = native/declare_function(context, "bad", :U64, [["f", [:FunctionPointer, "Unary"], :Value], ["x", :U8, :Value]], [], [], :Value, "")
context = native/define_function(context, "bad", [native/return_value(native/indirect_call_expression("f", ["x"]))])
state = context[:state]"#,
            "indirect call argument type mismatch",
        ),
        (
            r#"let pointer = [:Pointer, :U64, :Mutable]
let context = native/module_context(state, {})
context = native/declare_function(context, "bad", pointer, [["f", [:FunctionPointer, "Borrower"], :Value], ["x", pointer, :Borrow], ["y", pointer, :Borrow]], [], [], :Borrow, "y")
context = native/define_function(context, "bad", [native/return_value(native/indirect_call_expression("f", ["x"]))])
state = context[:state]"#,
            "result ownership mismatch",
        ),
        (
            r#"let pointer = [:Pointer, :U64, :Mutable]
let [callback, next] = native/function_pointer_type(state, "Destroy", :Void, [["x", pointer, :Consume]], [:Release], :Value, "")
let context = native/module_context(next, {})
context = native/declare_function(context, "bad", :Void, [["f", callback, :Value], ["x", pointer, :Borrow]], [], [:Release], :Value, "")
context = native/define_function(context, "bad", [native/do_statement(native/indirect_call_expression("f", ["x"])), native/return_void()])
state = context[:state]"#,
            "cannot consume borrowed storage in indirect call",
        ),
        (
            r#"let pointer = [:Pointer, :U64, :Mutable]
let [callback, next] = native/function_pointer_type(state, "Region", :Void, [["owner", pointer, :Borrow], ["member", pointer, [:Borrow, "owner"]]], [], :Value, "")
let context = native/module_context(next, {})
context = native/declare_function(context, "bad", :Void, [["f", callback, :Value], ["x", pointer, :Borrow], ["y", pointer, :Borrow]], [], [], :Value, "")
context = native/define_function(context, "bad", [native/do_statement(native/indirect_call_expression("f", ["x", "y"])), native/return_void()])
state = context[:state]"#,
            "indirect argument does not share borrow region",
        ),
        (
            r#"let context = native/module_context(state, {})
context = native/declare_function(context, "bad", [:FunctionPointer, "Unary"], [["x", [:Pointer, :U8, :Mutable], :Borrow]], [], [], :Value, "")
context = native/define_function(context, "bad", [native/return_value(native/unary_expression(:PointerCast, "x"))])
state = context[:state]"#,
            "PointerCast requires pointer result",
        ),
    ];
    for interpreter_only in [true, false] {
        let mut runner = callable_runner(interpreter_only);
        for (change, expected) in cases {
            let source = format!(
                "let state = native/callable_example()\n{change}\nreturn native/emit_c(state)"
            );
            let report = runner.run_source(&source).unwrap_or_else(|error| {
                panic!("{change}: {}", runner.render_source_task_error(&error));
            });
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. }),
                "{change}: {}",
                report.render()
            );
            assert!(
                report.render().contains(expected),
                "{change}: expected {expected}: {}",
                report.render()
            );
        }
    }
}

// These programs isolate contracts that cannot be inferred from ordinary C calls.
const EXECUTION_PROGRAM: &str = r#"
let pointer = [:Pointer, :U64, :Mutable]
let slot = [:Pointer, pointer, :Mutable]
let parameters = [["root", slot, :Borrow], ["value", pointer, :Gc], ["condition", :Bool, :Value]]
let effects = [:ReadMemory, :WriteMemory, :Relocate]
let [entry, state] = native/execution_pointer_type(native/program(), "Entry", :U64, parameters, effects, :Tail)
let [constants, declared] = native/constants(state, [["zero", :U64, "0000000000000000"]])
let context = native/module_context(declared, constants)
context = native/declare_function(context, "collect", :Void, [["root", slot, :Borrow]], [], [:ReadMemory, :WriteMemory, :Relocate], :Value, "")
context[:state] = native/foreign(context[:state], context[:functions]["collect"])
context = native/declare_function(context, "entry", :U64, parameters, [], effects, :Value, "")
context = native/execution_function(context, "entry", :Tail)
let prefix = [native/let_statement("next", entry, native/function_address_expression("entry"))]
let transfer = native/tail_call_statement("next", ["root", "value", "condition"])
let body = [@prefix, transfer]
"#;

#[test]
fn execution_contracts_reject_invalid_transfers_and_stale_roots() {
    let cases = [
        (
            r#"state = context[:state]
state[:executions] = [[context[:functions]["entry"], :Unknown]]
context[:state] = state"#,
            "invalid execution policy",
        ),
        (
            r#"state = context[:state]
state[:executions] = []
context[:state] = state
body = [@prefix, native/return_value("zero")]"#,
            "function address execution policy mismatch",
        ),
        (
            r#"body = [@prefix, native/tail_call_statement("next", ["root", "value"])]"#,
            "transfer arity mismatch",
        ),
        (
            r#"body = [@prefix, native/let_statement("local", pointer, "value"),
  native/let_statement("address", slot, native/unary_expression(:Address, "local")),
  native/tail_call_statement("next", ["address", "value", "condition"])]"#,
            "required tail call cannot retain frame addresses",
        ),
        (
            r#"body = [native/let_statement("managed", pointer, native/manage_expression("value")), native/return_value("zero")]"#,
            "tail entries allocate through collector helpers",
        ),
        (
            r#"body = [native/let_statement("local", :U64, "zero"),
  native/let_statement("address", pointer, native/unary_expression(:Address, "local")),
  native/root_store_statement("root", "address"), native/return_value("zero")]"#,
            "RootStore requires managed references",
        ),
        (
            r#"body = [native/store_statement("root", "value"), native/return_value("zero")]"#,
            "managed reference requires a managed object or RootStore",
        ),
        (
            r#"body = [native/root_store_statement("root", "value"),
  native/do_statement(native/call_expression("collect", ["root"])),
  native/return_value(native/load_expression("value"))]"#,
            "managed reference used after relocation",
        ),
        (
            r#"body = [native/root_store_statement("root", "value"),
  native/if_statement("condition", [native/do_statement(native/call_expression("collect", ["root"]))], []),
  native/return_value(native/load_expression("value"))]"#,
            "managed reference used after relocation",
        ),
        (
            r#"body = [native/root_store_statement("root", "value"),
  native/while_statement("condition", [
    native/let_statement("loaded", :U64, native/load_expression("value")),
    native/do_statement(native/call_expression("collect", ["root"]))]),
  native/return_value("zero")]"#,
            "managed reference used after relocation",
        ),
        (
            r#"body = [@prefix, native/do_statement(native/call_expression("collect", ["root"])), transfer]"#,
            "transfer or return uses stale managed reference",
        ),
        (
            r#"context = native/declare_function(context, "caller", :U64, parameters, [], [], :Value, "")
context = native/define_function(context, "caller", [native/return_value(native/call_expression("entry", ["root", "value", "condition"]))])"#,
            "exceeds contract of caller",
        ),
        (
            r#"state = context[:state]
state[:executions] = [[context[:functions]["entry"], :Collector]]
context[:state] = state
body = [native/let_statement("local", :U64, "zero"),
  native/let_statement("address", pointer, native/unary_expression(:Address, "local")),
  native/let_statement("managed", pointer, native/manage_expression("address")),
  native/return_value("zero")]"#,
            "stack storage cannot become managed",
        ),
    ];
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for (change, expected) in cases {
            let source = format!(
                "{EXECUTION_PROGRAM}\n{change}\ncontext = native/define_function(context, \"entry\", body)\nreturn native/emit_c(context[:state])"
            );
            let report = runner.run_source(&source).unwrap_or_else(|error| {
                panic!("{change}: {}", runner.render_source_task_error(&error));
            });
            assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
            assert!(
                report.render().contains(expected),
                "{change}: expected {expected}: {}",
                report.render()
            );
        }
        let source = format!(
            r#"{EXECUTION_PROGRAM}
body = [native/root_store_statement("root", "value"),
  native/while_statement("condition", [
    native/let_statement("fresh", pointer, native/root_load_expression("root")),
    native/let_statement("loaded", :U64, native/load_expression("fresh")),
    native/do_statement(native/call_expression("collect", ["root"]))]),
  native/return_value("zero")]
context = native/define_function(context, "entry", body)
return native/emit_c(context[:state])"#
        );
        assert!(
            eval(&mut runner, &source)
                .with_str(|c| !c.is_empty())
                .unwrap()
        );
    }
}

#[test]
fn ordinary_helpers_preserve_managed_argument_and_relocation_contracts() {
    let cases = [
        (
            r#"
let pointer = [:Pointer, :U64, :Mutable]
let [callback, declared] = native/function_pointer_type(native/program(), "Managed", :Void, [["value", pointer, :Gc]], [], :Value, "")
let context = native/module_context(declared, {})
context = native/declare_function(context, "bad", :Void, [["callback", callback, :Value], ["value", pointer, :Borrow]], [], [], :Value, "")
context = native/define_function(context, "bad", [native/do_statement(native/indirect_call_expression("callback", ["value"])), native/return_void()])
return native/emit_c(context[:state])
"#,
            "managed argument requires Manage or RootLoad",
        ),
        (
            r#"
let context = native/module_context(native/program(), {})
context = native/declare_function(context, "collect", :Void, [], [], [:Relocate], :Value, "")
context = native/define_function(context, "collect", [native/return_void()])
context = native/declare_function(context, "caller", :Void, [], [], [], :Value, "")
context = native/define_function(context, "caller", [native/do_statement(native/call_expression("collect", [])), native/return_void()])
return native/emit_c(context[:state])
"#,
            "exceeds contract of caller",
        ),
    ];
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for (source, expected) in cases {
            let report = runner.run_source(source).unwrap();
            assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
            assert!(report.render().contains(expected), "{}", report.render());
        }
    }
}

#[test]
fn managed_roots_reload_after_relocation_and_tail_calls_remain_required() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let tail = eval(&mut runner, &format!(
            "{EXECUTION_PROGRAM}\ncontext = native/define_function(context, \"entry\", body)\nreturn native/emit_c(context[:state])"
        )).with_str(str::to_owned).unwrap();
        assert!(tail.contains("__attribute__((musttail)) return"));
        let source = format!(
            r#"{EXECUTION_PROGRAM}
body = [native/root_store_statement("root", "value"),
  native/if_statement("condition", [native/do_statement(native/call_expression("collect", ["root"]))], []),
  native/let_statement("current", pointer, native/root_load_expression("root")),
  native/return_value(native/load_expression("current"))]
context = native/define_function(context, "entry", body)
return native/emit_c(context[:state])"#
        );
        assert!(
            eval(&mut runner, &source)
                .with_str(|text| text.contains("mica_entry("))
                .unwrap()
        );
    }
}

#[test]
fn value_examples_execute_with_address_and_undefined_behaviour_sanitizers() {
    let mut previous = None;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let started = Instant::now();
        let generated = eval(&mut runner, "return native/emit_c(native/value_examples())")
            .with_str(str::to_owned)
            .unwrap();
        let generation_elapsed = started.elapsed();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated.clone());
        let scratch = Scratch::new();
        let source = scratch.0.join("values.c");
        let binary = scratch.0.join("values");
        fs::write(
            &source,
            format!(
                "{generated}\n#define mica_foreign_allocate native_test_allocate\n\
                 #define mica_foreign_release native_test_release\n{}\n\
                 #undef mica_foreign_allocate\n#undef mica_foreign_release\n{}",
                include_str!("../../../native/platform/allocation.c"),
                include_str!("../../../apps/native/tests/values.c")
            ),
        )
        .unwrap();
        let started = Instant::now();
        let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
            .args([
                "-std=c11",
                "-O1",
                "-g",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-fsanitize=address,undefined",
                "-fno-sanitize-recover=all",
                "-fno-omit-frame-pointer",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("value examples require a C11 compiler with AddressSanitizer and UBSan");
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let compilation_elapsed = started.elapsed();
        let executed = Command::new(&binary)
            .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
            .output()
            .unwrap();
        assert!(executed.status.success(), "{executed:?}");
        assert!(executed.stderr.is_empty(), "{executed:?}");
        eprintln!(
            "native examples: interpreter_only={interpreter_only}, generation={generation_elapsed:?}, C bytes={}, C compilation={compilation_elapsed:?}",
            generated.len()
        );
    }
}

fn eval(runner: &mut SourceRunner, source: &str) -> Value {
    let report = runner.run_source(source).unwrap_or_else(|error| {
        panic!("{}", runner.render_source_task_error(&error));
    });
    let TaskOutcome::Complete { value, .. } = report.outcome else {
        panic!("{}", report.render());
    };
    value
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("mica-native-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn generated_c_executes_on_both_mica_tiers() {
    let mut previous = None;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let generated = eval(
            &mut runner,
            r#"
let state = native/scalar_example()
for spec in [["subtract", :Subtract, :U64, :U64], ["multiply", :Multiply, :U64, :U64],
             ["bit_and", :BitAnd, :U64, :U64], ["bit_or", :BitOr, :U64, :U64],
             ["bit_xor", :BitXor, :U64, :U64], ["equal", :Equal, :Bool, :U64],
             ["less", :Less, :Bool, :U64], ["same_bool", :Equal, :Bool, :Bool]]
  let [fn_id, s0] = native/add_function(state, spec[0], spec[2])
  let [left, s1] = native/add_parameter(s0, fn_id, 0, "left", spec[3])
  let [right, s2] = native/add_parameter(s1, fn_id, 1, "right", spec[3])
  let [result, s3] = native/add_local(s2, fn_id, "result", spec[2])
  let [entry, s4] = native/add_block(s3, fn_id, "entry", true)
  let [operation, s5] = native/add_instruction(s4, entry, 0, spec[1], result, [left, right])
  state = native/terminate(s5, entry, :Return, [result])
end
for spec in [["maximum", :U64, "ffffffffffffffff"], ["truth", :Bool, "true"],
             ["signed_minimum", :I64, "-8000000000000000"], ["signed_maximum", :I64, "7fffffffffffffff"]]
  let [fn_id, s0] = native/add_function(state, spec[0], spec[1])
  let [constant, s1] = native/add_constant(s0, spec[1], spec[2])
  let [entry, s2] = native/add_block(s1, fn_id, "entry", true)
  state = native/terminate(s2, entry, :Return, [constant])
end
return native/emit_c(state)
"#,
        )
        .with_str(str::to_owned)
        .unwrap();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated.clone());
        let scratch = Scratch::new();
        let source = scratch.0.join("test.c");
        let binary = scratch.0.join("test");
        fs::write(
            &source,
            format!(
                "{generated}\n{}",
                r#"
#include <assert.h>
int main(void) {
    assert(mica_tag(UINT64_C(0xab123456789abcde)) == 0xab);
    assert(mica_tag(UINT64_MAX) == 255);
    assert(mica_tag(0) == 0);
    assert(mica_add(UINT64_MAX, 1) == 0);
    assert(mica_add(UINT64_C(0x8000000000000000), 7) == UINT64_C(0x8000000000000007));
    assert(mica_subtract(0, 1) == UINT64_MAX);
    assert(mica_multiply(UINT64_MAX, 2) == UINT64_MAX - 1);
    assert(mica_bit_and(UINT64_MAX, 0x55) == 0x55);
    assert(mica_bit_or(0x55, 0xaa) == 0xff);
    assert(mica_bit_xor(UINT64_MAX, 0xff) == UINT64_MAX - 0xff);
    assert(mica_equal(UINT64_MAX, UINT64_MAX));
    assert(!mica_equal(0, UINT64_MAX));
    assert(mica_less(0, UINT64_MAX));
    assert(!mica_less(UINT64_MAX, 0));
    assert(mica_same_bool(false, false));
    assert(!mica_same_bool(true, false));
    assert(mica_maximum() == UINT64_MAX);
    assert(mica_truth());
    assert(mica_signed_minimum() == INT64_MIN);
    assert(mica_signed_maximum() == INT64_MAX);
    for (uint64_t n = 0; n < 100; ++n) {
        assert(mica_sum(n) == n * (n - 1) / 2);
    }
    return 0;
}
"#
            ),
        )
        .unwrap();
        let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
            .args([
                "-std=c11",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("C generator tests require a C11 compiler named cc");
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new(&binary).output().unwrap();
        assert!(executed.status.success(), "{executed:?}");
    }
}

#[test]
fn rejects_invalid_programs_before_emission() {
    let edit = |table: &str, ordinal: usize, change: &str| {
        format!(
            "let rows = state[:{table}]\nlet row = rows[{ordinal}]\n{change}\nrows[{ordinal}] = row\nstate[:{table}] = rows"
        )
    };
    let cases = [
        (edit("functions", 0, "row[1] = \"x); bad(\""), "invalid identifier"),
        (edit("functions", 1, "row[1] = \"tag\""), "duplicate function name"),
        (edit("constants", 0, "row[2] = \"fffffffffffffffff\""), "16 hexadecimal digits"),
        (edit("constants", 2, "row[2] = \"0000000000000040\""), "shift count must be below 64"),
        ("state[:terminators] = [@state[:terminators], state[:terminators][0]]".to_owned(), "duplicate terminator"),
        (edit("instructions", 0, "row[4] = state[:parameters][0][0]"), "cannot assign a parameter"),
        (edit("instructions", 0, "row[5] = [state[:parameters][1][0], state[:constants][2][0]]"), "another function"),
        (edit("instructions", 0, "row[3] = :RawC"), "unsupported operation"),
        (edit("instructions", 0, "row[2] = 1"), "instruction positions"),
        (edit("parameters", 2, "row[2] = 0"), "duplicate parameter position"),
        (edit("functions", 0, "row[2] = :Pointer"), "unsupported type"),
        (edit("functions", 0, "row[2] = :Bool"), "return type mismatch"),
        (edit("instructions", 0, "row[3] = :Less"), "comparison result must be Bool"),
        (edit("parameters", 0, "row[2] = 1"), "parameter positions"),
        (edit("constants", 0, "row[2] = \"000000000000000z\""), "invalid U64 or I64 constant"),
        (edit("constants", 0, "row[0] = state[:functions][0][0]"), "duplicate node ID"),
        (edit("terminators", 2, "row[2] = [state[:blocks][0][0]]"), "branch crosses function boundary"),
        (edit("terminators", 3, "row[2] = [state[:parameters][3][0], state[:blocks][4][0], state[:blocks][5][0]]"), "branch condition must be Bool"),
        ("state[:terminators] = [row for row in state[:terminators] if row[0] != state[:blocks][0][0]]".to_owned(), "block has no terminator"),
        ("state = native/add_block(state, state[:functions][0][0], \"dead\", false)[1]\nstate = native/terminate(state, state[:next] - 1, :Return, [state[:constants][0][0]])".to_owned(), "unreachable block"),
        ("state[:instructions] = [row for row in state[:instructions] if row[1] != state[:blocks][2][0]]".to_owned(), "read before definite assignment"),
        (edit("instructions", 5, "row[5] = [state[:functions][1][0]]"), "call arity mismatch"),
    ];
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for (mutation, expected) in &cases {
            let source = format!(
                "let state = native/scalar_example()\n{mutation}\nreturn native/emit_c(state)"
            );
            let report = runner.run_source(&source).unwrap_or_else(|error| {
                panic!("{mutation}: {}", runner.render_source_task_error(&error));
            });
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. }),
                "{mutation}: {}",
                report.render()
            );
            assert!(
                report.render().contains(expected),
                "{mutation}: expected {expected}: {}",
                report.render()
            );
        }
    }
}

#[test]
fn row_order_and_node_allocation_do_not_change_c() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        assert_eq!(
            eval(
                &mut runner,
                r#"
let state = native/scalar_example()
let expected = native/emit_c(state)
for table in [:functions, :parameters, :locals, :blocks, :instructions, :terminators, :constants]
  let rows = []
  for row in state[table]
    rows = [row, @rows]
  end
  state[table] = rows
end
return expected == native/emit_c(state)
"#
            ),
            Value::bool(true)
        );
        assert_eq!(
            eval(
                &mut runner,
                r#"
let first = native/scalar_example()
let second = first
for table in [:functions, :parameters, :locals, :blocks, :instructions, :constants]
  let rows = []
  for original in first[table]
    let row = original
    row[0] = row[0] + 1000
    if table == :parameters || table == :locals || table == :blocks || table == :instructions
      row[1] = row[1] + 1000
    end
    if table == :instructions
      row[4] = row[4] + 1000
      row[5] = [id + 1000 for id in row[5]]
    end
    rows = [@rows, row]
  end
  second[table] = rows
end
second[:terminators] = [[row[0] + 1000, row[1], [id + 1000 for id in row[2]]] for row in first[:terminators]]
return native/emit_c(first) == native/emit_c(second)
"#
            ),
            Value::bool(true)
        );
        assert_eq!(
            eval(
                &mut runner,
                "return len(native/relations(native/scalar_example())[:Function])"
            ),
            Value::int(3).unwrap()
        );
    }
}

#[test]
fn function_validation_rebuilds_parameter_groups_after_edits() {
    let source = r#"
let state = native/program()
state[:functions] = [[0, "caller", :U64], [1, "callee", :U64], [2, "empty", :Void]]
state[:parameters] = [[3, 1, 1, "b", :U64], [4, 1, 0, "a", :U64]]
state[:locals] = [[5, 0, "result", :U64]]
state[:constants] = [[6, :U64, "0000000000000001"]]
state[:blocks] = [[7, 0, "entry", true], [8, 1, "entry", true], [9, 2, "entry", true]]
state[:instructions] = [[10, 7, 0, :Call, 5, [1, 6, 6]]]
state[:terminators] = [[7, :Return, [5]], [8, :Return, [4]], [9, :Return, []]]
let original = native/emit_c(state)
"#;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let emitted = eval(&mut runner, &format!("{source}\nreturn original"));
        emitted
            .with_str(|c| {
                assert!(c.contains("mica_callee(uint64_t v_a, uint64_t v_b)"));
                assert!(c.contains("mica_empty(void)"));
            })
            .unwrap();
        let report = runner.run_source(&format!(
            "{source}\nstate[:parameters] = [@state[:parameters], [11, 1, 2, \"c\", :U64]]\nreturn native/emit_c(state)"
        )).unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
        assert!(
            report.render().contains("call arity mismatch"),
            "{}",
            report.render()
        );
    }
}

#[test]
fn definite_assignment_intersects_all_incoming_paths() {
    let diamond = r#"
let state = native/program()
state[:functions] = [[0, "choose", :U64]]
state[:parameters] = [[1, 0, 0, "condition", :Bool]]
state[:locals] = [[2, 0, "result", :U64]]
state[:constants] = [[3, :U64, "0000000000000001"]]
state[:blocks] = [[4, 0, "entry", true], [5, 0, "yes", false],
                  [6, 0, "no", false], [7, 0, "join", false]]
state[:instructions] = [[8, 5, 0, :Copy, 2, [3]], [9, 6, 0, :Copy, 2, [3]]]
state[:terminators] = [[4, :Branch, [1, 5, 6]], [5, :Jump, [7]],
                       [6, :Jump, [7]], [7, :Return, [2]]]
"#;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        eval(
            &mut runner,
            &format!("{diamond}\nreturn native/emit_c(state)"),
        );
        let report = runner
            .run_source(&format!(
                "{diamond}\nstate[:instructions] = [state[:instructions][0]]\nreturn native/emit_c(state)"
            ))
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
        assert!(report.render().contains("read before definite assignment"));
    }
}

#[test]
fn definite_assignment_handles_backwards_construction_and_loop_backedges() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only).with_task_limits(TaskLimits {
            instruction_budget: 20_000_000,
            ..TaskLimits::default()
        });
        assert_eq!(
            eval(
                &mut runner,
                r#"
let state = native/program()
state[:functions] = [[0, "chain", :U64]]
state[:constants] = [[1, :U64, "0000000000000001"]]
for position in 0..199
  let block = 2 + position * 3
  let local = block + 1
  state[:locals] = [@state[:locals], [local, 0, string_concat("local_", to_literal(position)), :U64]]
  state[:blocks] = [[block, 0, string_concat("step_", to_literal(position)), position == 0], @state[:blocks]]
  state[:instructions] = [@state[:instructions], [block + 2, block, 0, :Copy, local, [1]]]
  if position == 199
    state[:terminators] = [@state[:terminators], [block, :Return, [local]]]
  else
    state[:terminators] = [@state[:terminators], [block, :Jump, [block + 3]]]
  end
end
let index = native/check(state)
let flow = native/flow_analysis(index, 0)
native/ensure(len(flow[:dominators][2]) == 1, "entry dominates itself")
native/ensure(len(flow[:dominators][599]) == 200, "every earlier block dominates the exit")
native/ensure(flow[:joins][2] == 5 && flow[:joins][599] == :Exit, "linear postdominators")
return true
"#
            ),
            Value::bool(true)
        );
        let loop_source = r#"
let state = native/program()
state[:functions] = [[0, "loop_assignment", :U64]]
state[:parameters] = [[1, 0, 0, "condition", :Bool]]
state[:locals] = [[2, 0, "result", :U64]]
state[:constants] = [[3, :U64, "0000000000000001"]]
state[:blocks] = [[7, 0, "exit", false], [6, 0, "body", false],
                  [5, 0, "head", false], [4, 0, "entry", true]]
state[:instructions] = [[8, 4, 0, :Copy, 2, [3]], [9, 6, 0, :Copy, 2, [3]]]
state[:terminators] = [[4, :Jump, [5]], [5, :Branch, [1, 6, 7]],
                       [6, :Jump, [5]], [7, :Return, [2]]]
"#;
        eval(
            &mut runner,
            &format!("{loop_source}\nnative/check(state)\nreturn true"),
        );
        let rejected = runner.run_source(&format!("{loop_source}\nstate[:instructions] = [state[:instructions][1]]\nnative/check(state)")).unwrap();
        assert!(matches!(rejected.outcome, TaskOutcome::Aborted { .. }));
        assert!(
            rejected
                .render()
                .contains("read before definite assignment")
        );
    }
}

#[test]
fn value_checks_report_instruction_types_effect_paths_and_ownership() {
    let cases = [
        (
            r#"
let rows = []
for original in state[:instructions]
  let row = original
  if row[3] == :CopyBytes
    row[5] = [row[5][2], row[5][1], row[5][2]]
  end
  rows = [@rows, row]
end
state[:instructions] = rows
"#,
            "block copy instruction 1: native: CopyBytes destination must be mutable bytes",
        ),
        (
            r#"
let [fn_id, v, next_state] = native/function_scope(state, "wrapper", [:Record, "ArenaResult"],
  [["capacity", :U64, :Value]], [["result", [:Record, "ArenaResult"]]], ["entry"])
state = native/contract(next_state, fn_id, [], :Owned, "")
let callee = [row[0] for row in state[:functions] if row[1] == "arena_create"][0]
state = native/block_body(state, v["entry"], [[:Call, v["result"], [callee, v["capacity"]]]], :Return, [v["result"]])
"#,
            "wrapper/entry instruction 0 -> arena_create/allocate instruction 0 -> foreign allocate",
        ),
        (
            r#"
let fn_id = [row[0] for row in state[:functions] if row[1] == "arena_release"][0]
let parameter = [row[0] for row in state[:parameters] if row[1] == fn_id][0]
let rows = []
for original in state[:parameter_contracts]
  let row = original
  if row[0] == parameter
    row[1] = :Borrow
  end
  rows = [@rows, row]
end
state[:parameter_contracts] = rows
"#,
            "cannot consume borrowed storage at arena_release/entry instruction 2",
        ),
        (
            r#"
let rows = []
for original in state[:contracts]
  let row = original
  if row[2] == :Borrow
    row[3] = "source"
  end
  rows = [@rows, row]
end
state[:contracts] = rows
"#,
            "result ownership mismatch in string_copy/",
        ),
        (
            "state[:parameter_contracts] = []",
            "missing pointer parameter contract",
        ),
        (
            r#"
let text = [:Record, "String"]
let [fn_id, v, next_state] = native/function_scope(state, "bad_store", :Void,
  [["destination", [:Pointer, text, :Mutable], :Borrow],
   ["source", [:Pointer, :U8, :Const], :Borrow], ["count", :U64, :Value]],
  [["text", text]], ["entry"])
state = native/contract(next_state, fn_id, [:WriteMemory], :Value, "")
state = native/block_body(state, v["entry"], [[:Record, v["text"], [v["source"], v["count"]]],
  [:Store, :Discard, [v["destination"], v["text"]]]], :Return, [])
"#,
            "borrow escapes through store at bad_store/entry instruction 1",
        ),
        (
            "state = native/add_constant(state, :I64, \"8000000000000000\")[1]",
            "I64 constant out of range",
        ),
        (
            r#"
let [bad_type, next_state] = native/record_type(state, "Cycle", [["self", [:Record, "Cycle"]]])
state = next_state
"#,
            "record contains a by-value cycle",
        ),
    ];
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for (mutation, expected) in cases {
            let source = format!(
                "let state = native/value_examples()\n{mutation}\nreturn native/emit_c(state)"
            );
            let report = runner.run_source(&source).unwrap_or_else(|error| {
                panic!("{}", runner.render_source_task_error(&error));
            });
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. }),
                "{}",
                report.render()
            );
            assert!(
                report.render().contains(expected),
                "expected {expected}: {}",
                report.render()
            );
        }
    }
}

#[test]
fn value_example_emission_ignores_fact_and_symbol_insertion_order() {
    if let Ok(order) = std::env::var("MICA_NATIVE_SYMBOL_ORDER") {
        // Each child has a fresh interner. Intern the IR vocabulary before
        // loading the runtime or any Mica source, in opposite orders.
        let mut symbols = [
            "U64",
            "I64",
            "Pointer",
            "Record",
            "Allocate",
            "Release",
            "Borrow",
            "CopyBytes",
            "ReadMemory",
            "WriteMemory",
            "Source",
            "Purpose",
            "Invariant",
        ];
        if order == "reverse" {
            symbols.reverse();
        }
        for spelling in symbols {
            mica_var::Symbol::intern(spelling);
        }
        let mut runner = runner(order == "forward");
        let generated = eval(
            &mut runner,
            r#"
let state = native/value_examples()
state = native/annotate(state, state[:functions][0][0], :Source, "source note")
state = native/annotate(state, state[:functions][0][0], :Purpose, "purpose note")
state = native/annotate(state, state[:functions][0][0], :Invariant, "invariant note")
let expected = native/emit_c(state)
for table in [:functions, :parameters, :locals, :blocks, :instructions, :terminators, :constants,
              :records, :fields, :foreign, :contracts, :parameter_contracts, :annotations]
  let rows = []
  for row in state[table]
    rows = [row, @rows]
  end
  state[table] = rows
end
let actual = native/emit_c(state)
if actual != expected
  raise E_INVARG, "row order changed C output"
end
return actual
"#,
        )
        .with_str(str::to_owned)
        .unwrap();
        fs::write(std::env::var_os("MICA_NATIVE_OUTPUT").unwrap(), generated).unwrap();
        return;
    }
    let scratch = Scratch::new();
    let mut previous = None;
    for order in ["forward", "reverse"] {
        let output_path = scratch.0.join(order);
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "value_example_emission_ignores_fact_and_symbol_insertion_order",
            ])
            .env("MICA_NATIVE_SYMBOL_ORDER", order)
            .env("MICA_NATIVE_OUTPUT", &output_path)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let generated = fs::read(output_path).unwrap();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated);
    }
}

#[test]
fn value_surface_executes_with_defined_numeric_and_heap_semantics() {
    let mut previous = None;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        runner
            .run_filein(include_str!("../../../apps/native/tests/surface.mica"))
            .unwrap();
        assert_eq!(
            eval(
                &mut runner,
                r#"
let state = native/surface_program()
let expected = native/emit_module(state, "surface")
for table in [:functions, :parameters, :locals, :blocks, :instructions, :terminators,
              :constants, :records, :fields, :globals, :foreign, :contracts, :parameter_contracts]
  let reversed = []
  for row in state[table]
    reversed = [row, @reversed]
  end
  state[table] = reversed
end
return expected == native/emit_module(state, "surface")
"#
            ),
            Value::bool(true)
        );
        let header = eval(
            &mut runner,
            "return native/emit_module(native/surface_program(), \"surface\")[:header]",
        )
        .with_str(str::to_owned)
        .unwrap();
        let generated = eval(
            &mut runner,
            "return native/emit_module(native/surface_program(), \"surface\")[:source]",
        )
        .with_str(str::to_owned)
        .unwrap();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated.clone());
        let scratch = Scratch::new();
        fs::write(scratch.0.join("surface.h"), header).unwrap();
        assert!(generated.contains("mica_internal_word_payload(v_value)"));
        assert!(generated.contains("mica_internal_word_tag(v_value)"));
        assert!(!generated.contains("& UINT64_C(0x00ffffffffffffff)"));
        let source = scratch.0.join("surface.c");
        let harness = scratch.0.join("harness.c");
        let binary = scratch.0.join("surface");
        fs::write(&source, &generated).unwrap();
        fs::write(
            &harness,
            format!(
                "#include \"surface.h\"\n#include \"surface.h\"\n{}",
                include_str!("../../../apps/native/tests/surface.c")
            ),
        )
        .unwrap();
        let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
            .args([
                "-std=c11",
                "-O2",
                "-g",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-ffp-contract=off",
                "-pthread",
                "-fsanitize=address,undefined,float-cast-overflow",
                "-fno-sanitize-recover=all",
                "-fno-omit-frame-pointer",
            ])
            .arg(&source)
            .arg(&harness)
            .args(["-lm", "-o"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new(&binary)
            .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
            .output()
            .unwrap();
        assert!(executed.status.success(), "{executed:?}");
        assert!(executed.stderr.is_empty(), "{executed:?}");
        for violation in [
            "shift",
            "division",
            "array",
            "payload",
            "heap",
            "unpack",
            "erase",
            "misaligned",
            "null_adoption",
        ] {
            let rejected = Command::new(&binary).arg(violation).output().unwrap();
            #[cfg(unix)]
            {
                assert_eq!(
                    rejected.status.signal(),
                    Some(6),
                    "{violation}: {rejected:?}"
                );
            }
            assert!(!rejected.status.success());
            assert!(rejected.stderr.is_empty(), "{rejected:?}");
        }
    }
}

#[test]
fn tagged_helpers_support_separate_modules_and_immediate_only_words() {
    let mut runner = runner(true);
    let scratch = Scratch::new();
    for (name, operation, result, heap_tags) in [
        ("payload", "Payload", "U64", "[7]"),
        ("tag", "Tag", "U8", "[]"),
        ("immediate_payload", "Payload", "U64", "[]"),
    ] {
        let emitted = eval(
            &mut runner,
            &format!(
                r#"
let word = [:Tagged, "{name}", :Const, {heap_tags}]
let [id, names, state] = native/function_scope(native/program(), "{name}", :{result},
  [["word", word, :Borrow]], [["result", :{result}]], ["entry"])
state = native/block_body(state, names["entry"], [[:{operation}, names["result"], [names["word"]]]], :Return, [names["result"]])
let module = native/emit_module(state, "{name}")
return [module[:header], module[:source]]
"#
            ),
        );
        emitted
            .with_list(|parts| {
                for (part, extension) in parts.iter().zip(["h", "c"]) {
                    fs::write(
                        scratch.0.join(format!("{name}.{extension}")),
                        part.with_str(str::to_owned).unwrap(),
                    )
                    .unwrap();
                }
            })
            .unwrap();
    }
    fs::write(
        scratch.0.join("main.c"),
        r#"
#include "payload.h"
#include "tag.h"
#include "immediate_payload.h"
#include <assert.h>
int main(void) {
    uint64_t word = UINT64_C(0xabfedcba98765432);
    assert(mica_payload(word) == UINT64_C(0x00fedcba98765432));
    assert(mica_tag(word) == 0xab);
    assert(mica_immediate_payload(UINT64_MAX) == UINT64_C(0x00ffffffffffffff));
    assert(mica_internal_word_payload(word++) == UINT64_C(0x00fedcba98765432));
    assert(word == UINT64_C(0xabfedcba98765433));
    return 0;
}
"#,
    )
    .unwrap();
    let binary = scratch.0.join("helpers");
    let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .current_dir(&scratch.0)
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pedantic",
            "payload.c",
            "tag.c",
            "immediate_payload.c",
            "main.c",
            "-o",
        ])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(binary).output().unwrap();
    assert!(executed.status.success(), "{executed:?}");
    assert!(executed.stderr.is_empty(), "{executed:?}");
    for name in ["internal_word_payload", "internal_word_tag"] {
        let report = runner
            .run_source(&format!(
                r#"
let [id, names, state] = native/function_scope(native/program(), "{name}", :Void, [], [], ["entry"])
state = native/terminate(state, names["entry"], :Return, [])
return native/emit_c(state)
"#
            ))
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Aborted { .. })
                && report.render().contains("reserved C helper name"),
            "{}",
            report.render()
        );
    }
}

#[test]
fn value_surface_rejects_invalid_types_layouts_and_pointer_escapes() {
    let cases = [
        (
            r#"
let [id, s] = native/record_type(state, "CycleArray", [["self", [:Array, [:Record, "CycleArray"], 1]]])
state = s
"#,
            "by-value cycle",
        ),
        (
            r#"
state = native/add_global(state, "bad", [:Array, :U8, 2], :Constant, [256, 0])[1]
"#,
            "invalid literal byte",
        ),
        (
            r#"
state = native/add_global(state, "bad", [:Array, :U8, 2], :Constant, [0])[1]
"#,
            "byte literal extent mismatch",
        ),
        (
            r#"
state = native/add_global(state, "bad", [:Scalar, "Symbol", :U64], :Mutable, :Zero)[1]
"#,
            "conflicting emitted type name",
        ),
        (
            r#"
state = native/add_global(state, "bad", [:Tagged, "Bad", :Mutable, [7, 7]], :Mutable, :Zero)[1]
"#,
            "heap tags must be increasing",
        ),
        (
            r#"
let [fn_id, v, s] = native/function_scope(state, "bad", :U64,
  [["input", :F32, :Value]], [["result", :U64]], ["entry"])
state = native/block_body(s, v["entry"], [[:Convert, v["result"], [v["input"]]]], :Return, [v["result"]])
"#,
            "float to integer requires CheckedFloatToInt",
        ),
        (
            r#"
let [fn_id, v, s] = native/function_scope(state, "bad", [:Pointer, :U64, :Mutable],
  [["input", [:Pointer, :U8, :Const], :Borrow]], [["result", [:Pointer, :U64, :Mutable]]], ["entry"])
state = native/contract(s, fn_id, [], :Borrow, "input")
state = native/block_body(state, v["entry"], [[:PointerCast, v["result"], [v["input"]]]], :Return, [v["result"]])
"#,
            "PointerCast cannot remove const",
        ),
        (
            r#"
let [fn_id, v, s] = native/function_scope(state, "bad", [:Pointer, :U64, :Mutable], [],
  [["number", :U64], ["result", [:Pointer, :U64, :Mutable]]], ["entry"])
state = native/contract(s, fn_id, [], :Owned, "")
state = native/block_body(state, v["entry"], [[:Zero, v["number"], []], [:Address, v["result"], [v["number"]]]], :Return, [v["result"]])
"#,
            "result ownership mismatch in bad",
        ),
        (
            r#"
let type_id = [:Tagged, "Value", :Mutable, [7, 8, 9, 10, 11, 12, 14, 16]]
let [fn_id, v, s] = native/function_scope(state, "bad", type_id,
  [["input", type_id, :Borrow]], [["result", type_id]], ["entry"])
state = native/contract(s, fn_id, [], :Owned, "")
state = native/block_body(state, v["entry"], [[:Copy, v["result"], [v["input"]]]], :Return, [v["result"]])
"#,
            "result ownership mismatch in bad",
        ),
        (
            r#"
let type_id = [:Tagged, "Value", :Mutable, [7, 8, 9, 10, 11, 12, 14, 16]]
let [destroy, dv, s] = native/function_scope(state, "destroy", :Void, [["value", type_id, :Consume]], [], [])
state = native/contract(native/foreign(s, destroy), destroy, [:Release], :Value, "")
let [fn_id, v, next] = native/function_scope(state, "bad", :Void, [["input", type_id, :Borrow]], [], ["entry"])
state = native/contract(next, fn_id, [:Release], :Value, "")
state = native/block_body(state, v["entry"], [[:Call, :Discard, [destroy, v["input"]]]], :Return, [])
"#,
            "cannot consume borrowed storage at bad",
        ),
        (
            r#"
let type_id = [:Scalar, "Symbol", :U32]
let [fn_id, v, s] = native/function_scope(state, "bad", :U32,
  [["input", type_id, :Value]], [["result", :U32]], ["entry"])
state = native/block_body(s, v["entry"], [[:Copy, v["result"], [v["input"]]]], :Return, [v["result"]])
"#,
            "copy type mismatch",
        ),
        (
            r#"
let type_id = [:Tagged, "Value", :Mutable, [7, 8, 9, 10, 11, 12, 14, 16]]
let target = [row[0] for row in state[:functions] if row[1] == "slot_store"][0]
let [fn_id, v, s] = native/function_scope(state, "bad", :Void,
  [["storage", [:Pointer, type_id, :Mutable], :Borrow], ["value", type_id, :Borrow]], [["result", type_id]], ["entry"])
state = native/contract(s, fn_id, [:WriteMemory], :Value, "")
state = native/block_body(state, v["entry"], [[:Call, v["result"], [target, v["storage"], v["value"]]]], :Return, [])
"#,
            "argument does not share borrow region",
        ),
        (
            r#"
let bytes = [:Pointer, :U8, :Mutable]
let [fn_id, v, s] = native/function_scope(state, "bad", bytes,
  [["slot", [:Pointer, bytes, :Mutable], :Borrow], ["input", bytes, :Borrow]], [["result", bytes]], ["entry"])
state = native/contract(s, fn_id, [:WriteMemory], :Borrow, "slot")
state = native/block_body(state, v["entry"], [[:Adopt, v["result"], [v["slot"], v["input"]]]], :Return, [v["result"]])
"#,
            "cannot adopt borrowed storage at bad",
        ),
        (
            r#"
let word = [:Tagged, "Value", :Mutable, [7, 8, 9, 10, 11, 12, 14, 16]]
let [fn_id, v, s] = native/function_scope(state, "bad", word, [["value", word, :Borrow]], [], ["entry"])
state = native/contract(s, fn_id, [], :Immediate, "")
state = native/terminate(state, v["entry"], :Return, [v["value"]])
"#,
            "result ownership mismatch in bad",
        ),
    ];
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        runner
            .run_filein(include_str!("../../../apps/native/tests/surface.mica"))
            .unwrap();
        for (mutation, expected) in cases {
            let report = runner
                .run_source(&format!(
                    "let state = native/surface_program()\n{mutation}\nreturn native/emit_c(state)"
                ))
                .unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. }),
                "{}",
                report.render()
            );
            assert!(
                report.render().contains(expected),
                "expected {expected}: {}",
                report.render()
            );
        }
    }
}

#[test]
fn adoption_and_immediate_contracts_reject_invalid_ownership() {
    let adopt = r#"
let state = native/program()
let bytes = [:Pointer, :U8, :Mutable]
let [fn_id, v, s] = native/function_scope(state, "adopt", bytes,
  [["slot", [:Pointer, bytes, :Mutable], :Borrow], ["allocation", bytes, :Consume]],
  [["result", bytes]], ["entry"])
state = native/contract(s, fn_id, [:WriteMemory], :Borrow, "slot")
state = native/block_body(state, v["entry"], [[:Adopt, v["result"], [v["slot"], v["allocation"]]]], :Return, [v["result"]])
"#;
    let cases = [
        (
            "state[:contracts] = [[fn_id, [], :Borrow, \"slot\"]]",
            "effect :WriteMemory exceeds contract",
        ),
        (
            "state[:contracts] = [[fn_id, [:WriteMemory], :Owned, \"\"]]",
            "result ownership mismatch in adopt",
        ),
        (
            "state[:parameter_contracts] = [[v[\"slot\"], :Borrow], [v[\"allocation\"], :Borrow]]",
            "cannot adopt borrowed storage at adopt",
        ),
        (
            r#"
let rows = state[:parameters]
rows[0] = [v["slot"], fn_id, 0, "slot", [:Pointer, bytes, :Const]]
state[:parameters] = rows
"#,
            "Adopt owner slot type mismatch",
        ),
        (
            r#"
let rows = state[:parameters]
rows[1] = [v["allocation"], fn_id, 1, "allocation", [:Pointer, :U8, :Const]]
state[:parameters] = rows
"#,
            "Adopt allocation type mismatch",
        ),
        (
            r#"
let [null, ns] = native/add_constant(state, bytes, "null")
state = ns
let instruction = state[:instructions][0]
instruction[5] = [v["slot"], null]
state[:instructions] = [instruction]
"#,
            "Adopt requires an owned allocation",
        ),
        (
            r#"
let [release, rv, rs] = native/function_scope(state, "release", :Void, [["allocation", bytes, :Consume]], [], [])
state = native/contract(native/foreign(rs, release), release, [:Release], :Value, "")
state[:contracts] = [[fn_id, [:WriteMemory, :Release], :Borrow, "slot"], [release, [:Release], :Value, ""]]
state = native/add_instruction(state, v["entry"], 1, :Call, :Discard, [release, v["result"]])[1]
"#,
            "cannot consume borrowed storage at adopt",
        ),
    ];
    let immediate = r#"
let state = native/program()
let word = [:Tagged, "Word", :Mutable, [7]]
let bytes = [:Pointer, :U8, :Mutable]
"#;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let mut sources: Vec<(String, &str)> = cases
            .iter()
            .map(|(mutation, expected)| (format!("{adopt}\n{mutation}"), *expected))
            .collect();
        // Native addresses are forbidden even when zeroed or nested in aggregates.
        for result in ["bytes", "[:Array, bytes, 2]", "[:Record, \"Wrapper\"]"] {
            sources.push((
                format!(
                    r#"{immediate}
state = native/record_type(state, "Wrapper", [["values", [:Array, bytes, 2]]])[1]
let [fn_id, v, s] = native/function_scope(state, "bad", {result}, [], [["result", {result}]], ["entry"])
state = native/contract(s, fn_id, [], :Immediate, "")
state = native/block_body(state, v["entry"], [[:Zero, v["result"], []]], :Return, [v["result"]])
"#
                ),
                "Immediate result cannot contain pointer fields",
            ));
        }
        sources.push((
            format!(
                r#"{immediate}
let [pair, s0] = native/record_type(state, "Pair", [["value", word]])
let [tag, s1] = native/add_constant(s0, :U8, "0000000000000007")
let [fn_id, v, s] = native/function_scope(s1, "bad", pair, [["input", bytes, :Borrow]],
  [["word", word], ["result", pair]], ["entry"])
state = native/contract(s, fn_id, [], :Immediate, "")
state = native/block_body(state, v["entry"], [[:PackPointer, v["word"], [v["input"], tag]],
  [:Record, v["result"], [v["word"]]]], :Return, [v["result"]])
"#
            ),
            "result ownership mismatch in bad",
        ));
        for (source, expected) in sources {
            let report = runner
                .run_source(&format!("{source}\nreturn native/emit_c(state)"))
                .unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. })
                    && report.render().contains(expected),
                "expected {expected}: {}",
                report.render()
            );
        }
    }
}

fn load_native_values(runner: &mut SourceRunner) {
    for source in [
        include_str!("../../../apps/native/value/program.mica"),
        include_str!("../../../apps/native/value/immediates.mica"),
        include_str!("../../../apps/native/value/numbers.mica"),
        include_str!("../../../apps/native/value/symbol_storage.mica"),
        include_str!("../../../apps/native/memory/program.mica"),
        include_str!("../../../apps/native/memory/allocation.mica"),
        include_str!("../../../apps/native/memory/collection.mica"),
        include_str!("../../../apps/native/memory/lifecycle.mica"),
        include_str!("../../../apps/native/memory/platform.mica"),
        include_str!("../../../apps/native/value/memory.mica"),
        include_str!("../../../apps/native/value/tracing.mica"),
        include_str!("../../../apps/native/value/heap.mica"),
        include_str!("../../../apps/native/value/utf8.mica"),
        include_str!("../../../apps/native/value/strings.mica"),
        include_str!("../../../apps/native/value/symbols.mica"),
        include_str!("../../../apps/native/value/string_append.mica"),
        include_str!("../../../apps/native/value/string_search.mica"),
        include_str!("../../../apps/native/value/compare.mica"),
        include_str!("../../../apps/native/value/maps.mica"),
        include_str!("../../../apps/native/value/collections.mica"),
        include_str!("../../../apps/native/value/relations.mica"),
        include_str!("../../../apps/native/value/hash.mica"),
        include_str!("../../../apps/native/value/copy.mica"),
        include_str!("../../../apps/native/value/buffer.mica"),
        include_str!("../../../apps/native/value/codec.mica"),
        include_str!("../../../apps/native/value/codec_decode.mica"),
        include_str!("../../../apps/native/value/persistence.mica"),
    ] {
        runner.run_filein(source).unwrap_or_else(|error| {
            panic!("{}", runner.render_source_task_error(&error));
        });
    }
}

#[test]
fn native_value_layer_executes_on_both_mica_tiers() {
    let sanitizer = if std::env::var_os("MICA_NATIVE_THREAD_SANITIZER").is_some() {
        "-fsanitize=thread"
    } else {
        "-fsanitize=address,undefined,float-cast-overflow"
    };
    let mut previous = None;
    for interpreter_only in [true, false] {
        // Match the comparison harness budget for the complete value module.
        let mut runner = runner(interpreter_only).with_task_limits(TaskLimits {
            instruction_budget: 500_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
        load_native_values(&mut runner);
        let generated = eval(&mut runner, r#"
let state = native_value/program()
let functions = {}
for row in state[:functions]
  functions[row[1]] = row[0]
end
let word = native_value/word()
let result = [:Record, "ValueResult"]
let [number, s0] = native/add_constant(state, :I64, "0000000000000003")
let [has_end, s1] = native/add_constant(s0, :Bool, "false")
let [fn_id, v, s2] = native/function_scope(s1, "test_composed", result,
  [["arena", [:Pointer, [:Record, "MemoryWorker"], :Mutable], :Borrow]],
  [["integer", result], ["value", word], ["empty", word], ["result", result]], ["entry"])
state = native/contract(s2, fn_id, [:Allocate, :ReadMemory, :WriteMemory], :Gc, "")
state = native/block_body(state, v["entry"], [[:Call, v["integer"], [functions["value_int"], number]],
  [:Field, v["value"], [v["integer"], "value"]], [:Zero, v["empty"], []],
  [:Call, v["result"], [functions["value_range"], v["arena"], v["value"], has_end, v["empty"]]]], :Return, [v["result"]])
return native/emit_c(state)
"#)
            .with_str(str::to_owned)
            .unwrap();
        let sort_columns = generated
            .split("bool mica_relation_sort_columns(")
            .nth(1)
            .unwrap()
            .split_once(" {\n")
            .unwrap()
            .1;
        assert!(sort_columns.starts_with(
            "  if (!(UINT64_C(0x0000000000000001) < v_length)) {\n    return true;\n  }\n"
        ));
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated.clone());
        let platform = eval(&mut runner, "return native_memory/platform()")
            .with_str(str::to_owned)
            .unwrap();
        let semantic = format!(
            "#define MICA_MEMORY_TEST_ALLOCATOR\n{platform}\n\
             #define mica_foreign_allocate native_test_allocate\n\
             #define mica_foreign_release native_test_release\n{}\n\
             #undef mica_foreign_allocate\n#undef mica_foreign_release\n{}\n{}",
            include_str!("../../../native/platform/allocation.c"),
            include_str!("../../../native/platform/mutex.c"),
            include_str!("../../../apps/native/value/tests.c")
        );
        let collected = format!(
            "{platform}\n{}\n{}\n{}",
            include_str!("../../../native/platform/allocation.c"),
            include_str!("../../../native/platform/mutex.c"),
            include_str!("../../../apps/native/value/gc_tests.c")
        );
        for oracle in [semantic, collected] {
            let scratch = Scratch::new();
            let source = scratch.0.join("value.c");
            let binary = scratch.0.join("value");
            fs::write(&source, format!("{generated}\n{oracle}")).unwrap();
            let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
                .args([
                    "-std=c11",
                    "-pthread",
                    "-O2",
                    "-g",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-pedantic",
                    "-ffp-contract=off",
                    "-fno-fast-math",
                    sanitizer,
                    "-fno-sanitize-recover=all",
                    "-fno-omit-frame-pointer",
                ])
                .arg(&source)
                .args(["-lm", "-o"])
                .arg(&binary)
                .output()
                .unwrap();
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let executed = Command::new(binary)
                .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
                .env("TSAN_OPTIONS", "halt_on_error=1")
                .output()
                .unwrap();
            assert!(executed.status.success(), "{executed:?}");
            assert!(executed.stderr.is_empty(), "{executed:?}");
        }
    }
}

#[test]
fn graph_builders_scope_labels_and_reconstruct_loops() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        runner
            .run_filein(include_str!("../../../apps/native/tests/graphs.mica"))
            .unwrap();
        let generated = eval(&mut runner, "return native/emit_c(native/graph_example())")
            .with_str(str::to_owned)
            .unwrap();
        assert!(generated.contains("while ("), "{generated}");
        assert!(!generated.contains("goto "), "{generated}");
        let scratch = Scratch::new();
        let source = scratch.0.join("graph.c");
        let binary = scratch.0.join("graph");
        fs::write(&source, format!("{generated}\n#include <assert.h>\nint main(void) {{ for (uint64_t n = 0; n < 100; ++n) {{ uint64_t sum = n * (n - 1) / 2; assert(mica_graph_sum(n) == sum); assert(mica_graph_skip(n) == sum - (n > 1 ? 1 : 0)); }} }}\n")).unwrap();
        let compiled = Command::new("cc")
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new(&binary).output().unwrap();
        assert!(
            executed.status.success() && executed.stderr.is_empty(),
            "{executed:?}"
        );
        for (blocks, expected) in [
            (
                "[[\"entry\", fn(b) => native_gen/jump_to(b, \"missing\")]]",
                "unknown generated graph target",
            ),
            ("[[\"entry\", fn(b) => b]]", "block can fall through"),
            (
                "[[\"entry\", fn(b) => native_gen/return_zero(b)], [\"entry\", fn(b) => native_gen/return_zero(b)]]",
                "duplicate graph label",
            ),
        ] {
            let message = eval(
                &mut runner,
                &format!(
                    r#"
let module = native/module_context(native/program(), {{}})
module = native/declare_function(module, "invalid", :U64, [], [], [], :Value, "")
try
  native_gen/graph(native_gen/context(module, "invalid"), "entry", {blocks})
  return "accepted"
catch E_INVARG as problem
  return native/error_message(problem)
end
"#
                ),
            )
            .with_str(str::to_owned)
            .unwrap();
            assert!(message.contains(expected), "{message}");
        }
        assert_eq!(
            eval(
                &mut runner,
                r#"
let renamed = native/rename_bindings(native/graph_statement("value", [["value", [native/branch_statement("value", "yes", "value")]], ["yes", [native/return_value("value")]]]), {"value" -> "renamed"})
return renamed == native/graph_statement("value", [["value", [native/branch_statement("renamed", "yes", "value")]], ["yes", [native/return_value("renamed")]]])
"#
            ),
            Value::bool(true)
        );
    }
}

#[test]
fn native_early_return_chains_do_not_consume_bootstrap_call_depth() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let generated = eval(&mut runner, r#"
let [constants, state] = native/constants(native/program(), [["zero", :U64, "0000000000000000"], ["one", :U64, "0000000000000001"]])
let module = native/module_context(state, constants)
module = native/declare_function(module, "guard_chain", :U64, [["stop", :Bool, :Value]], [], [], :Value, "")
let statements = [native/when_statement("stop", [native/return_value("zero")]) for position in 1..300]
module = native/define_function(module, "guard_chain", [@statements, native/return_value("one")])
return native/emit_c(module[:state])
"#).with_str(str::to_owned).unwrap();
        assert_eq!(generated.matches("if (").count(), 300);
        assert!(!generated.contains("\n    if ("));
        assert!(!generated.contains("goto "));
    }
}

#[test]
fn native_terminal_guard_regions_do_not_consume_bootstrap_call_depth() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        let generated = eval(&mut runner, r#"
let [constants, state] = native/constants(native/program(), [["zero", :U64, "0000000000000000"], ["one", :U64, "0000000000000001"]])
let module = native/module_context(state, constants)
module = native/declare_function(module, "cleanup_guards", :U64, [["stop", :Bool, :Value], ["check", :Bool, :Value]], [], [], :Value, "")
let cleanup = [native/when_statement("check", [native/return_value("zero")]), native/return_value("one")]
let statements = [native/when_statement("stop", cleanup) for position in 1..300]
module = native/define_function(module, "cleanup_guards", [@statements, native/return_value("zero")])
return native/emit_c(module[:state])
"#).with_str(str::to_owned).unwrap();
        assert_eq!(generated.matches("if (").count(), 600);
        assert!(!generated.contains("\n      if ("));
        assert!(!generated.contains("goto "));
    }
}

#[test]
fn structured_builders_preserve_control_flow_and_notes() {
    let mut previous = None;
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        runner
            .run_filein(include_str!("../../../apps/native/tests/structured.mica"))
            .unwrap();
        let generated = eval(
            &mut runner,
            "return native/emit_c(native/structured_example(0))",
        )
        .with_str(str::to_owned)
        .unwrap();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated.clone());
        assert!(generated.contains("apps/native/tests/structured.mica: native/structured_example"));
        assert!(generated.contains("Nested calls execute left to right."));
        assert!(generated.contains("Allowed effects:"));
        assert!(generated.contains("while (v_i < v_limit) {"));
        assert!(generated.contains("switch ("));
        assert!(generated.contains("} else if ("));
        assert!(!generated.contains("uint64_t mica_structured_sum(uint64_t v_limit);"));
        assert!(generated.contains("uint64_t mica_cycle_a(uint64_t v_n);"));
        assert!(!generated.contains("uint64_t mica_cycle_b(uint64_t v_n);"));
        // Only the irreducible cycle and a loop exit inside a switch need labels.
        let sum = generated
            .split("uint64_t mica_structured_sum(uint64_t v_limit) {")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        assert!(!sum.contains("goto "), "{sum}");
        let shared = generated
            .split("uint64_t mica_structured_shared_tail(uint64_t v_limit) {")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        assert!(!shared.contains("goto "), "{shared}");
        assert_eq!(shared.matches("mica_foreign_structured_tick()").count(), 1);
        let body = |name: &str| {
            generated
                .split(&format!(" mica_{name}("))
                .nth(1)
                .unwrap()
                .split_once(" {\n")
                .unwrap()
                .1
                .split("\n}")
                .next()
                .unwrap()
        };
        let branch = body("scoped_branch");
        let guards = body("structured_guards");
        assert!(
            guards.starts_with("  if (!(UINT64_C(0x0000000000000001) < v_limit)) {"),
            "{guards}"
        );
        assert!(
            guards.contains("\n  if (!(UINT64_C(0x0000000000000002) < v_limit)) {"),
            "{guards}"
        );
        assert!(guards.contains("\n  uint64_t v_first = "), "{guards}");
        assert!(!guards.contains("\n    if ("), "{guards}");
        let guard_effects = body("structured_guard_effects");
        assert!(
            guard_effects.starts_with("  if (!(v_choice)) {"),
            "{guard_effects}"
        );
        assert_eq!(
            guard_effects
                .matches("mica_foreign_structured_tick()")
                .count(),
            4
        );
        assert!(
            branch.starts_with("  uint64_t v_result;\n  if (v_choice) {"),
            "{branch}"
        );
        assert!(
            branch.contains("\n    uint64_t v_left_value = "),
            "{branch}"
        );
        assert!(
            branch.contains("} else {\n    uint64_t v_right_value = "),
            "{branch}"
        );
        let scoped_loop = body("scoped_loop");
        assert!(
            scoped_loop.starts_with("  uint64_t v_i = "),
            "{scoped_loop}"
        );
        assert!(
            scoped_loop.contains("while (v_i < v_limit) {\n    uint64_t v_iteration_value = "),
            "{scoped_loop}"
        );
        assert!(!scoped_loop.contains("bool v_"), "{scoped_loop}");
        let header = body("scoped_header");
        assert!(header.contains("while (true) {\n    uint64_t "), "{header}");
        assert_eq!(header.matches("mica_foreign_structured_tick()").count(), 1);
        let irreducible = body("irreducible");
        assert!(
            irreducible
                .lines()
                .any(|line| line.starts_with("  bool v_condition")),
            "{irreducible}"
        );
        let nested = body("structured_nested");
        assert!(nested.contains("\n    uint64_t v_j = "), "{nested}");
        let switch = body("scoped_switch");
        for (name, value) in [("one", 1), ("two", 2), ("three", 3)] {
            assert!(
                switch.contains(&format!(
                    "case UINT64_C(0x{value:016x}): {{\n      uint64_t v_case_{name} = "
                )),
                "{switch}"
            );
        }
        let addressed = body("scoped_address");
        assert!(
            addressed.contains("  uint64_t v_left_storage, v_right_storage;"),
            "{addressed}"
        );
        assert!(
            !addressed.contains("\n    uint64_t v_left_storage"),
            "{addressed}"
        );
        assert!(!generated.contains("\n#error injected"));
        let reordered = eval(
            &mut runner,
            r#"
let state = native/structured_example(1000)
for table in [:functions, :parameters, :locals, :blocks, :instructions, :terminators,
              :constants, :records, :fields, :globals, :foreign, :contracts, :parameter_contracts, :annotations]
  let rows = []
  for row in state[table]
    rows = [row, @rows]
  end
  state[table] = rows
end
let module = native/emit_module(state, "structured")
return [native/emit_c(state), module[:header], module[:source]]
"#,
        );
        reordered
            .with_list(|items| {
                assert_eq!(items[0].with_str(str::to_owned).unwrap(), generated);
                assert!(
                    items[1]
                        .with_str(|s| s.contains("Skip three and stop before ten."))
                        .unwrap()
                );
                assert!(
                    items[2]
                        .with_str(|s| s.contains("Nested calls execute left to right."))
                        .unwrap()
                );
            })
            .unwrap();
        let scratch = Scratch::new();
        let source = scratch.0.join("structured.c");
        let binary = scratch.0.join("structured");
        fs::write(
            &source,
            format!(
                r#"{generated}
#include <assert.h>
static uint64_t calls;
uint64_t mica_foreign_structured_tick(void) {{ return ++calls; }}
int main(void) {{
    for (uint64_t limit = 0; limit < 100; ++limit) {{
        uint64_t sum = 0;
        for (uint64_t i = 1; i <= limit && i < 10; ++i) if (i != 3) sum += i;
        assert(mica_structured_sum(limit) == sum);
        assert(mica_structured_nested(limit) == (limit >= 2 ? limit : 0));
        assert(mica_structured_dispatch(limit) == (limit >= 1 && limit <= 3 ? limit : 10));
        assert(mica_structured_ladder(limit) == (limit == 0 ? 11 : limit == 1 ? 12 : 13));
        assert(mica_structured_loop_switch(limit) == (limit >= 2 ? 11 : 0));
        assert(mica_scoped_loop(limit) == limit * (limit + 1) / 2);
        assert(mica_scoped_switch(limit) == (limit >= 1 && limit <= 3 ? limit + 10 : 0));
        assert(mica_irreducible(limit, true) == (limit ? limit : 1));
        assert(mica_irreducible(limit, false) == (limit ? limit : 1));
        assert(mica_cycle_a(limit) == (limit % 2 ? 2 : 1));
        assert(mica_cycle_b(limit) == (limit % 2 ? 1 : 2));
        assert(mica_semantic_range(limit) == (limit == 0 ? 0 : limit < 3 ? 1 : 4));
        assert(mica_semantic_first(limit) == (limit == 0 ? 10 : 0));
    }}
    assert(mica_structured_choice(true) == 1);
    assert(mica_structured_choice(false) == 2);
    assert(mica_scoped_branch(true) == 13);
    assert(mica_scoped_branch(false) == 15);
    assert(mica_scoped_address(true) == 1);
    assert(mica_scoped_address(false) == 2);
    assert(mica_structured_order() == UINT64_MAX && calls == 2);
    assert(mica_structured_zero(true) == 0 && calls == 3);
    assert(mica_structured_zero(false) == 0 && calls == 4);
    mica_structured_void();
    calls = 0;
    assert(mica_structured_tick_switch() == 10 && calls == 1);
    calls = 0;
    assert(mica_structured_shared_tail(0) == 11 && calls == 1);
    assert(mica_structured_shared_tail(2) == 0 && calls == 1);
    assert(mica_structured_shared_tail(3) == 12 && calls == 2);
    calls = 0;
    assert(mica_scoped_header() == 2 && calls == 3);
    calls = 0;
    assert(mica_structured_guards(0) == 0 && calls == 0);
    assert(mica_structured_guards(1) == 0 && calls == 0);
    assert(mica_structured_guards(2) == 1 && calls == 0);
    assert(mica_structured_guards(3) == 3 && calls == 2);
    calls = 0;
    assert(mica_structured_guard_effects(false) == 1 && calls == 1);
    calls = 0;
    assert(mica_structured_guard_effects(true) == 0 && calls == 3);
    struct mica_BuilderNode tail = {{ .f_next = NULL, .f_value = 2 }};
    struct mica_BuilderNode head = {{ .f_next = &tail, .f_value = 1 }};
    assert(mica_semantic_walk(NULL) == 0);
    assert(mica_semantic_walk(&head) == 2);
    struct mica_BuilderPair pair = mica_semantic_pair();
    assert(pair.f_left == 1 && pair.f_right == 2);
    uint64_t source[] = {{ 4, 5, 6 }}, destination[] = {{ 0, 0, 0 }};
    assert(mica_semantic_elements(NULL, NULL, 0, 0) == 10);
    assert(mica_semantic_elements(destination, source, 3, 2) == 6);
    assert(destination[0] == 4 && destination[1] == 5 && destination[2] == 6);
    assert(mica_semantic_elements(destination, source, 3, 3) == 10);
    assert(mica_semantic_elements(source, source, 3, 0) == 4);
    calls = 0;
    struct mica_BuilderPair copy = mica_semantic_copy(&pair);
    assert(copy.f_left == 1 && copy.f_right == 1 && calls == 1);
    assert(pair.f_left == 1 && pair.f_right == 2);
    return 0;
}}
"#
            ),
        )
        .unwrap();
        let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
            .args([
                "-std=c11",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-fsanitize=address,undefined",
                "-fno-sanitize-recover=all",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .arg("-lm")
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new(&binary).output().unwrap();
        assert!(executed.status.success(), "{executed:?}");
        assert!(executed.stderr.is_empty(), "{executed:?}");
    }
}

#[test]
fn graph_traversal_uses_work_stacks_for_long_chains() {
    let mut runner = runner(true);
    let mut source = String::from(
        "let [constants, state] = native/constants(native/program(), [[\"one\", :U64, \"0000000000000001\"]])\n\
         let context = native/module_context(state, constants)\n",
    );
    for i in 0..80 {
        source.push_str(&format!(
            "context = native/declare_function(context, \"chain_{i}\", :U64, [], [], [], :Value, \"\")\n"
        ));
    }
    for i in 0..80 {
        let value = if i == 79 {
            "\"one\"".to_owned()
        } else {
            format!("native/call_expression(\"chain_{}\", [])", i + 1)
        };
        source.push_str(&format!(
            "context = native/define_function(context, \"chain_{i}\", [native/return_value({value})])\n"
        ));
    }
    let blocks: Vec<String> = (0..80).map(|i| format!("step_{i}")).collect();
    source.push_str(&format!(
        "let [id, names, declared] = native/function_scope(context[:state], \"blocks\", :U64, [], [], {blocks:?})\nstate = declared\n"
    ));
    for i in 0..79 {
        source.push_str(&format!(
            "state = native/terminate(state, names[\"step_{i}\"], :Jump, [names[\"step_{}\"]])\n",
            i + 1
        ));
    }
    source.push_str(
        "state = native/terminate(state, names[\"step_79\"], :Return, [constants[\"one\"]])\nreturn native/emit_c(state)",
    );
    let generated = eval(&mut runner, &source).with_str(str::to_owned).unwrap();
    assert!(!generated.contains("goto "));
    assert!(!generated.contains("uint64_t mica_chain_0(void);"));
    assert!(
        generated.find("mica_chain_79(void) {").unwrap()
            < generated.find("mica_chain_0(void) {").unwrap()
    );
}

#[test]
fn structured_builders_reject_invalid_flow_and_annotations() {
    let mut runner = runner(true);
    for (expression, expected) in [
        (
            r#"native/copy_elements(:U64, "dst", "src", "length", :Owned)"#,
            "element copy requires const or mutable source access",
        ),
        (
            r#"native/checked_element("slot", :U64, "data", "length", "index", [], :Owned)"#,
            "element access requires const or mutable access",
        ),
        (
            r#"native/record_arguments(["Pair", [["x", :U64], ["y", :U64]]], {"x" -> "one"})"#,
            "field count for Pair",
        ),
        (
            r#"native/record_arguments(["Pair", [["x", :U64], ["y", :U64]]], {"x" -> "one", "z" -> "one"})"#,
            "missing field Pair.y",
        ),
        (
            r#"native/record_copy_arguments(["Pair", [["x", :U64]]], "pair", {"typo" -> "one"})"#,
            "unknown field Pair.typo",
        ),
        (
            r#"native/record_arguments(["Pair", [["x", :U64], ["x", :U64]]], {"x" -> "one", "y" -> "one"})"#,
            "duplicate record field",
        ),
        (
            r#"native/walk_chain("cursor", [:Pointer, :U64, :Mutable], "head", "next", [], [])"#,
            "chain exhaustion must leave the traversal",
        ),
        (
            r#"native/walk_chain("cursor", [:Pointer, :U64, :Mutable], "head", "next", [], [native/continue_statement()])"#,
            "chain exhaustion must leave the traversal",
        ),
    ] {
        let source = format!(
            "try\n {expression}\n return \"accepted\"\ncatch E_INVARG as problem\n return native/error_message(problem)\nend"
        );
        let message = eval(&mut runner, &source).with_str(str::to_owned).unwrap();
        assert!(message.contains(expected), "{expression}: {message}");
    }
    let prefix = r#"
let state = native/program()
let [constants, with_constants] = native/constants(state, [["one", :U64, "0000000000000001"]])
let [fn_id, names, declared] = native/function_scope(with_constants, "test", :U64, [["condition", :Bool, :Value]], [["value", :U64]], [])
names["one"] = constants["one"]
state = declared
"#;
    for (body, expected) in [
        ("[[:Break]]", "loop control outside a loop"),
        ("[[:Continue]]", "loop control outside a loop"),
        (
            "[[:Return, \"one\"], [:Return, \"one\"]]",
            "statement after control flow terminates",
        ),
        ("[[:Set, \"value\", \"one\"]]", "fall through"),
        (
            "[[:If, \"condition\", [[:Set, \"value\", \"one\"]], []], [:Return, \"value\"]]",
            "read before definite assignment",
        ),
        (
            "[[:If, \"one\", [[:Return, \"one\"]], [[:Return, \"one\"]]]]",
            "branch condition must be Bool",
        ),
        (
            "[[:Let, \"body_reserved\", :U64, \"one\"], [:Return, \"one\"]]",
            "names are reserved",
        ),
        (
            "[[:Let, \"value\", :U64, \"one\"], [:Return, \"one\"]]",
            "duplicate structured local",
        ),
        ("[[:Return, \"missing\"]]", "unknown structured operand"),
        (
            "[native/switch_statement(:U64, \"one\", [native/case_arm(\"one\", [native/return_value(\"one\")]), native/case_arm(\"one\", [native/return_value(\"one\")])], [native/return_value(\"one\")])]",
            "duplicate Switch case",
        ),
        (
            "[native/switch_statement(:U64, \"one\", [native/case_arm(\"condition\", [native/return_value(\"one\")])], [native/return_value(\"one\")])]",
            "Switch case must be a constant",
        ),
    ] {
        let source = format!(
            "{prefix}\ntry\n state = native/function_body(state, fn_id, names, {body})\n native/emit_c(state)\n return \"accepted\"\ncatch E_INVARG as problem\n return native/error_message(problem)\nend"
        );
        let message = eval(&mut runner, &source).with_str(str::to_owned).unwrap();
        assert!(message.contains(expected), "{body}: {message}");
    }
    for (annotation, expected) in [
        ("[99999, :Purpose, \"missing\"]", "known node"),
        (
            "[fn_id, :Assume, \"not a proof\"]",
            "unknown annotation kind",
        ),
        ("[fn_id, :Purpose, 1]", "text must be a string"),
    ] {
        let source = format!(
            "{prefix}\nstate = native/function_body(state, fn_id, names, [[:Return, \"one\"]])\nstate = native/append(state, :annotations, {annotation})\ntry\n native/emit_c(state)\n return \"accepted\"\ncatch E_INVARG as problem\n return native/error_message(problem)\nend"
        );
        let message = eval(&mut runner, &source).with_str(str::to_owned).unwrap();
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn native_relation_kernel_executes_on_both_mica_tiers() {
    check_native_kernel_fixture(false);
}

#[test]
fn native_tasks_execute_on_both_mica_tiers() {
    check_native_kernel_fixture(true);
}

fn load_native_source_compiler(runner: &mut SourceRunner) {
    for source in [
        include_str!("../../../apps/compiler/emit.mica"),
        include_str!("../../../apps/native/compiler/exceptions.mica"),
        include_str!("../../../apps/native/compiler/analysis.mica"),
        include_str!("../../../apps/native/compiler/flow.mica"),
        include_str!("../../../apps/native/compiler/builders.mica"),
        include_str!("../../../apps/native/compiler/errors.mica"),
        include_str!("../../../apps/native/compiler/operations.mica"),
        include_str!("../../../apps/native/compiler/builtins.mica"),
        include_str!("../../../apps/native/compiler/patterns.mica"),
        include_str!("../../../apps/native/compiler/queries.mica"),
        include_str!("../../../apps/native/compiler/updates.mica"),
        include_str!("../../../apps/native/compiler/ordering.mica"),
        include_str!("../../../apps/native/compiler/time.mica"),
        include_str!("../../../apps/native/compiler/text.mica"),
        include_str!("../../../apps/native/compiler/text_scan.mica"),
        include_str!("../../../apps/native/compiler/text_search.mica"),
        include_str!("../../../apps/native/compiler/functions.mica"),
        include_str!("../../../apps/native/compiler/collections.mica"),
        include_str!("../../../apps/native/compiler/access.mica"),
        include_str!("../../../apps/native/compiler/expressions.mica"),
        include_str!("../../../apps/native/compiler/prepare.mica"),
        include_str!("../../../apps/native/compiler/continuations.mica"),
        include_str!("../../../apps/native/compiler/install.mica"),
        include_str!("../../../apps/native/compiler/lower.mica"),
        include_str!("../../../apps/native/compiler/program.mica"),
    ] {
        runner.run_filein(source).unwrap_or_else(|error| {
            panic!("{}", runner.render_source_task_error(&error));
        });
    }
}

#[test]
fn native_source_analysis_tracks_continuations_and_definite_assignment() {
    let (conformance_source, _, _) = source_expression_cases();
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for source in [
            include_str!("../../../apps/native/value/program.mica"),
            include_str!("../../../apps/compiler/lex.mica"),
            include_str!("../../../apps/compiler/parse.mica"),
            include_str!("../../../apps/compiler/ast.mica"),
        ] {
            runner.run_filein(source).unwrap();
        }
        load_native_source_compiler(&mut runner);
        eval(&mut runner, "return make_relation(:NativeQuery, 3)");
        let example = include_str!("../../../apps/native/compiler/examples/roundtrip.mica");
        let checked = eval(
            &mut runner,
            &format!(
                r#"
make_relation(:NativeResult, 2)
let parsed = parse_rows({example:?})
native/ensure(parsed[:errors] == [], to_literal(parsed[:errors]))
let module = emit_program(parsed[:rows], parsed[:root])
native/ensure(module[:errors] == [], to_literal(module[:errors]))
let parent = native_source/analyse(module[:entry], 0)
let child = native_source/analyse(module[:methods][0][:program], 3)
native/ensure(len(parent[:captures]) == 2, "spawn and receive require continuations")
native/ensure(len(child[:captures]) == 0, "child sends without suspension")
let regions = native_source/regions(parent)
native/ensure(len(regions) == 3, "parent lowers to three native entries")
native/ensure(regions[0][:restore] == [] && regions[0][:input] == -1, "initial entry has no reply")
for index in 1..2
  let previous = regions[index][:resume]
  native/ensure(regions[index][:restore] == parent[:captures][previous], "region restores only live captured registers")
  native/ensure(regions[index][:input] == module[:entry][:code][previous][1], "region receives delivered result")
end
let linked = native_source/link_relations(module[:methods][0][:program], {{:NativeResult -> 701}})
let changes = [instruction for instruction in linked[:code] if instruction[0] == :AssertDynamic]
native/ensure(len(changes) == 1 && changes[0][1] == 701, "relation names link to native catalogue IDs")
let literals = native_source/program_literals(parent, [])
native/ensure(native/contains(literals, "parent continuation") && native/contains(literals, 42), "source literals retained")
native/ensure(len(native_source/pool([42], 42.0)) == 2, "literal pool preserves numeric kinds")
native/ensure(native_source/symbol_text(:"line\nMontréal") == "line\nMontréal", "quoted symbol names survive lowering")
for source in [
  "let x = 1\ntry\n x = 2\nfinally\n x = x + 3\nend\nreturn x",
  "try\n return 1\nfinally\n return 2\nend",
  "let x = 0\nwhile x < 3\n try\n x = x + 1\n continue\n finally\n x = x + 1\n end\nend\nreturn x",
  "let x = 1\ntry\n x = 2\n let y = 1 / 0\ncatch E_DIV as error\n return [x, error.code]\nfinally\n x = x + 1\nend",
  "try\n try\n raise E_DIV\n finally\n let x = 7\n end\ncatch E_DIV\n return 8\nend"
]
  let parsed = parse_rows(source)
  let emitted = emit_program(parsed[:rows], parsed[:root])
  native/ensure(emitted[:errors] == [], to_literal(emitted[:errors]))
  let lowered = native_exceptions/lower(emitted[:entry])
  let checked = native_source/analyse(lowered, 0)
  native/ensure(len(checked[:reachable]) > 0, "exception flow is reachable")
end
for pc, captures in parent[:captures]
  native/ensure(len(captures) > 0, "parent must retain locals")
  let destination = module[:entry][:code][pc][1]
  native/ensure(!native/contains(captures, destination), "reply is supplied by resumption")
end
let live = native_source/analyse({{:registers -> 4, :code -> [
  [:Load, 0, 42], [:Load, 1, 99], [:Load, 2, []],
  [:MailboxRecv, 3, [:Register, 2], none],
  [:Binary, 3, :Add, 0, 3], [:Return, [:Register, 3]]]}}, 0)
native/ensure(live[:captures][3] == [0], "capture excludes dead values and delivered result")
let loop = native_source/analyse({{:registers -> 2, :code -> [
  [:Load, 0, true], [:Load, 1, 42], [:CommitValue, 0],
  [:Branch, 0, 2, 4], [:Return, [:Register, 1]]]}}, 0)
native/ensure(loop[:captures][2] == [1], "loop liveness reaches a fixed point")
return true
"#,
            ),
        );
        assert_eq!(checked, Value::bool(true));
        assert_eq!(
            eval(
                &mut runner,
                &format!(
                    r#"
let parsed = parse_rows({conformance_source:?})
native/ensure(parsed[:errors] == [], to_literal(parsed[:errors]))
let emitted = emit_program(parsed[:rows], parsed[:root])
native/ensure(emitted[:errors] == [], to_literal(emitted[:errors]))
for definition in emitted[:methods]
  let analysis = native_source/analyse(native_exceptions/lower(definition[:program]), len(definition[:parameters]))
  for region in native_source/regions(analysis)
    let starts = native_source/set([block[:start] for block in region[:blocks]])
    native/ensure(map_contains_key(starts, region[:start]), "region entry requires a block")
    for block in region[:blocks]
      if !analysis[:instructions][block[:last]][:boundary]
        for target in analysis[:instructions][block[:last]][:successors]
          native/ensure(map_contains_key(starts, target), "native edge must reach a block start")
        end
      end
    end
  end
end
return true
"#
                )
            ),
            Value::bool(true)
        );
        for (description, expected) in [
            (
                "{:registers -> 2, :code -> [[:Load, 0, true], [:Branch, 0, 2, 3], [:Load, 1, 42], [:Return, [:Register, 1]]]}",
                "read before assignment",
            ),
            (
                "{:registers -> 1, :code -> [[:Move, 0, 1], [:Return, [:Register, 0]]]}",
                "outside its frame",
            ),
            (
                "{:registers -> 0, :code -> [[:Jump, 1]]}",
                "leaves its program",
            ),
            (
                "{:registers -> 1, :code -> [[:BuildRelation, 0, [:x, :x], [], 0], [:Return, [:Register, 0]]]}",
                "duplicate columns",
            ),
            (
                "{:registers -> 1, :code -> [[:BuildRelation, 0, [:x], [], 1], [:Return, [:Register, 0]]]}",
                "cell count",
            ),
            (
                "{:registers -> 1, :code -> [[:BuildRelation, 0, [:x], [[:Splice, [:Constant, [1]]]], 1], [:Return, [:Register, 0]]]}",
                "cannot be spliced",
            ),
            (
                "{:registers -> 2, :code -> [[:Load, 0, none], [:RelationPattern, 1, 0, [:x], 1, [[:missing, 1]]], [:Return, [:Register, 1]]]}",
                "unknown column",
            ),
            (
                "{:registers -> 1, :code -> [[:ScanDynamic, 0, 1, [[:Query, 7]]], [:Return, [:Register, 0]]]}",
                "invalid source query variable",
            ),
            (
                "{:registers -> 0, :code -> [[:AssertDynamic, 1, [[:Hole]]], [:Return, [:Constant, 0]]]}",
                "invalid source relation hole",
            ),
            (
                "{:registers -> 2, :code -> [[:ScanDynamic, 0, 1, [[:Splice, [:Register, 1]]]], [:Return, [:Register, 0]]]}",
                "read before assignment",
            ),
            (
                "{:registers -> 1, :code -> [[:LoadFunction, 0, {}, [], 0, 0, false]]}",
                "invalid source function description",
            ),
        ] {
            let message = eval(
                &mut runner,
                &format!(
                    "try\n native_source/analyse({description}, 0)\n return \"accepted\"\ncatch E_INVARG as problem\n return native/error_message(problem)\nend"
                ),
            )
            .with_str(str::to_owned)
            .unwrap();
            assert!(message.contains(expected), "{description}: {message}");
        }
    }
}

// Generate all cases into one module: runtime generation dominates this test's
// cost, while adding another source expression is cheap. Expected results come
// from Rust execution, including error code, message, and operand payload.
fn source_expression_cases() -> (String, String, String) {
    let operations = [
        "-value",
        "!value",
        "left + right",
        "left - right",
        "left * right",
        "left / right",
        "left % right",
        "left == right",
        "left != right",
        "left < right",
        "left <= right",
        "left > right",
        "left >= right",
        "if left < right\n 1\nelseif left == right\n 2\nelse\n 3\nend",
        "begin\n let seen = 0\n let result = left && begin\n seen = seen + 1\n right\n end\n [result, seen]\nend",
        "begin\n let seen = 0\n let result = left || begin\n seen = seen + 1\n right\n end\n [result, seen]\nend",
        "begin\n let total = 0\n let i = 0\n let saved = [left, right]\n while i < left\n i = i + 1\n if i == 3\n continue\n end\n if i == right\n break\n end\n let item = [i, saved]\n total = total + item[0]\n end\n [total, i, saved]\nend",
        "begin\n let total = 0\n let i = 0\n while i < left\n let j = 0\n i = i + 1\n while j < right\n j = j + 1\n if i == j\n continue\n end\n total = total + 1\n end\n end\n total\nend",
        "begin\n if left < 0\n return left\n end\n if right < 0\n return right\n end\n left + right\nend",
        "if left\n right / 0\nelse\n right\nend",
        "left && (right / 0)",
        ":native_expression_2(right: right, left: left)",
        "native_expression_3(left, right)",
        "invoke(:native_expression_4, {:right -> right, :left -> left})",
        "begin\n let saved = [left, right]\n let first = native_expression_2(left, right)\n let second = :native_expression_3(left: first, right: right)\n [first, second, saved]\nend",
        "native_call_sum(left) + right",
        "begin\n let result = 0\n let index = 0\n while index < left\n result = native_expression_2(result, right)\n index = index + 1\n end\n result\nend",
        "native_default_values(left)",
        "native_default_values(left, right, 3, 4)",
        ":native_default_values(first: left, tail: [right, 3])",
        "native_default_none()",
        "native_default_option()",
        "native_default_bytes()",
        "native_default_values(@[left], @[right, 3], 4)",
        ":native_default_values(@{:first -> left}, @{:options -> right}, tail: [3, 4])",
        "[@left, 7, @right, 8]",
        ":native_default_values(@left, first: 7, @right)",
        "invoke(@[:native_expression_2, {:left -> left, :right -> right}])",
        "invoke(@left)",
        "native_default_only_rest(@left, @right)",
        "native_default_only_rest(@left, @right)[256]",
        "left[right]",
        "left[0..right]",
        "left[right.._]",
        "begin\n let changed = left\n let assigned = changed[right] = 7\n [left, changed, assigned]\nend",
        "[item for item in left]",
        "[[key, value] for key, value in left]",
        "[item + right for item in left if item >= 0]",
        "begin\n let total = right\n for item in left\n total = native_expression_2(total, item)\n end\n total\nend",
        "[item for item in left..right]",
        "begin\n let changed = left\n let assigned = changed[0][right] = 9\n [left, changed, assigned]\nend",
        "begin\n let total = 0\n for item in left\n if item == 2\n continue\n end\n if item == right\n break\n end\n total = total + item\n end\n total\nend",
        "begin\n let [first, @rest] = left\n [first, rest]\nend",
        "begin\n let changed = left\n let key = right\n let assigned = changed[0][key] = begin key = key + 1; 9 end\n [changed, assigned, key]\nend",
        "try\n left / right\ncatch E_DIV as error\n [error.code, error.message, error.value]\nend",
        "try\n raise E_INVARG, \"explicit error\", [left, right]\ncatch E_INVARG as error\n [error.code, error.message, error.value]\nend",
        "begin\n let changed = left\n try\n changed = changed + 1\n let ignored = changed / right\n catch E_DIV\n return changed\n end\n changed\nend",
        "begin\n let changed = left\n try\n changed = changed + right\n finally\n changed = changed + 1\n end\n changed\nend",
        "try\n return left\nfinally\n let ignored = right\nend",
        "try\n return left\nfinally\n return right\nend",
        "begin\n let changed = left\n try\n try\n changed = changed + 1\n let ignored = changed / right\n finally\n changed = changed + 10\n end\n catch E_DIV as error\n return [changed, error.code]\n end\n changed\nend",
        "begin\n let changed = left + 1\n try\n native_expression_5(changed, right)\n catch E_DIV as error\n [changed, error.code, error.value]\n end\nend",
        "try\n try\n raise E_INVARG, \"original\", left\n catch E_INVARG as error\n raise error\n end\ncatch E_INVARG as error\n [error.code, error.message, error.value]\nend",
        "begin\n let count = 0\n let trace = []\n while count < left\n try\n count = count + 1\n if count == right\n break\n end\n continue\n finally\n trace = [@trace, count]\n end\n end\n [count, trace]\nend",
        "try\n return left\nfinally\n try\n raise E_DIV\n catch E_DIV\n let ignored = right\n end\nend",
        "[E_DIV.code, E_DIV.message, E_DIV.value]",
        "try\n left.message\ncatch E_TYPE as error\n [error.code, error.value]\nend",
        "try\n raise E_INVARG, \"original\", left\ncatch E_INVARG as error\n raise error, \"replacement\", right\nend",
        "begin\n try\n let result = native_expression_2(left, right)\n catch E_DIV\n return 999\n end\n left / 0\nend",
        "begin\n let changed = left\n try\n return native_expression_2(left, right)\n finally\n changed = native_expression_2(changed, 1)\n end\nend",
        "len(left)",
        "string_len(left)",
        "string_append(left, right)",
        "map_contains_key(left, right)",
        "map_pairs(left)",
        "index_or(left, right, 99)",
        "try\n is_kind(left, right)\ncatch E_INVARG as error\n [error.code, error.message, error.value]\nend",
        "len(@left)",
        "index_or(@left)",
        "try\n is_kind(@left)\ncatch E_INVARG as error\n [error.code, error.message, error.value]\nend",
        "len()",
        "begin\n let data = left\n for i in 0..right\n data[i] = [i]\n end\n let pairs = map_pairs(data)\n [len(pairs), pairs[0], pairs[right]]\nend",
        "match left\ncase []\n []\ncase [1, @middle, 4] if len(middle) == right\n middle\ncase [first, @rest]\n [first, rest]\ncase _\n false\nend",
        "match left\ncase {:items -> [x, 2]}\n x\ncase {:present -> none}\n 7\ncase _\n false\nend",
        "match left\ncase none\n :none\ncase some(x)\n [:some, x]\ncase ok(x)\n [:ok, x]\ncase err(problem)\n [:err, problem.code]\ncase _\n :unmatched\nend",
        "[some(left), ok(right), some(ok([left, right]))]",
        "try\n raise E_INVARG, \"constructor payload\", left\ncatch E_INVARG as error\n let result = err(error)\n match result\n case err(problem)\n  [result, problem.code, problem.message, problem.value]\n end\nend",
        "begin\n let exactly {:b -> second, a} = left\n [a, second]\nend",
        "begin\n let result = []\n try\n for {:b -> second, a} in left\n  result = [@result, [a, second]]\n end\n catch E_MATCH as problem\n return [result, problem.code, problem.message, problem.value]\n finally\n result = [@result, [:finished]]\n end\n result\nend",
        "match left\ncase [2]\n right\nend",
        "begin\n let total = 0\n for item in left\n  try\n   match item\n   case 2\n    continue\n   case number\n    total = total + number\n   end\n  finally\n   total = total + 10\n  end\n end\n total\nend",
        "match left\ncase {:absent -> x}\n x\ncase {:present -> x}\n x\ncase _\n false\nend",
        "begin\n let exactly {} = left\n true\nend",
        "err(left)",
        "match left\ncase 1\n :one\ncase [@prefix, last]\n [prefix, last]\ncase _\n :other\nend",
        "match left\ncase [x] if native_expression_5(x, right) > 0\n x\ncase _\n false\nend",
        "begin\n let item: int = left\n item\nend",
        "left",
        "begin\n assert NativeQuery(left, right, 0)\n let result = NativeQuery(left, right, 0)\n retract NativeQuery(left, right, 0)\n [result, NativeQuery(_, _, _)]\nend",
        "begin\n assert NativeQuery(left, right, 0)\n let result = NativeQuery(_, right, _)\n retract NativeQuery(left, right, 0)\n result\nend",
        "begin\n assert NativeQuery(left, right, 0)\n let result = NativeQuery(?z, ?a, _)\n retract NativeQuery(left, right, 0)\n result\nend",
        "begin\n assert NativeQuery(left, right, 0)\n let result = NativeQuery(?same, ?same, _)\n retract NativeQuery(left, right, 0)\n result\nend",
        "NativeQuery(?absent, _, ?missing)",
        "begin\n assert NativeQuery(1, 2, 3)\n let result = NativeQuery(@left, ?middle, @right)\n retract NativeQuery(1, 2, 3)\n result\nend",
        "begin\n assert NativeQuery(1, 2, 2)\n let result = NativeQuery(@left, ?same, ?same)\n retract NativeQuery(1, 2, 2)\n result\nend",
        "begin\n assert NativeQuery(left, right, 0)\n assert NativeQuery(left, right, 1)\n let result = NativeQuery(?x, ?y, _)\n retract NativeQuery(left, right, 0)\n retract NativeQuery(left, right, 1)\n result\nend",
        "begin\n assert NativeQuery(left, right, 0)\n let result = [row[:x] for row in NativeQuery(?x, _, _)]\n retract NativeQuery(left, right, 0)\n result\nend",
        "NativeQuery(@left, @right)",
        "begin\n let add = fn(x) => x + left\n add(right)\nend",
        "begin\n let make = fn(x) => fn(y) => x + y\n let add = make(left)\n add(right)\nend",
        "begin\n let captured = left\n let read = fn() => captured\n captured = right\n [read(), captured]\nend",
        "begin\n let apply = fn(f, x) => f(x)\n apply(fn(x) => x + left, right)\nend",
        "begin\n let f = fn(?x = left, @tail) => [x, tail]\n [f(), f(right, 3, 4), f(@[right], @[5, 6])]\nend",
        "begin\n fn sum(n)\n if n == 0\n return 0\n end\n return n + sum(n - 1)\n end\n let alias = sum\n alias(left) + right\nend",
        "begin\n let divide = fn(x) => x / right\n try\n divide(left)\n catch E_DIV as problem\n [problem.code, problem.value]\n end\nend",
        "begin\n let f = fn(x: int) -> int => x + left\n try\n f(right)\n catch E_TYPE as problem\n problem.code\n end\nend",
        "begin\n let make = fn(x) => fn() => x\n let first = make(left)\n let second = make(left)\n let alias = first\n [first == second, first == alias, first < second, len({first -> 1, second -> 2, alias -> 3}), first(), second()]\nend",
        "begin\n let f = fn() => left\n let saved = [f, {f -> right}]\n [saved[0] == f, saved[1][f]]\nend",
        "begin\n let f = fn()\n let total = 0\n for i in 0..left\n total = total + native_expression_2(i, right)\n end\n return total\n end\n f()\nend",
        "begin\n let total = 0\n let previous = fn() => 0\n for i in 0..left\n let current = fn() => i\n if current != previous\n total = total + 1\n end\n previous = current\n end\n [total, previous()]\nend",
        "begin\n let f = fn()\n try\n return left\n finally\n return right\n end\n end\n f()\nend",
    ];
    let mut cases: Vec<(usize, Vec<&str>)> = Vec::new();
    for value in ["7", "2.5", "-36028797018963968", "true"] {
        cases.push((0, vec![value]));
    }
    for value in [
        "true",
        "false",
        "0",
        "0.0",
        "\"\"",
        "\"text\"",
        "[]",
        "[0]",
        "none",
        "{}",
        "{:key -> 0}",
        ":name",
        "E_TYPE",
    ] {
        cases.push((1, vec![value]));
    }
    for operation in 2..7 {
        for (left, right) in [
            ("12", "3"),
            ("-12", "5"),
            ("2.5", "0.5"),
            ("1", "1.0"),
            ("1.0", "1"),
            ("\"x\"", "true"),
        ] {
            cases.push((operation, vec![left, right]));
        }
    }
    cases.extend([
        (2, vec!["36028797018963967", "1"]),
        (3, vec!["-36028797018963968", "1"]),
        (4, vec!["36028797018963967", "2"]),
        (4, vec!["3.4e38", "2.0"]),
        (5, vec!["1", "0"]),
        (5, vec!["1.0", "0.0"]),
        (5, vec!["true", "0"]),
        (5, vec!["1", "0.0"]),
        (6, vec!["1", "0"]),
        (6, vec!["1.0", "0.0"]),
        (6, vec!["true", "0"]),
        (6, vec!["1.0", "0"]),
        (5, vec!["7", "2"]),
        (5, vec!["-36028797018963968", "-1"]),
        (6, vec!["-36028797018963968", "-1"]),
    ]);
    for operation in 7..13 {
        for (left, right) in [
            ("3", "3.0"),
            ("16777217", "16777216.0"),
            ("16777216.0", "16777217"),
            ("-36028797018963968", "-36028797018963968.0"),
            ("[1]", "[1.0]"),
            ("\"a\"", "\"b\""),
        ] {
            cases.push((operation, vec![left, right]));
        }
    }
    let mut source = String::new();
    for operands in [["0", "1"], ["1", "1"], ["2", "1"]] {
        cases.push((13, operands.to_vec()));
    }
    for operation in [14, 15] {
        for operands in [
            ["false", "7"],
            ["true", "8"],
            ["0", "9"],
            ["[]", "10"],
            ["[1]", "11"],
        ] {
            cases.push((operation, operands.to_vec()));
        }
    }
    for operands in [["0", "8"], ["10", "8"], ["1000", "2000"]] {
        cases.push((16, operands.to_vec()));
    }
    for operands in [["0", "5"], ["3", "4"], ["10", "8"]] {
        cases.push((17, operands.to_vec()));
    }
    for operands in [["-1", "8"], ["7", "-2"], ["7", "8"]] {
        cases.push((18, operands.to_vec()));
    }
    for operation in [19, 20] {
        for operands in [["false", "3"], ["true", "3"]] {
            cases.push((operation, operands.to_vec()));
        }
    }
    for operation in 21..27 {
        for operands in [["0", "2"], ["5", "3"]] {
            cases.push((operation, operands.to_vec()));
        }
    }
    cases.push((21, vec!["true", "1"]));
    for operation in 27..35 {
        for operands in [["1", "2"], ["-3", "9"]] {
            cases.push((operation, operands.to_vec()));
        }
    }
    for operands in [
        ["[]", "[]"],
        ["[1, 2]", "[3, 4]"],
        ["true", "[]"],
        ["[]", "7"],
    ] {
        cases.push((35, operands.to_vec()));
    }
    for operands in [
        ["{}", "{}"],
        ["{:first -> 1, :options -> 2}", "{:first -> 3}"],
        ["{1 -> 2}", "{3 -> 4}"],
    ] {
        cases.push((36, operands.to_vec()));
    }
    cases.push((37, vec!["1", "2"]));
    cases.push((38, vec!["[]", "0"]));
    cases.push((38, vec!["[:native_expression_2]", "0"]));
    cases.push((38, vec!["[:native_expression_2, {}, 3]", "0"]));
    for operands in [["[]", "[]"], ["[1, 2]", "[3, 4]"]] {
        cases.push((39, operands.to_vec()));
    }
    let long_arguments = format!(
        "[{}]",
        (0..257)
            .map(|value| format!("[\"retained across collection\", {value}]"))
            .collect::<Vec<_>>()
            .join(",")
    );
    cases.push((40, vec![&long_arguments, "[]"]));
    for operands in [
        ["[1, 2, 3]", "1"],
        ["[1]", "-1"],
        ["[1]", "1"],
        ["[1]", "1.0"],
        ["[]", "0"],
        ["true", "0"],
        ["{:answer -> [42]}", ":answer"],
        ["{}", ":missing"],
        ["{true -> 3, -1 -> 4}", "true"],
        ["{true -> 3, -1 -> 4}", "-1"],
        ["\"aé🙂\"", "0"],
        ["\"aé🙂\"", "1"],
        ["\"aé🙂\"", "2"],
        ["\"aé🙂\"", "3"],
        ["b\"AP8=\"", "1"],
        ["b\"\"", "0"],
        ["b\"AP8=\"", "-1"],
        ["[:name, :age] { [\"Ada\", 37], [\"Sam\", 25] }", "0"],
        ["[:name, :age] { [\"Ada\", 37], [\"Sam\", 25] }", "1"],
        ["[:name] {}", "0"],
        ["[] {[]}", "0"],
        ["[0, 1, 2, 3]", "1..2"],
        ["[0, 1, 2, 3]", "2.._"],
        ["[0, 1]", "-1..1"],
        ["[0, 1]", "1..0"],
        ["[0, 1]", "0..true"],
    ] {
        cases.push((41, operands.to_vec()));
    }
    for operation in [42, 43] {
        for index in ["-1", "0", "1", "2", "3", "4", "true"] {
            cases.push((operation, vec!["[10, 20, 30]", index]));
        }
    }
    for operands in [
        ["[1, 2]", "1"],
        ["[1]", "-1"],
        ["[1]", "1"],
        ["[1]", "true"],
        ["{}", ":added"],
        ["{:added -> [1]}", ":added"],
        ["{1 -> 2}", "[3]"],
        ["\"text\"", "0"],
        ["b\"AP8=\"", "0"],
        ["none", "0"],
    ] {
        cases.push((44, operands.to_vec()));
    }
    for operation in [45, 46] {
        for collection in [
            "[]",
            "[1, 2, 3]",
            "{3 -> [30], 1 -> [10]}",
            "{}",
            "\"aé🙂\"",
            "[:name, :age] { [\"Ada\", 37], [\"Sam\", 25] }",
            "[] {[]}",
            "none",
            "-2..2",
            "3..1",
            "1.._",
            "true",
            "b\"AP8=\"",
        ] {
            cases.push((operation, vec![collection, "0"]));
        }
    }
    cases.push((47, vec!["[-2, 0, 3]", "7"]));
    cases.push((48, vec!["[1, 2, 3]", "10"]));
    for operands in [
        ["-2", "2"],
        ["3", "1"],
        ["36028797018963966", "36028797018963967"],
        ["-36028797018963968", "-36028797018963967"],
    ] {
        cases.push((49, operands.to_vec()));
    }
    cases.push((50, vec!["[[1, 2], [3, 4]]", "1"]));
    cases.push((50, vec!["[{1 -> 2}]", "1"]));
    cases.push((51, vec!["[1, 2, 3, 4, 5]", "4"]));
    cases.push((52, vec!["[1, 2, 3]", "0"]));
    cases.push((52, vec!["[]", "0"]));
    cases.push((53, vec!["[[1, 2], [3, 4]]", "1"]));
    let mut ids = vec!["[602, 8003]".to_owned(), "[603, 8004]".to_owned()];
    for (index, operation) in operations.iter().enumerate() {
        let parameters = if index < 2 {
            "value"
        } else if index == 97 {
            "left: int, right"
        } else {
            "left, right"
        };
        source.push_str(&format!(
            "\nverb native_expression_{index}({parameters})\n return {operation}\nend\n"
        ));
        ids.push(format!("[{}, {}]", 1000 + index, 10000 + index));
    }
    cases.push((54, vec!["7", "0"]));
    cases.push((54, vec!["7", "2"]));
    cases.push((54, vec!["true", "2"]));
    cases.push((55, vec!["3", "4"]));
    cases.push((56, vec!["3", "0"]));
    cases.push((56, vec!["3", "2"]));
    cases.push((57, vec!["3", "4"]));
    cases.push((58, vec!["3", "4"]));
    cases.push((59, vec!["3", "4"]));
    cases.push((60, vec!["3", "0"]));
    cases.push((60, vec!["3", "2"]));
    cases.push((61, vec!["3", "0"]));
    cases.push((61, vec!["3", "2"]));
    cases.push((62, vec!["3", "4"]));
    cases.push((63, vec!["4", "2"]));
    cases.push((63, vec!["3", "9"]));
    cases.push((64, vec!["3", "4"]));
    cases.push((65, vec!["3", "4"]));
    cases.push((66, vec!["3", "4"]));
    cases.push((67, vec!["3", "4"]));
    cases.push((68, vec!["3", "4"]));
    cases.push((69, vec!["3", "4"]));
    cases.push((70, vec!["\"Aé🍁\"", "0"]));
    cases.push((70, vec!["b\"AAEC\"", "0"]));
    cases.push((70, vec!["[1, 2]", "0"]));
    cases.push((70, vec!["{:x -> none}", "0"]));
    cases.push((70, vec!["[:x] {[1], [2]}", "0"]));
    cases.push((70, vec!["none", "0"]));
    cases.push((70, vec!["0..3", "0"]));
    cases.push((71, vec!["\"Aé🍁\"", "0"]));
    cases.push((71, vec!["3", "0"]));
    cases.push((72, vec!["\"Aé\"", "\"🍁B\""]));
    cases.push((72, vec!["\"\"", "\"\""]));
    cases.push((72, vec!["1", "\"a\""]));
    cases.push((73, vec!["{:x -> none}", ":x"]));
    cases.push((73, vec!["{1 -> 7}", "1.0"]));
    cases.push((73, vec!["[]", ":x"]));
    cases.push((74, vec!["{2 -> [3], 1 -> none}", "0"]));
    cases.push((74, vec!["{}", "0"]));
    cases.push((74, vec!["4", "0"]));
    cases.push((75, vec!["\"Aé🍁\"", "2"]));
    cases.push((75, vec!["\"é\"", "1"]));
    cases.push((75, vec!["[none]", "0"]));
    cases.push((75, vec!["[2]", "1"]));
    cases.push((75, vec!["{:x -> none}", ":x"]));
    cases.push((75, vec!["{}", ":x"]));
    cases.push((75, vec!["[:x] {[7]}", "0"]));
    cases.push((75, vec!["none", "0"]));
    cases.push((75, vec!["[1]", "-1"]));
    cases.push((75, vec!["[1]", "0.0"]));
    cases.push((75, vec!["b\"AA==\"", "0"]));
    cases.push((76, vec!["[]", ":list"]));
    cases.push((76, vec!["none", ":relation"]));
    cases.push((76, vec!["2", ":float"]));
    cases.push((76, vec!["E_DIV", ":error_code"]));
    cases.push((76, vec!["true", ":bogus"]));
    cases.push((76, vec!["true", "4"]));
    cases.push((77, vec!["[[1, 2, 3]]", "0"]));
    cases.push((77, vec!["[]", "0"]));
    cases.push((77, vec!["[[], []]", "0"]));
    cases.push((78, vec!["[{:x -> none}, :x, 99]", "0"]));
    cases.push((79, vec!["[1, :int]", "0"]));
    cases.push((79, vec!["[]", "0"]));
    cases.push((79, vec!["[1, :int, 3]", "0"]));
    cases.push((80, vec!["0", "0"]));
    cases.push((81, vec!["{}", "128"]));
    cases.push((82, vec!["[]", "0"]));
    cases.push((82, vec!["[1, 4]", "0"]));
    cases.push((82, vec!["[1, 2, 3, 4]", "2"]));
    cases.push((82, vec!["[1, 2, 3, 4]", "9"]));
    cases.push((82, vec!["[1]", "0"]));
    cases.push((82, vec!["7", "0"]));
    cases.push((83, vec!["{:items -> [7, 2], :extra -> true}", "0"]));
    cases.push((83, vec!["{:items -> [7, 3]}", "0"]));
    cases.push((83, vec!["{:present -> none}", "0"]));
    cases.push((83, vec!["[:items] {[[8, 2]]}", "0"]));
    cases.push((83, vec!["[:items, :extra] {[[8, 2], 1]}", "0"]));
    cases.push((84, vec!["none", "0"]));
    cases.push((84, vec!["some([1, 2])", "0"]));
    cases.push((84, vec!["ok({:x -> 2})", "0"]));
    cases.push((84, vec!["err(error(E_TEST))", "0"]));
    cases.push((84, vec!["[] {}", "0"]));
    cases.push((84, vec!["[:value] {[1], [2]}", "0"]));
    cases.push((84, vec!["[:case, :value] {[:other, 3]}", "0"]));
    cases.push((84, vec!["[:case, :value, :extra] {[:ok, 3, 4]}", "0"]));
    cases.push((85, vec!["[1, {:nested -> none}]", "[2]"]));
    cases.push((85, vec!["none", "true"]));
    cases.push((86, vec!["[1, 2]", "0"]));
    cases.push((87, vec!["[:a, :b] {[1, 2]}", "0"]));
    cases.push((87, vec!["[:a, :b] {}", "0"]));
    cases.push((87, vec!["[:a, :b] {[1, 2], [3, 4]}", "0"]));
    cases.push((87, vec!["[:a, :b, :extra] {[1, 2, 3]}", "0"]));
    cases.push((87, vec!["{:a -> 1, :b -> 2}", "0"]));
    cases.push((88, vec!["[:a, :b] {[1, 2], [3, 4]}", "0"]));
    cases.push((88, vec!["[:a, :b] {}", "0"]));
    cases.push((
        88,
        vec!["[{:a -> 1, :b -> 2, :extra -> 3}, {:a -> 4, :b -> 5}]", "0"],
    ));
    cases.push((88, vec!["{1 -> {:a -> 2, :b -> 3}}", "0"]));
    cases.push((88, vec!["[{:a -> 1}]", "0"]));
    cases.push((88, vec!["[3]", "0"]));
    cases.push((88, vec!["[:a, :b, :extra] {[1, 2, 3]}", "0"]));
    cases.push((88, vec!["[{:a -> [1], :b -> [2]}, {:a -> [3]}]", "0"]));
    cases.push((89, vec!["[2]", "7"]));
    cases.push((89, vec!["[1]", "7"]));
    cases.push((89, vec!["true", "7"]));
    cases.push((90, vec!["[1, 2, 3]", "0"]));
    cases.push((91, vec!["{:present -> none}", "0"]));
    cases.push((91, vec!["{}", "0"]));
    cases.push((91, vec!["[:present] {[7]}", "0"]));
    cases.push((91, vec!["[:present] {}", "0"]));
    cases.push((92, vec!["[] {[]}", "0"]));
    cases.push((92, vec!["[] {}", "0"]));
    cases.push((92, vec!["none", "0"]));
    cases.push((93, vec!["7", "0"]));
    cases.push((93, vec!["error(E_TEST)", "0"]));
    cases.push((93, vec!["error(E_TEST, \"message\", [3])", "0"]));
    cases.push((94, vec!["1.0", "0"]));
    cases.push((94, vec!["[1, 2, 3]", "0"]));
    cases.push((94, vec!["[9]", "0"]));
    cases.push((94, vec!["[]", "0"]));
    cases.push((95, vec!["[8]", "2"]));
    cases.push((95, vec!["[8]", "0"]));
    cases.push((95, vec!["[]", "0"]));
    for operation in [96, 97] {
        for value in ["7", "true", "1.0", "\"text\"", "[]", "{}", "none", "E_TYPE"] {
            cases.push((operation, vec![value, "0"]));
        }
    }
    for operation in [98, 99, 100, 101, 102, 105, 106] {
        for operands in [
            ["1", "1"],
            ["1", "1.0"],
            ["[1, 2]", "[1, 2]"],
            ["\"café\"", "\"café\""],
        ] {
            cases.push((operation, operands.to_vec()));
        }
    }
    cases.push((103, vec!["[1]", "[3]"]));
    cases.push((103, vec!["[9]", "[3]"]));
    cases.push((103, vec!["[]", "[2, 3]"]));
    cases.push((103, vec!["[1, 2]", "[]"]));
    cases.push((104, vec!["[1]", "0"]));
    cases.push((104, vec!["[9]", "0"]));
    cases.push((107, vec!["[1, 2]", "[3]"]));
    for operation in 108..121 {
        cases.push((operation, vec!["5", "2"]));
    }
    cases.push((114, vec!["5", "0"]));
    cases.push((115, vec!["5", "\"wrong\""]));
    cases.push((119, vec!["300", "0"]));
    let mut oracle = SourceRunner::new_empty()
        .with_interpreter_only(true)
        .with_task_limits(TaskLimits {
            instruction_budget: 100_000_000,
            max_call_depth: 256,
            ..TaskLimits::default()
        });
    eval(&mut oracle, "return make_relation(:NativeQuery, 3)");
    let mut checks = String::new();
    source.push_str("\nverb native_control_spin()\n while true\n end\nend\n");
    ids.push("[2000, 20000]".to_owned());
    checks.push_str(&format!("static uint64_t (*const source_spin_entry)(struct mica_TaskAttempt *) = mica_source_roundtrip_method_{}_0;\n", operations.len() + 2));
    let recursive = "\nverb native_call_sum(number)\n if number == 0\n return 0\n end\n return number + native_call_sum(number - 1)\nend\n";
    source.push_str(recursive);
    ids.push("[2001, 20001]".to_owned());
    let defaults = [
        "verb native_default_values(first, ?options = {:nested -> [1, {2 -> 3.5}], :empty -> none}, @tail)\n return [first, options, tail]\nend",
        "verb native_default_none(?value)\n return value\nend",
        "verb native_default_option(?value = some([1, 2]))\n return value\nend",
        "verb native_default_bytes(?value = b\"AP8=\")\n return value\nend",
        "verb native_default_only_rest(@values)\n return values\nend",
    ];
    for (index, definition) in defaults.iter().enumerate() {
        source.push_str(&format!("\n{definition}\n"));
        ids.push(format!("[{}, {}]", 2010 + index, 20010 + index));
    }
    // Use the Mica frontend for syntax it already supports beyond Rust's
    // parser. The resulting register programs execute on the Rust VM oracle.
    for filein in [
        include_str!("../../../apps/compiler/lex.mica"),
        include_str!("../../../apps/compiler/parse.mica"),
        include_str!("../../../apps/compiler/ast.mica"),
        include_str!("../../../apps/compiler/emit.mica"),
        include_str!("../../../apps/compiler/install.mica"),
    ] {
        oracle.run_filein(filein).unwrap();
    }
    assert_eq!(
        eval(
            &mut oracle,
            &format!(
                "let module = emit_source({source:?})\nif !module[:ok]\nreturn module[:errors]\nend\nlet installed = compiler/install(module, :native_conformance_entry)\nif !installed[:ok]\nreturn installed[:errors]\nend\nreturn true"
            )
        ),
        Value::bool(true)
    );
    checks.push_str(&format!("static uint64_t (*const source_recursive_entry)(struct mica_TaskAttempt *) = mica_source_roundtrip_method_{}_0;\n", operations.len() + 3));
    checks.push_str("static uint64_t (*const source_splice_entry)(struct mica_TaskAttempt *) = mica_source_roundtrip_method_37_0;\n");
    checks.push_str("static uint64_t (*const source_named_entry)(struct mica_TaskAttempt *) = mica_source_roundtrip_method_23_0;\n");
    let mut entries = Vec::new();
    for (index, (operation, operands)) in cases.iter().enumerate() {
        let expression = operations[*operation];
        let bindings = if operands.len() == 1 {
            format!("let value = {}", operands[0])
        } else {
            format!("let left = {}\nlet right = {}", operands[0], operands[1])
        };
        let request = if *operation < 82 {
            // Preserve the independent Rust frontend check for shared syntax.
            format!("{bindings}\nreturn {expression}")
        } else {
            format!("{bindings}\nreturn :native_expression_{operation}(left: left, right: right)")
        };
        // Fresh native closure identity deliberately differs from bootstrap
        // interning. Pin the approved result rather than copying that policy.
        let request = if *operation == 116 {
            format!("{bindings}\nreturn [false, true, true, 2, left, left]")
        } else {
            request
        };
        let report = oracle.run_source(&request);
        let (status, value) = match report {
            Ok(report) => match report.outcome {
                TaskOutcome::Complete { value, .. } => (0, value),
                TaskOutcome::Aborted { error, .. } => (14, error),
                _ => panic!("expression suspended: {expression}"),
            },
            Err(mica_runtime::SourceTaskError::TaskManager(
                mica_runtime::TaskManagerError::Task(mica_runtime::TaskError::Runtime(
                    mica_vm::RuntimeError::InvalidBuiltinCall { .. },
                )),
            )) => (7, Value::option_none()),
            Err(error) => panic!(
                "{expression}, {operands:?}: {}",
                oracle.render_source_task_error(&error)
            ),
        };
        let operands_value = eval(&mut oracle, &format!("return [{}]", operands.join(",")));
        let mut serial = 0;
        let (literal, construction) = source_value_literal(&operands_value, &mut serial);
        checks.push_str(&format!("static mica_type_Value source_arguments_{index}(struct mica_MemoryWorker *worker, struct mica_SymbolTable *symbols) {{\n (void)symbols; // Most inputs contain no symbols.\n{construction}\n return {literal};\n}}\n"));
        checks.push_str(&format!("static void source_check_{index}(mica_type_Value actual, struct mica_SymbolTable *symbols, struct mica_MemoryWorker *worker) {{\n    (void)symbols; (void)worker; // Scalar checks need no allocation or interning.\n{}\n}}\n", source_value_check(&value, "actual")));
        entries.push(format!("{{mica_source_roundtrip_method_{}_0, {status}, source_arguments_{index}, source_check_{index}}}", operation + 2));
    }
    checks.push_str(&format!("static const struct {{\n    uint64_t (*entry)(struct mica_TaskAttempt *);\n    uint64_t status;\n    mica_type_Value (*arguments)(struct mica_MemoryWorker *, struct mica_SymbolTable *);\n    void (*check)(mica_type_Value, struct mica_SymbolTable *, struct mica_MemoryWorker *);\n}} source_cases[] = {{\n{}\n}};\n", entries.join(",\n")));
    (source, format!("[{}]", ids.join(",")), checks)
}

// Fixture data uses generated value constructors, with no collection boundary
// until the complete argument list has entered the task's rooted image.
fn source_value_literal(value: &Value, serial: &mut usize) -> (String, String) {
    let name = format!("literal_{}", *serial);
    *serial += 1;
    let mut setup = String::new();
    let expression = match value.kind() {
        ValueKind::Bool => {
            return (
                format!("mica_value_bool({})", value.as_bool().unwrap()),
                setup,
            );
        }
        ValueKind::Int => format!("mica_value_int(INT64_C({}))", value.as_int().unwrap()),
        ValueKind::Float => format!(
            "mica_value_float_from_bits(UINT32_C({}))",
            value.as_float().unwrap().to_bits()
        ),
        ValueKind::String => value
            .with_str(|text| {
                let bytes = text
                    .bytes()
                    .map(|byte| byte.to_string())
                    .collect::<Vec<_>>();
                setup.push_str(&format!(
                    "const uint8_t {name}_bytes[] = {{{}}};\n",
                    if bytes.is_empty() {
                        "0".to_owned()
                    } else {
                        bytes.join(",")
                    }
                ));
                format!("mica_value_string(worker, {name}_bytes, {})", text.len())
            })
            .unwrap(),
        ValueKind::List => value
            .with_list(|values| {
                let mut cells = Vec::new();
                for value in values.iter() {
                    let (cell, construction) = source_value_literal(value, serial);
                    setup.push_str(&construction);
                    cells.push(cell);
                }
                setup.push_str(&format!(
                    "const mica_type_Value {name}_cells[] = {{{}}};\n",
                    if cells.is_empty() {
                        "0".to_owned()
                    } else {
                        cells.join(",")
                    }
                ));
                format!("mica_value_list(worker, {name}_cells, {})", values.len())
            })
            .unwrap(),
        ValueKind::Map => value
            .with_map(|values| {
                let mut entries = Vec::new();
                for (key, value) in values.iter() {
                    let (key, construction) = source_value_literal(key, serial);
                    setup.push_str(&construction);
                    let (value, construction) = source_value_literal(value, serial);
                    setup.push_str(&construction);
                    entries.push(format!("{{{key}, {value}}}"));
                }
                setup.push_str(&format!(
                    "const struct mica_ValueMapEntry {name}_entries[] = {{{}}};\n",
                    if entries.is_empty() {
                        "{0, 0}".to_owned()
                    } else {
                        entries.join(",")
                    }
                ));
                format!("mica_value_map(worker, {name}_entries, {})", values.len())
            })
            .unwrap(),
        ValueKind::Symbol | ValueKind::ErrorCode => {
            let symbol = value
                .as_symbol()
                .or_else(|| value.as_error_code())
                .unwrap()
                .name()
                .unwrap();
            setup.push_str(&format!("struct mica_IdResult {name}_symbol = mica_value_symbol_intern(symbols, (const uint8_t *){symbol:?}, {});\nassert({name}_symbol.f_ok);\n", symbol.len()));
            let constructor = if value.kind() == ValueKind::Symbol {
                "symbol"
            } else {
                "error_code"
            };
            format!("mica_value_{constructor}((uint32_t){name}_symbol.f_number)")
        }
        ValueKind::Bytes => value
            .with_bytes(|bytes| {
                let data = bytes
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                setup.push_str(&format!(
                    "const uint8_t {name}_bytes[] = {{{}}};\n",
                    if data.is_empty() { "0" } else { &data }
                ));
                format!("mica_value_bytes(worker, {name}_bytes, {})", bytes.len())
            })
            .unwrap(),
        ValueKind::Error => value
            .with_error(|error| {
                let (code, construction) = source_value_literal(&Value::error_code(error.code()), serial);
                setup.push_str(&construction);
                let mut optional = |field: Option<Value>| {
                    let Some(field) = field else { return "0".to_owned() };
                    let (value, construction) = source_value_literal(&field, serial);
                    setup.push_str(&construction);
                    value
                };
                let message = optional(error.message().map(Value::string));
                let payload = optional(error.value().cloned());
                format!("mica_value_error(worker, (uint32_t)mica_value_as_error_code({code}).f_number, {}, {message}, {}, {payload})", error.message().is_some(), error.value().is_some())
            })
            .unwrap(),
        ValueKind::Range => value
            .with_range(|start, end| {
                let (start, construction) = source_value_literal(start, serial);
                setup.push_str(&construction);
                let (present, finish) = if let Some(end) = end {
                    let (end, construction) = source_value_literal(end, serial);
                    setup.push_str(&construction);
                    (true, end)
                } else {
                    (false, "0".to_owned())
                };
                format!("mica_value_range(worker, {start}, {present}, {finish})")
            })
            .unwrap(),
        ValueKind::Relation => value
            .with_relation(|relation| {
                let mut columns = Vec::new();
                for column in relation.heading() {
                    let (symbol, construction) =
                        source_value_literal(&Value::symbol(*column), serial);
                    setup.push_str(&construction);
                    columns.push(format!("(uint32_t)mica_value_as_symbol({symbol}).f_number"));
                }
                setup.push_str(&format!(
                    "const uint32_t {name}_heading[] = {{{}}};\n",
                    if columns.is_empty() {
                        "0".to_owned()
                    } else {
                        columns.join(",")
                    }
                ));
                let mut tuples = Vec::new();
                for (index, row) in relation.rows().iter().enumerate() {
                    let mut cells = Vec::new();
                    for cell in row.values() {
                        let (cell, construction) = source_value_literal(cell, serial);
                        setup.push_str(&construction);
                        cells.push(cell);
                    }
                    setup.push_str(&format!(
                        "mica_type_Value {name}_row_{index}[] = {{{}}};\n",
                        if cells.is_empty() {
                            "0".to_owned()
                        } else {
                            cells.join(",")
                        }
                    ));
                    tuples.push(format!(
                        "{{.f_arity = {}, .f_data = {name}_row_{index}}}",
                        cells.len()
                    ));
                }
                setup.push_str(&format!(
                    "const struct mica_ValueTuple {name}_rows[] = {{{}}};\n",
                    if tuples.is_empty() {
                        "{0}".to_owned()
                    } else {
                        tuples.join(",")
                    }
                ));
                format!(
                    "mica_value_relation(worker, {name}_heading, {}, {name}_rows, {})",
                    columns.len(),
                    tuples.len()
                )
            })
            .unwrap(),
        kind => panic!("unhandled conformance input kind: {kind:?}"),
    };
    setup.push_str(&format!(
        "struct mica_ValueResult {name} = {expression};\nassert({name}.f_ok);\n"
    ));
    (format!("{name}.f_value"), setup)
}

fn source_value_check(expected: &Value, actual: &str) -> String {
    match expected.kind() {
        ValueKind::Bool => format!("assert({actual} == mica_value_bool({}));", expected.as_bool().unwrap()),
        ValueKind::Int => format!("assert({actual} == mica_value_int(INT64_C({})).f_value);", expected.as_int().unwrap()),
        ValueKind::Float => format!("assert({actual} == mica_value_float_from_bits(UINT32_C({})).f_value);", expected.as_float().unwrap().to_bits()),
        ValueKind::String => expected.with_str(|text| {
            // Numeric byte arrays avoid C escaping and source-encoding ambiguity.
            let bytes = text.bytes().map(|byte| byte.to_string()).collect::<Vec<_>>();
            format!("{{ struct mica_HeapStringResult text = mica_value_as_string({actual}); assert(text.f_ok && text.f_header->f_length == {}); const unsigned char bytes[] = {{{}}}; assert(!memcmp(text.f_header->f_data, bytes, {})); }}", text.len(), if bytes.is_empty() { "0".to_owned() } else { bytes.join(",") }, text.len())
        }).unwrap(),
        ValueKind::List => expected.with_list(|values| {
            let mut checks = format!("assert(mica_value_list_length({actual}).f_ok && mica_value_list_length({actual}).f_number == {});\n", values.len());
            for (index, value) in values.iter().enumerate() {
                checks.push_str(&source_value_check(value, &format!("mica_value_list_get({actual}, {index}).f_value")));
                checks.push('\n');
            }
            checks
        }).unwrap(),
        ValueKind::Map | ValueKind::Relation | ValueKind::Bytes | ValueKind::Range | ValueKind::Symbol | ValueKind::ErrorCode | ValueKind::Error => {
            let (value, construction) = source_value_literal(expected, &mut 0);
            format!("{{ {construction}\n struct mica_IntResult compared = mica_value_compare({actual}, {value}); assert(compared.f_ok && compared.f_number == 0); }}")
        }
        kind => panic!("unhandled conformance result kind: {kind:?}"),
    }
}

#[test]
fn native_source_composite_literals_preserve_ownership() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        load_native_kernel(&mut runner, false);
        for source in [
            include_str!("../../../apps/native/compiler/collections.mica"),
            include_str!("../../../apps/native/compiler/access.mica"),
            include_str!("../../../apps/native/compiler/prepare.mica"),
        ] {
            runner.run_filein(source).unwrap();
        }
        assert_eq!(
            eval(
                &mut runner,
                r#"
let module = native_value/definitions()
module[:source_prefix] = "literal_fixture"
module = native_source/length_body(module)
module = native_access/definitions(module)
let description = native_source/literal_description(some({:nested -> [1, none, {2 -> 3.5}], :bytes -> b"AP8="}))
let [literal, prepared] = native_literal/prepare(module, "composite", description)
module = native_memory/declare(prepared, "literal_fixture", native_value/word(), [
  ["worker", native_memory/record_pointer("MemoryWorker"), :Borrow],
  ["symbols", native_memory/record_pointer("SymbolTable"), :Borrow]],
  [:ReadMemory, :WriteMemory, :Allocate, :Release, :Block], :Gc, "")
let failure = fn(failed) => native_roots/return_value(failed, native_gen/zero(native_value/word()))
let builder = native_roots/context(module, "literal_fixture", fn(failed, reason) => failure(failed))
let [value, ready] = native_literal/emit(builder, literal, native_gen/ref(builder, "symbols"), failure)
module = native_gen/finish(module, native_roots/return_value(ready, value))
native/check(module[:state])
return true
"#,
            ),
            Value::bool(true)
        );
    }
}

fn generate_native_source(
    interpreter_only: bool,
    conformance_source: &str,
    method_ids: &str,
) -> (String, String, String) {
    let example = format!(
        "{}\n{conformance_source}",
        include_str!("../../../apps/native/compiler/examples/roundtrip.mica")
    );
    generate_native_source_example(interpreter_only, &example, method_ids)
}

fn generate_native_source_example(
    interpreter_only: bool,
    example: &str,
    method_ids: &str,
) -> (String, String, String) {
    let generation_started = Instant::now();
    let mut runner = runner(interpreter_only).with_task_limits(TaskLimits {
        // The conformance module includes lifted closure bodies and continuations.
        instruction_budget: 20_000_000_000,
        max_call_depth: 256,
        ..TaskLimits::default()
    });
    load_native_kernel(&mut runner, true);
    load_native_source_compiler(&mut runner);
    // Bootstrap catalogue lookup uses a committed setup transaction. Native
    // catalogue installation and rollback are exercised by the C fixture.
    eval(
        &mut runner,
        "return make_functional_relation(:NativeResult, 2, [0])",
    );
    eval(&mut runner, "return make_relation(:NativeQuery, 3)");
    let module = eval(
        &mut runner,
        &format!(
            "return native_source/compile(native_tasks/definitions(), \"source_roundtrip\", {example:?}, {{:NativeResult -> 705, :NativeQuery -> 706}}, {method_ids})"
        ),
    );
    eprintln!(
        "native source: interpreter_only={interpreter_only}, module construction={:?}",
        generation_started.elapsed()
    );
    let emission_started = Instant::now();
    let state = module
        .map_get(&Value::symbol(Symbol::intern("state")))
        .unwrap();
    let mut request = SourceRunner::root_source_request("");
    request.input = TaskInput::Invocation {
        selector: Symbol::intern("native/emit_c"),
        roles: vec![(Symbol::intern("state"), state)],
    };
    let submitted = runner.submit_invocation(request).unwrap();
    let TaskOutcome::Complete { value, .. } = submitted.outcome else {
        panic!("C emission: {:?}", submitted.outcome);
    };
    let generated = value.with_str(str::to_owned).unwrap();
    eprintln!(
        "native source: interpreter_only={interpreter_only}, C emission={:?}",
        emission_started.elapsed()
    );
    eprintln!(
        "native source: interpreter_only={interpreter_only}, generation={:?}, C bytes={}",
        generation_started.elapsed(),
        generated.len()
    );
    let memory = eval(&mut runner, "return native_memory/platform()")
        .with_str(str::to_owned)
        .unwrap();
    let tasks = eval(&mut runner, "return native_tasks/platform()")
        .with_str(str::to_owned)
        .unwrap();
    (generated, memory, tasks)
}

#[test]
fn native_source_program_executes_on_generated_runtime() {
    let (conformance_source, method_ids, checks) = source_expression_cases();
    // Each bootstrap owns its runner. Generate concurrently, then require the
    // same C and compile it once; runtime generation dominates this test.
    thread::scope(|scope| {
        let builds = [true, false].map(|interpreter_only| {
            let source = &conformance_source;
            let ids = &method_ids;
            scope.spawn(move || generate_native_source(interpreter_only, source, ids))
        });
        let mut previous = None;
        for build in builds {
            let (generated, memory, tasks) = build.join().unwrap();
            if let Some(previous) = &previous {
                assert_eq!(&generated, previous);
                // Identical C has already passed the compiler and execution checks.
                continue;
            }
            previous = Some(generated.clone());
            let fixture = format!(
                "{generated}\n{memory}\n{tasks}\n{}\n{}\n#include <assert.h>\n{checks}\n{}\n#define MICA_SOURCE_CONFORMANCE\n{}",
                include_str!("../../../native/platform/allocation.c"),
                include_str!("../../../native/platform/mutex.c"),
                include_str!("../../../apps/native/compiler/conformance.c"),
                include_str!("../../../apps/native/compiler/tests.c")
            );
            check_native_source_fixture(&fixture);
        }
    });
}

#[test]
fn native_source_commits_suspends_and_delays_tasks() {
    let example = include_str!("../../../apps/native/compiler/examples/roundtrip.mica")
        .replace(
            "  let round = 0",
            "  suspend(0)\n  commit()\n  let round = 0",
        )
        .replace("mailbox_recv(receivers)", "mailbox_recv(receivers, 5)")
        .replace(
            "let count = 0",
            "let count = len(mailbox_recv([caps[0]], 0)) + len(mailbox_recv([caps[0]], 0.001))",
        )
        .replace("@[saved[1] + count])", "@[saved[1] + count]) after 0.002");
    let example = format!(
        "{example}\n{}",
        include_str!("../../../apps/native/compiler/time_tests.mica")
    );
    thread::scope(|scope| {
        let builds = [true, false].map(|tier| {
            let example = &example;
            scope.spawn(move || {
                generate_native_source_example(
                    tier,
                    example,
                    "[[602, 8003], [603, 8004], [610, 8010], [611, 8011]]",
                )
            })
        });
        let mut previous = None;
        for build in builds {
            let (generated, memory, tasks) = build.join().unwrap();
            if let Some(previous) = &previous {
                assert_eq!(&generated, previous);
                continue;
            }
            previous = Some(generated.clone());
            let fixture = format!(
                "{generated}\n{memory}\n{tasks}\n{}\n{}\n#include <assert.h>\n{}\n#define MICA_SOURCE_TIME\n{}",
                include_str!("../../../native/platform/allocation.c"),
                include_str!("../../../native/platform/mutex.c"),
                include_str!("../../../apps/native/compiler/time_tests.c"),
                include_str!("../../../apps/native/compiler/tests.c")
            );
            check_native_source_fixture(&fixture);
        }
    });
}

#[test]
fn native_source_updates_preserve_atomicity_and_authority() {
    check_native_source_body(
        include_str!("../../../apps/native/compiler/updates_tests.mica"),
        include_str!("../../../apps/native/compiler/updates_tests.c"),
    );
}

#[test]
fn native_source_sorts_and_constructs_relations() {
    check_native_source_body(
        include_str!("../../../apps/native/compiler/ordering_tests.mica"),
        include_str!("../../../apps/native/compiler/ordering_tests.c"),
    );
}

#[test]
fn native_source_emissions_obey_target_authority_and_transactions() {
    thread::scope(|scope| {
        let builds = [true, false].map(|tier| {
            scope.spawn(move || {
                generate_native_source_example(
                    tier,
                    include_str!("../../../apps/native/compiler/effects_tests.mica"),
                    "[[610, 8010]]",
                )
            })
        });
        let mut previous = None;
        for build in builds {
            let (generated, memory, tasks) = build.join().unwrap();
            if let Some(previous) = &previous {
                assert_eq!(&generated, previous);
                continue;
            }
            previous = Some(generated.clone());
            let fixture = format!(
                "{generated}\n{memory}\n{tasks}\n{}\n{}\n#include <assert.h>\n{}",
                include_str!("../../../native/platform/allocation.c"),
                include_str!("../../../native/platform/mutex.c"),
                include_str!("../../../apps/native/compiler/effects_tests.c"),
            );
            check_native_source_fixture(&fixture);
        }
    });
}

#[test]
fn native_source_constructs_and_slices_unicode_text() {
    check_native_source_body(
        include_str!("../../../apps/native/compiler/text_tests.mica"),
        include_str!("../../../apps/native/compiler/text_tests.c"),
    );
}

fn check_native_source_body(body: &str, extra_checks: &str) {
    let mut oracle = SourceRunner::new_empty().with_interpreter_only(true);
    oracle
        .run_filein(
            "make_functional_relation(:NativeResult, 2, [0])\nmake_relation(:NativeQuery, 3)",
        )
        .unwrap();
    let expected = eval(&mut oracle, body);
    let checks = format!(
        "static void check_source_value(mica_type_Value actual, struct mica_SymbolTable *symbols, struct mica_MemoryWorker *worker) {{ (void)symbols; (void)worker; {} }}",
        source_value_check(&expected, "actual")
    );
    let source = format!("verb native_body_fixture()\n{body}\nend");
    thread::scope(|scope| {
        let builds = [true, false].map(|tier| {
            let source = &source;
            scope.spawn(move || {
                generate_native_source(tier, source, "[[602, 8003], [603, 8004], [610, 8010]]")
            })
        });
        let mut previous = None;
        for build in builds {
            let (generated, memory, tasks) = build.join().unwrap();
            if let Some(previous) = &previous {
                assert_eq!(&generated, previous);
                continue;
            }
            previous = Some(generated.clone());
            let fixture = format!(
                "{generated}\n{memory}\n{tasks}\n{}\n{}\n#include <assert.h>\n{checks}\n{}\n{extra_checks}",
                include_str!("../../../native/platform/allocation.c"),
                include_str!("../../../native/platform/mutex.c"),
                include_str!("../../../apps/native/compiler/body_tests.c")
            );
            check_native_source_fixture(&fixture);
        }
    });
}

fn check_native_source_fixture(fixture: &str) {
    let scratch = Scratch::new();
    let source = scratch.0.join("source.c");
    let binary = scratch.0.join("source");
    fs::write(&source, fixture).unwrap();
    let sanitizer = if std::env::var_os("MICA_NATIVE_THREAD_SANITIZER").is_some() {
        "-fsanitize=thread"
    } else {
        "-fsanitize=address,undefined,float-cast-overflow"
    };
    let compiled = Command::new("clang")
        .args([
            "-std=c11",
            "-pthread",
            "-O2",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pedantic",
            "-ffp-contract=off",
            "-fno-fast-math",
            sanitizer,
            "-fno-sanitize-recover=all",
            "-fno-omit-frame-pointer",
        ])
        .arg(&source)
        .args(["-lm", "-ldl", "-o"])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(binary)
        .current_dir(&scratch.0)
        .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
        .env("TSAN_OPTIONS", "halt_on_error=1")
        .output()
        .unwrap();
    assert!(executed.status.success(), "{executed:?}");
    assert!(executed.stderr.is_empty(), "{executed:?}");
    eprintln!("native source: sanitizer execution passed");
}

fn load_native_kernel(runner: &mut SourceRunner, tasks: bool) {
    load_native_values(runner);
    for source in [
        include_str!("../../../apps/native/memory/roots.mica"),
        include_str!("../../../apps/native/rules/schema.mica"),
        include_str!("../../../apps/native/rules/builders.mica"),
        include_str!("../../../apps/native/rules/validate.mica"),
        include_str!("../../../apps/native/rules/stratify.mica"),
        include_str!("../../../apps/native/rules/catalog.mica"),
        include_str!("../../../apps/native/rules/program.mica"),
        include_str!("../../../apps/native/kernel/code_catalog.mica"),
        include_str!("../../../apps/native/kernel/program.mica"),
        include_str!("../../../apps/native/kernel/execution.mica"),
        include_str!("../../../apps/native/kernel/authority.mica"),
        include_str!("../../../apps/native/kernel/policy.mica"),
        include_str!("../../../apps/native/kernel/indexes.mica"),
        include_str!("../../../apps/native/kernel/relations.mica"),
        include_str!("../../../apps/native/kernel/transactions.mica"),
        include_str!("../../../apps/native/kernel/rule_indexes.mica"),
        include_str!("../../../apps/native/kernel/rules.mica"),
        include_str!("../../../apps/native/kernel/history.mica"),
        include_str!("../../../apps/native/kernel/lifecycle.mica"),
        include_str!("../../../apps/native/query/builders.mica"),
        include_str!("../../../apps/native/query/roots.mica"),
        include_str!("../../../apps/native/query/indexes.mica"),
        include_str!("../../../apps/native/query/operators.mica"),
        include_str!("../../../apps/native/query/sources.mica"),
        include_str!("../../../apps/native/query/probes.mica"),
        include_str!("../../../apps/native/query/program.mica"),
        include_str!("../../../apps/native/rules/execution_builders.mica"),
        include_str!("../../../apps/native/rules/lower.mica"),
        include_str!("../../../apps/native/rules/bindings.mica"),
        include_str!("../../../apps/native/rules/evaluate.mica"),
        include_str!("../../../apps/native/rules/maintain.mica"),
        include_str!("../../../apps/compiler/lex.mica"),
        include_str!("../../../apps/compiler/parse.mica"),
        include_str!("../../../apps/compiler/ast.mica"),
        include_str!("../../../apps/native/value/literals.mica"),
        include_str!("../../../apps/native/rules/source.mica"),
        include_str!("../../../apps/native/rules/install.mica"),
        include_str!("../../../apps/native/tests/rule_sources.mica"),
    ] {
        runner.run_filein(source).unwrap_or_else(|error| {
            panic!("{}", runner.render_source_task_error(&error));
        });
    }
    if tasks {
        for source in [
            include_str!("../../../apps/native/tasks/program.mica"),
            include_str!("../../../apps/native/tasks/platform.mica"),
            include_str!("../../../apps/native/tasks/code.mica"),
            include_str!("../../../apps/native/tasks/code_lifecycle.mica"),
            include_str!("../../../apps/native/tasks/code_tests.mica"),
            include_str!("../../../apps/native/tasks/images.mica"),
            include_str!("../../../apps/native/tasks/dispatch.mica"),
            include_str!("../../../apps/native/tasks/dispatch_bindings.mica"),
            include_str!("../../../apps/native/tasks/dispatch_context.mica"),
            include_str!("../../../apps/native/tasks/dispatch_positional.mica"),
            include_str!("../../../apps/native/tasks/dispatch_restrictions.mica"),
            include_str!("../../../apps/native/tasks/dispatch_signatures.mica"),
            include_str!("../../../apps/native/tasks/dispatch_selection.mica"),
            include_str!("../../../apps/native/tasks/attempts.mica"),
            include_str!("../../../apps/native/tasks/invocation.mica"),
            include_str!("../../../apps/native/tasks/handlers.mica"),
            include_str!("../../../apps/native/tasks/control.mica"),
            include_str!("../../../apps/native/tasks/boundaries.mica"),
            include_str!("../../../apps/native/tasks/execution.mica"),
            include_str!("../../../apps/native/tasks/calls.mica"),
            include_str!("../../../apps/native/tasks/functions.mica"),
            include_str!("../../../apps/native/tasks/queue.mica"),
            include_str!("../../../apps/native/tasks/services.mica"),
            include_str!("../../../apps/native/tasks/resources.mica"),
            include_str!("../../../apps/native/tasks/mailboxes.mica"),
            include_str!("../../../apps/native/tasks/mailbox_streams.mica"),
            include_str!("../../../apps/native/tasks/mailbox_lifecycle.mica"),
            include_str!("../../../apps/native/tasks/mailbox_delivery.mica"),
            include_str!("../../../apps/native/tasks/requests.mica"),
            include_str!("../../../apps/native/tasks/subscription_requests.mica"),
            include_str!("../../../apps/native/tasks/subscription_messages.mica"),
            include_str!("../../../apps/native/tasks/subscription_rows.mica"),
            include_str!("../../../apps/native/tasks/subscription_delivery.mica"),
            include_str!("../../../apps/native/tasks/subscription_catalogue.mica"),
            include_str!("../../../apps/native/tasks/subscription_pump.mica"),
            include_str!("../../../apps/native/tasks/subscription_events.mica"),
            include_str!("../../../apps/native/tasks/subscriptions.mica"),
            include_str!("../../../apps/native/tasks/subscription_lifecycle.mica"),
            include_str!("../../../apps/native/tasks/scheduler.mica"),
            include_str!("../../../apps/native/tasks/scheduler_launch.mica"),
            include_str!("../../../apps/native/tasks/spawn.mica"),
            include_str!("../../../apps/native/tasks/scheduler_lifecycle.mica"),
            include_str!("../../../apps/native/tasks/scheduler_admission.mica"),
            include_str!("../../../apps/native/tasks/scheduler_workers.mica"),
            include_str!("../../../apps/native/tasks/scheduler_events.mica"),
            include_str!("../../../apps/native/tasks/services_tests.mica"),
            include_str!("../../../apps/native/tasks/scheduler_tests.mica"),
            include_str!("../../../apps/native/tasks/handlers_tests.mica"),
            include_str!("../../../apps/native/tasks/calls_tests.mica"),
            include_str!("../../../apps/native/tasks/spawn_tests.mica"),
            include_str!("../../../apps/native/tasks/tests.mica"),
        ] {
            runner.run_filein(source).unwrap_or_else(|error| {
                panic!("{}", runner.render_source_task_error(&error));
            });
        }
    }
}

fn check_native_kernel_fixture(tasks: bool) {
    let mut previous = None;
    let sanitizer = if std::env::var_os("MICA_NATIVE_THREAD_SANITIZER").is_some() {
        "-fsanitize=thread"
    } else {
        "-fsanitize=address,undefined,float-cast-overflow"
    };
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only).with_task_limits(TaskLimits {
            // The task fixture also emits services and native event dispatchers.
            instruction_budget: if tasks { 12_000_000_000 } else { 8_000_000_000 },
            max_call_depth: 256,
            ..TaskLimits::default()
        });
        load_native_kernel(&mut runner, tasks);
        let expression = if tasks {
            "let state = native_tasks/test_program()\nreturn [native/emit_c(state), native/emit_c(native_code/test_library(state, false)), native/emit_c(native_code/test_library(state, true))]"
        } else {
            "return native/emit_c(native_rules/test_program())"
        };
        let output = eval(&mut runner, expression);
        let generated = if tasks {
            output.list_get(0).unwrap()
        } else {
            output.clone()
        }
        .with_str(str::to_owned)
        .unwrap();
        if let Some(previous) = &previous {
            assert_eq!(&generated, previous);
        }
        previous = Some(generated.clone());
        let platform = eval(&mut runner, "return native_memory/platform()")
            .with_str(str::to_owned)
            .unwrap();
        let platform = if tasks {
            format!(
                "{platform}\n{}",
                eval(&mut runner, "return native_tasks/platform()")
                    .with_str(str::to_owned)
                    .unwrap()
            )
        } else {
            platform
        };
        let scratch = Scratch::new();
        fs::write(
            scratch.0.join("measurements.c"),
            include_str!("../../../apps/native/kernel/measurements.c"),
        )
        .unwrap();
        let source = scratch.0.join("kernel.c");
        let binary = scratch.0.join("kernel");
        if tasks {
            for (index, name) in [(1, "code-library"), (2, "code-replacement")] {
                fs::write(
                    scratch.0.join(format!("{name}.c")),
                    output
                        .list_get(index)
                        .unwrap()
                        .with_str(str::to_owned)
                        .unwrap(),
                )
                .unwrap();
                let library = Command::new("clang")
                    .args([
                        "-std=c11", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror",
                    ])
                    .args([sanitizer, "-fno-sanitize-recover=all"])
                    .arg(scratch.0.join(format!("{name}.c")))
                    .arg("-o")
                    .arg(scratch.0.join(format!("{name}.so")))
                    .output()
                    .unwrap();
                assert!(library.status.success(), "{library:?}");
            }
            fs::write(
                scratch.0.join("code_tests.c"),
                include_str!("../../../apps/native/tasks/code_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("services_tests.c"),
                include_str!("../../../apps/native/tasks/services_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("mailboxes_tests.c"),
                include_str!("../../../apps/native/tasks/mailboxes_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("mailbox_streams_tests.c"),
                include_str!("../../../apps/native/tasks/mailbox_streams_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("dispatch_tests.c"),
                include_str!("../../../apps/native/tasks/dispatch_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("invocation_tests.c"),
                include_str!("../../../apps/native/tasks/invocation_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("spawn_tests.c"),
                include_str!("../../../apps/native/tasks/spawn_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("calls_tests.c"),
                include_str!("../../../apps/native/tasks/calls_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("requests_tests.c"),
                include_str!("../../../apps/native/tasks/requests_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscription_requests_tests.c"),
                include_str!("../../../apps/native/tasks/subscription_requests_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscriptions_tests.c"),
                include_str!("../../../apps/native/tasks/subscriptions_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscription_messages_tests.c"),
                include_str!("../../../apps/native/tasks/subscription_messages_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscription_rows_tests.c"),
                include_str!("../../../apps/native/tasks/subscription_rows_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscription_delivery_tests.c"),
                include_str!("../../../apps/native/tasks/subscription_delivery_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscription_catalogue_tests.c"),
                include_str!("../../../apps/native/tasks/subscription_catalogue_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("subscription_pump_tests.c"),
                include_str!("../../../apps/native/tasks/subscription_pump_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("scheduler_tests.c"),
                include_str!("../../../apps/native/tasks/scheduler_tests.c"),
            )
            .unwrap();
            fs::write(
                scratch.0.join("handlers_tests.c"),
                include_str!("../../../apps/native/tasks/handlers_tests.c"),
            )
            .unwrap();
        }
        fs::write(
            &source,
            format!(
                "{generated}\n#define MICA_MEMORY_TEST_ALLOCATOR\n{platform}\n{}\n{}\n{}",
                include_str!("../../../native/platform/allocation.c"),
                include_str!("../../../native/platform/mutex.c"),
                if tasks {
                    include_str!("../../../apps/native/tasks/tests.c")
                } else {
                    include_str!("../../../apps/native/kernel/tests.c")
                }
            ),
        )
        .unwrap();
        // Task entries require Clang musttail for bounded native stack use.
        let compiler = if tasks { "clang" } else { "cc" };
        let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| compiler.into()))
            .args([
                "-std=c11",
                "-pthread",
                "-O2",
                "-g",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-ffp-contract=off",
                "-fno-fast-math",
                sanitizer,
                "-fno-sanitize-recover=all",
                "-fno-omit-frame-pointer",
            ])
            .arg(&source)
            .args(["-lm", "-ldl", "-o"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new(binary)
            .current_dir(&scratch.0)
            .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
            .env("TSAN_OPTIONS", "halt_on_error=1")
            .output()
            .unwrap();
        assert!(executed.status.success(), "{executed:?}");
        assert!(executed.stderr.is_empty(), "{executed:?}");
    }
}

#[test]
fn typed_generation_preserves_evaluation_scopes_and_schema() {
    let fixtures = [
        (
            "generated_example",
            r#"
static uint64_t calls;
uint64_t mica_foreign_generated_tick(void) { return ++calls; }
int main(void) {
    for (uint64_t limit = 0; limit < 100; ++limit) {
        uint64_t expected = 0;
        for (uint64_t i = 1; i <= limit && i < 10; ++i) if (i != 3) expected += i;
        assert(mica_generated_sum(limit) == expected);
    }
    assert(mica_generated_sum(12) == 42);
    assert(mica_generated_order() == UINT64_MAX && calls == 2);
    calls = 0;
    assert(mica_generated_once() == 2 && calls == 1);
    calls = 0;
    assert(mica_generated_cleanup(0) == 1 && calls == 1);
    calls = 0;
    assert(mica_generated_cleanup(1) == 4 && calls == 2);
    for (uint64_t choice = 0; choice < 12; ++choice)
        assert(mica_generated_switch(choice) == (choice == 1 ? 10 : choice == 3 ? 3 : 0));
    calls = 0;
    assert(mica_generated_switch_once() == 10 && calls == 1);
    return 0;
}
"#,
        ),
        (
            "generated_range_example",
            r#"
int main(void) {
    for (uint64_t limit = 0; limit < 20; ++limit) {
        uint64_t expected = 0;
        for (uint64_t i = 0; i < limit; ++i) if (i != 3) expected += i;
        assert(mica_generated_range(limit) == expected);
        assert(mica_generated_nested_range(limit) == limit * expected + limit - (limit > 3));
    }
    return 0;
}
"#,
        ),
        (
            "generated_range_once_example",
            r#"
static uint64_t calls;
uint64_t mica_foreign_generated_tick(void) { return ++calls; }
int main(void) {
    assert(mica_generated_range_once() == 1 && calls == 3);
    return 0;
}
"#,
        ),
        (
            "generated_fields_example",
            r#"
int main(void) {
    struct mica_GeneratedPair pair = { .f_left = 7, .f_right = 11 };
    assert(mica_generated_field_value(pair) == 11);
    assert(mica_generated_field_pointer(&pair) == 7);
    mica_generated_field_write(&pair, 19);
    assert(pair.f_left == 7 && pair.f_right == 19);
    return 0;
}
"#,
        ),
        (
            "generated_hygiene_example",
            r#"
int main(void) {
    assert(mica_generated_hygiene(0) == 8);
    assert(mica_generated_hygiene(11) == 19);
    return 0;
}
"#,
        ),
    ];
    let failures = [
        ("generated_scope_failure", "reference escaped its scope"),
        (
            "generated_switch_label_failure",
            "switch label requires a constant",
        ),
        (
            "generated_const_failure",
            "write requires a mutable location",
        ),
        (
            "generated_owner_failure",
            "reference belongs to another function",
        ),
        (
            "generated_binding_failure",
            "reference requires its updated builder",
        ),
        (
            "generated_context_failure",
            "reference belongs to another context",
        ),
        (
            "generated_call_context_failure",
            "expression belongs to another context",
        ),
        (
            "generated_call_arity_failure",
            "generated call arity mismatch",
        ),
        (
            "generated_call_type_failure",
            "generated call argument type mismatch",
        ),
        (
            "generated_fork_failure",
            "reference does not match its binding",
        ),
        (
            "generated_field_const_failure",
            "write requires a mutable location",
        ),
    ];
    let mut previous = Vec::new();
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        runner
            .run_filein(include_str!("../../../apps/native/tests/structured.mica"))
            .unwrap();
        for (ordinal, (fixture, oracle)) in fixtures.iter().enumerate() {
            let generated = eval(
                &mut runner,
                &format!("return native/emit_c(native/{fixture}(0))"),
            )
            .with_str(str::to_owned)
            .unwrap();
            let renumbered = eval(
                &mut runner,
                &format!("return native/emit_c(native/{fixture}(1000))"),
            )
            .with_str(str::to_owned)
            .unwrap();
            assert_eq!(generated, renumbered, "{fixture}");
            if interpreter_only {
                previous.push(generated.clone());
            } else {
                assert_eq!(generated, previous[ordinal], "{fixture}");
            }
            let scratch = Scratch::new();
            let source = scratch.0.join("generated.c");
            let binary = scratch.0.join("generated");
            fs::write(
                &source,
                format!("{generated}\n#include <assert.h>\n{oracle}"),
            )
            .unwrap();
            let compiled = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
                .args([
                    "-std=c11",
                    "-O2",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-pedantic",
                    "-fsanitize=address,undefined",
                    "-fno-sanitize-recover=all",
                ])
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .arg("-lm")
                .output()
                .unwrap();
            assert!(
                compiled.status.success(),
                "{fixture}: {}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let executed = Command::new(&binary).output().unwrap();
            assert!(executed.status.success(), "{fixture}: {executed:?}");
            assert!(executed.stderr.is_empty(), "{fixture}: {executed:?}");
        }
        for (fixture, message) in failures {
            let report = runner
                .run_source(&format!("return native/{fixture}()"))
                .unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. }),
                "{fixture}: {}",
                report.render()
            );
            assert!(report.render().contains(message), "{}", report.render());
        }
    }
}

#[test]
fn gccjit_argument_scratch_rejects_nested_consumers() {
    let nested = r#"native_jit/j("record", ["ctx", "type", "n1", native_jit/argument_buffer()])"#;
    let failures = [
        (
            format!(
                r#"return native_jit/consume_arguments([{nested}], "record", ["ctx", "type"], [:Set, "value"])"#
            ),
            "nested JIT argument scratch preparation",
        ),
        (
            format!(
                r#"return native_jit/consume_arguments(["left"], "record", ["ctx", {nested}], [:Set, "value"])"#
            ),
            "nested JIT argument scratch consumer",
        ),
        (
            format!(
                r#"return native_jit/consume_prepared_arguments([native/do_statement({nested})], "record", ["ctx", "type"], "n1", [:Set, "value"])"#
            ),
            "nested JIT argument scratch preparation",
        ),
        (
            r#"return native_jit/consume_prepared_arguments([], "record", ["ctx", "type"], native_jit/argument_buffer(), [:Set, "value"])"#.to_owned(),
            "nested JIT argument scratch consumer",
        ),
        (
            r#"return native_jit/consume_arguments(["left"], "record", ["ctx", "type"], [:Set, native_jit/argument_buffer()])"#.to_owned(),
            "nested JIT argument scratch consumer",
        ),
        (
            r#"return native_jit/consume_arguments(["left"], "zero", ["ctx", "type"], [:Set, "value"])"#.to_owned(),
            "JIT argument consumer signature mismatch",
        ),
    ];
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for source in [
            include_str!("../../../apps/native/gccjit/data.mica"),
            include_str!("../../../apps/native/gccjit/bindings.mica"),
            include_str!("../../../apps/native/gccjit/program.mica"),
        ] {
            for report in runner.run_filein(source).unwrap() {
                assert!(
                    matches!(report.outcome, TaskOutcome::Complete { .. }),
                    "{}",
                    report.render()
                );
            }
        }
        // A slot-zero store uses the same address as the call argument buffer.
        // Preparation must accept this address without accepting nested calls.
        assert_eq!(
            eval(
                &mut runner,
                r#"
native_jit/consume_arguments(["left"], "record", ["ctx", "type"], [:Set, "value"])
native_jit/consume_arguments([], "record", ["ctx", "type"], [:Return])
native_jit/consume_arguments(["first", "second"], "fields", ["structure"], [:Do])
native_jit/consume_prepared_arguments([native_jit/publish(:Arguments, "n0", "left")], "record", ["ctx", "type"], "n1", [:Set, "value"])
return true
"#,
            ),
            Value::bool(true)
        );
        for (source, message) in &failures {
            let report = runner.run_source(source).unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Aborted { .. }),
                "{}",
                report.render()
            );
            assert!(report.render().contains(message), "{}", report.render());
        }
    }
}

#[test]
fn native_rule_source_preserves_terms_and_rejects_invalid_forms() {
    for interpreter_only in [true, false] {
        let mut runner = runner(interpreter_only);
        for source in [
            include_str!("../../../apps/compiler/lex.mica"),
            include_str!("../../../apps/compiler/parse.mica"),
            include_str!("../../../apps/compiler/ast.mica"),
            include_str!("../../../apps/native/rules/source.mica"),
            include_str!("../../../apps/native/value/program.mica"),
            include_str!("../../../apps/native/value/literals.mica"),
            include_str!("../../../apps/native/rules/install.mica"),
        ] {
            runner.run_filein(source).unwrap_or_else(|error| {
                panic!("{}", runner.render_source_task_error(&error));
            });
        }
        assert_eq!(
            eval(
                &mut runner,
                r#"
require native_literal/utf8("Aé€🙂") == [65, 195, 169, 226, 130, 172, 240, 159, 153, 130]
require native_literal/utf8("") == []
require native_literal/integer_encoding(-36028797018963968) == "-0080000000000000"
require native_literal/integer_encoding(36028797018963967) == "007fffffffffffff"
require native_literal/integer_encoding(-7) == "-0000000000000007"
require native_literal/float_bits(1.5) == "3fc00000"
require native_literal/float_bits(-1.5) == "bfc00000"
require native_literal/float_bits(0.0) == "00000000"
require native_literal/float_bits(1.401298464324817e-45) == "00000001"
let relations = {"Reach" -> 11, "Edge" -> 12, "Blocked" -> 13}
let source = "Reach(x, z) :- Edge(?x, y), Reach(y, z), not Blocked(z), x != -1.5"
let compiled = native_rules/parse_source(source, relations)
require compiled[:ok]
let rule = compiled[:rules][0]
require rule[:source] == source
require rule[:head][:relation] == 11
require rule[:head][:terms] == [{:kind -> :Variable, :id -> 0}, {:kind -> :Variable, :id -> 1}]
require rule[:atoms][0][:terms] == [{:kind -> :Variable, :id -> 0}, {:kind -> :Variable, :id -> 2}]
require rule[:atoms][1][:terms] == [{:kind -> :Variable, :id -> 2}, {:kind -> :Variable, :id -> 1}]
require rule[:atoms][2][:negative]
require rule[:guards] == [{:operation -> 1, :left -> {:kind -> :Variable, :id -> 0}, :right -> {:kind -> :Constant, :literal -> {:kind -> :Float, :value -> -1.5}}}]
let constants = native_rules/parse_source("Reach(#native, :\"é\") :- Edge(-7, true), Edge(\"hi\", b\"AP8=\"), Edge(E_DIV, _)", relations, {"native" -> 42})
require constants[:ok]
let values = constants[:rules][0]
require values[:head][:terms][0][:literal] == {:kind -> :Identity, :value -> 42}
require values[:head][:terms][1][:literal] == {:kind -> :Symbol, :value -> "é"}
require values[:atoms][0][:terms][0][:literal] == {:kind -> :Integer, :value -> -7}
require values[:atoms][0][:terms][1][:literal] == {:kind -> :Bool, :value -> true}
require values[:atoms][1][:terms][0][:literal] == {:kind -> :String, :value -> "hi"}
require values[:atoms][1][:terms][1][:literal][:kind] == :Bytes
require values[:atoms][2][:terms][0][:literal] == {:kind -> :ErrorCode, :value -> "E_DIV"}
require values[:atoms][2][:terms][1] == {:kind -> :Hole}
let separate = native_rules/parse_source("Reach(a, b) :- Edge(a, b)\nReach(c, d) :- Edge(c, d)", relations)
require separate[:ok]
require separate[:rules][0][:head][:terms] == separate[:rules][1][:head][:terms]
for bad in ["return 1", "Reach(x, y) :- Missing(x, y)", "Reach(#missing, x) :- Edge(x, y)", "Reach(_, x) :- Edge(x, y)", "Reach(x, y) :- not Edge(_, y)", "Reach(x, y) :- Edge(x, y), x == _", "Reach(x, y) :- Edge(first: x, y)", "Reach(x, y) :- Edge(x + 1, y)", "Reach(x, y) :- Edge(x, y), x + y", "Reach(x, y) :- Edge(x, y),"]
  let rejected = native_rules/parse_source(bad, relations)
  require !rejected[:ok]
  require len(rejected[:errors]) > 0
  require rejected[:rules] == []
end
return true
"#
            ),
            Value::bool(true)
        );
    }
}
