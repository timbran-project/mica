// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

// source_cases and their structural result checks come from Rust execution of
// the same source. Every case runs as a native task, including commit or abort.
static void source_conformance(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskRuntime runtime = {.f_symbols = &symbols};
    struct mica_MemoryRoot code = {0}, image = {0}, result = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    mica_type_Value tenant = mica_value_int(7).f_value;
    assert(mica_kernel_init(&kernel, &worker, tenant, 1));
    assert(mica_task_code_init(&registry, &worker) == 0);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &image));
    assert(mica_memory_root_push(&worker, &result));
    void *library = dlopen(NULL, RTLD_NOW);
    assert(library);
    assert(mica_task_code_register(&registry, &worker, library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry, (void *)code.f_pointer) == 0);
    for (size_t i = 0; i < sizeof(source_cases) / sizeof(source_cases[0]); ++i) {
        mica_type_Value arguments = source_cases[i].arguments(&worker, &symbols);
        image.f_pointer = (void *)mica_task_new_Image(&worker, source_cases[i].entry,
            (void *)code.f_pointer, tenant, arguments, 0, NULL, NULL, 0, NULL, NULL);
        assert(image.f_pointer);
        struct mica_TaskControl task = {0};
        assert(mica_task_init(&task, &worker, &kernel, &image, 100000, 3, 16, &runtime) == 0);
        assert(mica_memory_safepoint(&worker, true));
        uint64_t status = mica_task_execute(&task, &worker);
        assert(status == source_cases[i].status);
        struct mica_MemoryRoot acquired = {0};
        if (status) {
            assert(mica_memory_acquire(&worker, &task.f_error, &acquired));
            mica_value_root_set(&result, mica_value_root_get(&acquired));
        } else {
            struct mica_TaskServices services = {0};
            assert(mica_task_dispatch(&task, &worker, &services) == 0);
            assert(mica_task_observe(&task, &worker).f_state == 5);
            assert(mica_memory_acquire(&worker, &task.f_image, &acquired));
            mica_value_root_set(&result, ((struct mica_TaskImage *)acquired.f_pointer)->f_input);
        }
        assert(mica_memory_root_pop(&worker, &acquired));
        assert(mica_task_release(&task, &worker));
        assert(mica_memory_safepoint(&worker, true));
        source_cases[i].check(mica_value_root_get(&result), &symbols);
    }
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
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}
