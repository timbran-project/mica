// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel) {
    struct mica_MemoryRoot input = {0}, encoded = {0}, output = {0};
    assert(mica_memory_root_push(worker, &input));
    assert(mica_memory_root_push(worker, &encoded));
    assert(mica_memory_root_push(worker, &output));
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    const char *invalid[] = {"%", "%0", "%xz", "%0g", "%g0", "%80", "%BF", "%C0%80", "%C1%BF",
        "%C2", "%E0%80%80", "%E0%A0", "%ED%A0%80", "%F0%80%80%80", "%F4%90%80%80",
        "%F5%80%80%80", "%FF", "%E2%28%A1", "%C3+", "%C3%00"};
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        struct mica_ValueResult value = mica_value_string(worker, (const uint8_t *)invalid[i], strlen(invalid[i]));
        assert(value.f_ok);
        mica_value_root_set(&input, value.f_value);
        mica_value_root_set(&output, source_integer(99));
        assert(mica_source_roundtrip_text_url_decode(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 7);
        assert(mica_value_root_get(&output) == source_integer(99));
    }
    mica_value_root_set(&input, source_integer(1));
    assert(mica_source_roundtrip_text_url_encode(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 7);
    assert(mica_source_roundtrip_text_url_decode(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 7);
    // Exercise every ASCII byte, and every buffer alignment for multibyte scalars.
    uint8_t bytes[4096];
    for (unsigned i = 0; i < sizeof(bytes); ++i) bytes[i] = (uint8_t)(i % 128);
    struct mica_ValueResult value = mica_value_string(worker, bytes, sizeof(bytes));
    assert(value.f_ok);
    mica_value_root_set(&input, value.f_value);
    attempt.f_remaining = 10;
    assert(mica_source_roundtrip_text_url_encode(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_url_encode(worker, &attempt, &tx, mica_value_root_get(&input), &encoded) == 0);
    assert(worker->f_heap->f_collections > collections && attempt.f_remaining < 1000000);
    attempt.f_remaining = 10;
    assert(mica_source_roundtrip_text_url_decode(worker, &attempt, &tx, mica_value_root_get(&encoded), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    worker->f_heap->f_collection_limit = 0;
    collections = worker->f_heap->f_collections;
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_url_decode(worker, &attempt, &tx, mica_value_root_get(&encoded), &output) == 0);
    assert(worker->f_heap->f_collections > collections);
    struct mica_IntResult equal = mica_value_compare(mica_value_root_get(&input), mica_value_root_get(&output));
    assert(equal.f_ok && equal.f_number == 0);
    for (unsigned prefix = 249; prefix <= 255; ++prefix) {
        memset(bytes, 'x', prefix);
        const uint8_t suffix[] = "\xc2\xa2\xe4\xb8\x96\xf0\x9f\x99\x82";
        memcpy(bytes + prefix, suffix, sizeof(suffix) - 1);
        value = mica_value_string(worker, bytes, prefix + sizeof(suffix) - 1);
        assert(value.f_ok);
        mica_value_root_set(&input, value.f_value);
        assert(mica_source_roundtrip_text_url_encode(worker, &attempt, &tx, mica_value_root_get(&input), &encoded) == 0);
        assert(mica_source_roundtrip_text_url_decode(worker, &attempt, &tx, mica_value_root_get(&encoded), &output) == 0);
        equal = mica_value_compare(mica_value_root_get(&input), mica_value_root_get(&output));
        assert(equal.f_ok && equal.f_number == 0);
    }
    assert(mica_kernel_cancel(&tx));
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_url_encode(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 11);
    assert(mica_source_roundtrip_text_url_decode(worker, &attempt, &tx, mica_value_root_get(&encoded), &output) == 11);
    assert(mica_value_root_get(&output) == source_integer(99));
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &output));
    assert(mica_memory_root_pop(worker, &encoded));
    assert(mica_memory_root_pop(worker, &input));
}
