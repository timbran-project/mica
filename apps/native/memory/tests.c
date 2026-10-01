// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include <stdatomic.h>
#include <pthread.h>
#include <sched.h>

static _Atomic size_t allocations;
static _Atomic size_t releases;
static size_t fail_after = SIZE_MAX;

uint8_t *mica_foreign_memory_platform_allocate(uint64_t size) {
    if (fail_after == 0) return NULL;
    if (fail_after != SIZE_MAX) --fail_after;
    uint8_t *result = malloc(size);
    if (result) ++allocations;
    return result;
}

void mica_foreign_memory_platform_release(uint8_t *pointer) {
    if (pointer) ++releases;
    free(pointer);
}

static void relocation_and_cycles(void) {
    struct mica_MemoryHeap heap = {.f_trace = mica_memory_test_trace};
    struct mica_MemoryWorker worker = {.f_heap = &heap, .f_capacity = 65536};
    worker.f_nursery = malloc(worker.f_capacity);
    assert(worker.f_nursery);
    heap.f_workers = &worker;

    uint8_t *bytes = mica_memory_nursery_allocate(&worker, 1001, 0);
    assert(bytes && mica_memory_payload(mica_memory_object(bytes)) == bytes);
    for (unsigned i = 0; i < 1001; ++i) bytes[i] = (uint8_t)i;
    struct mica_MemoryTestPair *a = (void *)mica_memory_nursery_allocate(&worker, sizeof(*a), 1);
    struct mica_MemoryTestPair *b = (void *)mica_memory_nursery_allocate(&worker, sizeof(*b), 1);
    struct mica_MemoryTestView *view = (void *)mica_memory_nursery_allocate(&worker, sizeof(*view), 2);
    assert(a && b && view);
    *a = (struct mica_MemoryTestPair){.f_left = (void *)b, .f_right = (void *)view, .f_value = 17};
    *b = (struct mica_MemoryTestPair){.f_left = (void *)a, .f_right = bytes, .f_value = 23};
    *view = (struct mica_MemoryTestView){.f_base = bytes, .f_offset = 19, .f_data = bytes + 19};
    struct mica_MemoryRoot second = {.f_pointer = (void *)a};
    struct mica_MemoryRoot first = {.f_next = &second, .f_pointer = (void *)a};
    worker.f_roots = &first;
    assert(mica_memory_collect_stopped(&heap));
    assert(worker.f_used == 0 && first.f_pointer == second.f_pointer && first.f_pointer != (void *)a);
    memset(worker.f_nursery, 0xa5, worker.f_capacity);
    a = (void *)first.f_pointer;
    b = (void *)a->f_left;
    view = (void *)a->f_right;
    assert(a->f_value == 17 && b->f_value == 23 && b->f_left == (void *)a);
    assert(view->f_base == b->f_right && view->f_data == view->f_base + 19);
    for (unsigned i = 0; i < 1001; ++i) assert(b->f_right[i] == (uint8_t)i);
    uint64_t retained = 1001 + 2 * sizeof(*a) + sizeof(*view);
    assert(heap.f_retained == retained && heap.f_copied == retained);
    assert(mica_memory_collect_stopped(&heap));
    assert(first.f_pointer == (void *)a && heap.f_retained == retained);
    worker.f_roots = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 0 && heap.f_allocated == 0 && heap.f_pages == NULL);
    free(worker.f_nursery);
}

static void failed_promotion_preserves_roots(void) {
    struct mica_MemoryHeap heap = {.f_trace = mica_memory_test_trace};
    struct mica_MemoryWorker worker = {.f_heap = &heap, .f_capacity = 65536};
    worker.f_nursery = malloc(worker.f_capacity);
    assert(worker.f_nursery);
    heap.f_workers = &worker;
    uint8_t *small = mica_memory_nursery_allocate(&worker, 13, 0);
    uint8_t *large = mica_memory_nursery_allocate(&worker, 4096, 0);
    assert(small && large);
    memset(small, 0x12, 13);
    memset(large, 0x34, 4096);
    struct mica_MemoryRoot second = {.f_pointer = large};
    struct mica_MemoryRoot first = {.f_next = &second, .f_pointer = small};
    worker.f_roots = &first;
    uint64_t used = worker.f_used;
    fail_after = 1;
    assert(!mica_memory_collect_stopped(&heap));
    fail_after = SIZE_MAX;
    assert(first.f_pointer == small && second.f_pointer == large && worker.f_used == used);
    assert(mica_memory_object(small)->f_forward == NULL && mica_memory_object(large)->f_forward == NULL);
    assert(mica_memory_object(small)->f_mark == 0 && mica_memory_object(large)->f_mark == 0);
    assert(heap.f_allocated == 0 && heap.f_copied == 0 && heap.f_collections == 0);
    for (unsigned i = 0; i < 13; ++i) assert(small[i] == 0x12);
    for (unsigned i = 0; i < 4096; ++i) assert(large[i] == 0x34);
    assert(mica_memory_collect_stopped(&heap));
    assert(first.f_pointer != small && second.f_pointer != large);
    worker.f_roots = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_allocated == 0);
    free(worker.f_nursery);
}

static void page_reuse_and_large_objects(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {.f_heap = &heap};
    heap.f_workers = &worker;
    struct mica_MemoryObject *kept = mica_memory_mature_allocate(&heap, 27, 0);
    struct mica_MemoryObject *discarded = mica_memory_mature_allocate(&heap, 31, 0);
    struct mica_MemoryObject *large = mica_memory_mature_allocate(&heap, 1000003, 0);
    assert(kept && discarded && large);
    memset(mica_memory_payload(kept), 0x66, 27);
    memset(mica_memory_payload(large), 0x77, 1000003);
    struct mica_MemoryRoot second = {.f_pointer = mica_memory_payload(large)};
    struct mica_MemoryRoot first = {.f_next = &second, .f_pointer = mica_memory_payload(kept)};
    worker.f_roots = &first;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 27 + 1000003);
    uint64_t allocated = heap.f_allocated;
    assert(mica_memory_mature_allocate(&heap, 19, 0));
    assert(heap.f_allocated == allocated);
    assert(first.f_pointer[26] == 0x66 && second.f_pointer[1000002] == 0x77);
    first.f_next = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 27 && heap.f_allocated < allocated);
    worker.f_roots = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_allocated == 0);
    assert(!mica_memory_mature_allocate(&heap, 0, 0));
    assert(!mica_memory_mature_allocate(&heap, UINT64_MAX, 0));
    assert(!mica_memory_nursery_allocate(&worker, UINT64_MAX, 0));
}

static void failed_collection_preserves_private_allocations(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker spill = {0}, nursery = {0};
    struct mica_MemoryRoot spill_root = {0}, nursery_root = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_worker_init(&spill, &heap, 64));
    assert(mica_memory_enter(&spill));
    assert(mica_memory_root_push(&spill, &spill_root));
    spill_root.f_pointer = mica_memory_allocate(&spill, 27, 0);
    assert(spill_root.f_pointer);
    memset(spill_root.f_pointer, 0x37, 27);
    uint64_t private_state = mica_memory_object(spill_root.f_pointer)->f_state;
    assert(mica_memory_leave(&spill));
    assert(mica_memory_worker_init(&nursery, &heap, 8192));
    assert(mica_memory_enter(&nursery));
    assert(mica_memory_root_push(&nursery, &nursery_root));
    nursery_root.f_pointer = mica_memory_allocate(&nursery, 4096, 0);
    assert(nursery_root.f_pointer);
    memset(nursery_root.f_pointer, 0x49, 4096);
    fail_after = 0;
    assert(!mica_memory_safepoint(&nursery, true));
    fail_after = SIZE_MAX;
    assert(heap.f_retained == 27);
    assert(mica_memory_enter(&spill));
    assert(mica_memory_object(spill_root.f_pointer)->f_state == private_state);
    for (unsigned i = 0; i < 27; ++i) assert(spill_root.f_pointer[i] == 0x37);
    assert(mica_memory_leave(&spill));
    assert(mica_memory_safepoint(&nursery, true));
    assert(heap.f_retained == 27 + 4096);
    assert(mica_memory_root_pop(&nursery, &nursery_root));
    assert(mica_memory_leave(&nursery));
    assert(mica_memory_worker_release(&nursery));
    assert(mica_memory_enter(&spill));
    assert(mica_memory_root_pop(&spill, &spill_root));
    assert(mica_memory_safepoint(&spill, true));
    assert(heap.f_retained == 0 && heap.f_allocated == 0);
    assert(mica_memory_leave(&spill));
    assert(mica_memory_worker_release(&spill));
    assert(mica_memory_heap_release(&heap));
}

static void sweep_unlinks_empty_pages_from_free_lists(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryRoot roots[2] = {{0}, {0}};
    struct mica_MemoryWorker worker = {.f_heap = &heap, .f_roots = roots};
    heap.f_workers = &worker;
    roots[0].f_next = &roots[1];
    struct mica_MemoryObject *first = mica_memory_mature_allocate(&heap, 4096, 0);
    assert(first);
    uint64_t capacity = first->f_page->f_capacity;
    roots[0].f_pointer = mica_memory_payload(first);
    for (uint64_t i = 1; i < 3 * capacity; ++i) {
        struct mica_MemoryObject *object = mica_memory_mature_allocate(&heap, 4096, 0);
        assert(object);
        if (i == 2 * capacity) roots[1].f_pointer = mica_memory_payload(object);
    }
    // Retain the first and last pages, with an empty page between them.
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 8192);
    assert(heap.f_pages && heap.f_pages->f_next && !heap.f_pages->f_next->f_next);
    size_t before = allocations;
    for (uint64_t i = 0; i < 2 * capacity - 2; ++i) {
        struct mica_MemoryObject *object = mica_memory_mature_allocate(&heap, 4096, 0);
        assert(object);
        assert(object->f_page == heap.f_pages || object->f_page == heap.f_pages->f_next);
        memset(mica_memory_payload(object), 0x81, 4096);
    }
    assert(allocations == before);
    assert(mica_memory_mature_allocate(&heap, 4096, 0));
    assert(allocations == before + 1);
    worker.f_roots = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 0 && heap.f_allocated == 0);
    for (unsigned i = 0; i < 10; ++i) assert(heap.f_free.elements[i] == NULL);
}

struct worker_test {
    struct mica_MemoryHeap *heap;
    unsigned rounds;
};

static void *worker_run(void *argument) {
    struct worker_test *test = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, test->heap, 1024));
    assert(mica_memory_enter(&worker));
    assert(!mica_memory_enter(&worker));
    struct mica_MemoryRoot root = {0};
    mica_memory_root_push(&worker, &root);
    for (unsigned i = 0; i < test->rounds; ++i) {
        struct mica_MemoryTestPair *node = (void *)mica_memory_allocate(&worker, sizeof(*node), 1);
        assert(node);
        *node = (struct mica_MemoryTestPair){.f_left = root.f_pointer, .f_value = i};
        root.f_pointer = (void *)node;
        assert(mica_memory_safepoint(&worker, i % 17 == 0));
        if (i % 47 == 0) {
            node = (void *)root.f_pointer;
            for (unsigned n = i + 1; n != 0; --n) {
                assert(node && node->f_value == n - 1);
                node = (void *)node->f_left;
            }
            assert(!node);
            assert(mica_memory_leave(&worker));
            sched_yield();
            assert(mica_memory_enter(&worker));
        }
    }
    assert(!mica_memory_worker_release(&worker));
    assert(mica_memory_root_pop(&worker, &root));
    assert(!mica_memory_root_pop(&worker, &root));
    assert(mica_memory_leave(&worker));
    assert(!mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void worker_lifecycle(void) {
    struct mica_MemoryHeap heap = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    struct mica_MemoryWorker idle = {0};
    assert(mica_memory_worker_init(&idle, &heap, 4096));
    assert(!mica_memory_heap_release(&heap));
    assert(!mica_memory_allocate(&idle, 13, 0));
    pthread_t threads[4];
    struct worker_test tests[4];
    for (unsigned i = 0; i < 4; ++i) {
        tests[i] = (struct worker_test){.heap = &heap, .rounds = 150 + 250 * i};
        assert(!pthread_create(&threads[i], NULL, worker_run, &tests[i]));
    }
    for (unsigned i = 0; i < 4; ++i) assert(!pthread_join(threads[i], NULL));
    assert(heap.f_collections != 0);
    assert(mica_memory_enter(&idle));
    assert(mica_memory_safepoint(&idle, true));
    assert(heap.f_allocated == 0 && heap.f_retained == 0);
    assert(mica_memory_leave(&idle));
    assert(mica_memory_worker_release(&idle));
    assert(mica_memory_heap_release(&heap));
}

int main(void) {
    relocation_and_cycles();
    failed_promotion_preserves_roots();
    page_reuse_and_large_objects();
    failed_collection_preserves_private_allocations();
    sweep_unlinks_empty_pages_from_free_lists();
    worker_lifecycle();
    assert(allocations == releases);
    return 0;
}
