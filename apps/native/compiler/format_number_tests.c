// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

static void number_text_is(mica_type_Value text, const char *expected) {
    struct mica_HeapStringResult actual = mica_value_as_string(text);
    assert(actual.f_ok);
    if (actual.f_header->f_length != strlen(expected) ||
            memcmp(actual.f_header->f_data, expected, strlen(expected)) != 0) {
        fprintf(stderr, "numeric literal: expected %s, received %.*s\n", expected,
            (int)actual.f_header->f_length, actual.f_header->f_data);
        abort();
    }
}

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code; (void)symbols;
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_MemoryRoot text = {0};
    assert(mica_memory_root_push(worker, &text));
    static const struct { uint32_t bits; const char *text; } cases[] = {
        /* FLOAT_LITERAL_CASES */
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        struct mica_ValueResult input = mica_value_float_from_bits(cases[i].bits);
        assert(input.f_ok);
        struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
        struct mica_source_roundtrip_LiteralTextResult formatted = mica_source_roundtrip_format_number(
            worker, &attempt, &tx, input.f_value);
        assert(!formatted.f_status && !formatted.f_shape);
        number_text_is(formatted.f_value, cases[i].text);
        mica_value_root_set(&text, formatted.f_value);
        struct mica_source_roundtrip_ParsedNumber parsed = mica_source_roundtrip_decimal_parse(
            worker, &attempt, &tx, mica_value_root_get(&text));
        assert(!parsed.f_status && !parsed.f_shape && parsed.f_value == input.f_value);
        assert(attempt.f_remaining < 1000000);
    }
    uint64_t random = 17;
    for (unsigned i = 0; i < 1000; ++i) {
        random = random * UINT64_C(6364136223846793005) + 1;
        int64_t number = (int64_t)(random & UINT64_C(0x00ffffffffffffff)) - INT64_C(36028797018963968);
        if (i == 0) number = -INT64_C(36028797018963968);
        if (i == 1) number = INT64_C(36028797018963967);
        if (i == 2) number = 0;
        char expected[32];
        snprintf(expected, sizeof(expected), "%lld", (long long)number);
        struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
        struct mica_source_roundtrip_LiteralTextResult formatted = mica_source_roundtrip_format_number(
            worker, &attempt, &tx, source_integer(number));
        assert(!formatted.f_status && !formatted.f_shape);
        number_text_is(formatted.f_value, expected);
    }
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_source_roundtrip_LiteralTextResult formatted = mica_source_roundtrip_format_number(
        worker, &attempt, &tx, mica_value_float_from_bits(1).f_value);
    assert(!formatted.f_status && !formatted.f_shape && worker->f_heap->f_collections > collections);
    number_text_is(formatted.f_value, "1e-45");
    worker->f_heap->f_collection_limit = collection_limit;
    formatted = mica_source_roundtrip_format_number(worker, &attempt, &tx, mica_value_bool(true));
    assert(!formatted.f_status && formatted.f_shape == 1 && !formatted.f_value);
    attempt.f_remaining = 1;
    formatted = mica_source_roundtrip_format_number(worker, &attempt, &tx, source_integer(42));
    assert(formatted.f_status == 10 && !formatted.f_value && attempt.f_remaining <= 1);
    attempt.f_remaining = 1000000;
    uint64_t bytes_limit = mica_kernel_work(&tx)->f_bytes_limit;
    mica_kernel_work(&tx)->f_bytes_limit = 1;
    formatted = mica_source_roundtrip_format_number(worker, &attempt, &tx, source_integer(42));
    assert(formatted.f_status == 10 && !formatted.f_value);
    mica_kernel_work(&tx)->f_bytes_limit = bytes_limit;
    assert(mica_kernel_cancel(&tx));
    struct mica_MemoryRoot *roots = worker->f_roots;
    struct mica_MemoryControl *control = worker->f_control;
    formatted = mica_source_roundtrip_format_number(worker, &attempt, &tx, source_integer(42));
    assert(formatted.f_status == 11 && !formatted.f_value);
    assert(worker->f_roots == roots && worker->f_control == control);
    assert(mica_memory_root_pop(worker, &text));
    assert(mica_kernel_end(&tx));
}
