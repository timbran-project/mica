// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>

static mica_type_Value source_integer(int64_t number) {
    struct mica_ValueResult value = mica_value_int(number);
    assert(value.f_ok);
    return value.f_value;
}
static void run_frontend(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime,
    struct mica_MemoryRoot *input, struct mica_MemoryRoot *output, uint64_t steps, bool collect) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    mica_type_Value value = mica_value_root_get(input);
    struct mica_ValueResult arguments = mica_value_list(worker, &value, 1);
    assert(arguments.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_0_0,
        (void *)code->f_pointer, source_integer(7), arguments.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl task = {0};
    struct mica_TaskServices services = {0};
    // Match the call-depth limit used by the Rust-hosted frontend oracle.
    assert(mica_task_init(&task, worker, kernel, &image, steps, 3, 256, runtime) == 0);
    uint64_t limit = worker->f_heap->f_collection_limit;
    if (collect) worker->f_heap->f_collection_limit = 0;
    uint64_t collections = worker->f_heap->f_collections;
    assert(mica_task_execute(&task, worker) == 0);
    if (collect) assert(worker->f_heap->f_collections > collections);
    assert(mica_task_dispatch(&task, worker, &services) == 0);
    struct mica_MemoryRoot acquired = {0};
    assert(mica_memory_acquire(worker, &task.f_image, &acquired));
    mica_value_root_set(output, ((struct mica_TaskImage *)acquired.f_pointer)->f_input);
    assert(mica_memory_root_pop(worker, &acquired));
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &image));
    worker->f_heap->f_collection_limit = limit;
}
static void frontend_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime, struct mica_SymbolTable *symbols) {
    struct mica_MemoryRoot input = {0}, output = {0};
    assert(mica_memory_root_push(worker, &input) && mica_memory_root_push(worker, &output));
    /* FRONTEND_CONFORMANCE_CASES */
    assert(mica_memory_root_pop(worker, &output) && mica_memory_root_pop(worker, &input));
}
int main(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskDispatchContext dispatch = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0};
    struct mica_MemoryRoot code = {0}, prototypes = {0};
    assert(mica_task_resources_start() && mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096) && mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == 0);
    assert(mica_kernel_init(&kernel, &worker, source_integer(7), 1));
    assert(mica_task_code_init(&registry, &worker) == 0);
    assert(mica_memory_root_push(&worker, &code) && mica_memory_root_push(&worker, &prototypes));
    void *library = dlopen(NULL, RTLD_NOW);
    assert(library && mica_task_code_register(&registry, &worker, library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry, (void *)code.f_pointer) == 0);
    mica_type_Value kinds[16];
    for (unsigned i = 0; i < 16; ++i) {
        struct mica_ValueResult kind = mica_value_identity(9001 + i);
        assert(kind.f_ok);
        kinds[i] = kind.f_value;
    }
    struct mica_ValueResult list = mica_value_list(&worker, kinds, 16);
    assert(list.f_ok);
    mica_value_root_set(&prototypes, list.f_value);
    assert(mica_task_dispatch_context_init(&dispatch, &worker, &vocabulary,
        702, 703, 701, 704, mica_value_root_get(&prototypes)) == 0);
    struct mica_KernelTransaction install = {0};
    assert(mica_kernel_begin(&kernel, &worker, &install, source_integer(7)) == 0);
    for (uint64_t id = 701; id <= 704; ++id) {
        uint64_t arity = id == 701 ? 3 : id == 703 ? 6 : 2;
        // Dispatch binds column 1 of MethodSelector(method, selector).
        // The other catalogue lookups bind the primary index's first column.
        uint64_t indexes = id == 702 ? 2 : 0;
        assert(mica_kernel_declare(&install, id, id, arity, 0, 0, indexes) == 0);
    }
    const char *catalogues[] = {"RelationName", "Arity", "FunctionalKey"};
    for (unsigned i = 0; i < 3; ++i) {
        struct mica_IdResult name = mica_value_symbol_intern(&symbols,
            (const uint8_t *)catalogues[i], strlen(catalogues[i]), NULL);
        assert(name.f_ok);
        assert(mica_kernel_declare_catalogue(&install, 707 + i, name.f_number, i + 1) == 0);
    }
    assert(mica_task_source_roundtrip_install(&install, &dispatch, (void *)code.f_pointer, source_integer(7)) == 0);
    assert(mica_kernel_commit(&install) == 0 && mica_kernel_end(&install));
    struct mica_TaskRuntime runtime = {.f_symbols = &symbols, .f_dispatch = &dispatch};
    frontend_checks(&worker, &kernel, &code, &runtime, &symbols);
    assert(mica_task_dispatch_context_release(&dispatch, &worker) == 0);
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_root_pop(&worker, &prototypes) && mica_memory_root_pop(&worker, &code));
    assert(mica_task_code_close(&registry, &worker) == 0);
    struct mica_TaskCodeReap reaped = mica_task_code_reap(&registry, &worker);
    assert(reaped.f_status == 0 && reaped.f_released == 1);
    assert(mica_task_code_release(&registry, &worker) == 0);
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker) && mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
    assert(mica_task_resources_stop());
    return 0;
}
