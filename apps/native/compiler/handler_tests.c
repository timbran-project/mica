// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>

static mica_type_Value source_integer(int64_t number);

struct handler_segment {
    struct mica_MemoryHeap *heap;
    struct mica_TaskControl *task;
};

static void *handler_execute(void *opaque) {
    struct handler_segment *segment = opaque;
    struct mica_MemoryWorker worker = {0};
    struct mica_TaskServices services = {0};
    assert(mica_memory_worker_init(&worker, segment->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_execute(segment->task, &worker) == 0);
    assert(mica_task_dispatch(segment->task, &worker, &services) == 0);
    assert(mica_memory_safepoint(&worker, true));
    assert(!worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void handler_run_segment(struct mica_MemoryWorker *worker, struct mica_TaskControl *task) {
    struct handler_segment segment = {worker->f_heap, task};
    pthread_t thread;
    // The controlling worker parks while a distinct worker executes the task.
    // Joining guarantees that worker is gone before the next resumption.
    assert(mica_memory_leave(worker));
    assert(!pthread_create(&thread, NULL, handler_execute, &segment));
    assert(!pthread_join(thread, NULL));
    assert(mica_memory_enter(worker));
    assert(mica_memory_safepoint(worker, true));
}

static bool handler_fact(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel, int64_t key) {
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    mica_type_Value cells[] = {source_integer(key), source_integer(1)}, found[1];
    struct mica_ValueResult pattern = mica_value_list(worker, cells, 2);
    assert(pattern.f_ok);
    struct mica_KernelScanResult scanned = mica_kernel_scan(&tx, 705, pattern.f_value, 3, 0, found, 1);
    assert(scanned.f_status == 0);
    assert(mica_kernel_end(&tx));
    return scanned.f_count == 1;
}

static void handler_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime) {
    struct mica_KernelTransaction setup = {0};
    assert(mica_kernel_begin(kernel, worker, &setup, source_integer(7)) == 0);
    assert(mica_kernel_declare(&setup, 705, 705, 2, 1, 1, 0) == 0);
    assert(mica_kernel_commit(&setup) == 0 && mica_kernel_end(&setup));
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    for (unsigned mode = 0; mode < 5; ++mode) {
        mica_type_Value argument = source_integer(mode);
        struct mica_ValueResult arguments = mica_value_list(worker, &argument, 1);
        assert(arguments.f_ok);
        image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_1_0,
            (void *)code->f_pointer, source_integer(7), arguments.f_value, 0, NULL, NULL, 0, NULL, NULL);
        assert(image.f_pointer);
        struct mica_TaskControl task = {0};
        assert(mica_task_init(&task, worker, kernel, &image, 100000, 3, 16, runtime) == 0);
        uint64_t collections = worker->f_heap->f_collections;
        uint64_t prior_generation = 0;
        for (unsigned phase = 0; phase < 3; ++phase) {
            uint64_t remaining = task.f_remaining;
            handler_run_segment(worker, &task);
            assert(task.f_remaining < remaining);
            assert(handler_fact(worker, kernel, 100 + mode * 10 + phase));
            if (phase == 2) {
                assert(mica_task_observe(&task, worker).f_state == 5);
                struct mica_MemoryRoot completed = {0};
                assert(mica_memory_acquire(worker, &task.f_image, &completed));
                mica_type_Value value = ((struct mica_TaskImage *)completed.f_pointer)->f_input;
                assert(mica_value_list_length(value).f_number == 2);
                assert(mica_value_list_get(value, 0).f_value == source_integer(mode));
                struct mica_ValueResult payload = mica_value_list_get(value, 1);
                assert(payload.f_ok);
                assert(mica_value_list_get(payload.f_value, 1).f_value == source_integer(mode));
                struct mica_HeapStringResult text = mica_value_as_string(mica_value_list_get(payload.f_value, 0).f_value);
                assert(text.f_ok && text.f_header->f_length == 24);
                assert(!memcmp(text.f_header->f_data, "retained handler payload", 24));
                assert(mica_memory_root_pop(worker, &completed));
                break;
            }
            assert(mica_task_observe(&task, worker).f_state == 4);
            uint64_t generation = task.f_generation;
            if (prior_generation)
                assert(mica_task_wake(&task, worker, prior_generation, source_integer(99)) != 0);
            if ((mode == 3 && phase == 0) || (mode == 4 && phase == 1)) {
                // Cancellation discards the continuation without running cleanup.
                assert(mica_memory_leave(worker));
                assert(mica_task_cancel(&task));
                assert(mica_memory_enter(worker));
                assert(mica_task_observe(&task, worker).f_state == 6);
                assert(mica_task_wake(&task, worker, generation, source_integer(99)) != 0);
                assert(!handler_fact(worker, kernel, 101 + mode * 10 + phase));
                break;
            }
            assert(!handler_fact(worker, kernel, 101 + mode * 10 + phase));
            assert(mica_task_wake(&task, worker, generation, source_integer(41 + phase)) == 0);
            assert(mica_task_wake(&task, worker, generation, source_integer(99)) != 0);
            prior_generation = generation;
        }
        assert(worker->f_heap->f_collections > collections);
        assert(mica_task_release(&task, worker));
    }
    assert(mica_memory_root_pop(worker, &image));
}
