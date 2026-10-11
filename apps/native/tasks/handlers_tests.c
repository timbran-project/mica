// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

enum { LANGUAGE_ERROR = 14 };

static void handler_delivery(struct mica_TaskControl *task, struct mica_MemoryWorker *worker,
                             const int64_t *expected, size_t count) {
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(task, worker, &delivery) == OK);
    assert(mica_memory_safepoint(worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    size_t position = 0;
    for (struct mica_TaskEffect *effect = boundary->f_effects; effect; effect = effect->f_next) {
        assert(position < count && effect->f_kind == 1);
        assert(effect->f_payload == integer(expected[position++]));
    }
    assert(position == count);
    assert(mica_memory_root_pop(worker, &delivery));
    assert(mica_task_acknowledge(task, worker, OK) == OK);
    assert(mica_task_delivery(task, worker, &delivery) == CLOSED);
}

struct handler_runner {
    struct mica_MemoryHeap *heap;
    struct mica_Kernel *kernel;
    struct mica_TaskControl *task;
    unsigned scenario;
    bool resume;
};

static void *handler_thread(void *argument) {
    struct handler_runner *run = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, run->heap, 4096));
    assert(mica_memory_enter(&worker));
    if (!run->resume) {
        struct mica_MemoryRoot image = {0};
        assert(mica_memory_root_push(&worker, &image));
        image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_handler_start, NULL,
            integer(7), integer(run->scenario == 9 ? 2 : run->scenario), integer(0), NULL, NULL, 3, NULL, NULL);
        assert(image.f_pointer);
        assert(mica_task_init(run->task, &worker, run->kernel, &image, 100, 2, 8) == OK);
        assert(mica_memory_root_pop(&worker, &image));
        assert(mica_task_execute(run->task, &worker) == OK);
        handler_delivery(run->task, &worker, (int64_t[]){1}, 1);
        assert(run->task->f_state == WAITING);
    } else if (run->scenario == 7) {
        assert(mica_task_execute(run->task, &worker) == LIMIT);
        assert(run->task->f_state == FAILED && !run->task->f_committed);
    } else {
        struct mica_TaskAttempt attempt = {0};
        assert(mica_task_claim(run->task, &worker, &attempt) == OK);
        assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
        struct mica_TaskImage *current = (void *)attempt.f_current.f_pointer;
        assert(current->f_entry == mica_task_test_handler_resume && current->f_input == integer(42));
        assert(current->f_unwind && current->f_handlers && current->f_depth == 3);
        if (run->scenario == 8) {
            // A return from inside the suspended finalizer replaces its pending
            // error. The outer finalizer must still run before that return.
            struct mica_MemoryRoot target = {0};
            assert(mica_memory_root_push(&worker, &target));
            target.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_handler_return, NULL,
                integer(7), integer(8), integer(0), NULL, NULL, 3, NULL, NULL);
            assert(target.f_pointer);
            assert(mica_task_leave(&attempt, &worker, (void *)target.f_pointer, integer(77)) == OK);
            assert(mica_memory_root_pop(&worker, &target));
            assert(mica_memory_safepoint(&worker, true));
            current = (void *)attempt.f_current.f_pointer;
        }
        uint64_t status = current->f_entry(&attempt);
        if (run->scenario == 2 || run->scenario == 9) {
            assert(status == LANGUAGE_ERROR);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
            if (run->scenario == 9) {
                // The original pending error is already shared. Give the
                // terminal error a private overflow graph so publication must
                // allocate even when the shared small-object pools have slack.
                struct mica_ValueResult error = mica_value_error(&worker, 17, false, 0,
                    true, retry_capture_graph(&worker));
                assert(error.f_ok);
                mica_value_root_set(&attempt.f_error, error.f_value);
                atomic_store(&allocation_budget, 0);
            }
#endif
            uint64_t outcome = mica_task_finish(run->task, &worker, &attempt, status);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
            atomic_store(&allocation_budget, -1);
            assert(outcome == (run->scenario == 9 ? OOM : LANGUAGE_ERROR));
#else
            assert(outcome == LANGUAGE_ERROR);
#endif
            assert(run->task->f_state == FAILED && !run->task->f_committed);
            struct mica_MemoryRoot error = {0};
            if (outcome == LANGUAGE_ERROR) {
                assert(mica_task_error(run->task, &worker, &error) == OK);
                assert(mica_memory_safepoint(&worker, true));
                struct mica_HeapErrorResult value = mica_value_as_error(mica_value_root_get(&error));
                assert(value.f_ok && value.f_header->f_code == 17);
                assert(value.f_header->f_value == integer(2));
                assert(mica_memory_root_pop(&worker, &error));
            } else {
                assert(mica_task_error(run->task, &worker, &error) == CLOSED);
                assert(!run->task->f_error.f_registration);
            }
        } else {
            assert(status == OK && attempt.f_boundary == 1);
            if (run->scenario == 6) {
                struct mica_KernelTransaction winner = {0};
                assert(mica_kernel_begin(run->kernel, &worker, &winner, integer(7)) == OK);
                assert(mica_kernel_write(&winner, 1, pair(&worker, 1, 9), true) == OK);
                assert(mica_kernel_commit(&winner) == OK);
                assert(mica_kernel_end(&winner));
                assert(mica_task_prepare(run->task, &worker, &attempt) == OK);
                assert(mica_task_commit(run->task, &worker, &attempt) == CONFLICT);
                assert(mica_task_detach(run->task, &worker) == OK);
                assert(mica_task_attempt_retry(&attempt) == OK);
                assert(mica_task_attach(run->task, &worker, &attempt) == OK);
                assert(mica_memory_safepoint(&worker, true));
                current = (void *)attempt.f_current.f_pointer;
                assert(current == (void *)attempt.f_retry.f_pointer);
                assert(current->f_entry == mica_task_test_handler_resume && current->f_input == integer(42));
                assert(current->f_unwind && !attempt.f_effects.f_pointer);
                assert(mica_value_as_error(current->f_unwind->f_value).f_header->f_code == 17);
                assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 9), false) == OK);
                assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
                current = (void *)attempt.f_current.f_pointer;
                assert(current->f_entry(&attempt) == OK);
            }
            assert(mica_task_prepare(run->task, &worker, &attempt) == OK);
            assert(mica_task_commit(run->task, &worker, &attempt) == OK);
            assert(mica_task_finish(run->task, &worker, &attempt, OK) == OK);
            if (run->scenario == 4) handler_delivery(run->task, &worker, (int64_t[]){2, 5, 3, 4}, 4);
            else if (run->scenario == 8) handler_delivery(run->task, &worker, (int64_t[]){3, 4}, 2);
            else handler_delivery(run->task, &worker, (int64_t[]){2, 3, 4}, 3);
            assert(run->task->f_state == COMPLETED);
        }
    }
    assert(!worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void handler_continuations(void) {
    for (unsigned scenario = 0; scenario < 10; ++scenario) {
        struct mica_MemoryHeap heap = {0};
        struct mica_MemoryWorker worker = {0};
        struct mica_Kernel kernel = {0};
        struct mica_KernelTransaction setup = {0};
        struct mica_TaskControl task = {0};
        assert(mica_task_heap_init(&heap));
        assert(mica_memory_worker_init(&worker, &heap, 4096));
        assert(mica_memory_enter(&worker));
        assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
        assert(mica_kernel_begin(&kernel, &worker, &setup, integer(7)) == OK);
        assert(mica_kernel_declare(&setup, 1, 1, 2, 1, 1, 0) == OK);
        assert(mica_kernel_commit(&setup) == OK);
        assert(mica_kernel_end(&setup));
        assert(mica_memory_leave(&worker));
        struct handler_runner run = {&heap, &kernel, &task, scenario, false};
        pthread_t thread;
        assert(!pthread_create(&thread, NULL, handler_thread, &run));
        assert(!pthread_join(thread, NULL));
        assert(mica_memory_enter(&worker));
        for (unsigned round = 0; round < 3; ++round) assert(mica_memory_safepoint(&worker, true));
        assert(mica_memory_leave(&worker));
        if (scenario == 5) {
            assert(mica_task_cancel(&task));
            assert(task.f_state == TASK_CANCELLED);
        } else {
            assert(mica_memory_enter(&worker));
            assert(mica_task_wake(&task, &worker, 1, integer(42)) == OK);
            if (scenario == 7) task.f_remaining = 0;
            assert(mica_memory_leave(&worker));
            run.resume = true;
            assert(!pthread_create(&thread, NULL, handler_thread, &run));
            assert(!pthread_join(thread, NULL));
        }
        assert(mica_memory_enter(&worker));
        assert(mica_kernel_begin(&kernel, &worker, &setup, integer(7)) == OK);
        mica_type_Value row[1];
        struct mica_KernelScanResult scan = mica_kernel_scan(&setup, 1, pair(&worker, 1, 2), 3, 0, row, 1);
        assert(scan.f_status == OK && scan.f_count == (scenario == 2 || scenario == 5 || scenario == 7 || scenario == 9 ? 0 : 1));
        assert(mica_kernel_end(&setup));
        assert(mica_task_release(&task, &worker));
        assert(mica_kernel_release(&kernel, &worker));
        assert(mica_memory_safepoint(&worker, true));
        assert(heap.f_retained == 0);
        assert(mica_memory_leave(&worker));
        assert(mica_memory_worker_release(&worker));
        assert(mica_memory_heap_release(&heap));
    }
}
