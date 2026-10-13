// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

// Rust checks the source-level results and diagnostics. These checks add
// independent C conversion and exact midpoint oracles over the binary32 range.
static void check_decimal(struct mica_MemoryWorker *worker,
        struct mica_KernelTransaction *tx, const char *text, uint32_t expected) {
    struct mica_MemoryRoot input = {0};
    assert(mica_memory_root_push(worker, &input));
    struct mica_ValueResult value = mica_value_string(worker,
        (const uint8_t *)text, strlen(text));
    assert(value.f_ok);
    mica_value_root_set(&input, value.f_value);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_source_roundtrip_ParsedNumber parsed = mica_source_roundtrip_decimal_parse(
        worker, &attempt, tx, mica_value_root_get(&input));
    assert(parsed.f_status == 0);
    struct mica_ValueResult wanted = mica_value_float_from_bits(expected);
    if (wanted.f_ok) {
        if (parsed.f_shape != 0 || parsed.f_value != wanted.f_value) {
            fprintf(stderr, "decimal mismatch: %s, wanted %08x, shape %llu\n",
                text, expected, (unsigned long long)parsed.f_shape);
            abort();
        }
    } else {
        assert(parsed.f_shape == 3 && !parsed.f_value);
    }
    assert(mica_memory_root_pop(worker, &input));
}

static void decimal_boundaries(struct mica_MemoryWorker *worker,
        struct mica_KernelTransaction *tx) {
    const uint32_t fractions[] = {0, 1, 2, 0x3fffff, 0x7ffffd, 0x7ffffe, 0x7fffff};
    for (unsigned exponent = 0; exponent < 255; ++exponent) {
        for (unsigned f = 0; f < sizeof(fractions) / sizeof(fractions[0]); ++f) {
            uint32_t bits = (exponent << 23) | fractions[f];
            uint32_t significand = fractions[f] + (exponent ? 0x800000 : 0);
            double midpoint = ldexp((double)(2 * significand + 1),
                exponent ? (int)exponent - 151 : -150);
            char exact[200], neighbour[200];
            int n = snprintf(exact, sizeof(exact), "%.160e", midpoint);
            assert(n > 0 && (size_t)n < sizeof(exact));
            check_decimal(worker, tx, exact, bits + (bits & 1));
            // All exact midpoint digits finish before this final position.
            char *end = strchr(exact, 'e');
            assert(end && end[-1] == '0');
            size_t last = (size_t)(end - exact) - 1;
            memcpy(neighbour, exact, (size_t)n + 1);
            neighbour[last] = '1';
            check_decimal(worker, tx, neighbour, bits + 1);
            memcpy(neighbour, exact, (size_t)n + 1);
            for (size_t i = last;; --i) {
                if (neighbour[i] == '.') continue;
                if (neighbour[i] != '0') { --neighbour[i]; break; }
                neighbour[i] = '9';
                assert(i != 0);
            }
            check_decimal(worker, tx, neighbour, bits);
        }
    }
    uint32_t bits = 1;
    for (unsigned i = 0; i < 2000; ++i) {
        bits = bits * UINT32_C(1664525) + UINT32_C(1013904223);
        if ((bits & UINT32_C(0x7f800000)) == UINT32_C(0x7f800000)) continue;
        float value;
        memcpy(&value, &bits, sizeof(value));
        char text[200];
        snprintf(text, sizeof(text), "%.9g", (double)value);
        check_decimal(worker, tx, text, bits);
        snprintf(text, sizeof(text), "%u.%09ue%+d", bits, bits ^ UINT32_C(0x87654321), (int)(i % 161) - 80);
        char *end = NULL;
        float oracle = strtof(text, &end);
        assert(end && !*end);
        uint32_t wanted;
        memcpy(&wanted, &oracle, sizeof(wanted));
        check_decimal(worker, tx, text, wanted);
    }
}

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
        struct mica_source_roundtrip_ParsedNumber parsed = mica_source_roundtrip_parse_int(
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
    struct mica_source_roundtrip_ParsedNumber parsed = mica_source_roundtrip_parse_int(
        worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 10 && !parsed.f_value && !parsed.f_shape);
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    attempt.f_remaining = 1000000;
    parsed = mica_source_roundtrip_parse_int(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 0 && parsed.f_shape == 0 && parsed.f_value == source_integer(42));
    assert(worker->f_heap->f_collections > collections && attempt.f_remaining < 1000000);
    decimal_boundaries(worker, &tx);
    char small[4200];
    small[0] = '0'; small[1] = '.';
    memset(small + 2, '0', 4096);
    memcpy(small + 4098, "1e4097", 7);
    check_decimal(worker, &tx, small, UINT32_C(0x3f800000));
    // Long inputs must poll and reload their rooted string after collection.
    value = mica_value_string(worker, zeros, sizeof(zeros));
    assert(value.f_ok);
    mica_value_root_set(&text, value.f_value);
    attempt.f_remaining = 10;
    parsed = mica_source_roundtrip_decimal_parse(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 10 && !parsed.f_value && !parsed.f_shape);
    collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    attempt.f_remaining = 1000000;
    parsed = mica_source_roundtrip_decimal_parse(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 0 && parsed.f_shape == 0 && parsed.f_value == mica_value_float(42.0f).f_value);
    assert(worker->f_heap->f_collections > collections);
    assert(mica_kernel_cancel(&tx));
    parsed = mica_source_roundtrip_decimal_parse(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 11 && !parsed.f_value && !parsed.f_shape);
    parsed = mica_source_roundtrip_parse_int(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 11 && !parsed.f_value && !parsed.f_shape);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &text));
}
