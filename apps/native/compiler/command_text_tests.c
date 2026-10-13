// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

typedef struct mica_source_roundtrip_CommandTextResult (*CommandTextOperation)(
    struct mica_MemoryWorker *, struct mica_TaskAttempt *,
    struct mica_KernelTransaction *, mica_type_Value);

struct CommandCancellation {
    struct mica_KernelTransaction *transaction;
    unsigned checks;
};
static bool command_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)bytes; (void)steps;
    struct CommandCancellation *state = (void *)context;
    if (++state->checks != 2) return true;
    assert(mica_kernel_cancel(state->transaction));
    return false;
}
static void command_set_text(struct mica_MemoryWorker *worker,
    struct mica_MemoryRoot *root, const char *bytes) {
    struct mica_ValueResult text = mica_value_string(worker,
        (const uint8_t *)bytes, strlen(bytes));
    assert(text.f_ok);
    mica_value_root_set(root, text.f_value);
}
static void command_control_checks(struct mica_MemoryWorker *worker,
    struct mica_Kernel *kernel, CommandTextOperation operation, bool words) {
    struct mica_MemoryRoot text = {0};
    assert(mica_memory_root_push(worker, &text));
    char input[1029];
    memset(input, words ? 'x' : '0', 1024);
    memcpy(input + 1024, words ? " end" : "21st", 5);
    command_set_text(worker, &text, input);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    struct mica_MemoryRoot *roots = worker->f_roots;
    struct mica_MemoryControl *control = worker->f_control;
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    worker->f_heap->f_collection_limit = 0;
    uint64_t collections = worker->f_heap->f_collections;
    struct mica_source_roundtrip_CommandTextResult parsed = operation(worker,
        &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 0 && parsed.f_shape == 0);
    if (words) {
        assert(worker->f_heap->f_collections > collections);
        assert(mica_value_list_length(parsed.f_value).f_number == 2);
    } else {
        assert(parsed.f_value == source_integer(21));
    }
    assert(attempt.f_remaining < 1000000 - 1024);
    assert(worker->f_roots == roots && worker->f_control == control);
    for (unsigned budget = 0; budget < 2; ++budget) {
        attempt.f_remaining = budget ? 500 : 0;
        parsed = operation(worker, &attempt, &tx, mica_value_root_get(&text));
        assert(parsed.f_status == 10 && !parsed.f_value);
        assert(worker->f_roots == roots && worker->f_control == control);
    }
    if (words) {
        uint64_t bytes_limit = mica_kernel_work(&tx)->f_bytes_limit;
        mica_kernel_work(&tx)->f_bytes_limit = 1;
        attempt.f_remaining = 1000000;
        parsed = operation(worker, &attempt, &tx, mica_value_root_get(&text));
        assert(parsed.f_status == 10 && !parsed.f_value);
        mica_kernel_work(&tx)->f_bytes_limit = bytes_limit;
    }
    attempt.f_remaining = 1000000;
    parsed = operation(worker, &attempt, &tx, source_integer(1));
    assert(parsed.f_status == 0 && parsed.f_shape == 1 && !parsed.f_value);
    if (words) {
        // Word construction allocates; cancel from its second allocation check.
        struct CommandCancellation state = {.transaction = &tx};
        struct mica_MemoryControl cancel = {.f_context = (void *)&state, .f_check = command_cancel};
        mica_memory_control_push(worker, &cancel);
        parsed = operation(worker, &attempt, &tx, mica_value_root_get(&text));
        assert(parsed.f_status == 11 && !parsed.f_value);
        assert(state.checks == 2 && worker->f_roots == roots && worker->f_control == &cancel);
        mica_memory_control_pop(worker);
    } else {
        // Ordinal parsing allocates nothing, so allocator hooks cannot cancel it.
        assert(mica_kernel_cancel(&tx));
        parsed = operation(worker, &attempt, &tx, mica_value_root_get(&text));
        assert(parsed.f_status == 11 && !parsed.f_value);
        assert(worker->f_roots == roots && worker->f_control == control);
    }
    command_set_text(worker, &text, "");
    parsed = operation(worker, &attempt, &tx, mica_value_root_get(&text));
    assert(parsed.f_status == 11 && !parsed.f_value);
    worker->f_heap->f_collection_limit = collection_limit;
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &text));
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    (void)code; (void)symbols;
    command_control_checks(worker, kernel, mica_source_roundtrip_command_words, true);
    command_control_checks(worker, kernel, mica_source_roundtrip_command_parse_ordinal, false);
    struct mica_MemoryRoot text = {0};
    assert(mica_memory_root_push(worker, &text));
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    const char *cases[] = {"36028797018963968", "9223372036854775807", "9223372036854775808", "36028797018963968x", "first-"};
    const uint64_t shapes[] = {3, 3, 2, 2, 2};
    for (unsigned i = 0; i < sizeof(cases) / sizeof(*cases); ++i) {
        command_set_text(worker, &text, cases[i]);
        struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
        struct mica_source_roundtrip_CommandTextResult parsed = mica_source_roundtrip_command_parse_ordinal(
            worker, &attempt, &tx, mica_value_root_get(&text));
        assert(parsed.f_status == 0 && parsed.f_shape == shapes[i] && !parsed.f_value);
    }
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &text));
}
