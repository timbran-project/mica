// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code; (void)symbols;
    struct mica_MemoryRoot text = {0};
    assert(mica_memory_root_push(worker, &text));
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    // Sample the full signed 56-bit range, including exact boundaries.
    uint64_t bits = 0;
    for (unsigned i = 0; i < 1000; ++i) {
        bits = bits * UINT64_C(6364136223846793005) + 1;
        int64_t number = (int64_t)(bits & UINT64_C(0x00ffffffffffffff)) - INT64_C(36028797018963968);
        if (i == 0) number = -INT64_C(36028797018963968);
        if (i == 1) number = INT64_C(36028797018963967);
        char buffer[32];
        int length = snprintf(buffer, sizeof(buffer), "%lld", (long long)number);
        assert(length > 0 && (size_t)length < sizeof(buffer));
        struct mica_ValueResult value = mica_value_string(worker, (const uint8_t *)buffer, (uint64_t)length);
        assert(value.f_ok);
        mica_value_root_set(&text, value.f_value);
        struct mica_source_roundtrip_ParsedInteger parsed = mica_source_roundtrip_parse_int(
            worker, &attempt, &tx, mica_value_root_get(&text));
        assert(parsed.f_status == 0 && parsed.f_shape == 0 && parsed.f_value == source_integer(number));
    }
    uint8_t zeros[4098];
    memset(zeros, '0', sizeof(zeros));
    zeros[4096] = '4';
    zeros[4097] = '2';
    struct mica_ValueResult value = mica_value_string(worker, zeros, sizeof(zeros));
    assert(value.f_ok);
    mica_value_root_set(&text, value.f_value);
    attempt.f_remaining = 10;
    struct mica_source_roundtrip_ParsedInteger parsed = mica_source_roundtrip_parse_int(
        worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 10 && !parsed.f_value && !parsed.f_shape);
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    attempt.f_remaining = 1000000;
    parsed = mica_source_roundtrip_parse_int(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 0 && parsed.f_shape == 0 && parsed.f_value == source_integer(42));
    assert(worker->f_heap->f_collections > collections && attempt.f_remaining < 1000000);
    assert(mica_kernel_cancel(&tx));
    parsed = mica_source_roundtrip_parse_int(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 11 && !parsed.f_value && !parsed.f_shape);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &text));
}
