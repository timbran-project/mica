// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

#ifdef MICA_MEMORY_TEST_ALLOCATOR
static _Atomic long allocation_budget = -1;
uint8_t *mica_foreign_memory_platform_allocate(uint64_t size) {
    long remaining = atomic_load(&allocation_budget);
    if (remaining == 0) return NULL;
    if (remaining > 0) atomic_fetch_sub(&allocation_budget, 1);
    return malloc(size);
}
void mica_foreign_memory_platform_release(uint8_t *pointer) { free(pointer); }
#endif

enum { OK, UNKNOWN, ARITY, NONPERSISTENT, KEY, CONFLICT, NAME, SCHEMA, OOM, CLOSED, LIMIT, CANCELLED, PERMISSION };

static mica_type_Value integer(int64_t number) {
    struct mica_ValueResult value = mica_value_int(number);
    assert(value.f_ok);
    return value.f_value;
}

struct fixture {
    struct mica_MemoryHeap heap;
    struct mica_MemoryRoot image;
    struct mica_MemoryRoot effects;
};

enum { RUNNABLE = 1, RUNNING, DELIVERING, WAITING, COMPLETED, TASK_CANCELLED, FAILED };

// Only the fixture controls this rendezvous. Generated entries leave managed
// execution before waiting, so collection can proceed while both drafts exist.

static void *prepare(void *argument) {
    struct fixture *fixture = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot image = {0}, effects = {0}, ordered = {0};
    assert(mica_memory_worker_init(&worker, &fixture->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &image));
    assert(mica_memory_root_push(&worker, &effects));
    assert(mica_memory_root_push(&worker, &ordered));
    uint8_t backing[512];
    memset(backing, 's', sizeof(backing));
    struct mica_ValueResult text = mica_value_string(&worker, backing, sizeof(backing));
    assert(text.f_ok);
    text = mica_value_string_slice(&worker, text.f_value, 19, 36);
    assert(text.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), text.f_value, integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_memory_publish(&worker, &image, &fixture->image));
    for (uint64_t kind = 1; kind <= 4; ++kind) {
        struct mica_TaskImage *saved = (void *)image.f_pointer;
        assert(mica_task_stage(&worker, &effects, kind, integer((int64_t)kind), saved->f_environment) == OK);
        assert(mica_memory_safepoint(&worker, true));
    }
    uint8_t *before = effects.f_pointer;
    assert(mica_task_stage(&worker, &effects, 0, integer(0), integer(0)) == SCHEMA);
    assert(effects.f_pointer == before);
    assert(mica_task_order_effects(&worker, &effects, &ordered) == OK);
    assert(((struct mica_TaskEffect *)effects.f_pointer)->f_kind == 4);
    assert(((struct mica_TaskEffect *)ordered.f_pointer)->f_kind == 1);
    assert(mica_memory_publish(&worker, &ordered, &fixture->effects));
    assert(mica_memory_root_pop(&worker, &ordered));
    assert(mica_memory_root_pop(&worker, &effects));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void *resume(void *argument) {
    struct fixture *fixture = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot image = {0}, effects = {0}, original = {0};
    assert(mica_memory_worker_init(&worker, &fixture->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_acquire(&worker, &fixture->image, &image));
    assert(mica_memory_acquire(&worker, &fixture->effects, &effects));
    assert(mica_memory_acquire(&worker, &fixture->image, &original));
    struct mica_ValueResult input = mica_value_string(&worker, (const uint8_t *)"delivered once", 14);
    assert(input.f_ok);
    assert(mica_task_deliver_image(&worker, &image, input.f_value) == OK);
    for (unsigned round = 0; round < 8; ++round) {
        assert(mica_memory_safepoint(&worker, true));
        struct mica_TaskImage *saved = (void *)image.f_pointer;
        struct mica_TaskImage *waiting = (void *)original.f_pointer;
        assert(saved != waiting);
        assert(waiting->f_input == integer(0));
        assert(mica_value_string_length(saved->f_input).f_number == 14);
        assert(mica_value_string_length(saved->f_environment).f_number == 17);
        assert(saved->f_handlers == NULL && saved->f_depth == 3);
        assert(saved->f_entry == mica_task_test_entry);
        unsigned count = 0;
        for (struct mica_TaskEffect *effect = (void *)effects.f_pointer; effect; effect = effect->f_next) {
            assert(effect->f_kind == ++count);
            assert(effect->f_target == integer(count));
            assert(mica_value_string_length(effect->f_payload).f_number == 17);
        }
        assert(count == 4);
    }
    assert(mica_memory_unpublish(&worker, &fixture->effects));
    assert(mica_memory_unpublish(&worker, &fixture->image));
    assert(mica_memory_root_pop(&worker, &original));
    assert(mica_memory_root_pop(&worker, &effects));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_safepoint(&worker, true));
    assert(fixture->heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static mica_type_Value pair(struct mica_MemoryWorker *worker, int64_t key, int64_t value) {
    mica_type_Value cells[] = {integer(key), integer(value)};
    struct mica_ValueResult row = mica_value_list(worker, cells, 2);
    assert(row.f_ok);
    return row.f_value;
}

static mica_type_Value permission(struct mica_MemoryWorker *worker, int64_t tenant, uint64_t relation) {
    struct mica_ValueResult identity = mica_value_identity(relation);
    assert(identity.f_ok);
    mica_type_Value cells[] = {integer(tenant), identity.f_value};
    struct mica_ValueResult row = mica_value_list(worker, cells, 2);
    assert(row.f_ok);
    return row.f_value;
}

static void attempts(struct mica_MemoryHeap *heap) {
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction setup = {0};
    struct mica_MemoryRoot source = {0}, shared = {0};
    struct mica_TaskAttempt attempt = {0};
    assert(mica_memory_worker_init(&worker, heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &setup, integer(7)) == OK);
    assert(mica_kernel_declare(&setup, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&setup) == OK);
    assert(mica_kernel_end(&setup));
    assert(mica_memory_root_push(&worker, &source));
    source.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), integer(8), integer(42), NULL, NULL, 3, NULL, NULL);
    assert(source.f_pointer);
    assert(mica_memory_publish(&worker, &source, &shared));
    assert(mica_task_attempt_open(&attempt, &worker, &kernel, &shared, 100, 2, 8, NULL) == OK);
    assert(mica_task_attempt_open(&attempt, &worker, &kernel, &shared, 100, 2, 8, NULL) == CLOSED);
    assert(mica_task_attempt_retry(&attempt) == CLOSED);
    assert(mica_task_poll(&attempt, 4) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
    assert(mica_task_stage(&worker, &attempt.f_effects, 1, integer(7), integer(2)) == OK);
    assert(mica_task_deliver_image(&worker, &attempt.f_current, integer(999)) == OK);
    struct mica_KernelTransaction winner = {0};
    assert(mica_kernel_begin(&kernel, &worker, &winner, integer(7)) == OK);
    assert(mica_kernel_write(&winner, 1, pair(&worker, 1, 3), true) == OK);
    assert(mica_kernel_commit(&winner) == OK);
    assert(mica_kernel_end(&winner));
    assert(mica_kernel_commit(&attempt.f_transaction) == CONFLICT);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(attempt.f_remaining == 96 && attempt.f_retries == 1);
    assert(!attempt.f_effects.f_pointer);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskImage *retry = (void *)attempt.f_retry.f_pointer;
    struct mica_TaskImage *current = (void *)attempt.f_current.f_pointer;
    assert(retry == current && retry->f_input == integer(42));
    assert(retry->f_handlers == NULL && retry->f_depth == 3);
    assert(mica_kernel_work(&attempt.f_transaction)->f_authority == (void *)attempt.f_authority.f_pointer);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 3), false) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
    assert(mica_kernel_commit(&attempt.f_transaction) == OK);
    assert(mica_task_attempt_retry(&attempt) == CLOSED);
    assert(mica_task_attempt_close(&attempt));
    assert(mica_task_attempt_open(&attempt, &worker, &kernel, &shared, 100, 2, 8, NULL) == OK);
    assert(mica_kernel_cancel(&attempt.f_transaction));
    assert(mica_task_poll(&attempt, 1) == CANCELLED);
    assert(mica_kernel_commit(&attempt.f_transaction) == CANCELLED);
    assert(mica_task_attempt_retry(&attempt) == CLOSED);
    assert(mica_task_attempt_close(&attempt));
    assert(mica_task_attempt_open(&attempt, &worker, &kernel, &shared, 2, 2, 8, NULL) == OK);
    assert(mica_task_poll(&attempt, 3) == LIMIT);
    assert(attempt.f_remaining == 0);
    assert(mica_task_attempt_close(&attempt));
    assert(mica_task_attempt_open(&attempt, &worker, &kernel, &shared, 100, 2, 2, NULL) == OK);
    assert(mica_task_poll(&attempt, 1) == LIMIT);
    assert(mica_task_attempt_close(&attempt));
    assert(!mica_task_attempt_close(&attempt));
    assert(mica_memory_unpublish(&worker, &shared));
    assert(mica_memory_root_pop(&worker, &source));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap->f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
}

struct task_fixture {
    struct mica_MemoryHeap heap;
    struct mica_Kernel kernel;
    struct mica_TaskControl task;
};

static void *run_to_wait(void *argument) {
    struct task_fixture *fixture = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_worker_init(&worker, &fixture->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&fixture->kernel, &worker, integer(7), 64));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), integer(8), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&fixture->task, &worker, &fixture->kernel, &image, 100, 3, 8, NULL) == OK);
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_task_execute(&fixture->task, &worker) == OK);
    assert(fixture->task.f_state == DELIVERING && fixture->task.f_committed);
    assert(!fixture->task.f_active);
    assert(!mica_task_release(&fixture->task, &worker));
    assert(mica_task_execute(&fixture->task, &worker) == CLOSED);
    struct mica_MemoryRoot delivery = {0}, duplicate = {0};
    assert(mica_task_acknowledge(&fixture->task, &worker, OK) == CLOSED);
    assert(mica_task_delivery(&fixture->task, &worker, &delivery) == OK);
    assert(mica_task_delivery(&fixture->task, &worker, &duplicate) == CLOSED);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    assert(boundary->f_kind == 2 && boundary->f_effects);
    assert(boundary->f_effects->f_payload == integer(0));
    assert(!boundary->f_effects->f_next);
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&fixture->task, &worker, OK) == OK);
    assert(fixture->task.f_state == WAITING && fixture->task.f_generation == 1);
    assert(mica_task_acknowledge(&fixture->task, &worker, OK) == CLOSED);
    assert(mica_task_execute(&fixture->task, &worker) == CLOSED);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void *run_after_wait(void *argument) {
    struct task_fixture *fixture = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, &fixture->heap, 4096));
    assert(mica_memory_enter(&worker));
    for (unsigned i = 0; i < 5; ++i) assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_wake(&fixture->task, &worker, 0, integer(42)) == CLOSED);
    assert(fixture->task.f_state == WAITING);
    assert(mica_task_wake(&fixture->task, &worker, 1, integer(42)) == OK);
    assert(mica_task_wake(&fixture->task, &worker, 1, integer(999)) == CLOSED);
    assert(mica_task_execute(&fixture->task, &worker) == OK);
    assert(fixture->task.f_state == DELIVERING && fixture->task.f_committed);
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(&fixture->task, &worker, &delivery) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    assert(boundary->f_kind == 1 && boundary->f_effects);
    assert(boundary->f_image->f_input == integer(42));
    assert(boundary->f_effects->f_payload == integer(42));
    assert(boundary->f_image->f_handlers == NULL && boundary->f_image->f_depth == 3);
    assert(fixture->task.f_remaining == 98);
    assert(mica_memory_root_pop(&worker, &delivery));
    // Simulate an effect-service failure after publication. No replay is legal.
    assert(mica_task_acknowledge(&fixture->task, &worker, PERMISSION) == OK);
    assert(fixture->task.f_state == FAILED && fixture->task.f_committed);
    assert(fixture->task.f_status == PERMISSION);
    assert(mica_task_execute(&fixture->task, &worker) == CLOSED);
    assert(mica_task_release(&fixture->task, &worker));
    assert(mica_kernel_release(&fixture->kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(fixture->heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

struct concurrent_task {
    struct mica_MemoryHeap *heap;
    struct mica_TaskControl *task;
    uint64_t status;
    bool rendezvous;
};

static void *execute_task(void *argument) {
    struct concurrent_task *task = argument;
    if (task->rendezvous) mica_task_test_barrier();
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, task->heap, 4096));
    assert(mica_memory_enter(&worker));
    task->status = mica_task_execute(task->task, &worker);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void concurrent_commits(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskControl tasks[2] = {{0}};
    struct mica_KernelTransaction setup = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &setup, integer(7)) == OK);
    assert(mica_kernel_declare(&setup, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&setup) == OK);
    assert(mica_kernel_end(&setup));
    for (unsigned i = 0; i < 2; ++i) {
        struct mica_MemoryRoot image = {0};
        assert(mica_memory_root_push(&worker, &image));
        mica_type_Value row = pair(&worker, 1, i + 1);
        image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_write, NULL,
            integer(7), row, integer(i + 1), NULL, NULL, 3, NULL, NULL);
        assert(image.f_pointer);
        assert(mica_task_init(&tasks[i], &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
        assert(mica_memory_root_pop(&worker, &image));
    }
    assert(mica_memory_leave(&worker));
    struct concurrent_task inputs[2] = {{&heap, &tasks[0], CLOSED, false}, {&heap, &tasks[1], CLOSED, false}};
    pthread_t threads[2];
    mica_task_test_gate()->f_arrived = 0;
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_create(&threads[i], NULL, execute_task, &inputs[i]));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_memory_enter(&worker));
    assert(inputs[0].status == OK && inputs[1].status == OK);
    assert(tasks[0].f_retries + tasks[1].f_retries == 1);
    for (unsigned i = 0; i < 2; ++i) {
        assert(tasks[i].f_committed && tasks[i].f_state == DELIVERING);
        struct mica_MemoryRoot delivery = {0};
        assert(mica_task_delivery(&tasks[i], &worker, &delivery) == OK);
        struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
        assert(boundary->f_effects && !boundary->f_effects->f_next);
        assert(boundary->f_effects->f_payload == integer(i + 1));
        assert(mica_memory_root_pop(&worker, &delivery));
        assert(mica_task_acknowledge(&tasks[i], &worker, OK) == OK);
        assert(tasks[i].f_state == COMPLETED);
        assert(mica_task_release(&tasks[i], &worker));
    }
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct commit_waiter {
    struct mica_MemoryWorker worker;
    struct mica_TaskControl *task;
    struct mica_MemoryHeap *heap;
    _Atomic unsigned phase;
};

static void *wait_to_commit(void *argument) {
    struct commit_waiter *waiter = argument;
    struct mica_MemoryWorker *worker = &waiter->worker;
    struct mica_TaskAttempt attempt = {0};
    assert(mica_memory_worker_init(worker, waiter->heap, 4096));
    assert(mica_memory_enter(worker));
    assert(mica_task_claim(waiter->task, worker, &attempt) == OK);
    assert(mica_task_test_write(&attempt) == OK);
    assert(mica_task_prepare(waiter->task, worker, &attempt) == OK);
    assert(mica_memory_leave(worker));
    atomic_store(&waiter->phase, 1);
    while (atomic_load(&waiter->phase) != 2) sched_yield();
    assert(mica_memory_enter(worker));
    atomic_store(&waiter->phase, 3);
    assert(mica_task_commit(waiter->task, worker, &attempt) == OK);
    assert(mica_task_finish(waiter->task, worker, &attempt, OK) == OK);
    assert(mica_memory_leave(worker));
    assert(mica_memory_worker_release(worker));
    return NULL;
}

static void collection_with_commit_waiter(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction tx = {0};
    struct mica_TaskControl task = {0};
    struct mica_MemoryRoot image = {0}, delivery = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_write, NULL,
        integer(7), pair(&worker, 1, 2), integer(42), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 0, 8, NULL) == OK);
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_leave(&worker));
    struct commit_waiter waiter = {.task = &task, .heap = &heap};
    pthread_t thread;
    assert(!pthread_create(&thread, NULL, wait_to_commit, &waiter));
    while (atomic_load(&waiter.phase) != 1) sched_yield();
    mica_foreign_memory_lock(kernel.f_mutex);
    atomic_store(&waiter.phase, 2);
    while (atomic_load(&waiter.phase) != 3) sched_yield();
    // Observe parking under the heap mutex, which protects worker activity.
    // The publication lock stays held throughout the forced collection.
    for (;;) {
        mica_foreign_memory_lock(heap.f_mutex);
        bool parked = !waiter.worker.f_active;
        mica_foreign_memory_unlock(heap.f_mutex);
        if (parked) break;
        sched_yield();
    }
    assert(mica_memory_enter(&worker));
    uint64_t collections = heap.f_collections;
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_collections == collections + 1);
    assert(mica_memory_leave(&worker));
    mica_foreign_memory_unlock(kernel.f_mutex);
    assert(!pthread_join(thread, NULL));
    assert(mica_memory_enter(&worker));
    assert(task.f_committed && task.f_state == DELIVERING);
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    assert(boundary->f_effects && !boundary->f_effects->f_next);
    assert(boundary->f_effects->f_payload == integer(42));
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    mica_type_Value rows[2];
    struct mica_KernelScanResult scan = mica_kernel_scan(&tx, 1, pair(&worker, 1, 2), 3, 0, rows, 2);
    assert(scan.f_status == OK && scan.f_count == 1);
    assert(mica_kernel_end(&tx));
    assert(mica_task_release(&task, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void authority_renewal(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction owner = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    for (uint64_t id = 1; id <= 3; ++id)
        assert(mica_kernel_declare(&owner, id, id, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&owner, 4, 4, 1, 0, 0, 0) == OK);
    mica_type_Value principal = integer(7);
    struct mica_ValueResult root_grant = mica_value_list(&worker, &principal, 1);
    assert(root_grant.f_ok && mica_kernel_write(&owner, 4, root_grant.f_value, true) == OK);
    assert(mica_kernel_write(&owner, 2, permission(&worker, 8, 1), true) == OK);
    assert(mica_kernel_write(&owner, 3, permission(&worker, 8, 1), true) == OK);
    assert(mica_kernel_set_authority_policy(&owner, 4, 0, 0, 2, 3, 0, 0) == OK);
    assert(mica_kernel_commit(&owner) == OK);
    assert(mica_kernel_end(&owner));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(8), integer(8), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    assert(mica_kernel_write(&owner, 3, permission(&worker, 8, 1), false) == OK);
    assert(mica_kernel_commit(&owner) == OK);
    assert(mica_kernel_end(&owner));
    assert(mica_kernel_commit(&attempt.f_transaction) == CONFLICT);
    assert(mica_task_detach(&task, &worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(mica_task_attach(&task, &worker, &attempt) == OK);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_kernel_authority_allows(mica_kernel_work(&attempt.f_transaction)->f_authority, 1, 2));
    assert(mica_task_finish(&task, &worker, &attempt, CLOSED) == CLOSED);
    assert(mica_task_release(&task, &worker));

    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    assert(mica_kernel_write(&owner, 3, permission(&worker, 8, 1), true) == OK);
    assert(mica_kernel_commit(&owner) == OK);
    assert(mica_kernel_end(&owner));
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_task_execute(&task, &worker) == OK);
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(task.f_state == WAITING);
    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    assert(mica_kernel_write(&owner, 3, permission(&worker, 8, 1), false) == OK);
    assert(mica_kernel_commit(&owner) == OK);
    assert(mica_kernel_end(&owner));
    assert(mica_task_wake(&task, &worker, task.f_generation, integer(42)) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(!mica_kernel_authority_allows(mica_kernel_work(&attempt.f_transaction)->f_authority, 1, 2));
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == PERMISSION);
    assert(mica_task_finish(&task, &worker, &attempt, PERMISSION) == PERMISSION);
    assert(!task.f_committed && task.f_state == FAILED);
    assert(mica_task_release(&task, &worker));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void cancellation_and_collection(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskControl task = {0};
    struct mica_KernelTransaction tx = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_write, NULL,
        integer(7), pair(&worker, 1, 1), integer(42), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_memory_leave(&worker));
    mica_task_test_gate()->f_arrived = 0;
    struct concurrent_task input = {&heap, &task, CLOSED, false};
    pthread_t thread;
    assert(!pthread_create(&thread, NULL, execute_task, &input));
    struct mica_TaskTestGate *gate = mica_task_test_gate();
    mica_foreign_memory_lock(gate->f_mutex);
    while (gate->f_arrived < 1) mica_foreign_memory_wait(gate->f_mutex);
    assert(mica_task_cancel(&task));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    gate->f_arrived = 2;
    mica_foreign_memory_wake(gate->f_mutex);
    mica_foreign_memory_unlock(gate->f_mutex);
    assert(!pthread_join(thread, NULL));
    assert(input.status == CANCELLED);
    assert(task.f_state == TASK_CANCELLED && !task.f_committed && !task.f_active);
    assert(!task.f_prepared.f_registration && !task.f_committed_view.f_registration);
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    mica_type_Value rows[2];
    struct mica_KernelScanResult scan = mica_kernel_scan(&tx, 1, pair(&worker, 0, 0), 0, 0, rows, 2);
    assert(scan.f_status == OK && scan.f_count == 0);
    assert(mica_kernel_end(&tx));
    assert(mica_task_release(&task, &worker));

#ifdef MICA_MEMORY_TEST_ALLOCATOR
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 0, 8, NULL) == OK);
    struct mica_TaskAttempt attempt = {0};
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_task_test_write(&attempt) == OK);
    // Keep enough private effects live that ordering needs additional pages;
    // an allocator refusal must occur even when small-object pools have slack.
    for (unsigned i = 0; i < 1024; ++i)
        assert(mica_task_stage(&worker, &attempt.f_effects, 1, integer(7), integer(i)) == OK);
    assert(mica_memory_safepoint(&worker, true));
    atomic_store(&allocation_budget, 0);
    uint64_t status = mica_task_prepare(&task, &worker, &attempt);
    atomic_store(&allocation_budget, -1);
    assert(status == OOM);
    assert(!task.f_committed && !task.f_prepared.f_registration && !task.f_committed_view.f_registration);
    assert(mica_task_finish(&task, &worker, &attempt, status) == OOM);
    assert(task.f_state == FAILED);
    assert(mica_task_release(&task, &worker));
#endif

    // Preparation obeys transaction resource limits and preserves the draft
    // when it cannot reserve the frozen boundary graph.
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 0, 8, NULL) == OK);
    struct mica_TaskAttempt bounded = {0};
    assert(mica_task_claim(&task, &worker, &bounded) == OK);
    assert(mica_task_test_write(&bounded) == OK);
    assert(mica_kernel_set_limits(&bounded.f_transaction, UINT64_MAX, UINT64_MAX, UINT64_MAX, 0) == OK);
    assert(mica_task_prepare(&task, &worker, &bounded) == LIMIT);
    assert(!task.f_committed && !task.f_prepared.f_registration && !task.f_committed_view.f_registration);
    assert(bounded.f_effects.f_pointer);
    assert(!worker.f_control);
    assert(mica_task_finish(&task, &worker, &bounded, LIMIT) == LIMIT);
    assert(task.f_state == FAILED && task.f_status == LIMIT);
    assert(mica_task_release(&task, &worker));

    // Once publication has won, cancellation can stop continuation but cannot
    // roll back the write, discard its committed effects, or replay it.
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 0, 8, NULL) == OK);
    assert(mica_task_execute(&task, &worker) == OK);
    assert(mica_memory_leave(&worker));
    assert(mica_task_cancel(&task));
    assert(mica_memory_enter(&worker));
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    assert(boundary->f_effects && boundary->f_effects->f_payload == integer(42));
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(task.f_committed && task.f_state == TASK_CANCELLED);
    assert(mica_task_execute(&task, &worker) == CLOSED);
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    scan = mica_kernel_scan(&tx, 1, pair(&worker, 0, 0), 0, 0, rows, 2);
    assert(scan.f_status == OK && scan.f_count == 1);
    assert(mica_kernel_end(&tx));
    assert(mica_task_release(&task, &worker));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void competing_claims(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), integer(8), integer(42), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    for (unsigned round = 0; round < 16; ++round) {
        struct mica_TaskControl task = {0};
        assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
        assert(mica_memory_leave(&worker));
        mica_task_test_gate()->f_arrived = 0;
        struct concurrent_task inputs[2] = {{&heap, &task, CLOSED, true}, {&heap, &task, CLOSED, true}};
        pthread_t threads[2];
        for (unsigned i = 0; i < 2; ++i) assert(!pthread_create(&threads[i], NULL, execute_task, &inputs[i]));
        for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
        assert(mica_memory_enter(&worker));
        assert((inputs[0].status == OK && inputs[1].status == CLOSED) ||
               (inputs[1].status == OK && inputs[0].status == CLOSED));
        assert(task.f_committed && task.f_state == DELIVERING);
        assert(task.f_remaining == 99);
        struct mica_MemoryRoot delivery = {0};
        assert(mica_task_delivery(&task, &worker, &delivery) == OK);
        struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
        assert(boundary->f_effects && !boundary->f_effects->f_next);
        assert(mica_memory_root_pop(&worker, &delivery));
        assert(mica_task_acknowledge(&task, &worker, OK) == OK);
        assert(task.f_state == COMPLETED);
        assert(mica_task_release(&task, &worker));
    }
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}


struct boundary_event {
    struct mica_MemoryHeap *heap;
    struct mica_TaskControl *task;
    uint64_t generation;
    int64_t input;
    bool acknowledge;
    bool cancel;
    uint64_t status;
};

static void *boundary_event(void *argument) {
    struct boundary_event *event = argument;
    mica_task_test_barrier();
    if (event->cancel) {
        event->status = mica_task_cancel(event->task) ? OK : CLOSED;
        return NULL;
    }
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, event->heap, 4096));
    assert(mica_memory_enter(&worker));
    if (event->acknowledge)
        event->status = mica_task_acknowledge(event->task, &worker, OK);
    else
        event->status = mica_task_wake(event->task, &worker, event->generation, integer(event->input));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void boundary_races(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), integer(8), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    for (unsigned round = 0; round < 64; ++round) {
        struct mica_TaskControl task = {0};
        struct mica_MemoryRoot delivery = {0};
        assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
        assert(mica_task_execute(&task, &worker) == OK);
        assert(mica_task_delivery(&task, &worker, &delivery) == OK);
        struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
        uint64_t generation = boundary->f_generation;
        assert(generation == 1 && boundary->f_kind == 2);
        assert(mica_memory_root_pop(&worker, &delivery));
        int64_t delivered_input = 42;
        unsigned scenario = round % 4;
        if (scenario == 0) {
            // A reply can arrive while the effect service is registering the
            // committed wait. Keep it rooted but do not run it before ack.
            assert(mica_task_wake(&task, &worker, generation, integer(42)) == OK);
            assert(mica_task_wake(&task, &worker, generation, integer(99)) == CLOSED);
            assert(task.f_state == DELIVERING);
            assert(mica_task_execute(&task, &worker) == CLOSED);
            assert(mica_memory_safepoint(&worker, true));
            assert(mica_task_acknowledge(&task, &worker, OK) == OK);
        } else {
            assert(mica_memory_leave(&worker));
            mica_task_test_gate()->f_arrived = 0;
            struct boundary_event events[2] = {
                {.heap = &heap, .task = &task, .generation = generation, .input = 42},
                {.heap = &heap, .task = &task, .generation = generation, .input = 99,
                 .acknowledge = scenario == 1, .cancel = scenario == 2}
            };
            pthread_t threads[2];
            for (unsigned i = 0; i < 2; ++i)
                assert(!pthread_create(&threads[i], NULL, boundary_event, &events[i]));
            for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
            assert(mica_memory_enter(&worker));
            if (scenario == 3) {
                // Readiness and timeout are competing replies to one token.
                assert((events[0].status == OK && events[1].status == CLOSED) ||
                       (events[1].status == OK && events[0].status == CLOSED));
                delivered_input = events[0].status == OK ? 42 : 99;
                assert(mica_task_acknowledge(&task, &worker, OK) == OK);
            } else if (scenario == 1) {
                assert(events[0].status == OK && events[1].status == OK);
            } else {
                assert(events[1].status == OK);
                assert(events[0].status == OK || events[0].status == CLOSED);
                assert(mica_task_acknowledge(&task, &worker, OK) == OK);
                assert(task.f_state == TASK_CANCELLED && task.f_committed);
                assert(!task.f_event.f_registration);
                assert(mica_task_execute(&task, &worker) == CLOSED);
                assert(mica_task_release(&task, &worker));
                continue;
            }
        }
        assert(task.f_state == RUNNABLE && !task.f_event.f_registration);
        assert(mica_task_wake(&task, &worker, generation, integer(99)) == CLOSED);
        assert(mica_task_execute(&task, &worker) == OK);
        assert(mica_task_delivery(&task, &worker, &delivery) == OK);
        boundary = (void *)delivery.f_pointer;
        assert(boundary->f_image->f_input == integer(delivered_input));
        assert(boundary->f_effects->f_payload == integer(delivered_input));
        assert(!boundary->f_effects->f_next);
        assert(mica_memory_root_pop(&worker, &delivery));
        assert(mica_task_acknowledge(&task, &worker, OK) == OK);
        assert(task.f_state == COMPLETED);
        assert(mica_task_release(&task, &worker));
    }
    // An early reply owns its managed input across collection, even before
    // acknowledgement makes the receiving task runnable.
    struct mica_TaskControl early = {0};
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_init(&early, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_task_execute(&early, &worker) == OK);
    assert(mica_task_delivery(&early, &worker, &delivery) == OK);
    uint64_t generation = ((struct mica_TaskBoundary *)delivery.f_pointer)->f_generation;
    assert(mica_memory_root_pop(&worker, &delivery));
    uint8_t backing[512];
    memset(backing, 'r', sizeof(backing));
    struct mica_ValueResult reply = mica_value_string(&worker, backing, sizeof(backing));
    assert(reply.f_ok);
    reply = mica_value_string_slice(&worker, reply.f_value, 21, 421);
    assert(reply.f_ok);
    assert(mica_task_wake(&early, &worker, generation, reply.f_value) == OK);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_value_string_length(((struct mica_TaskImage *)early.f_event.f_pointer)->f_input).f_number == 400);
    assert(mica_task_acknowledge(&early, &worker, OK) == OK);
    struct mica_TaskAttempt attempt = {0};
    assert(mica_task_claim(&early, &worker, &attempt) == OK);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_value_string_length(((struct mica_TaskImage *)attempt.f_retry.f_pointer)->f_input).f_number == 400);
    assert(mica_task_finish(&early, &worker, &attempt, CLOSED) == CLOSED);
    assert(mica_task_release(&early, &worker));
    // Generation wrap must fail before publication, never reuse a wake token.
    struct mica_TaskControl exhausted = {0};
    assert(mica_task_init(&exhausted, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_memory_leave(&worker));
    struct mica_MemoryHeap other_heap = {0};
    struct mica_MemoryWorker other_worker = {0};
    assert(mica_task_heap_init(&other_heap));
    assert(mica_memory_worker_init(&other_worker, &other_heap, 4096));
    assert(mica_memory_enter(&other_worker));
    assert(mica_task_execute(&exhausted, &other_worker) == CLOSED);
    assert(mica_task_wake(&exhausted, &other_worker, 0, integer(1)) == CLOSED);
    assert(!other_worker.f_roots);
    assert(mica_memory_leave(&other_worker));
    assert(mica_memory_worker_release(&other_worker));
    assert(mica_memory_heap_release(&other_heap));
    assert(mica_memory_enter(&worker));
    exhausted.f_generation = UINT64_MAX;
    assert(mica_task_execute(&exhausted, &worker) == LIMIT);
    assert(!exhausted.f_committed && exhausted.f_state == FAILED);
    assert(mica_task_release(&exhausted, &worker));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}


struct publication_race {
    struct mica_MemoryHeap *heap;
    struct mica_TaskControl *task;
    uint64_t status;
};

static void *publish_task(void *argument) {
    struct publication_race *race = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_TaskAttempt attempt = {0};
    assert(mica_memory_worker_init(&worker, race->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_task_claim(race->task, &worker, &attempt) == OK);
    assert(mica_task_test_write(&attempt) == OK);
    assert(mica_task_prepare(race->task, &worker, &attempt) == OK);
    assert(mica_memory_leave(&worker));
    mica_task_test_barrier();
    assert(mica_memory_enter(&worker));
    race->status = mica_task_commit(race->task, &worker, &attempt);
    assert(mica_task_finish(race->task, &worker, &attempt, race->status) == race->status);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void publication_races(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction tx = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    for (unsigned round = 0; round < 32; ++round) {
        struct mica_TaskControl task = {0};
        struct mica_MemoryRoot image = {0};
        assert(mica_memory_root_push(&worker, &image));
        image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_write, NULL,
            integer(7), pair(&worker, round, 1), integer(42), NULL, NULL, 3, NULL, NULL);
        assert(image.f_pointer);
        assert(mica_task_init(&task, &worker, &kernel, &image, 100, 0, 8, NULL) == OK);
        assert(mica_memory_root_pop(&worker, &image));
        assert(mica_memory_leave(&worker));
        mica_task_test_gate()->f_arrived = 0;
        struct publication_race publisher = {&heap, &task, CLOSED};
        struct boundary_event canceller = {.heap = &heap, .task = &task, .cancel = true};
        pthread_t threads[2];
        assert(!pthread_create(&threads[0], NULL, publish_task, &publisher));
        assert(!pthread_create(&threads[1], NULL, boundary_event, &canceller));
        for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
        assert(mica_memory_enter(&worker));
        assert(mica_memory_safepoint(&worker, true));
        assert(publisher.status == OK || publisher.status == CANCELLED);
        assert(task.f_committed == (publisher.status == OK));
        assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
        mica_type_Value rows[1];
        struct mica_KernelScanResult scan = mica_kernel_scan(&tx, 1, pair(&worker, round, 1), 3, 0, rows, 1);
        assert(scan.f_status == OK && scan.f_count == (task.f_committed ? 1 : 0));
        assert(mica_kernel_end(&tx));
        if (task.f_committed) {
            struct mica_MemoryRoot delivery = {0};
            assert(mica_task_delivery(&task, &worker, &delivery) == OK);
            struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
            assert(boundary->f_effects && !boundary->f_effects->f_next);
            assert(boundary->f_effects->f_payload == integer(42));
            assert(mica_memory_root_pop(&worker, &delivery));
            assert(mica_task_acknowledge(&task, &worker, OK) == OK);
            assert(task.f_state == COMPLETED || task.f_state == TASK_CANCELLED);
        } else {
            assert(canceller.status == OK && task.f_state == TASK_CANCELLED);
            assert(!task.f_prepared.f_registration && !task.f_committed_view.f_registration);
        }
        assert(mica_task_execute(&task, &worker) == CLOSED);
        assert(mica_task_release(&task, &worker));
    }
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}


// Construct an adversarial capture graph directly: source collections are
// immutable, but the collector must also handle cycles in runtime captures.
static mica_type_Value retry_capture_graph(struct mica_MemoryWorker *worker) {
    struct mica_MemoryRoot text = {0};
    assert(mica_memory_root_push(worker, &text));
    uint8_t bytes[32768];
    memset(bytes, 'r', sizeof(bytes));
    struct mica_ValueResult backing = mica_value_string(worker, bytes, sizeof(bytes));
    assert(backing.f_ok);
    mica_value_root_set(&text, backing.f_value);
    struct mica_ValueResult slice = mica_value_string_slice(worker, mica_value_root_get(&text), 19, 67);
    assert(slice.f_ok);
    mica_type_Value cells[] = {integer(0), mica_value_root_get(&text), slice.f_value};
    struct mica_ValueResult graph = mica_value_list(worker, cells, 3);
    assert(graph.f_ok);
    const struct mica_HeapList *list = mica_value_as_list(graph.f_value).f_header;
    list->f_data[0] = graph.f_value;
    assert(mica_memory_root_pop(worker, &text));
    return graph.f_value;
}

static void check_retry_capture_graph(mica_type_Value graph) {
    const struct mica_HeapList *list = mica_value_as_list(graph).f_header;
    assert(list && list->f_length == 3 && list->f_data[0] == graph);
    assert(mica_value_string_length(list->f_data[1]).f_number == 32768);
    assert(mica_value_string_length(list->f_data[2]).f_number == 48);
    const struct mica_HeapString *backing = mica_value_as_string(list->f_data[1]).f_header;
    const struct mica_HeapString *slice = mica_value_as_string(list->f_data[2]).f_header;
    assert(backing->f_storage == slice->f_storage);
}

static void retry_after_delivery(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction tx = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot image = {0}, delivery = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), retry_capture_graph(&worker), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_task_execute(&task, &worker) == OK);
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    uint64_t generation = ((struct mica_TaskBoundary *)delivery.f_pointer)->f_generation;
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(mica_task_wake(&task, &worker, generation, integer(42)) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
    for (uint64_t kind = 1; kind <= 4; ++kind)
        assert(mica_task_stage(&worker, &attempt.f_effects, kind, integer(7), integer(999)) == OK);
    // The discarded attempt advances its private current image. Its retry
    // checkpoint must still contain the one delivered input after conflict.
    assert(mica_task_deliver_image(&worker, &attempt.f_current, integer(999)) == OK);
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_write(&tx, 1, pair(&worker, 1, 3), true) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == CONFLICT);
    assert(!task.f_committed && !task.f_prepared.f_registration && !task.f_committed_view.f_registration);
    assert(mica_task_delivery(&task, &worker, &delivery) == CLOSED);
    assert(mica_task_detach(&task, &worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(mica_task_attach(&task, &worker, &attempt) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskImage *checkpoint = (void *)attempt.f_retry.f_pointer;
    struct mica_TaskImage *current = (void *)attempt.f_current.f_pointer;
    assert(checkpoint->f_input == integer(42) && current->f_input == integer(42));
    assert(checkpoint->f_handlers == NULL && checkpoint->f_depth == 3);
    check_retry_capture_graph(checkpoint->f_environment);
    assert(current->f_environment == checkpoint->f_environment);
    assert(!attempt.f_effects.f_pointer);
    assert(mica_task_wake(&task, &worker, generation, integer(99)) == CLOSED);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 3), false) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 4), true) == OK);
    for (uint64_t kind = 1; kind <= 4; ++kind)
        assert(mica_task_stage(&worker, &attempt.f_effects, kind, integer(7), integer(42)) == OK);
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == OK);
    assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    check_retry_capture_graph(boundary->f_image->f_environment);
    unsigned count = 0;
    for (struct mica_TaskEffect *effect = boundary->f_effects; effect; effect = effect->f_next) {
        assert(effect->f_kind == ++count && effect->f_payload == integer(42));
    }
    assert(count == 4);
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_delivery(&task, &worker, &delivery) == CLOSED);
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(mica_task_release(&task, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct queued_worker {
    struct mica_MemoryHeap *heap;
    struct mica_TaskQueue *queue;
    struct mica_TaskControl *tasks;
    uint64_t count;
    _Atomic unsigned *started;
    struct mica_TaskDrainResult result;
    struct mica_TaskServiceProbe probe;
};

static void *drain_tasks(void *argument) {
    struct queued_worker *job = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, job->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &job->probe.f_last));
    struct mica_TaskServices services = {
        .f_context = (void *)&job->probe,
        .f_emit = mica_task_test_service_emit,
        .f_send = mica_task_test_service_send,
        .f_subscribe = mica_task_test_service_subscribe,
        .f_spawn = mica_task_test_service_spawn,
    };
    atomic_fetch_add(job->started, 1);
    job->result = mica_task_drain(job->queue, &worker, job->tasks, job->count, &services);
    assert(job->probe.f_delivered == job->result.f_completed);
    assert(mica_memory_root_pop(&worker, &job->probe.f_last));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void queued_tasks(void) {
    enum { TASKS = 32, WORKERS = 4, EMPTY = 13 };
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskControl tasks[TASKS] = {{0}};
    struct mica_TaskQueue queue = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_task_queue_init(&queue, 0) == SCHEMA);
    assert(mica_task_queue_init(&queue, UINT64_MAX) == LIMIT);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    atomic_store(&allocation_budget, 0);
    assert(mica_task_queue_init(&queue, TASKS) == OOM);
    atomic_store(&allocation_budget, -1);
    assert(!queue.f_mutex && !queue.f_slots);
#endif
    assert(mica_task_queue_init(&queue, TASKS) == OK);
    assert(!mica_task_queue_release(&queue));
    assert(mica_task_drain(&queue, &worker, tasks, TASKS, NULL).f_status == SCHEMA);
    assert(mica_task_queue_take(&queue, &worker, false).f_status == EMPTY);
    // Exercise FIFO wraparound and a full queue before starting consumers.
    for (unsigned round = 0; round < 3; ++round) {
        for (unsigned i = 0; i < TASKS; ++i) assert(mica_task_queue_push(&queue, i) == OK);
        assert(mica_task_queue_push(&queue, 0) == LIMIT);
        for (unsigned i = 0; i < TASKS; ++i) {
            struct mica_TaskQueueResult result = mica_task_queue_take(&queue, &worker, false);
            assert(result.f_status == OK && result.f_id == i);
        }
    }
    for (unsigned i = 0; i < TASKS; ++i) {
        assert(mica_memory_root_push(&worker, &image));
        image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
            integer(7), integer(8), integer(i + 1), NULL, NULL, 3, NULL, NULL);
        assert(image.f_pointer);
        assert(mica_task_init(&tasks[i], &worker, &kernel, &image, i == 0 ? 0 : 100, 1, 8, NULL) == OK);
        assert(mica_memory_root_pop(&worker, &image));
    }
    assert(mica_memory_leave(&worker));
    struct queued_worker jobs[WORKERS] = {0};
    _Atomic unsigned started = 0;
    pthread_t threads[WORKERS];
    for (unsigned i = 0; i < WORKERS; ++i) {
        jobs[i] = (struct queued_worker){.heap = &heap, .queue = &queue, .tasks = tasks, .count = TASKS, .started = &started};
        assert(!pthread_create(&threads[i], NULL, drain_tasks, &jobs[i]));
    }
    // Idle consumers leave managed execution, so this collection can finish
    // before any ready task is submitted. No timed sleep establishes the race.
    while (atomic_load(&started) < WORKERS) sched_yield();
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    for (unsigned i = 0; i < TASKS; ++i) assert(mica_task_queue_push(&queue, i) == OK);
    mica_task_queue_close(&queue);
    assert(mica_task_queue_push(&queue, 0) == CLOSED);
    uint64_t taken = 0, completed = 0;
    for (unsigned i = 0; i < WORKERS; ++i) {
        assert(!pthread_join(threads[i], NULL));
        assert(jobs[i].result.f_status == OK);
        taken += jobs[i].result.f_taken;
        completed += jobs[i].result.f_completed;
    }
    assert(taken == TASKS && completed == TASKS - 1);
    assert(mica_memory_enter(&worker));
    assert(mica_task_queue_take(&queue, &worker, false).f_status == CLOSED);
    for (unsigned i = 0; i < TASKS; ++i) {
        struct mica_MemoryRoot delivery = {0};
        if (i == 0) {
            assert(tasks[i].f_state == FAILED && tasks[i].f_status == LIMIT);
            assert(!tasks[i].f_committed && !tasks[i].f_active);
            assert(mica_task_delivery(&tasks[i], &worker, &delivery) == CLOSED);
            assert(mica_task_release(&tasks[i], &worker));
            continue;
        }
        assert(tasks[i].f_state == COMPLETED && tasks[i].f_committed);
        assert(mica_task_delivery(&tasks[i], &worker, &delivery) == CLOSED);
        assert(mica_task_release(&tasks[i], &worker));
    }
    assert(mica_task_queue_release(&queue));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

#include "services_tests.c"
#include "mailboxes_tests.c"
#include "mailbox_streams_tests.c"
#include "requests_tests.c"
#include "subscription_requests_tests.c"
#include "subscription_messages_tests.c"
#include "subscription_rows_tests.c"
#include "subscriptions_tests.c"
#include "subscription_delivery_tests.c"
#include "subscription_catalogue_tests.c"
#include "subscription_pump_tests.c"
#include "scheduler_tests.c"
#include "handlers_tests.c"
#include "code_tests.c"
#include "dispatch_tests.c"
#include "invocation_tests.c"
#include "calls_tests.c"
#include "spawn_tests.c"

int main(void) {
    assert(mica_task_resources_start());
    mica_task_test_gate()->f_mutex = mica_foreign_memory_sync_create();
    assert(mica_task_test_gate()->f_mutex);
    struct fixture fixture = {0};
    assert(mica_task_heap_init(&fixture.heap));
    pthread_t producer, consumer;
    assert(!pthread_create(&producer, NULL, prepare, &fixture));
    assert(!pthread_join(producer, NULL));
    assert(!pthread_create(&consumer, NULL, resume, &fixture));
    assert(!pthread_join(consumer, NULL));
    attempts(&fixture.heap);
    assert(mica_memory_heap_release(&fixture.heap));
    struct task_fixture task_fixture = {0};
    assert(mica_task_heap_init(&task_fixture.heap));
    assert(!pthread_create(&producer, NULL, run_to_wait, &task_fixture));
    assert(!pthread_join(producer, NULL));
    assert(!pthread_create(&consumer, NULL, run_after_wait, &task_fixture));
    assert(!pthread_join(consumer, NULL));
    assert(mica_memory_heap_release(&task_fixture.heap));
    concurrent_commits();
    collection_with_commit_waiter();
    authority_renewal();
    cancellation_and_collection();
    competing_claims();
    boundary_races();
    publication_races();
    retry_after_delivery();
    queued_tasks();
    service_delivery();
    handler_continuations();
    code_registry();
    code_catalogue_lifetimes();
    code_task_lifetimes();
    code_active_entry();
    dispatch_prototypes();
    dispatch_named_bindings();
    dispatch_restrictions();
    dispatch_signatures();
    dispatch_selection();
    invocation_authority();
    method_calls();
    method_call_drafts();
    method_call_authority();
    method_code_lifetimes();
    method_call_retry();
    method_return_cleanup();
    spawn_transactions();
    spawn_failures();
    spawn_concurrent();
    spawn_late_resolution(false);
    spawn_late_resolution(true);
    spawn_admission_failures();
    spawn_cancel_and_limits();
    assert(mica_task_resources_stop());
    receive_requests();
    subscription_requests();
    subscription_messages();
    subscription_rows();
    subscriptions();
    subscription_delivery();
    subscription_catalogue();
    subscription_pump();
    subscription_events();
    scheduled_tasks(SCHEDULER_CANCEL);
    scheduled_tasks(SCHEDULER_EXTERNAL);
    scheduled_tasks(SCHEDULER_MAILBOX);
    scheduled_timers();
    scheduled_timer_races();
    scheduler_admission();
    scheduler_shutdown_admission();
    mailbox_streams();
    // This fixture deliberately exhausts the process-wide capability namespace.
    mailboxes();
    mica_foreign_memory_sync_destroy(mica_task_test_gate()->f_mutex);
    return 0;
}
