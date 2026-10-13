// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

struct DistanceCancellation {
    struct mica_KernelTransaction *transaction;
    unsigned checks;
};
static bool distance_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct DistanceCancellation *state = (void *)context;
    if (++state->checks != 2) return true;
    assert(mica_kernel_cancel(state->transaction));
    return false;
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code; (void)symbols;
    struct mica_MemoryRoot left = {0}, right = {0}, short_text = {0}, output = {0};
    assert(mica_memory_root_push(worker, &left) && mica_memory_root_push(worker, &right));
    assert(mica_memory_root_push(worker, &short_text) && mica_memory_root_push(worker, &output));
    uint8_t bytes[512];
    memset(bytes, 'x', sizeof(bytes));
    struct mica_ValueResult text = mica_value_string(worker, bytes, sizeof(bytes));
    assert(text.f_ok);
    mica_value_root_set(&left, text.f_value);
    memset(bytes, 'y', sizeof(bytes));
    text = mica_value_string(worker, bytes, 64);
    assert(text.f_ok);
    mica_value_root_set(&right, text.f_value);
    text = mica_value_string(worker, bytes, 1);
    assert(text.f_ok);
    mica_value_root_set(&short_text, text.f_value);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_KernelWork *work = mica_kernel_work(&tx);
    uint64_t limit = work->f_bytes_limit;
    // A one-scalar operand keeps workspace small in either argument order.
    work->f_bytes_limit = 1024;
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        mica_value_root_get(&left), mica_value_root_get(&short_text), &output) == 0);
    assert(mica_value_root_get(&output) == source_integer(512));
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        mica_value_root_get(&short_text), mica_value_root_get(&left), &output) == 0);
    assert(mica_value_root_get(&output) == source_integer(512));
    mica_value_root_set(&output, source_integer(99));
    mica_kernel_work(&tx)->f_bytes_limit = 1;
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        mica_value_root_get(&left), mica_value_root_get(&right), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    mica_kernel_work(&tx)->f_bytes_limit = limit;
    attempt.f_remaining = 200;
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        mica_value_root_get(&left), mica_value_root_get(&right), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    worker->f_heap->f_collection_limit = 0;
    uint64_t collections = worker->f_heap->f_collections;
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        mica_value_root_get(&left), mica_value_root_get(&right), &output) == 0);
    assert(worker->f_heap->f_collections > collections);
    assert(mica_value_root_get(&output) == source_integer(512));
    assert(attempt.f_remaining < 1000000 - 512 * 64);
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        source_integer(1), mica_value_root_get(&right), &output) == 7);
    mica_value_root_set(&output, source_integer(99));
    attempt.f_remaining = 1000000;
    worker->f_heap->f_collection_limit = 0;
    // Cancel after the first scratch array is rooted, while reserving the second.
    struct DistanceCancellation state = {.transaction = &tx};
    struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = distance_cancel};
    struct mica_MemoryRoot *roots = worker->f_roots;
    mica_memory_control_push(worker, &control);
    uint64_t cancelled = mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        mica_value_root_get(&left), mica_value_root_get(&right), &output);
    if (cancelled != 11) fprintf(stderr, "distance cancellation: status=%llu checks=%u\n",
        (unsigned long long)cancelled, state.checks);
    assert(cancelled == 11);
    assert(state.checks == 2 && worker->f_control == &control && worker->f_roots == roots);
    assert(mica_value_root_get(&output) == source_integer(99));
    mica_memory_control_pop(worker);
    text = mica_value_string(worker, NULL, 0);
    assert(text.f_ok);
    // The empty-input fast path must also observe cancellation.
    assert(mica_source_roundtrip_text_edit_distance(worker, &attempt, &tx,
        text.f_value, mica_value_root_get(&right), &output) == 11);
    assert(mica_value_root_get(&output) == source_integer(99));
    worker->f_heap->f_collection_limit = collection_limit;
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &output) && mica_memory_root_pop(worker, &short_text));
    assert(mica_memory_root_pop(worker, &right) && mica_memory_root_pop(worker, &left));
}
