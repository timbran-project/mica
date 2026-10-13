// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>

static mica_type_Value source_integer(int64_t number);

static mica_type_Value source_body_arguments(struct mica_MemoryWorker *worker, struct mica_SymbolTable *symbols) {
    (void)symbols;
    struct mica_ValueResult text = mica_value_string(worker, NULL, 0);
    assert(text.f_ok);
    struct mica_ValueResult arguments = mica_value_list(worker, &text.f_value, 1);
    assert(arguments.f_ok);
    return arguments.f_value;
}

static void check_source_value(mica_type_Value value, struct mica_SymbolTable *symbols, struct mica_MemoryWorker *worker) {
    (void)symbols; (void)worker;
    assert(mica_value_list_length(value).f_ok && mica_value_list_length(value).f_number == 2);
}

static void run_lexer(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols,
    struct mica_MemoryRoot *input, struct mica_MemoryRoot *output, bool collect) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    mica_type_Value text = mica_value_root_get(input);
    struct mica_ValueResult arguments = mica_value_list(worker, &text, 1);
    assert(arguments.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_2_0,
        (void *)code->f_pointer, source_integer(7), arguments.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl task = {0};
    struct mica_TaskRuntime runtime = {.f_symbols = symbols};
    struct mica_TaskServices services = {0};
    assert(mica_task_init(&task, worker, kernel, &image, 1000000, 3, 16, &runtime) == 0);
    uint64_t collections = worker->f_heap->f_collections;
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    bool nonempty = mica_value_string_length(mica_value_root_get(input)).f_number != 0;
    if (collect) {
        assert(mica_memory_safepoint(worker, true));
        worker->f_heap->f_collection_limit = 0;
    }
    uint64_t before_execution = worker->f_heap->f_collections;
    assert(mica_task_execute(&task, worker) == 0);
    if (collect && nonempty) assert(worker->f_heap->f_collections > before_execution);
    assert(mica_task_dispatch(&task, worker, &services) == 0);
    struct mica_MemoryRoot acquired = {0};
    assert(mica_memory_acquire(worker, &task.f_image, &acquired));
    mica_value_root_set(output, ((struct mica_TaskImage *)acquired.f_pointer)->f_input);
    assert(mica_memory_root_pop(worker, &acquired));
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &image));
    if (collect) {
        assert(mica_memory_safepoint(worker, true));
        assert(worker->f_heap->f_collections > collections);
    }
    worker->f_heap->f_collection_limit = collection_limit;
}

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    struct mica_MemoryRoot input = {0}, output = {0};
    assert(mica_memory_root_push(worker, &input));
    assert(mica_memory_root_push(worker, &output));
    /* LEXER_CONFORMANCE_CASES */
    assert(mica_memory_root_pop(worker, &output));
    assert(mica_memory_root_pop(worker, &input));
}
