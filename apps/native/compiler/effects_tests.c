// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static mica_type_Value effect_integer(int64_t number) {
    struct mica_ValueResult value = mica_value_int(number);
    assert(value.f_ok);
    return value.f_value;
}
static mica_type_Value effect_target(uint64_t number) {
    struct mica_ValueResult value = mica_value_identity(number);
    assert(value.f_ok);
    return value.f_value;
}
static mica_type_Value effect_row(struct mica_MemoryWorker *worker, int64_t tenant, uint64_t target, bool unary) {
    mica_type_Value cells[] = {effect_integer(tenant), effect_target(target)};
    struct mica_ValueResult value = mica_value_list(worker, cells, unary ? 1 : 2);
    assert(value.f_ok);
    return value.f_value;
}
static void effect_task(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime, mica_type_Value tenant, mica_type_Value target,
        bool abort, uint64_t expected) {
    struct mica_MemoryRoot image = {0}, delivery = {0};
    assert(mica_memory_root_push(worker, &image));
    mica_type_Value cells[] = {target, effect_integer(42), mica_value_bool(abort)};
    struct mica_ValueResult arguments = mica_value_list(worker, cells, 3);
    assert(arguments.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_0_0,
        (void *)code->f_pointer, tenant, arguments.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl task = {0};
    assert(mica_task_init(&task, worker, kernel, &image, 100000, 3, 16, runtime) == 0);
    assert(mica_task_delivery(&task, worker, &delivery) == 9);
    assert(mica_task_execute(&task, worker) == expected);
    if (expected == 0) {
        assert(mica_task_delivery(&task, worker, &delivery) == 0);
        assert(mica_memory_safepoint(worker, true));
        struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
        assert(boundary->f_image->f_input == effect_integer(42));
        struct mica_TaskEffect *first = boundary->f_effects;
        assert(first && first->f_kind == 1 && first->f_target == target && first->f_payload == effect_integer(42));
        struct mica_TaskEffect *second = first->f_next;
        assert(second && second->f_kind == 1 && second->f_target == target && !second->f_next);
        assert(mica_value_list_length(second->f_payload).f_number == 2);
        assert(mica_value_list_get(second->f_payload, 0).f_value == effect_integer(42));
        assert(mica_memory_root_pop(worker, &delivery));
        assert(mica_task_acknowledge(&task, worker, 0) == 0);
    }
    assert(mica_task_delivery(&task, worker, &delivery) == 9);
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &image));
}
int main(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskRuntime runtime = {.f_symbols = &symbols};
    struct mica_MemoryRoot code = {0};
    assert(mica_task_resources_start() && mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096) && mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_kernel_init(&kernel, &worker, effect_integer(7), 1));
    assert(mica_task_code_init(&registry, &worker) == 0 && mica_memory_root_push(&worker, &code));
    void *library = dlopen(NULL, RTLD_NOW);
    assert(library && mica_task_code_register(&registry, &worker, library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry, (void *)code.f_pointer) == 0);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&kernel, &worker, &tx, effect_integer(7)) == 0);
    for (uint64_t id = 1; id <= 3; ++id)
        assert(mica_kernel_declare(&tx, id, id, id == 1 ? 1 : 2, 0, 0, 0) == 0);
    assert(mica_kernel_write(&tx, 1, effect_row(&worker, 7, 0, true), true) == 0);
    assert(mica_kernel_write(&tx, 2, effect_row(&worker, 8, 77, false), true) == 0);
    assert(mica_kernel_write(&tx, 2, effect_row(&worker, 8, 0, false), true) == 0);
    // Read rights with the same numeric identity confer no emission right.
    assert(mica_kernel_write(&tx, 3, effect_row(&worker, 8, 78, false), true) == 0);
    assert(mica_kernel_set_authority_policy(&tx, 1, 0, 0, 3, 0, 0, 0, 2) == 0);
    assert(mica_kernel_commit(&tx) == 0 && mica_kernel_end(&tx));
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(8), effect_target(77), false, 0);
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(8), effect_target(0), false, 0);
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(8), effect_target(78), false, 12);
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(9), effect_target(77), false, 12);
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(7), effect_target(78), false, 0);
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(7), effect_integer(77), false, 7);
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(8), effect_target(77), true, 14);
    assert(mica_kernel_begin(&kernel, &worker, &tx, effect_integer(7)) == 0);
    assert(mica_kernel_write(&tx, 2, effect_row(&worker, 8, 77, false), false) == 0);
    assert(mica_kernel_commit(&tx) == 0 && mica_kernel_end(&tx));
    effect_task(&worker, &kernel, &code, &runtime, effect_integer(8), effect_target(77), false, 12);
    assert(mica_kernel_release(&kernel, &worker));
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
