// Copied into tools/paritybench of an isolated Odin revision by capture.py.
package main

import "core:encoding/json"
import "core:fmt"
import "core:os"
import "core:strconv"
import "core:time"
import k "../../mica/kernel"
import r "../../mica/runtime"
import v "../../mica/var"
import vm "../../mica/vm"

Report :: struct {
    format: int,
    implementation: string,
    fixture: string,
    expected: string,
    tier: string,
    workers: int,
    relation_parallelism: int,
    accelerator: string,
    accelerator_placements: int,
    storage: string,
    durability: string,
    authority: string,
    instruction_budget: i64,
    max_call_depth: int,
    warmup_invocations: int,
    iterations_per_sample: int,
    timed_invocations: int,
    sample_elapsed_ns: []i64,
}

fail :: proc(message: string) {
    fmt.eprintln(message)
    os.exit(1)
}

number :: proc(text: string) -> int {
    value, ok := strconv.parse_int(text)
    if !ok || value < 0 {fail("invalid nonnegative count")}
    return value
}

invoke :: proc(world: ^r.World, expected: v.Value) {
    outcome := r.world_call(world, "bench", nil)
    if outcome.kind != .Complete {
        fmt.eprintf("%s: %s\n", outcome.message,
            r.world_value_literal(world, outcome.error, context.temp_allocator))
        os.exit(1)
    }
    if !v.value_eq(outcome.value, expected) {
        fmt.eprintf("result mismatch: expected %s, got %s\n",
            r.world_value_literal(world, expected, context.temp_allocator),
            r.world_value_literal(world, outcome.value, context.temp_allocator))
        os.exit(1)
    }
}

main :: proc() {
    if len(os.args) != 8 {fail("usage: paritybench FILE EXPECTED SAMPLES ITERATIONS WARMUP WORKERS SETUP")}
    path, literal := os.args[1], os.args[2]
    samples, iterations := number(os.args[3]), number(os.args[4])
    warmup, workers := number(os.args[5]), number(os.args[6])
    if samples == 0 || iterations == 0 || workers == 0 {fail("samples, iterations, and workers must be positive")}
    kernel: k.Kernel
    k.kernel_init(&kernel)
    defer k.kernel_destroy(&kernel)
    world, start := r.world_start(&kernel, []string{path}, context.allocator,
        r.World_Config{workers = workers, accel = .Cpu, instruction_budget = 100_000_000})
    if !start.ok {fail(start.message)}
    defer r.world_destroy(world)
    if entry := r.world_wait(world, world.entry); entry.kind != .Complete {fail(entry.message)}
    expected_number, expected_ok := strconv.parse_i64(literal)
    if !expected_ok {fail("expected result must be an integer")}
    expected, expected_in_range := v.value_int(expected_number)
    if !expected_in_range {fail("expected integer is out of range")}
    if os.args[7] == "yes" {
        if setup := r.world_call(world, "setup", nil); setup.kind != .Complete {fail(setup.message)}
    }
    for _ in 0..<warmup {invoke(world, expected)}
    elapsed := make([]i64, samples)
    defer delete(elapsed)
    for i in 0..<samples {
        before := time.tick_now()
        for _ in 0..<iterations {invoke(world, expected)}
        elapsed[i] = i64(time.tick_diff(before, time.tick_now()))
    }
    report := Report{
        format = 1, implementation = "odin", fixture = path, expected = literal,
        tier = "interpreter", workers = workers, relation_parallelism = 1,
        accelerator = "disabled", accelerator_placements = 0,
        storage = "memory", durability = "none", authority = "root",
        instruction_budget = 100_000_000, max_call_depth = vm.DEFAULT_MAX_CALL_DEPTH,
        warmup_invocations = warmup, iterations_per_sample = iterations,
        timed_invocations = samples * iterations, sample_elapsed_ns = elapsed,
    }
    encoded, err := json.marshal(report)
    if err != nil {fail("cannot encode benchmark report")}
    defer delete(encoded)
    fmt.println(string(encoded))
}
