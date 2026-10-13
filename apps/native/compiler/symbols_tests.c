// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

struct SourceSymbolCancel {
    struct mica_KernelTransaction *transaction;
    unsigned checks;
};

static bool source_symbol_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct SourceSymbolCancel *state = (void *)context;
    if (++state->checks != 5) return true;
    assert(mica_kernel_cancel(state->transaction));
    return false;
}

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code;
    struct mica_MemoryRoot input = {0}, output = {0};
    assert(mica_memory_root_push(worker, &input));
    assert(mica_memory_root_push(worker, &output));
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    uint8_t bytes[4096];
    memset(bytes, 'z', sizeof(bytes));
    struct mica_ValueResult text = mica_value_string(worker, bytes, sizeof(bytes));
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    mica_value_root_set(&output, source_integer(99));
    assert(mica_memory_safepoint(worker, true));
    uint64_t before = symbols->f_count;
    // A contended table must not keep this worker from collecting or stopping.
    mica_foreign_mutex_lock(symbols->f_mutex);
    attempt.f_remaining = 10;
    worker->f_heap->f_collection_limit = 0;
    uint64_t collections = worker->f_heap->f_collections;
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, mica_value_root_get(&input), &output) == 10);
    mica_foreign_mutex_unlock(symbols->f_mutex);
    assert(worker->f_heap->f_collections > collections);
    assert(symbols->f_count == before && mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 10;
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, mica_value_root_get(&input), &output) == 10);
    assert(symbols->f_count == before && mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    struct mica_KernelWork *work = mica_kernel_work(&tx);
    uint64_t limit = work->f_bytes_limit;
    work->f_bytes_limit = 1;
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, mica_value_root_get(&input), &output) == 10);
    assert(symbols->f_count == before && mica_value_root_get(&output) == source_integer(99));
    work->f_bytes_limit = limit;
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, mica_value_root_get(&input), &output) == 0);
    assert(symbols->f_count == before + 1 && attempt.f_remaining < 1000000);
    mica_type_Value symbol = mica_value_root_get(&output);
    struct mica_SymbolText name = mica_value_symbol_text(symbols, (uint32_t)mica_value_as_symbol(symbol).f_number);
    assert(name.f_ok && name.f_length == sizeof(bytes) && memcmp(name.f_data, bytes, sizeof(bytes)) == 0);
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, mica_value_root_get(&input), &output) == 0);
    assert(mica_value_root_get(&output) == symbol && symbols->f_count == before + 1);
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, NULL, symbol, &output) == 0);
    assert(mica_value_root_get(&output) == symbol);
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, source_integer(1), &output) == 7);
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, NULL, mica_value_root_get(&input), &output) == 9);
    assert(mica_value_root_get(&output) == source_integer(99));
    // Cancel during interning, after the helper's entry check.
    bytes[0] = 'y';
    text = mica_value_string(worker, bytes, sizeof(bytes));
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    struct SourceSymbolCancel state = {.transaction = &tx};
    struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = source_symbol_cancel};
    mica_memory_control_push(worker, &control);
    assert(mica_source_roundtrip_to_symbol(worker, &attempt, &tx, symbols, mica_value_root_get(&input), &output) == 11);
    assert(worker->f_control == &control);
    mica_memory_control_pop(worker);
    assert(symbols->f_count == before + 1 && mica_value_root_get(&output) == source_integer(99));
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &output));
    assert(mica_memory_root_pop(worker, &input));
}
