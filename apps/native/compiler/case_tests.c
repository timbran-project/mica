// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code; (void)symbols; // No code constants or symbols are needed by these direct helper checks.
    struct mica_MemoryRoot input = {0}, output = {0};
    assert(mica_memory_root_push(worker, &input) && mica_memory_root_push(worker, &output));
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    uint8_t bytes[4096];
    for (unsigned i = 0; i < 600; i += 2) { bytes[i] = 0xc4; bytes[i + 1] = 0xb0; }
    struct mica_ValueResult text = mica_value_string(worker, bytes, 600);
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    mica_value_root_set(&output, source_integer(99));
    attempt.f_remaining = 1;
    assert(mica_source_roundtrip_text_lower(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    uint64_t collections = worker->f_heap->f_collections;
    worker->f_heap->f_collection_limit = 0;
    assert(mica_source_roundtrip_text_lower(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 0);
    assert(worker->f_heap->f_collections > collections && attempt.f_remaining < 1000000);
    struct mica_HeapStringResult lowered = mica_value_as_string(mica_value_root_get(&output));
    assert(lowered.f_ok && lowered.f_header->f_length == 900);
    for (unsigned i = 0; i < 900; i += 3)
        assert(lowered.f_header->f_data[i] == 'i' && lowered.f_header->f_data[i + 1] == 0xcc && lowered.f_header->f_data[i + 2] == 0x87);

    // A long ignorable suffix must charge work and poll during sigma lookahead.
    bytes[0] = 'A'; bytes[1] = 0xce; bytes[2] = 0xa3;
    for (unsigned i = 3; i < 2051; i += 2) { bytes[i] = 0xcc; bytes[i + 1] = 0x81; }
    bytes[2051] = 'B';
    text = mica_value_string(worker, bytes, 2052);
    assert(text.f_ok);
    mica_value_root_set(&input, text.f_value);
    mica_value_root_set(&output, source_integer(99));
    attempt.f_remaining = 100;
    assert(mica_source_roundtrip_text_lower(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_text_lower(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 0);
    lowered = mica_value_as_string(mica_value_root_get(&output));
    assert(lowered.f_ok && lowered.f_header->f_length == 2052);
    assert(lowered.f_header->f_data[0] == 'a' && lowered.f_header->f_data[1] == 0xcf && lowered.f_header->f_data[2] == 0x83);
    assert(lowered.f_header->f_data[2051] == 'b');
    assert(mica_kernel_cancel(&tx));
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_text_lower(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 11);
    assert(mica_value_root_get(&output) == source_integer(99));
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &output) && mica_memory_root_pop(worker, &input));
}
