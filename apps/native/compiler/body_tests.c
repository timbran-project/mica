// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static mica_type_Value source_integer(int64_t number) {
    struct mica_ValueResult value = mica_value_int(number);
    assert(value.f_ok);
    return value.f_value;
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols);
int main(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_MemoryRoot code = {0}, image = {0}, result = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096) && mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_kernel_init(&kernel, &worker, source_integer(7), 1));
    assert(mica_task_code_init(&registry, &worker) == 0);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &image));
    assert(mica_memory_root_push(&worker, &result));
    void *library = dlopen(NULL, RTLD_NOW);
    assert(library && mica_task_code_register(&registry, &worker, library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry, (void *)code.f_pointer) == 0);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&kernel, &worker, &tx, source_integer(7)) == 0);
    const char *names[] = {"NativeResult", "NativeQuery", "RelationName", "Arity", "FunctionalKey"};
    for (unsigned i = 0; i < 5; ++i) {
        struct mica_IdResult name = mica_value_symbol_intern(&symbols, (const uint8_t *)names[i], strlen(names[i]), NULL);
        assert(name.f_ok);
        if (i < 2)
            assert(mica_kernel_declare(&tx, 705 + i, name.f_number, 2 + i, i == 0 ? 1 : 0, i == 0 ? 1 : 0, 0) == 0);
        else
            assert(mica_kernel_declare_catalogue(&tx, 705 + i, name.f_number, i - 1) == 0);
    }
    assert(mica_kernel_commit(&tx) == 0);
    assert(mica_kernel_end(&tx));
    mica_type_Value arguments = source_body_arguments(&worker, &symbols);
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_source_roundtrip_method_2_0,
        (void *)code.f_pointer, source_integer(7), arguments, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl task = {0};
    struct mica_TaskRuntime runtime = {.f_symbols = &symbols};
    struct mica_TaskServices services = {0};
    // Corpus helpers charge their internal work, including decimal rounding.
    assert(mica_task_init(&task, &worker, &kernel, &image, 1000000, 3, 16, &runtime) == 0);
    assert(mica_task_execute(&task, &worker) == 0);
    assert(mica_task_dispatch(&task, &worker, &services) == 0);
    struct mica_MemoryRoot acquired = {0};
    assert(mica_memory_acquire(&worker, &task.f_image, &acquired));
    mica_value_root_set(&result, ((struct mica_TaskImage *)acquired.f_pointer)->f_input);
    assert(mica_memory_root_pop(&worker, &acquired));
    assert(mica_task_release(&task, &worker));
    assert(mica_memory_safepoint(&worker, true));
    check_source_value(mica_value_root_get(&result), &symbols, &worker);
    source_body_checks(&worker, &kernel, &code, &symbols);
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_root_pop(&worker, &result));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_root_pop(&worker, &code));
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
