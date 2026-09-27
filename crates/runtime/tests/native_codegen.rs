// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mica_runtime::{SourceRunner, TaskLimits, TaskOutcome};
use mica_var::Value;

const SOURCES: &[&str] = &[
    include_str!("../../../apps/native/ir.mica"),
    include_str!("../../../apps/native/types.mica"),
    include_str!("../../../apps/native/numeric.mica"),
    include_str!("../../../apps/native/layout.mica"),
    include_str!("../../../apps/native/storage.mica"),
    include_str!("../../../apps/native/contracts.mica"),
    include_str!("../../../apps/native/check.mica"),
    include_str!("../../../apps/native/c.mica"),
    include_str!("../../../apps/native/examples/scalars.mica"),
    include_str!("../../../apps/native/examples/values.mica"),
];

fn runner(interpreter_only: bool) -> SourceRunner {
    let mut runner = SourceRunner::new_empty()
        .with_interpreter_only(interpreter_only)
        .with_task_limits(TaskLimits {
            instruction_budget: 100_000_000,
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
let expected = native/emit_c(state)
for table in [:functions, :parameters, :locals, :blocks, :instructions, :terminators, :constants,
              :records, :fields, :foreign, :contracts, :parameter_contracts]
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
