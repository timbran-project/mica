// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void quoted_text_is(mica_type_Value value, const uint8_t *bytes, size_t length) {
    struct mica_HeapStringResult text = mica_value_as_string(value);
    assert(text.f_ok && text.f_header->f_length == length);
    assert(length == 0 || memcmp(text.f_header->f_data, bytes, length) == 0);
}

static void quote_roundtrip(struct mica_MemoryWorker *worker, struct mica_KernelTransaction *tx,
        const uint8_t *bytes, size_t length, const char *expected) {
    struct mica_MemoryRoot input = {0}, quoted = {0};
    assert(mica_memory_root_push(worker, &input));
    assert(mica_memory_root_push(worker, &quoted));
    struct mica_ValueResult text = mica_value_string(worker, bytes, length);
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_source_roundtrip_LiteralTextResult result = mica_source_roundtrip_literal_quote(
        worker, &attempt, tx, mica_value_root_get(&input));
    assert(result.f_status == 0 && result.f_shape == 0);
    mica_value_root_set(&quoted, result.f_value);
    if (expected) quoted_text_is(result.f_value, (const uint8_t *)expected, strlen(expected));
    uint64_t end = mica_value_as_string(result.f_value).f_header->f_length;
    result = mica_source_roundtrip_literal_read_string(worker, &attempt, tx, mica_value_root_get(&quoted), 0);
    assert(result.f_status == 0 && result.f_shape == 0 && result.f_next == end);
    quoted_text_is(result.f_value, bytes, length);
    assert(mica_memory_root_pop(worker, &quoted));
    assert(mica_memory_root_pop(worker, &input));
}

static void read_string_token(struct mica_MemoryWorker *worker, struct mica_KernelTransaction *tx,
        const char *source, uint64_t start, uint64_t end, const char *expected) {
    struct mica_MemoryRoot input = {0};
    assert(mica_memory_root_push(worker, &input));
    struct mica_ValueResult text = mica_value_string(worker, (const uint8_t *)source, strlen(source));
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_source_roundtrip_LiteralTextResult result = mica_source_roundtrip_literal_read_string(
        worker, &attempt, tx, mica_value_root_get(&input), start);
    assert(result.f_status == 0);
    if (expected) {
        assert(result.f_shape == 0 && result.f_next == end);
        quoted_text_is(result.f_value, (const uint8_t *)expected, strlen(expected));
    } else {
        assert(result.f_shape == 2 && !result.f_value && !result.f_next);
    }
    assert(mica_memory_root_pop(worker, &input));
}

struct QuotingCancellation { struct mica_KernelTransaction *tx; unsigned calls; };
static bool quoting_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct QuotingCancellation *state = (void *)context;
    if (++state->calls != 2) return true;
    assert(mica_kernel_cancel(state->tx));
    return false;
}

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code; (void)symbols;
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    const struct { const char *text; const char *literal; } cases[] = {
        {"", "\"\""}, {"hello", "\"hello\""},
        {"café 🦊", "\"café 🦊\""}, {"e\xcc\x81", "\"e\xcc\x81\""},
        {"\t\n\r\"\\", "\"\\t\\n\\r\\\"\\\\\""},
        {"\x01\x08\x0b\x0c\x1f\x7f\xc2\x85", "\"\\u{1}\\u{8}\\u{b}\\u{c}\\u{1f}\\u{7f}\\u{85}\""},
        {"\xc2\xa0\xe2\x80\x8d\xe2\x80\xa8\xe2\x80\xae\xef\xb8\x8f", "\"\\u{a0}\\u{200d}\\u{2028}\\u{202e}\\u{fe0f}\""},
        {"\\u{41}\\0", "\"\\\\u{41}\\\\0\""},
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i)
        quote_roundtrip(worker, &tx, (const uint8_t *)cases[i].text, strlen(cases[i].text), cases[i].literal);
    quote_roundtrip(worker, &tx, (const uint8_t *)"a\0b", 3, "\"a\\0b\"");
    read_string_token(worker, &tx, "prefix\"café\"tail", 6, 13, "café");
    read_string_token(worker, &tx, "\"\\u{000041}\\u{E9}\\u{1F98A}\"", 0, 27, "Aé🦊");
    const char *preserved[] = {"\\q", "\\x41", "\\u{}", "\\u{d800}", "\\u{110000}", "\\u{1234567}", "\\u{xyz}", "\\u{41", "\\é"};
    for (size_t i = 0; i < sizeof(preserved) / sizeof(preserved[0]); ++i) {
        char token[64];
        size_t length = strlen(preserved[i]);
        token[0] = '"'; memcpy(token + 1, preserved[i], length);
        token[length + 1] = '"'; token[length + 2] = 0;
        read_string_token(worker, &tx, token, 0, length + 2, preserved[i]);
    }
    read_string_token(worker, &tx, "\"raw\nline\"", 0, 10, "raw\nline");
    const char *invalid[] = {"", "x", "\"", "\"abc", "\"abc\\", "\"abc\\\""};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i)
        read_string_token(worker, &tx, invalid[i], 0, 0, NULL);
    read_string_token(worker, &tx, "\"x\"", UINT64_MAX, 0, NULL);
    read_string_token(worker, &tx, "\"x\"", 3, 0, NULL);

    // Exercise scalar boundaries, embedded NULs, and every buffer alignment.
    uint8_t bytes[8192];
    size_t length = 0;
    uint32_t random = 17;
    for (unsigned i = 0; i < 1500; ++i) {
        random = random * UINT32_C(1664525) + UINT32_C(1013904223);
        uint32_t scalar = i < 160 ? i : random % UINT32_C(0x110000);
        if (scalar >= 0xd800 && scalar < 0xe000) scalar = 0x10ffff;
        struct mica_Utf8Encode encoded = mica_utf8_encode(scalar);
        assert(encoded.f_ok && length + encoded.f_width < sizeof(bytes));
        memcpy(bytes + length, encoded.f_bytes.elements, (size_t)encoded.f_width);
        length += (size_t)encoded.f_width;
    }
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    worker->f_heap->f_collection_limit = 0;
    uint64_t collections = worker->f_heap->f_collections;
    quote_roundtrip(worker, &tx, bytes, length, NULL);
    assert(worker->f_heap->f_collections > collections);
    worker->f_heap->f_collection_limit = collection_limit;

    struct mica_MemoryRoot input = {0};
    assert(mica_memory_root_push(worker, &input));
    struct mica_ValueResult text = mica_value_string(worker, bytes, length);
    assert(text.f_ok); mica_value_root_set(&input, text.f_value);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_source_roundtrip_LiteralTextResult result = mica_source_roundtrip_literal_quote(worker, &attempt, &tx, source_integer(1));
    assert(!result.f_status && result.f_shape == 1 && !result.f_value);
    result = mica_source_roundtrip_literal_read_string(worker, &attempt, &tx, source_integer(1), 0);
    assert(!result.f_status && result.f_shape == 1 && !result.f_value);
    uint64_t limit = mica_kernel_work(&tx)->f_bytes_limit;
    mica_kernel_work(&tx)->f_bytes_limit = 1;
    result = mica_source_roundtrip_literal_quote(worker, &attempt, &tx, mica_value_root_get(&input));
    assert(result.f_status == 10 && !result.f_value);
    mica_kernel_work(&tx)->f_bytes_limit = limit;
    attempt.f_remaining = 3;
    result = mica_source_roundtrip_literal_quote(worker, &attempt, &tx, mica_value_root_get(&input));
    assert(result.f_status == 10 && !result.f_value);
    attempt.f_remaining = 1000000;
    struct QuotingCancellation state = {.tx = &tx};
    struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = quoting_cancel};
    struct mica_MemoryRoot *roots = worker->f_roots;
    mica_memory_control_push(worker, &control);
    result = mica_source_roundtrip_literal_quote(worker, &attempt, &tx, mica_value_root_get(&input));
    assert(result.f_status == 11 && !result.f_value);
    assert(worker->f_roots == roots && worker->f_control == &control && state.calls == 2);
    mica_memory_control_pop(worker);
    result = mica_source_roundtrip_literal_read_string(worker, &attempt, &tx, mica_value_root_get(&input), 0);
    assert(result.f_status == 11 && !result.f_value);
    assert(mica_memory_root_pop(worker, &input));
    assert(mica_kernel_end(&tx));
}
