// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static uint64_t run_algebra(unsigned operation, struct mica_MemoryWorker *worker,
    struct mica_TaskAttempt *attempt, struct mica_KernelTransaction *tx,
    struct mica_MemoryRoot *left, struct mica_MemoryRoot *right,
    struct mica_MemoryRoot *arguments, struct mica_MemoryRoot *output) {
    mica_type_Value a = mica_value_root_get(left), b = mica_value_root_get(right);
    switch (operation) {
    case 0: return mica_source_roundtrip_algebra_union(worker, attempt, tx, a, b, output);
    case 1: return mica_source_roundtrip_algebra_difference(worker, attempt, tx, a, b, output);
    case 2: return mica_source_roundtrip_algebra_natural_join(worker, attempt, tx, a, b, output);
    default: return mica_source_roundtrip_algebra_project(worker, attempt, tx,
        mica_value_root_get(arguments), output);
    }
}
struct AlgebraCancellation {
    struct mica_KernelTransaction *transaction;
    unsigned checks;
};
static bool algebra_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct AlgebraCancellation *state = (void *)context;
    if (++state->checks != 2) return true;
    assert(mica_kernel_cancel(state->transaction));
    return false;
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code;
    struct mica_MemoryRoot left = {0}, right = {0}, arguments = {0}, output = {0}, text = {0};
    assert(mica_memory_root_push(worker, &left) && mica_memory_root_push(worker, &right));
    assert(mica_memory_root_push(worker, &arguments) && mica_memory_root_push(worker, &output));
    assert(mica_memory_root_push(worker, &text));
    const char *names[] = {"key", "payload", "missing"};
    uint32_t heading[3];
    for (unsigned i = 0; i < 3; ++i) {
        struct mica_IdResult id = mica_value_symbol_intern(symbols,
            (const uint8_t *)names[i], strlen(names[i]), NULL);
        assert(id.f_ok);
        heading[i] = (uint32_t)id.f_number;
    }
    struct mica_ValueResult value = mica_value_string(worker, (const uint8_t *)"retained", 8);
    assert(value.f_ok);
    mica_value_root_set(&text, value.f_value);
    mica_type_Value cells[128][2];
    struct mica_ValueTuple rows[128];
    for (unsigned side = 0; side < 2; ++side) {
        for (unsigned i = 0; i < 128; ++i) {
            cells[i][0] = source_integer(i + side * 64);
            cells[i][1] = mica_value_root_get(&text);
            rows[i] = (struct mica_ValueTuple){.f_data = cells[i], .f_arity = 2};
        }
        value = mica_value_relation(worker, heading, 2, rows, 128);
        assert(value.f_ok);
        mica_value_root_set(side ? &right : &left, value.f_value);
    }
    mica_type_Value args[] = {mica_value_root_get(&left), mica_value_symbol(heading[0]).f_value};
    value = mica_value_list(worker, args, 2);
    assert(value.f_ok);
    mica_value_root_set(&arguments, value.f_value);
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    const uint64_t lengths[] = {192, 64, 64, 128};
    for (unsigned operation = 0; operation < 4; ++operation) {
        struct mica_KernelTransaction tx = {0};
        assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
        struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
        worker->f_heap->f_collection_limit = 0;
        uint64_t collections = worker->f_heap->f_collections;
        struct mica_MemoryRoot *roots = worker->f_roots;
        assert(run_algebra(operation, worker, &attempt, &tx, &left, &right, &arguments, &output) == 0);
        assert(worker->f_heap->f_collections > collections && worker->f_roots == roots);
        struct mica_IdResult length = mica_value_relation_length(mica_value_root_get(&output));
        assert(length.f_ok && length.f_number == lengths[operation]);
        assert(attempt.f_remaining < 1000000);
        mica_value_root_set(&output, source_integer(99));
        attempt.f_remaining = 4;
        assert(run_algebra(operation, worker, &attempt, &tx, &left, &right, &arguments, &output) == 10);
        assert(mica_value_root_get(&output) == source_integer(99) && worker->f_roots == roots);
        attempt.f_remaining = 1000000;
        uint64_t bytes = mica_kernel_work(&tx)->f_bytes_limit;
        mica_kernel_work(&tx)->f_bytes_limit = 1;
        assert(run_algebra(operation, worker, &attempt, &tx, &left, &right, &arguments, &output) == 10);
        assert(mica_value_root_get(&output) == source_integer(99) && worker->f_roots == roots);
        mica_kernel_work(&tx)->f_bytes_limit = bytes;
        struct AlgebraCancellation state = {.transaction = &tx};
        struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = algebra_cancel};
        mica_memory_control_push(worker, &control);
        assert(run_algebra(operation, worker, &attempt, &tx, &left, &right, &arguments, &output) == 11);
        assert(state.checks == 2 && worker->f_control == &control && worker->f_roots == roots);
        assert(mica_value_root_get(&output) == source_integer(99));
        mica_memory_control_pop(worker);
        assert(mica_kernel_end(&tx));
    }
    worker->f_heap->f_collection_limit = collection_limit;
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    // Invalid inputs leave the caller's output unchanged.
    for (unsigned kind = 0; kind < 5; ++kind) {
        mica_type_Value invalid[] = {mica_value_root_get(&left), mica_value_symbol(heading[0]).f_value,
            mica_value_symbol(heading[0]).f_value};
        uint64_t count = 2;
        if (kind == 0) count = 0;
        if (kind == 1) invalid[0] = source_integer(1);
        if (kind == 2) invalid[1] = source_integer(1);
        if (kind == 3) invalid[1] = mica_value_symbol(heading[2]).f_value;
        if (kind == 4) count = 3;
        value = mica_value_list(worker, invalid, count);
        assert(value.f_ok);
        mica_value_root_set(&arguments, value.f_value);
        assert(run_algebra(3, worker, &attempt, &tx, &left, &right, &arguments, &output) == 7);
        assert(mica_value_root_get(&output) == source_integer(99));
    }
    value = mica_value_relation(worker, heading + 1, 2, NULL, 0);
    assert(value.f_ok);
    mica_value_root_set(&right, value.f_value);
    for (unsigned operation = 0; operation < 2; ++operation) {
        assert(run_algebra(operation, worker, &attempt, &tx, &left, &right, &arguments, &output) == 7);
        assert(mica_value_root_get(&output) == source_integer(99));
    }
    mica_value_root_set(&right, source_integer(1));
    for (unsigned operation = 0; operation < 3; ++operation)
        assert(run_algebra(operation, worker, &attempt, &tx, &left, &right, &arguments, &output) == 7);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &text) && mica_memory_root_pop(worker, &output));
    assert(mica_memory_root_pop(worker, &arguments) && mica_memory_root_pop(worker, &right));
    assert(mica_memory_root_pop(worker, &left));
}
