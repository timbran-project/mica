// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

typedef struct mica_source_roundtrip_LiteralTextResult (*JsonOperation)(
    struct mica_MemoryWorker *, struct mica_TaskAttempt *, struct mica_KernelTransaction *,
    struct mica_TaskCodeRef *, struct mica_SymbolTable *, mica_type_Value);

static struct mica_source_roundtrip_LiteralTextResult convert_json(JsonOperation operation,
        struct mica_MemoryWorker *worker, struct mica_KernelTransaction *tx,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols,
        struct mica_TaskAttempt *attempt, mica_type_Value value) {
    struct mica_MemoryRoot *roots = worker->f_roots;
    struct mica_MemoryControl *control = worker->f_control;
    struct mica_source_roundtrip_LiteralTextResult result = operation(
        worker, attempt, tx, (void *)code->f_pointer, symbols, value);
    assert(worker->f_roots == roots && worker->f_control == control);
    return result;
}
static void json_text(struct mica_MemoryWorker *worker, struct mica_MemoryRoot *root, const char *text) {
    struct mica_ValueResult copied = mica_value_string(worker, (const uint8_t *)text, strlen(text));
    assert(copied.f_ok);
    mica_value_root_set(root, copied.f_value);
}
struct JsonCancellation { struct mica_KernelTransaction *tx; unsigned calls; };
static bool json_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct JsonCancellation *state = (void *)context;
    if (++state->calls != 3) return true;
    assert(mica_kernel_cancel(state->tx));
    return false;
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    JsonOperation read = mica_source_roundtrip_json_decode;
    JsonOperation write = mica_source_roundtrip_json_encode;
    struct mica_KernelTransaction tx = {0};
    struct mica_TaskAttempt attempt = {.f_remaining = 10000000};
    struct mica_MemoryRoot text = {0}, value = {0}, output = {0};
    assert(mica_memory_root_push(worker, &text) && mica_memory_root_push(worker, &value)
        && mica_memory_root_push(worker, &output));
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);

    // Many buffer flushes, including a surrogate pair and embedded NUL.
    char long_json[8192] = "{\"key\":[\"";
    const char *piece = "é🦊\\uD83E\\uDD8A\\u0000";
    for (unsigned i = 0; i < 200; ++i) strcat(long_json, piece);
    strcat(long_json, "\", {\"later\":[true,1.25,null]}]}");
    json_text(worker, &text, long_json);
    struct mica_source_roundtrip_LiteralTextResult result = convert_json(read,
        worker, &tx, code, symbols, &attempt, mica_value_root_get(&text));
    assert(!result.f_status && !result.f_shape);
    mica_value_root_set(&value, result.f_value);
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    result = convert_json(read, worker, &tx, code, symbols, &attempt, mica_value_root_get(&text));
    assert(!result.f_status && !result.f_shape);
    assert(mica_value_equal(result.f_value, mica_value_root_get(&value)).f_value);
    result = convert_json(write, worker, &tx, code, symbols, &attempt, mica_value_root_get(&value));
    assert(!result.f_status && !result.f_shape);
    mica_value_root_set(&output, result.f_value);
    result = convert_json(read, worker, &tx, code, symbols, &attempt, mica_value_root_get(&output));
    assert(!result.f_status && !result.f_shape);
    assert(mica_value_equal(result.f_value, mica_value_root_get(&value)).f_value);
    assert(worker->f_heap->f_collections > collections);
    worker->f_heap->f_collection_limit = collection_limit;

    // Unsorted objects exercise merge passes and relocation during map copying.
    char object[32768] = "{";
    size_t used = 1;
    for (unsigned i = 320; i-- > 0;) {
        int written = snprintf(object + used, sizeof(object) - used,
            "\"k%03u\":[%u,\"é🦊\"],", i, i);
        assert(written > 0 && (size_t)written < sizeof(object) - used);
        used += (size_t)written;
    }
    strcpy(object + used, "\"k000\":[999,\"last\"]}");
    json_text(worker, &output, object);
    worker->f_heap->f_collection_limit = 0;
    result = convert_json(read, worker, &tx, code, symbols, &attempt, mica_value_root_get(&output));
    assert(!result.f_status && !result.f_shape && mica_value_map_length(result.f_value).f_number == 320);
    mica_value_root_set(&output, result.f_value);
    for (unsigned i = 0; i < 320; ++i) {
        char name[8];
        assert(snprintf(name, sizeof(name), "k%03u", i) == 4);
        struct mica_IdResult id = mica_value_symbol_intern(symbols, (const uint8_t *)name, 4, NULL);
        assert(id.f_ok);
        struct mica_ValueResult item = mica_value_map_get(mica_value_root_get(&output), mica_value_symbol((uint32_t)id.f_number).f_value);
        assert(item.f_ok && mica_kernel_cell(item.f_value, 0) == source_integer(i ? i : 999));
    }
    result = convert_json(write, worker, &tx, code, symbols, &attempt, mica_value_root_get(&output));
    assert(!result.f_status && !result.f_shape);
    // A small remaining budget must also stop the shared map sort itself.
    struct mica_ValueResult empty = mica_value_list(worker, NULL, 0);
    assert(empty.f_ok);
    mica_value_root_set(&output, empty.f_value);
    for (unsigned i = 320; i-- > 0;) {
        struct mica_ValueResult appended = mica_value_list_append(worker, mica_value_root_get(&output), source_integer(i));
        assert(appended.f_ok);
        mica_value_root_set(&output, appended.f_value);
        appended = mica_value_list_append(worker, mica_value_root_get(&output), source_integer(i));
        assert(appended.f_ok);
        mica_value_root_set(&output, appended.f_value);
    }
    struct mica_KernelBudget budget = {.f_steps = 350, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX,
        .f_bytes = UINT64_MAX, .f_signal = tx.f_signal};
    struct mica_MemoryRoot *roots = worker->f_roots;
    result = mica_source_roundtrip_json_map(worker, &tx, mica_value_root_get(&output), &budget);
    assert(result.f_status == 10 && !result.f_value && worker->f_roots == roots);
    worker->f_heap->f_collection_limit = collection_limit;

    const char *open[] = {"[", "{\"x\":"};
    const char *close[] = {"]", "}"};
    for (unsigned kind = 0; kind < 2; ++kind) {
        for (unsigned depth = 63; depth <= 64; ++depth) {
            char nested[512] = "";
            for (unsigned i = 0; i < depth; ++i) strcat(nested, open[kind]);
            strcat(nested, "0");
            for (unsigned i = 0; i < depth; ++i) strcat(nested, close[kind]);
            json_text(worker, &output, nested);
            result = convert_json(read, worker, &tx, code, symbols, &attempt, mica_value_root_get(&output));
            if (depth == 63) {
                assert(!result.f_status && !result.f_shape);
                mica_value_root_set(&output, result.f_value);
                result = convert_json(write, worker, &tx, code, symbols, &attempt, mica_value_root_get(&output));
                assert(!result.f_status && !result.f_shape);
            } else assert(result.f_status == 10 && !result.f_value);
        }
    }
    mica_value_root_set(&output, source_integer(0));
    for (unsigned i = 0; i < 64; ++i) {
        mica_type_Value child = mica_value_root_get(&output);
        struct mica_ValueResult parent = mica_value_list(worker, &child, 1);
        assert(parent.f_ok);
        mica_value_root_set(&output, parent.f_value);
    }
    result = convert_json(write, worker, &tx, code, symbols, &attempt, mica_value_root_get(&output));
    assert(result.f_status == 10 && !result.f_value);

    for (unsigned operation = 0; operation < 2; ++operation) {
        JsonOperation convert = operation ? write : read;
        struct mica_MemoryRoot *input = operation ? &value : &text;
        for (unsigned budget = 0; budget < 2; ++budget) {
            attempt.f_remaining = budget ? 500 : 0;
            result = convert_json(convert, worker, &tx, code, symbols, &attempt, mica_value_root_get(input));
            assert(result.f_status == 10 && !result.f_value);
        }
        attempt.f_remaining = 10000000;
        uint64_t bytes_limit = mica_kernel_work(&tx)->f_bytes_limit;
        mica_kernel_work(&tx)->f_bytes_limit = 1;
        result = convert_json(convert, worker, &tx, code, symbols, &attempt, mica_value_root_get(input));
        assert(result.f_status == 10 && !result.f_value);
        mica_kernel_work(&tx)->f_bytes_limit = bytes_limit;
        struct JsonCancellation state = {.tx = &tx};
        struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = json_cancel};
        mica_memory_control_push(worker, &control);
        result = convert_json(convert, worker, &tx, code, symbols, &attempt, mica_value_root_get(input));
        assert(result.f_status == 11 && !result.f_value && state.calls == 3);
        mica_memory_control_pop(worker);
        assert(mica_kernel_end(&tx));
        assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    }
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &output) && mica_memory_root_pop(worker, &value)
        && mica_memory_root_pop(worker, &text));
}
