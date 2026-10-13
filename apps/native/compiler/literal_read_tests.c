// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static struct mica_source_roundtrip_LiteralTextResult read_literal(
        struct mica_MemoryWorker *worker, struct mica_KernelTransaction *tx,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols,
        struct mica_TaskAttempt *attempt, const char *source) {
    struct mica_MemoryRoot input = {0};
    assert(mica_memory_root_push(worker, &input));
    struct mica_ValueResult text = mica_value_string(worker, (const uint8_t *)source, strlen(source));
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    struct mica_MemoryRoot *roots = worker->f_roots;
    struct mica_MemoryControl *control = worker->f_control;
    struct mica_source_roundtrip_LiteralTextResult result = mica_source_roundtrip_literal_parse(
        worker, attempt, tx, (void *)code->f_pointer, symbols, mica_value_root_get(&input));
    assert(worker->f_roots == roots && worker->f_control == control);
    assert(mica_memory_root_pop(worker, &input));
    return result;
}

struct LiteralCancellation { struct mica_KernelTransaction *tx; unsigned calls; };
static bool literal_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct LiteralCancellation *state = (void *)context;
    if (++state->calls != 8) return true;
    assert(mica_kernel_cancel(state->tx));
    return false;
}

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    const char *rejected[] = {"1+2", "unknown", "let x = 1", "from_literal(\"1\")",
        "[1, @xs]", "true false", "#named", ":a/", ":a// comment\n:other"};
    for (size_t i = 0; i < sizeof(rejected) / sizeof(rejected[0]); ++i) {
        struct mica_source_roundtrip_LiteralTextResult parsed = read_literal(worker, &tx, code, symbols, &attempt, rejected[i]);
        assert(!parsed.f_status && parsed.f_shape == 2 && !parsed.f_value);
    }
    struct mica_source_roundtrip_LiteralTextResult parsed = mica_source_roundtrip_literal_parse(
        worker, &attempt, &tx, (void *)code->f_pointer, symbols, source_integer(1));
    assert(!parsed.f_status && parsed.f_shape == 1 && !parsed.f_value);

    // Relocation during a sibling read must preserve earlier nested values.
    const char *nested = "[{:a -> [1, 2, \"café\"], :b -> [:value] {[[3]]}}, b\"AAH-_w==\", error(E_FAIL, \"bad\", [4]), [:x] {[1], [2]}, :a// comment\n]";
    struct mica_MemoryRoot expected = {0};
    assert(mica_memory_root_push(worker, &expected));
    parsed = read_literal(worker, &tx, code, symbols, &attempt, nested);
    assert(!parsed.f_status && !parsed.f_shape);
    mica_value_root_set(&expected, parsed.f_value);
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    parsed = read_literal(worker, &tx, code, symbols, &attempt, nested);
    assert(!parsed.f_status && !parsed.f_shape);
    struct mica_BoolResult equal = mica_value_equal(mica_value_root_get(&expected), parsed.f_value);
    assert(equal.f_ok && equal.f_value && worker->f_heap->f_collections > collections);
    worker->f_heap->f_collection_limit = collection_limit;
    assert(mica_memory_root_pop(worker, &expected));

    // Depth is bounded independently of the amount of input or remaining work.
    const struct { const char *open; const char *close; } nesting[] = {
        {"[", "]"}, {"(", ")"}, {"- ", ""}, {"{0 ->", "}"},
        {"#1<", ">"}, {"error(E_FAIL, none, ", ")"},
    };
    for (size_t kind = 0; kind < sizeof(nesting) / sizeof(nesting[0]); ++kind) {
        for (unsigned depth = 63; depth <= 64; ++depth) {
            char deep[2048];
            size_t length = 0;
            for (unsigned i = 0; i < depth; ++i) {
                size_t width = strlen(nesting[kind].open);
                memcpy(deep + length, nesting[kind].open, width); length += width;
            }
            deep[length++] = '0';
            for (unsigned i = 0; i < depth; ++i) {
                size_t width = strlen(nesting[kind].close);
                memcpy(deep + length, nesting[kind].close, width); length += width;
            }
            deep[length] = 0;
            parsed = read_literal(worker, &tx, code, symbols, &attempt, deep);
            if (depth == 63) assert(!parsed.f_status && !parsed.f_shape);
            else assert(parsed.f_status == 10 && !parsed.f_value);
        }
    }
    attempt.f_remaining = 3;
    parsed = read_literal(worker, &tx, code, symbols, &attempt, nested);
    assert(parsed.f_status == 10 && !parsed.f_value && attempt.f_remaining <= 3);
    attempt.f_remaining = 1000000;
    uint64_t bytes_limit = mica_kernel_work(&tx)->f_bytes_limit;
    mica_kernel_work(&tx)->f_bytes_limit = 1;
    parsed = read_literal(worker, &tx, code, symbols, &attempt, nested);
    assert(parsed.f_status == 10 && !parsed.f_value);
    mica_kernel_work(&tx)->f_bytes_limit = bytes_limit;

    struct LiteralCancellation state = {.tx = &tx};
    struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = literal_cancel};
    mica_memory_control_push(worker, &control);
    parsed = read_literal(worker, &tx, code, symbols, &attempt, nested);
    assert(parsed.f_status == 11 && !parsed.f_value && state.calls == 8);
    mica_memory_control_pop(worker);
    assert(mica_kernel_end(&tx));
}
