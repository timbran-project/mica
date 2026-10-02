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
    struct mica_MemoryHeap heap = {.f_trace = mica_memory_test_trace, .f_capacity_limit = UINT64_MAX};
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
    struct mica_MemoryHeap heap = {.f_trace = mica_memory_test_trace, .f_capacity_limit = UINT64_MAX};
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
    struct mica_MemoryHeap heap = {.f_capacity_limit = UINT64_MAX};
    struct mica_MemoryWorker worker = {.f_heap = &heap};
    heap.f_workers = &worker;
    struct mica_MemoryObject *kept = mica_memory_mature_allocate(&heap, 27, 0).f_object;
    struct mica_MemoryObject *discarded = mica_memory_mature_allocate(&heap, 31, 0).f_object;
    struct mica_MemoryObject *large = mica_memory_mature_allocate(&heap, 1000003, 0).f_object;
    assert(kept && discarded && large);
    memset(mica_memory_payload(kept), 0x66, 27);
    memset(mica_memory_payload(large), 0x77, 1000003);
    struct mica_MemoryRoot second = {.f_pointer = mica_memory_payload(large)};
    struct mica_MemoryRoot first = {.f_next = &second, .f_pointer = mica_memory_payload(kept)};
    worker.f_roots = &first;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 27 + 1000003);
    uint64_t allocated = heap.f_allocated;
    assert(mica_memory_mature_allocate(&heap, 19, 0).f_object);
    assert(heap.f_allocated == allocated);
    assert(first.f_pointer[26] == 0x66 && second.f_pointer[1000002] == 0x77);
    first.f_next = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 27 && heap.f_allocated < allocated);
    worker.f_roots = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_allocated == 0);
    assert(!mica_memory_mature_allocate(&heap, 0, 0).f_object);
    assert(!mica_memory_mature_allocate(&heap, UINT64_MAX, 0).f_object);
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
    struct mica_MemoryHeap heap = {.f_capacity_limit = UINT64_MAX};
    struct mica_MemoryRoot roots[2] = {{0}, {0}};
    struct mica_MemoryWorker worker = {.f_heap = &heap, .f_roots = roots};
    heap.f_workers = &worker;
    roots[0].f_next = &roots[1];
    struct mica_MemoryObject *first = mica_memory_mature_allocate(&heap, 4096, 0).f_object;
    assert(first);
    uint64_t capacity = first->f_page->f_capacity;
    roots[0].f_pointer = mica_memory_payload(first);
    for (uint64_t i = 1; i < 3 * capacity; ++i) {
        struct mica_MemoryObject *object = mica_memory_mature_allocate(&heap, 4096, 0).f_object;
        assert(object);
        if (i == 2 * capacity) roots[1].f_pointer = mica_memory_payload(object);
    }
    // Retain the first and last pages, with an empty page between them.
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 8192);
    assert(heap.f_pages && heap.f_pages->f_next && !heap.f_pages->f_next->f_next);
    size_t before = allocations;
    for (uint64_t i = 0; i < 2 * capacity - 2; ++i) {
        struct mica_MemoryObject *object = mica_memory_mature_allocate(&heap, 4096, 0).f_object;
        assert(object);
        assert(object->f_page == heap.f_pages || object->f_page == heap.f_pages->f_next);
        memset(mica_memory_payload(object), 0x81, 4096);
    }
    assert(allocations == before);
    assert(mica_memory_mature_allocate(&heap, 4096, 0).f_object);
    assert(allocations == before + 1);
    worker.f_roots = NULL;
    assert(mica_memory_collect_stopped(&heap));
    assert(heap.f_retained == 0 && heap.f_allocated == 0);
    for (unsigned i = 0; i < 10; ++i) assert(heap.f_free.elements[i] == NULL);
}

// Copying for publication must preserve private aliases, cycles, and views.
static void share_graph_without_collection(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot root = {0}, shared = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_worker_init(&worker, &heap, 32768));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &root));
    struct mica_MemoryTestPair *pair = (void *)mica_memory_allocate(&worker, sizeof(*pair), 1);
    *pair = (struct mica_MemoryTestPair){.f_value = 71};
    root.f_pointer = (void *)pair;
    // Surviving GC alone must not make a mutable worker object immutable.
    assert(mica_memory_safepoint(&worker, true));
    pair = (void *)root.f_pointer;
    struct mica_MemoryTestView *view = (void *)mica_memory_allocate(&worker, sizeof(*view), 2);
    uint8_t *bytes = mica_memory_allocate(&worker, 100, 0);
    memset(bytes, 0x79, 100);
    *view = (struct mica_MemoryTestView){.f_base = bytes, .f_offset = 9, .f_data = bytes + 9};
    pair->f_left = (void *)pair;
    pair->f_right = (void *)view;
    uint64_t collections = heap.f_collections, used = worker.f_used;
    struct mica_MemoryTestPair *copy = (void *)mica_memory_share(&worker, (void *)pair);
    assert(copy && copy != pair && copy->f_left == (void *)copy && copy->f_value == 71);
    assert(pair->f_left == (void *)pair && pair->f_right == (void *)view);
    struct mica_MemoryTestView *copied_view = (void *)copy->f_right;
    assert(copied_view != view && copied_view->f_base != bytes);
    assert(copied_view->f_data == copied_view->f_base + 9 && copied_view->f_data[0] == 0x79);
    assert(heap.f_collections == collections && worker.f_used == used);
    assert(mica_memory_object((void *)pair)->f_next == NULL);
    root.f_pointer = (void *)copy;
    uint64_t copied = heap.f_copied;
    assert(mica_memory_share(&worker, (void *)copy) == (void *)copy);
    assert(heap.f_copied == copied);
    assert(mica_memory_publish(&worker, &root, &shared));
    assert(heap.f_collections == collections && heap.f_copied == copied);
    assert(mica_memory_root_pop(&worker, &root));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    // The published graph outlives the worker and its nursery.
    assert(mica_memory_worker_init(&worker, &heap, 1024));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(shared.f_pointer == (void *)copy && copy->f_left == (void *)copy);
    assert(copied_view->f_data[0] == 0x79);
    copied = heap.f_copied;
    assert(mica_memory_share(&worker, shared.f_pointer) == shared.f_pointer);
    assert(heap.f_copied == copied); // Major GC preserves the immutable state.
    assert(mica_memory_unpublish(&worker, &shared));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void failed_share_preserves_source(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_worker_init(&worker, &heap, 262144));
    assert(mica_memory_enter(&worker));
    struct mica_MemoryTestPair *pair = (void *)mica_memory_allocate(&worker, sizeof(*pair), 1);
    uint8_t *bytes = mica_memory_allocate(&worker, 100000, 0);
    memset(bytes, 0x38, 100000);
    *pair = (struct mica_MemoryTestPair){.f_left = bytes, .f_right = (void *)pair};
    uint64_t used = worker.f_used;
    fail_after = 1; // Reserve the pair, then fail on its large child.
    assert(!mica_memory_share(&worker, (void *)pair));
    fail_after = SIZE_MAX;
    assert(pair->f_left == bytes && pair->f_right == (void *)pair);
    assert(bytes[99999] == 0x38 && worker.f_used == used);
    assert(mica_memory_object((void *)pair)->f_next == NULL);
    assert(mica_memory_object((void *)pair)->f_work == NULL);
    assert(mica_memory_object(bytes)->f_next == NULL);
    assert(heap.f_copied == 0 && heap.f_collections == 0);
    struct mica_MemoryTestPair *copy = (void *)mica_memory_share(&worker, (void *)pair);
    assert(copy && copy->f_right == (void *)copy && copy->f_left[99999] == 0x38);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct copy_control { uint64_t remaining, calls, bytes; };
static bool check_copy_control(uint8_t *context, uint64_t bytes, uint64_t steps) {
    struct copy_control *control = (void *)context;
    (void)steps;
    ++control->calls;
    control->bytes += bytes;
    if (!control->remaining) return false;
    --control->remaining;
    return true;
}

// Refuse every checkpoint in turn, including partial payload copying and
// destination rewriting. No failed attempt may change the source graph.
static void bounded_publication(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_worker_init(&worker, &heap, 262144));
    assert(mica_memory_enter(&worker));
    struct mica_MemoryTestPair *pair = (void *)mica_memory_allocate(&worker, sizeof(*pair), 1);
    uint8_t *bytes = mica_memory_allocate(&worker, 100000, 0);
    assert(pair && bytes);
    memset(bytes, 0x38, 100000);
    *pair = (struct mica_MemoryTestPair){.f_left = bytes, .f_right = (void *)pair};
    unsigned failures = 0;
    bool succeeded = false;
    for (unsigned allowance = 0; allowance < 100; ++allowance) {
        struct copy_control accounting = {.remaining = allowance};
        struct mica_MemoryControl control = {.f_context = (void *)&accounting, .f_check = check_copy_control};
        mica_memory_control_push(&worker, &control);
        struct mica_MemoryTestPair *copy = (void *)mica_memory_share(&worker, (void *)pair);
        mica_memory_control_pop(&worker);
        assert(!worker.f_control);
        assert(pair->f_left == bytes && pair->f_right == (void *)pair);
        for (unsigned i = 0; i < 100000; ++i) assert(bytes[i] == 0x38);
        assert(!mica_memory_object((void *)pair)->f_next && !mica_memory_object((void *)pair)->f_work);
        assert(!mica_memory_object(bytes)->f_next && !mica_memory_object(bytes)->f_work);
        if (copy) {
            assert(copy != pair && copy->f_left != bytes && copy->f_right == (void *)copy);
            assert(!memcmp(copy->f_left, bytes, 100000));
            assert(accounting.bytes == sizeof(*pair) + 100000);
            succeeded = true;
            break;
        }
        ++failures;
        assert(!heap.f_copied && !heap.f_collections);
    }
    assert(succeeded && failures > 25);
    struct copy_control outer = {.remaining = 0}, inner = {.remaining = UINT64_MAX};
    struct mica_MemoryControl a = {.f_context = (void *)&outer, .f_check = check_copy_control};
    struct mica_MemoryControl b = {.f_context = (void *)&inner, .f_check = check_copy_control};
    mica_memory_control_push(&worker, &a);
    mica_memory_control_push(&worker, &b);
    assert(!mica_memory_allocate(&worker, 16, 0));
    assert(outer.calls == 1 && inner.calls == 1);
    mica_memory_control_pop(&worker);
    assert(worker.f_control == &a);
    mica_memory_control_pop(&worker);
    assert(!worker.f_control && mica_memory_allocate(&worker, 16, 0));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void nursery_spill_uses_heap_budget(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_worker_init(&worker, &heap, 256));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &root));
    root.f_pointer = mica_memory_allocate(&worker, 32, 0);
    assert(root.f_pointer);
    memset(root.f_pointer, 0x52, 32);
    for (unsigned i = 0; i < 16; ++i) assert(mica_memory_allocate(&worker, 4096, 0));
    assert(heap.f_allocated > 0 && heap.f_allocated < heap.f_collection_limit);
    assert(mica_memory_safepoint(&worker, false));
    assert(heap.f_collections == 0 && root.f_pointer[31] == 0x52);
    while (heap.f_allocated < heap.f_collection_limit) assert(mica_memory_allocate(&worker, 4096, 0));
    assert(mica_memory_safepoint(&worker, false));
    assert(heap.f_collections == 1 && root.f_pointer[31] == 0x52);
    assert(mica_memory_root_pop(&worker, &root));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void heap_growth_requests_collection(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_worker_init(&worker, &heap, 262144));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &root));
    root.f_pointer = mica_memory_allocate(&worker, 100000, 0);
    assert(root.f_pointer);
    memset(root.f_pointer, 0x82, 100000);
    for (unsigned i = 0; i < 20; ++i) assert(mica_memory_share(&worker, root.f_pointer));
    assert(worker.f_used < worker.f_capacity && heap.f_allocated >= heap.f_collection_limit);
    assert(heap.f_collections == 0);
    assert(mica_memory_safepoint(&worker, false));
    assert(heap.f_collections == 1 && heap.f_retained == 100000);
    assert(root.f_pointer[99999] == 0x82);
    assert(mica_memory_root_pop(&worker, &root));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
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

static void capacity_admission(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0}, other = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_set_capacity(&heap, 8192));
    assert(mica_memory_worker_init(&worker, &heap, 8192));
    assert(heap.f_nursery_bytes == 8192);
    assert(!mica_memory_worker_init(&other, &heap, 1));
    assert(!other.f_heap && heap.f_nursery_bytes == 8192);
    assert(!mica_memory_set_capacity(&heap, 8191) && heap.f_capacity_limit == 8192);
    assert(mica_memory_enter(&worker));
    struct copy_control accounting = {.remaining = UINT64_MAX};
    struct mica_MemoryControl control = {.f_context = (void *)&accounting, .f_check = check_copy_control};
    mica_memory_control_push(&worker, &control);
    size_t before = allocations;
    assert(!mica_memory_allocate(&worker, 90000, 0));
    assert(control.f_denied && allocations == before && !heap.f_allocated);
    mica_memory_control_pop(&worker);
    uint64_t page = sizeof(struct mica_MemoryPage) + ((90000 + sizeof(struct mica_MemoryObject) + 7) & ~UINT64_C(7));
    assert(mica_memory_set_capacity(&heap, 8192 + page));
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_root_push(&worker, &root));
    root.f_pointer = mica_memory_allocate(&worker, 90000, 0);
    assert(root.f_pointer && heap.f_allocated == page);
    assert(!mica_memory_set_capacity(&heap, 8192 + page - 1));
    assert(!mica_memory_allocate(&worker, 16000, 0));
    assert(mica_memory_root_pop(&worker, &root));
    assert(heap.f_allocated == page); // Root release is not reclamation.
    assert(mica_memory_safepoint(&worker, false)); // Pressure requests collection.
    assert(!heap.f_allocated && !heap.f_pressure);
    assert(mica_memory_set_capacity(&heap, 8192 + page));
    mica_memory_control_push(&worker, &control);
    fail_after = 0;
    assert(!mica_memory_allocate(&worker, 90000, 0));
    fail_after = SIZE_MAX;
    assert(!control.f_denied && !heap.f_allocated); // Actual allocator failure.
    mica_memory_control_pop(&worker);
    assert(mica_memory_leave(&worker));
    fail_after = 0;
    assert(!mica_memory_worker_init(&other, &heap, 1024));
    fail_after = SIZE_MAX;
    assert(heap.f_nursery_bytes == 8192 && !other.f_heap);
    assert(mica_memory_worker_release(&worker));
    assert(!heap.f_nursery_bytes && mica_memory_set_capacity(&heap, 0));
    assert(mica_memory_worker_init(&other, &heap, 0));
    assert(mica_memory_worker_release(&other));
    assert(mica_memory_heap_release(&heap));
}

static void bounded_collection_and_sharing(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_set_capacity(&heap, 16384));
    assert(mica_memory_worker_init(&worker, &heap, 16384));
    assert(mica_memory_enter(&worker));
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_root_push(&worker, &root));
    root.f_pointer = mica_memory_allocate(&worker, 7000, 0);
    assert(root.f_pointer);
    uint8_t *source = root.f_pointer;
    memset(source, 0x45, 7000);
    uint64_t used = worker.f_used;
    struct copy_control accounting = {.remaining = UINT64_MAX};
    struct mica_MemoryControl control = {.f_context = (void *)&accounting, .f_check = check_copy_control};
    mica_memory_control_push(&worker, &control);
    assert(!mica_memory_share(&worker, source));
    assert(control.f_denied && !heap.f_allocated);
    assert(!mica_memory_object(source)->f_next && !mica_memory_object(source)->f_work);
    mica_memory_control_pop(&worker);
    mica_memory_control_push(&worker, &control);
    assert(!mica_memory_safepoint(&worker, true));
    assert(control.f_denied && heap.f_collection_limited && heap.f_pressure);
    assert(root.f_pointer == source && worker.f_used == used && !heap.f_allocated);
    for (unsigned i = 0; i < 7000; ++i) assert(source[i] == 0x45);
    mica_memory_control_pop(&worker);
    assert(mica_memory_root_pop(&worker, &root));
    assert(worker.f_used == used);
    assert(mica_memory_safepoint(&worker, false));
    assert(!heap.f_collection_limited && !heap.f_pressure && !worker.f_used);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct nursery_admission {
    struct mica_MemoryHeap *heap;
    _Atomic unsigned attempted, admitted;
    _Atomic bool release;
};
static void *admit_nursery(void *opaque) {
    struct nursery_admission *job = opaque;
    struct mica_MemoryWorker worker = {0};
    bool admitted = mica_memory_worker_init(&worker, job->heap, 4096);
    if (admitted) atomic_fetch_add(&job->admitted, 1);
    atomic_fetch_add(&job->attempted, 1);
    while (!atomic_load(&job->release)) sched_yield();
    if (admitted) assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void concurrent_capacity_admission(void) {
    struct mica_MemoryHeap heap = {0};
    assert(mica_memory_heap_init(&heap, mica_memory_test_trace));
    assert(mica_memory_set_capacity(&heap, 4 * 4096));
    struct nursery_admission job = {.heap = &heap};
    pthread_t threads[8];
    for (unsigned i = 0; i < 8; ++i) assert(!pthread_create(&threads[i], NULL, admit_nursery, &job));
    while (atomic_load(&job.attempted) != 8) sched_yield();
    assert(atomic_load(&job.admitted) == 4 && heap.f_nursery_bytes == 4 * 4096);
    atomic_store(&job.release, true);
    for (unsigned i = 0; i < 8; ++i) assert(!pthread_join(threads[i], NULL));
    assert(!heap.f_nursery_bytes);
    assert(mica_memory_heap_release(&heap));
}

int main(void) {
    capacity_admission();
    bounded_collection_and_sharing();
    concurrent_capacity_admission();
    relocation_and_cycles();
    failed_promotion_preserves_roots();
    page_reuse_and_large_objects();
    failed_collection_preserves_private_allocations();
    sweep_unlinks_empty_pages_from_free_lists();
    share_graph_without_collection();
    failed_share_preserves_source();
    bounded_publication();
    nursery_spill_uses_heap_budget();
    heap_growth_requests_collection();
    worker_lifecycle();
    assert(allocations == releases);
    return 0;
}
