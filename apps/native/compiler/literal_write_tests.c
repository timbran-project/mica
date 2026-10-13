// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static struct mica_source_roundtrip_LiteralTextResult write_literal(
        struct mica_MemoryWorker *worker, struct mica_KernelTransaction *tx,
        struct mica_SymbolTable *symbols, struct mica_TaskAttempt *attempt, mica_type_Value value) {
    struct mica_MemoryRoot *roots = worker->f_roots;
    struct mica_MemoryControl *control = worker->f_control;
    struct mica_source_roundtrip_LiteralTextResult result = mica_source_roundtrip_literal_write_text(
        worker, attempt, tx, symbols, value);
    assert(worker->f_roots == roots && worker->f_control == control);
    return result;
}

static void literal_text_is(mica_type_Value text, const char *expected) {
    struct mica_HeapStringResult actual = mica_value_as_string(text);
    assert(actual.f_ok && actual.f_header->f_length == strlen(expected));
    assert(!memcmp(actual.f_header->f_data, expected, strlen(expected)));
}

struct WriterCancellation { struct mica_KernelTransaction *tx; unsigned calls; };
static bool writer_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct WriterCancellation *state = (void *)context;
    if (++state->calls != 8) return true;
    assert(mica_kernel_cancel(state->tx));
    return false;
}

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_MemoryRoot value = {0}, text = {0};
    assert(mica_memory_root_push(worker, &value) && mica_memory_root_push(worker, &text));
    // Force several token-buffer flushes around complete UTF-8 quoted fragments.
    const char *pieces[] = {"café🦊", "\"\\\n", "a\xE2\x80\x8Bz"};
    struct mica_ValueResult list = mica_value_list(worker, NULL, 0);
    assert(list.f_ok);
    mica_value_root_set(&value, list.f_value);
    for (unsigned i = 0; i < 150; ++i) {
        const char *piece = pieces[i % 3];
        struct mica_ValueResult element = mica_value_string(worker, (const uint8_t *)piece, strlen(piece));
        assert(element.f_ok);
        list = mica_value_list_append(worker, mica_value_root_get(&value), element.f_value);
        assert(list.f_ok);
        mica_value_root_set(&value, list.f_value);
    }
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    struct mica_source_roundtrip_LiteralTextResult written = write_literal(worker, &tx, symbols, &attempt, mica_value_root_get(&value));
    assert(!written.f_status && !written.f_shape && worker->f_heap->f_collections > collections);
    mica_value_root_set(&text, written.f_value);
    struct mica_source_roundtrip_LiteralTextResult parsed = mica_source_roundtrip_literal_parse(
        worker, &attempt, &tx, (void *)code->f_pointer, symbols, mica_value_root_get(&text));
    assert(!parsed.f_status && !parsed.f_shape);
    struct mica_BoolResult equal = mica_value_equal(mica_value_root_get(&value), parsed.f_value);
    assert(equal.f_ok && equal.f_value);
    worker->f_heap->f_collection_limit = collection_limit;

    const char *nested = "[{:a -> [1, \"café\"], :b -> [:value] {[[3]]}}, error(E_FAIL, \"bad\", [4]), #72057594037927935<[true]>, b\"AAH-_w==\", 1..(2..3)]";
    struct mica_ValueResult input = mica_value_string(worker, (const uint8_t *)nested, strlen(nested));
    assert(input.f_ok);
    mica_value_root_set(&text, input.f_value);
    parsed = mica_source_roundtrip_literal_parse(worker, &attempt, &tx, (void *)code->f_pointer, symbols, mica_value_root_get(&text));
    assert(!parsed.f_status && !parsed.f_shape);
    mica_value_root_set(&value, parsed.f_value);
    worker->f_heap->f_collection_limit = 0;
    written = write_literal(worker, &tx, symbols, &attempt, mica_value_root_get(&value));
    assert(!written.f_status && !written.f_shape);
    mica_value_root_set(&text, written.f_value);
    parsed = mica_source_roundtrip_literal_parse(worker, &attempt, &tx, (void *)code->f_pointer, symbols, mica_value_root_get(&text));
    assert(!parsed.f_status && !parsed.f_shape);
    equal = mica_value_equal(mica_value_root_get(&value), parsed.f_value);
    assert(equal.f_ok && equal.f_value);
    worker->f_heap->f_collection_limit = collection_limit;

    // Unsupported children must fail the whole serialization.
    mica_type_Value capability = mica_value_capability(1).f_value;
    list = mica_value_list(worker, &capability, 1);
    assert(list.f_ok);
    written = write_literal(worker, &tx, symbols, &attempt, list.f_value);
    assert(!written.f_status && written.f_shape == 2 && !written.f_value);
    written = write_literal(worker, &tx, symbols, &attempt, mica_value_identity(UINT64_C(72057594037927935)).f_value);
    assert(!written.f_status && !written.f_shape);
    literal_text_is(written.f_value, "#72057594037927935");

    // Every recursive variant observes the same depth bound.
    for (unsigned kind = 0; kind < 3; ++kind) {
        mica_value_root_set(&value, source_integer(0));
        for (unsigned depth = 1; depth <= 64; ++depth) {
            mica_type_Value child = mica_value_root_get(&value);
            struct mica_ValueResult parent;
            if (kind == 0) parent = mica_value_list(worker, &child, 1);
            else if (kind == 1) parent = mica_value_frob(worker, 1, child);
            else parent = mica_value_range(worker, source_integer(0), true, child);
            assert(parent.f_ok);
            mica_value_root_set(&value, parent.f_value);
            if (depth < 63) continue;
            written = write_literal(worker, &tx, symbols, &attempt, mica_value_root_get(&value));
            if (depth == 63) assert(!written.f_status && !written.f_shape);
            else assert(written.f_status == 10 && !written.f_value);
        }
    }
    attempt.f_remaining = 1;
    written = write_literal(worker, &tx, symbols, &attempt, source_integer(42));
    assert(written.f_status == 10 && !written.f_value && attempt.f_remaining <= 1);
    attempt.f_remaining = 1000000;
    uint64_t bytes_limit = mica_kernel_work(&tx)->f_bytes_limit;
    mica_kernel_work(&tx)->f_bytes_limit = 1;
    written = write_literal(worker, &tx, symbols, &attempt, source_integer(42));
    assert(written.f_status == 10 && !written.f_value);
    mica_kernel_work(&tx)->f_bytes_limit = bytes_limit;
    struct WriterCancellation state = {.tx = &tx};
    struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = writer_cancel};
    mica_memory_control_push(worker, &control);
    written = write_literal(worker, &tx, symbols, &attempt, mica_value_root_get(&value));
    assert(written.f_status == 11 && !written.f_value && state.calls == 8);
    mica_memory_control_pop(worker);
    assert(mica_memory_root_pop(worker, &text) && mica_memory_root_pop(worker, &value));
    assert(mica_kernel_end(&tx));
}
