// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>

static const uint64_t steps = 1000000;
static const uint64_t first_segment = 997;

struct execution_test {
    struct mica_MemoryHeap heap;
    struct mica_MemoryRoot checkpoint;
};

static uint64_t sum(uint64_t n) { return n * (n + 1) / 2; }

static void *suspend_segment(void *argument) {
    struct execution_test *test = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_worker_init(&worker, &test->heap, 65536));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &root));
    struct mica_ExecutionState *state = (void *)mica_memory_allocate(&worker, sizeof(*state), 0);
    assert(state);
    *state = (struct mica_ExecutionState){.f_remaining = steps, .f_sum = 0};
    root.f_pointer = (void *)state;
    assert(mica_execution_step(&worker, &root, first_segment) == sum(steps) - sum(steps - first_segment));
    assert(mica_memory_publish(&worker, &root, &test->checkpoint));
    assert(mica_memory_root_pop(&worker, &root));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void *resume_segment(void *argument) {
    struct execution_test *test = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_worker_init(&worker, &test->heap, 65536));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_acquire(&worker, &test->checkpoint, &root));
    assert(mica_execution_step(&worker, &root, steps) == sum(steps));
    assert(mica_memory_safepoint(&worker, true));
    const struct mica_ExecutionState *state = (void *)root.f_pointer;
    assert(state->f_remaining == 0 && state->f_sum == sum(steps));
    assert(mica_memory_root_pop(&worker, &root));
    // Resumption preserves the published checkpoint for another replay.
    assert(mica_memory_acquire(&worker, &test->checkpoint, &root));
    state = (void *)root.f_pointer;
    assert(state->f_remaining == steps - first_segment);
    assert(state->f_sum == sum(steps) - sum(steps - first_segment));
    assert(mica_memory_unpublish(&worker, &test->checkpoint));
    assert(mica_memory_root_pop(&worker, &root));
    assert(mica_memory_safepoint(&worker, true));
    assert(test->heap.f_retained == 0 && test->heap.f_allocated == 0);
    assert(test->heap.f_collections > 100);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

int main(void) {
    struct execution_test test = {0};
    assert(mica_memory_heap_init(&test.heap, mica_memory_test_trace));
    pthread_attr_t attributes;
    assert(!pthread_attr_init(&attributes));
    // A million non-tail calls cannot fit on this stack.
    assert(!pthread_attr_setstacksize(&attributes, 256 * 1024));
    pthread_t producer, consumer;
    assert(!pthread_create(&producer, &attributes, suspend_segment, &test));
    assert(!pthread_join(producer, NULL));
    assert(!pthread_create(&consumer, &attributes, resume_segment, &test));
    assert(!pthread_join(consumer, NULL));
    assert(!pthread_attr_destroy(&attributes));
    assert(mica_memory_heap_release(&test.heap));
    return 0;
}
