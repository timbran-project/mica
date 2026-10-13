// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel) {
    struct mica_MemoryRoot input = {0}, text = {0}, separator = {0}, output = {0};
    assert(mica_memory_root_push(worker, &input) && mica_memory_root_push(worker, &text));
    assert(mica_memory_root_push(worker, &separator) && mica_memory_root_push(worker, &output));
    struct mica_ValueResult scalar = mica_value_string(worker, (const uint8_t *)"\xc3\xa9", 2);
    assert(scalar.f_ok);
    mica_value_root_set(&text, scalar.f_value);
    scalar = mica_value_string(worker, (const uint8_t *)"", 0);
    assert(scalar.f_ok);
    mica_value_root_set(&separator, scalar.f_value);
    struct mica_ValueResult parts = mica_value_list(worker, NULL, 0);
    assert(parts.f_ok);
    mica_value_root_set(&input, parts.f_value);
    for (unsigned i = 0; i < 1024; ++i) {
        parts = mica_value_list_append(worker, mica_value_root_get(&input), mica_value_root_get(&text));
        assert(parts.f_ok);
        mica_value_root_set(&input, parts.f_value);
    }
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 10};
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_assemble(worker, &attempt, &tx, mica_value_root_get(&input),
        mica_value_root_get(&separator), true, &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_assemble(worker, &attempt, &tx, mica_value_root_get(&input),
        mica_value_root_get(&separator), true, &output) == 0);
    assert(attempt.f_remaining < 1000000);
    mica_value_root_set(&text, mica_value_root_get(&output));
    assert(mica_memory_safepoint(worker, true));
    assert(mica_value_string_length(mica_value_root_get(&text)).f_number == 1024);
    attempt.f_remaining = 10;
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_characters(worker, &attempt, &tx, mica_value_root_get(&text), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_characters(worker, &attempt, &tx, mica_value_root_get(&text), &output) == 0);
    assert(mica_memory_safepoint(worker, true));
    struct mica_IntResult equal = mica_value_compare(mica_value_root_get(&input), mica_value_root_get(&output));
    assert(equal.f_ok && equal.f_number == 0);
    // Scans retain only rooted strings and numeric cursors across collection.
    attempt.f_remaining = 10;
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_scan(worker, &attempt, &tx, mica_value_root_get(&text), 0,
        mica_value_root_get(&separator), false, &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_scan(worker, &attempt, &tx, mica_value_root_get(&text), 0,
        mica_value_root_get(&separator), false, &output) == 0);
    assert(mica_value_root_get(&output) == source_integer(1024));
    assert(attempt.f_remaining < 1000000);
    // A long non-ASCII membership string is ignored, but still costs task budget.
    attempt.f_remaining = 10;
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_scan(worker, &attempt, &tx, mica_value_root_get(&text), 0,
        mica_value_root_get(&text), true, &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_scan(worker, &attempt, &tx, mica_value_root_get(&text), 0,
        mica_value_root_get(&text), true, &output) == 0);
    assert(mica_value_root_get(&output) == source_integer(0));
    assert(mica_source_roundtrip_text_scan(worker, &attempt, &tx, mica_value_root_get(&text), 9999,
        mica_value_root_get(&separator), false, &output) == 0);
    assert(mica_value_root_get(&output) == source_integer(1024));
    assert(mica_kernel_cancel(&tx));
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_scan(worker, &attempt, &tx, mica_value_root_get(&text), 0,
        mica_value_root_get(&separator), false, &output) == 11);
    assert(mica_value_root_get(&output) == source_integer(99));
    assert(mica_source_roundtrip_text_characters(worker, &attempt, &tx, mica_value_root_get(&text), &output) == 11);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &output) && mica_memory_root_pop(worker, &separator));
    assert(mica_memory_root_pop(worker, &text) && mica_memory_root_pop(worker, &input));
}
