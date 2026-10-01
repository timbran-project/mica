// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

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

// These values are the public status and conflict-policy codes.
enum { OK, UNKNOWN, ARITY, NONPERSISTENT, KEY, CONFLICT, NAME, SCHEMA, OOM, CLOSED };
enum { SET, FUNCTIONAL, EVENT };

struct fixture {
    struct mica_MemoryHeap heap;
    struct mica_MemoryWorker worker;
    struct mica_Kernel kernel;
};

static mica_type_Value integer(int64_t n) {
    struct mica_ValueResult value = mica_value_int(n);
    assert(value.f_ok);
    return value.f_value;
}
static mica_type_Value tuple(struct mica_MemoryWorker *worker, int64_t a, int64_t b) {
    mica_type_Value cells[] = {integer(a), integer(b)};
    struct mica_ValueResult value = mica_value_list(worker, cells, 2);
    assert(value.f_ok);
    return value.f_value;
}
static int64_t cell(mica_type_Value row, uint64_t i) {
    struct mica_IntResult value = mica_value_as_int(mica_kernel_cell(row, i));
    assert(value.f_ok);
    return value.f_number;
}
static void init(struct fixture *f) {
    memset(f, 0, sizeof(*f));
    assert(mica_kernel_heap_init(&f->heap));
    assert(mica_memory_worker_init(&f->worker, &f->heap, 32768));
    assert(mica_memory_enter(&f->worker));
    assert(mica_kernel_init(&f->kernel, &f->worker));
}
static void destroy(struct fixture *f) {
    assert(mica_kernel_release(&f->kernel, &f->worker));
    assert(mica_memory_safepoint(&f->worker, true));
    assert(f->heap.f_retained == 0);
    assert(mica_memory_leave(&f->worker));
    assert(mica_memory_worker_release(&f->worker));
    assert(mica_memory_heap_release(&f->heap));
}
static void begin(struct fixture *f, struct mica_KernelTransaction *tx) {
    memset(tx, 0, sizeof(*tx));
    assert(mica_kernel_begin(&f->kernel, &f->worker, tx) == OK);
}
static void declare(struct fixture *f, uint64_t id, uint64_t policy) {
    struct mica_KernelTransaction tx = {0};
    begin(f, &tx);
    assert(mica_kernel_declare(&tx, id, id, 2, policy, policy == FUNCTIONAL ? 1 : 0, 2) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
}
static uint64_t write_pair(struct mica_KernelTransaction *tx, uint64_t id, int64_t a, int64_t b, bool asserted) {
    return mica_kernel_write(tx, id, tuple(tx->f_worker, a, b), asserted);
}
static struct mica_KernelScanResult scan(struct mica_KernelTransaction *tx, uint64_t id, int64_t a, int64_t b, uint64_t mask, mica_type_Value *rows, uint64_t capacity) {
    return mica_kernel_scan(tx, id, tuple(tx->f_worker, a, b), mask, 0, rows, capacity);
}
static uint64_t count(struct mica_KernelTransaction *tx, uint64_t id) {
    mica_type_Value rows[4096];
    struct mica_KernelScanResult result = scan(tx, id, 0, 0, 0, rows, 4096);
    assert(result.f_status == OK && !result.f_more);
    return result.f_count;
}
static unsigned height(struct mica_KernelNode *node) {
    if (!node) return 0;
    unsigned left = height(node->f_left), right = height(node->f_right);
    assert(left <= right + 1 && right <= left + 1);
    assert(node->f_height == 1 + (left > right ? left : right));
    return (unsigned)node->f_height;
}
static void basics(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction old, tx;
    begin(&f, &old);
    begin(&f, &tx);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 2) == OK);
    assert(mica_kernel_declare(&tx, 1, 2, 3, SET, 0, 0) == SCHEMA);
    assert(mica_kernel_declare(&tx, 2, 1, 2, SET, 0, 0) == NAME);
    assert(write_pair(&tx, 1, 2, 9, true) == OK);
    assert(write_pair(&tx, 1, 1, 9, true) == OK);
    assert(write_pair(&tx, 1, 1, 9, true) == OK);
    assert(count(&tx, 1) == 2);
    assert(mica_kernel_write(&tx, 1, integer(9), true) == ARITY);
    mica_type_Value invalid_cells[] = {integer(3), mica_value_capability(1).f_value};
    struct mica_ValueResult invalid_row = mica_value_list(&f.worker, invalid_cells, 2);
    assert(invalid_row.f_ok && mica_kernel_write(&tx, 1, invalid_row.f_value, true) == NONPERSISTENT);
    mica_type_Value rows[8];
    assert(scan(&tx, 1, 0, 0, 4, rows, 8).f_status == SCHEMA);
    assert(scan(&tx, 1, 0, 0, 0, NULL, 8).f_status == SCHEMA);
    assert(scan(&old, 1, 0, 0, 0, rows, 8).f_status == UNKNOWN);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_version(&tx) == 1);
    unsigned deltas = 0;
    for (struct mica_KernelDelta *d = mica_kernel_deltas(&tx); d; d = d->f_next) {
        assert(d->f_relation == 1 && d->f_asserted);
        ++deltas;
    }
    assert(deltas == 2);
    assert(write_pair(&tx, 1, 3, 9, true) == CLOSED);
    assert(mica_kernel_commit(&tx) == CLOSED);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    assert(scan(&old, 1, 0, 0, 0, rows, 8).f_status == UNKNOWN);
    assert(mica_kernel_end(&old));
    begin(&f, &tx);
    assert(count(&tx, 1) == 2);
    assert(write_pair(&tx, 1, 3, 9, true) == OK);
    assert(mica_kernel_end(&tx)); // abort
    begin(&f, &tx);
    assert(count(&tx, 1) == 2);
    uint64_t version = mica_kernel_version(&tx);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_version(&tx) == version);
    assert(mica_kernel_end(&tx));
    declare(&f, 2, FUNCTIONAL);
    begin(&f, &tx);
    assert(write_pair(&tx, 2, 1, 10, true) == OK);
    assert(write_pair(&tx, 2, 1, 11, true) == KEY);
    assert(write_pair(&tx, 2, 1, 10, false) == OK);
    assert(write_pair(&tx, 2, 1, 11, true) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}
static void indexes_and_gc(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx, old;
    begin(&f, &tx);
    for (int64_t i = 0; i < 1024; ++i) assert(write_pair(&tx, 1, i, 1023-i, true) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    begin(&f, &old);
    assert(count(&old, 1) == 1024);
    mica_type_Value rows[4];
    struct mica_KernelScanResult result = scan(&old, 1, 0, 713, 2, rows, 4);
    assert(result.f_status == OK && result.f_count == 1 && !result.f_more);
    assert(cell(rows[0], 0) == 310 && result.f_visited < 32);
    result = scan(&old, 1, 713, 0, 1, rows, 4);
    assert(result.f_count == 1 && cell(rows[0], 1) == 310 && result.f_visited < 32);
    assert(scan(&old, 1, 0, 0, 0, rows, 4).f_more);
    mica_type_Value page[17], after = 0;
    uint64_t seen = 0;
    do {
        result = mica_kernel_scan(&old, 1, tuple(&f.worker, 0, 0), 0, after, page, 17);
        assert(result.f_status == OK);
        for (uint64_t i = 0; i < result.f_count; ++i) assert(cell(page[i], 0) == (int64_t)seen++);
        if (result.f_count) after = page[result.f_count-1];
    } while (result.f_more);
    assert(seen == 1024);
    struct mica_KernelRelation *r = mica_kernel_view(&old, 1);
    struct mica_KernelNode *untouched = mica_kernel_index(r, 0)->f_left;
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 2048, 2048, true) == OK);
    assert(mica_kernel_index(mica_kernel_view(&tx, 1), 0)->f_left == untouched);
    for (int64_t i = 0; i < 1024; i += 2) assert(write_pair(&tx, 1, i, 1023-i, false) == OK);
    height(mica_kernel_index(mica_kernel_view(&tx, 1), 0));
    assert(mica_kernel_commit(&tx) == OK);
    assert(count(&old, 1) == 1024);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    assert(count(&old, 1) == 1024);
    assert(mica_kernel_end(&old));
    begin(&f, &tx);
    assert(count(&tx, 1) == 513);
    assert(mica_kernel_end(&tx));
    // Heap-valued tuples survive nursery reuse and retain immutable string views.
    begin(&f, &tx);
    struct mica_ValueResult text = mica_value_string(&f.worker, (const uint8_t *)"h\xc3\xa9llo", 6);
    assert(text.f_ok);
    mica_type_Value cells[] = {integer(-1), text.f_value};
    struct mica_ValueResult row = mica_value_list(&f.worker, cells, 2);
    assert(row.f_ok && mica_kernel_write(&tx, 1, row.f_value, true) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
    memset(f.worker.f_nursery, 0xa5, f.worker.f_capacity);
    begin(&f, &tx);
    result = scan(&tx, 1, -1, 0, 1, rows, 4);
    assert(result.f_count == 1);
    const struct mica_HeapString *text_view = mica_value_as_string(mica_kernel_cell(rows[0], 1)).f_header;
    assert(text_view->f_length == 6 && !memcmp(text_view->f_data, "h\xc3\xa9llo", 6));
    assert(mica_kernel_end(&tx));
    destroy(&f);
}
static void conflicts(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, FUNCTIONAL);
    declare(&f, 2, SET);
    struct mica_KernelTransaction a, b;
    begin(&f, &a);
    assert(write_pair(&a, 1, 1, 1, true) == OK);
    assert(write_pair(&a, 2, 1, 1, true) == OK);
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    begin(&f, &a);
    assert(write_pair(&a, 1, 1, 1, false) == OK && write_pair(&a, 1, 1, 2, true) == OK);
    begin(&f, &b);
    assert(write_pair(&b, 1, 1, 1, false) == OK && write_pair(&b, 1, 1, 3, true) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT);
    assert(mica_kernel_work(&a)->f_conflict_relation == 1);
    assert(mica_kernel_width(mica_kernel_work(&a)->f_conflict_tuple) == 2);
    assert(mica_kernel_end(&a));
    begin(&f, &a);
    assert(write_pair(&a, 2, 1, 1, true) == OK);
    begin(&f, &b);
    assert(write_pair(&b, 2, 1, 1, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT && mica_kernel_end(&a));
    // Retracting an absent row must not erase a concurrent insertion.
    begin(&f, &a);
    assert(write_pair(&a, 2, 9, 9, false) == OK);
    begin(&f, &b);
    assert(write_pair(&b, 2, 9, 9, true) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == OK && mica_kernel_deltas(&a) == NULL && mica_kernel_end(&a));
    begin(&f, &a);
    assert(count(&a, 2) == 1);
    assert(mica_kernel_declare(&a, 3, 3, 2, SET, 0, 0) == OK);
    assert(write_pair(&a, 2, 8, 8, true) == OK);
    begin(&f, &b);
    assert(mica_kernel_declare(&b, 4, 3, 2, SET, 0, 0) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT && mica_kernel_end(&a));
    begin(&f, &a);
    assert(count(&a, 2) == 1 && mica_kernel_view(&a, 3) == NULL);
    assert(mica_kernel_end(&a));
    destroy(&f);
}

static void prepared_publication_rebases(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction retained, a, b;
    begin(&f, &retained);
    begin(&f, &a);
    assert(write_pair(&a, 1, 10, 20, true) == OK);
    // Preparation must work while the publication mutex is already held.
    mica_foreign_memory_lock(f.kernel.f_mutex);
    assert(mica_kernel_prepare(&a) == OK);
    mica_foreign_memory_unlock(f.kernel.f_mutex);
    struct mica_KernelWork *work = mica_kernel_work(&a);
    work->f_candidate = (void *)mica_memory_share(&f.worker, (void *)work->f_candidate);
    assert(work->f_candidate);
    uint64_t collections = f.heap.f_collections;
    begin(&f, &b);
    assert(write_pair(&b, 1, 30, 40, true) == OK);
    assert(mica_kernel_commit(&b) == OK);
    assert(mica_kernel_end(&b));
    assert(!mica_kernel_try_publish(&a));
    assert(!mica_kernel_work(&a)->f_done);
    assert(mica_kernel_commit(&a) == OK);
    assert(f.heap.f_collections == collections);
    unsigned deltas = 0;
    for (struct mica_KernelDelta *d = mica_kernel_deltas(&a); d; d = d->f_next) {
        assert(d->f_asserted && cell(d->f_row, 0) == 10);
        ++deltas;
    }
    assert(deltas == 1 && mica_kernel_end(&a));
    assert(mica_memory_safepoint(&f.worker, true));
    assert(count(&retained, 1) == 0);
    begin(&f, &a);
    assert(count(&a, 1) == 2);
    assert(mica_kernel_end(&a) && mica_kernel_end(&retained));
    destroy(&f);
}

static void small_commit_copies_changed_paths(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, FUNCTIONAL);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned i = 0; i < 1024; ++i) assert(write_pair(&tx, 1, i, i, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t live = f.heap.f_retained;
    uint64_t copied = f.heap.f_copied, collections = f.heap.f_collections;
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 511, 511, false) == OK);
    assert(write_pair(&tx, 1, 511, 9000, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(f.heap.f_collections == collections);
    assert(f.heap.f_copied > copied && f.heap.f_copied - copied < live / 8);
    begin(&f, &tx);
    assert(count(&tx, 1) == 1024);
    mica_type_Value rows[1];
    assert(scan(&tx, 1, 511, 0, 1, rows, 1).f_count == 1 && cell(rows[0], 1) == 9000);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

struct parallel {
    struct fixture *fixture;
    unsigned id, rounds;
    bool contended;
    uint64_t conflicts;
};
static void *parallel_worker(void *opaque) {
    struct parallel *p = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, &p->fixture->heap, 32768));
    assert(mica_memory_enter(&worker));
    for (unsigned i = 0; i < p->rounds; ++i) {
        for (;;) {
            struct mica_KernelTransaction tx = {0};
            assert(mica_kernel_begin(&p->fixture->kernel, &worker, &tx) == OK);
            if (p->contended) {
                mica_type_Value rows[1];
                struct mica_KernelScanResult read = scan(&tx, 1, 0, 0, 1, rows, 1);
                assert(read.f_count == 1 && !read.f_more);
                int64_t previous = cell(rows[0], 1);
                assert(mica_kernel_write(&tx, 1, rows[0], false) == OK);
                assert(write_pair(&tx, 1, 0, previous + 1, true) == OK);
            } else {
                assert(write_pair(&tx, 2, p->id, i, true) == OK);
            }
            uint64_t status = mica_kernel_commit(&tx);
            assert(mica_kernel_end(&tx));
            assert(status == OK || status == CONFLICT);
            if (status == OK) break;
            ++p->conflicts;
        }
    }
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void concurrency(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, FUNCTIONAL);
    declare(&f, 2, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 0, 0, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    enum { THREADS = 4, ROUNDS = 30 };
    for (unsigned mode = 0; mode < 2; ++mode) {
        pthread_t threads[THREADS];
        struct parallel jobs[THREADS];
        assert(mica_memory_leave(&f.worker));
        for (unsigned i = 0; i < THREADS; ++i) {
            jobs[i] = (struct parallel){.fixture = &f, .id = i, .rounds = ROUNDS, .contended = mode == 1};
            assert(!pthread_create(&threads[i], NULL, parallel_worker, &jobs[i]));
        }
        for (unsigned i = 0; i < THREADS; ++i) assert(!pthread_join(threads[i], NULL));
        assert(mica_memory_enter(&f.worker));
    }
    begin(&f, &tx);
    assert(count(&tx, 2) == THREADS * ROUNDS);
    mica_type_Value rows[1];
    assert(scan(&tx, 1, 0, 0, 1, rows, 1).f_count == 1);
    assert(cell(rows[0], 1) == THREADS * ROUNDS);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

static void allocation_failures(void) {
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    unsigned failures = 0;
    bool succeeded = false;
    for (long budget = 0; budget < 64; ++budget) {
        struct fixture f;
        init(&f);
        declare(&f, 1, SET);
        struct mica_KernelTransaction tx;
        begin(&f, &tx);
        assert(mica_kernel_declare(&tx, 2, 2, 2, SET, 0, 2) == OK);
        for (unsigned i = 0; i < 300; ++i) assert(write_pair(&tx, 1, i, i, true) == OK);
        atomic_store(&allocation_budget, budget);
        uint64_t status = mica_kernel_commit(&tx);
        atomic_store(&allocation_budget, -1);
        assert(status == OK || status == OOM);
        if (status == OOM) {
            ++failures;
            assert(mica_kernel_deltas(&tx) == NULL);
            assert(mica_memory_safepoint(&f.worker, true));
            assert(count(&tx, 1) == 300);
            struct mica_KernelTransaction observer;
            begin(&f, &observer);
            assert(count(&observer, 1) == 0 && mica_kernel_view(&observer, 2) == NULL);
            assert(mica_kernel_end(&observer));
            // The same transaction remains usable after allocation recovers.
            assert(mica_kernel_commit(&tx) == OK);
        } else succeeded = true;
        assert(mica_kernel_end(&tx));
        begin(&f, &tx);
        assert(count(&tx, 1) == 300 && mica_kernel_view(&tx, 2) != NULL);
        assert(mica_kernel_end(&tx));
        destroy(&f);
        if (succeeded) break;
    }
    assert(failures > 0 && succeeded);
#endif
}

static int trace_row_order(const void *left, const void *right) {
    return (int)mica_kernel_compare(*(const mica_type_Value *)left, *(const mica_type_Value *)right, 0, false);
}
struct trace_delta { uint64_t relation; bool asserted; mica_type_Value row; };
static int trace_delta_order(const void *left, const void *right) {
    const struct trace_delta *a = left, *b = right;
    if (a->relation != b->relation) return a->relation < b->relation ? -1 : 1;
    if (a->asserted != b->asserted) return a->asserted ? 1 : -1;
    return trace_row_order(&a->row, &b->row);
}
// Query rows are lists in canonical tuple order, including zero-column rows.
static mica_type_Value query_columns(struct mica_MemoryWorker *worker, const int64_t *columns, uint64_t count) {
    mica_type_Value cells[64];
    assert(count <= 64);
    for (uint64_t i = 0; i < count; ++i) cells[i] = integer(columns[i]);
    struct mica_ValueResult result = mica_value_list(worker, cells, count);
    assert(result.f_ok);
    return result.f_value;
}
static struct mica_KernelQuery *query_scan(struct fixture *f, uint64_t relation) {
    struct mica_KernelQuery *query = mica_kernel_query_scan(&f->worker, relation, tuple(&f->worker, 0, 0), 0);
    assert(query);
    return query;
}
static uint64_t query_count(struct mica_KernelTransaction *tx, struct mica_KernelQuery *query, uint64_t arity) {
    struct mica_KernelQueryResult result = mica_kernel_query_execute(tx, query);
    assert(result.f_status == OK && result.f_arity == arity);
    uint64_t length = mica_kernel_width(result.f_rows);
    for (uint64_t i = 0; i < length; ++i) {
        mica_type_Value row = mica_kernel_cell(result.f_rows, i);
        assert(mica_kernel_width(row) == arity);
        if (i) assert(mica_kernel_compare(mica_kernel_cell(result.f_rows, i - 1), row, 0, false) < 0);
    }
    return length;
}
static struct mica_KernelQuery *trace_query(struct fixture *f, uint64_t relation, int64_t a, int64_t b, uint64_t options) {
    struct mica_MemoryWorker *worker = &f->worker;
    struct mica_KernelQuery *left = mica_kernel_query_scan(worker, relation, tuple(worker, a, b), options & 8 ? 1 : 0);
    struct mica_KernelQuery *right = query_scan(f, 3 - relation);
    mica_type_Value lp = query_columns(worker, (int64_t[]){0}, 1);
    mica_type_Value rp = query_columns(worker, (int64_t[]){(int64_t)((options >> 4) & 1)}, 1);
    switch (options & 7) {
    case 0: return mica_kernel_query_project(worker, left, query_columns(worker, (int64_t[]){1, 0, 1}, 3));
    case 1: return mica_kernel_query_join(worker, left, right, lp, rp);
    case 2: return mica_kernel_query_semi(worker, left, right, lp, rp);
    case 3: return mica_kernel_query_anti(worker, left, right, lp, rp);
    case 4: return mica_kernel_query_union(worker, left, right);
    case 5: return mica_kernel_query_difference(worker, left, right);
    case 6: return mica_kernel_query_project(worker, mica_kernel_query_join(worker, left, right, lp, rp),
        query_columns(worker, (int64_t[]){1, 3}, 2));
    default: {
        uint32_t heading[] = {1, 2};
        mica_type_Value cells[] = {integer(a), integer(b)};
        struct mica_ValueTuple row = {.f_data = cells, .f_arity = 2};
        struct mica_ValueResult input = mica_value_relation(worker, heading, 2, &row, 1);
        assert(input.f_ok);
        return mica_kernel_query_union(worker, left, mica_kernel_query_input(worker, input.f_value));
    }
    }
}
static int trace(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, FUNCTIONAL);
    declare(&f, 2, SET);
    struct mica_KernelTransaction transactions[2] = {{0}};
    char op;
    unsigned slot;
    unsigned long long id, mask;
    long long a, b;
    while (scanf(" %c %u %llu %lld %lld %llu", &op, &slot, &id, &a, &b, &mask) == 6) {
        assert(slot < 2);
        struct mica_KernelTransaction *tx = &transactions[slot];
        uint64_t status = OK;
        if (op == 'b') status = mica_kernel_begin(&f.kernel, &f.worker, tx);
        else if (op == 'e') assert(mica_kernel_end(tx));
        else if (op == 'g') assert(mica_memory_safepoint(&f.worker, true));
        else if (op == 'a' || op == 'r') status = write_pair(tx, id, a, b, op == 'a');
        else if (op == 'q') {
            mica_type_Value rows[4096];
            struct mica_KernelScanResult result = scan(tx, id, a, b, mask, rows, 4096);
            assert(!result.f_more);
            qsort(rows, result.f_count, sizeof(*rows), trace_row_order);
            printf("q %llu %llu", (unsigned long long)result.f_status, (unsigned long long)result.f_count);
            for (uint64_t i = 0; i < result.f_count; ++i) printf(" %lld:%lld", (long long)cell(rows[i], 0), (long long)cell(rows[i], 1));
            putchar('\n');
            continue;
        } else if (op == 'p') {
            struct mica_KernelQueryResult result = mica_kernel_query_execute(tx, trace_query(&f, id, a, b, mask));
            assert(result.f_status == OK);
            uint64_t length = mica_kernel_width(result.f_rows);
            printf("p %llu %llu", (unsigned long long)result.f_status, (unsigned long long)length);
            for (uint64_t i = 0; i < length; ++i) {
                mica_type_Value row = mica_kernel_cell(result.f_rows, i);
                putchar(' ');
                for (uint64_t j = 0; j < result.f_arity; ++j) printf("%s%lld", j ? ":" : "", (long long)cell(row, j));
            }
            putchar('\n');
            continue;
        } else if (op == 'c') {
            status = mica_kernel_commit(tx);
            struct trace_delta changes[4096];
            size_t length = 0;
            for (struct mica_KernelDelta *d = mica_kernel_deltas(tx); d; d = d->f_next) {
                assert(length < 4096);
                changes[length++] = (struct trace_delta){d->f_relation, d->f_asserted, d->f_row};
            }
            qsort(changes, length, sizeof(*changes), trace_delta_order);
            printf("c %llu %zu", (unsigned long long)status, length);
            for (size_t i = 0; i < length; ++i) printf(" %llu:%u:%lld:%lld", (unsigned long long)changes[i].relation,
                changes[i].asserted ? 1u : 0u, (long long)cell(changes[i].row, 0), (long long)cell(changes[i].row, 1));
            putchar('\n');
            continue;
        } else abort();
        printf("%c %llu\n", op, (unsigned long long)status);
    }
    assert(feof(stdin));
    destroy(&f);
    return 0;
}
static uint64_t nanos(void) {
    struct timespec t;
    assert(!clock_gettime(CLOCK_MONOTONIC, &t));
    return (uint64_t)t.tv_sec * UINT64_C(1000000000) + (uint64_t)t.tv_nsec;
}
static int query_benchmark(const char *name, unsigned rounds) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET); declare(&f, 2, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned i = 0; i < 1024; ++i) {
        assert(write_pair(&tx, 1, i, i % 64, true) == OK);
        assert(write_pair(&tx, 2, i, i % 32, true) == OK);
    }
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    unsigned operation = !strcmp(name, "query-project") ? 0 : !strcmp(name, "query-join") ? 1 : !strcmp(name, "query-semi") ? 2 : 6;
    struct mica_KernelQuery *plan = trace_query(&f, 1, 0, 0, operation);
    struct mica_MemoryRoot root = {.f_pointer = (uint8_t *)plan};
    assert(mica_memory_root_push(&f.worker, &root));
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t expected = operation == 6 ? 64 : 1024;
    uint64_t sum = 0, start = nanos();
    for (unsigned i = 0; i < rounds; ++i) {
        struct mica_KernelQueryResult result = mica_kernel_query_execute(&tx, (struct mica_KernelQuery *)root.f_pointer);
        assert(result.f_status == OK && mica_kernel_width(result.f_rows) == expected);
        sum += mica_kernel_width(result.f_rows);
        assert(mica_memory_safepoint(&f.worker, false));
    }
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t elapsed = nanos() - start;
    assert(mica_memory_root_pop(&f.worker, &root));
    assert(mica_kernel_end(&tx));
    printf("%llu %llu 0\n", (unsigned long long)elapsed, (unsigned long long)sum);
    destroy(&f);
    return 0;
}

static int benchmark(const char *name, unsigned rounds, unsigned threads) {
#if defined(__SANITIZE_ADDRESS__) || defined(__SANITIZE_THREAD__)
    fputs("benchmark requires an unsanitized executable\n", stderr);
    return 2;
#endif
    if (!strncmp(name, "query-", 6)) return query_benchmark(name, rounds);
    struct fixture f;
    init(&f);
    declare(&f, 1, FUNCTIONAL);
    declare(&f, 2, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    unsigned rows = !strcmp(name, "read") ? 4096 : !strcmp(name, "update") ? 1024 : 1;
    for (unsigned i = 0; i < rows; ++i) assert(write_pair(&tx, 1, i, 0, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    uint64_t sum = 0, conflicts = 0;
    uint64_t start = nanos();
    if (!strcmp(name, "read")) {
        begin(&f, &tx);
        for (unsigned i = 0; i < rounds; ++i) {
            mica_type_Value found[1];
            struct mica_KernelScanResult result = scan(&tx, 1, i % rows, 0, 1, found, 1);
            assert(result.f_count == 1);
            sum += (uint64_t)cell(found[0], 0);
            if (!(i % 256)) assert(mica_memory_safepoint(&f.worker, false));
        }
        assert(mica_kernel_end(&tx));
    } else if (!strcmp(name, "update")) {
        for (unsigned i = 0; i < rounds; ++i) {
            begin(&f, &tx);
            assert(write_pair(&tx, 1, 0, i, false) == OK);
            assert(write_pair(&tx, 1, 0, i+1, true) == OK);
            assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        }
        sum = rounds;
    } else {
        assert(threads > 0 && threads <= 16);
        pthread_t handles[16];
        struct parallel jobs[16];
        assert(mica_memory_leave(&f.worker));
        for (unsigned i = 0; i < threads; ++i) {
            jobs[i] = (struct parallel){.fixture = &f, .id = i, .rounds = rounds, .contended = !strcmp(name, "contended")};
            assert(!pthread_create(&handles[i], NULL, parallel_worker, &jobs[i]));
        }
        for (unsigned i = 0; i < threads; ++i) { assert(!pthread_join(handles[i], NULL)); conflicts += jobs[i].conflicts; }
        assert(mica_memory_enter(&f.worker));
        sum = (uint64_t)threads * rounds;
    }
    // Include reclamation of the final transaction's temporary storage.
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t elapsed = nanos() - start;
    // Verify the resulting store outside the measured interval.
    if (strcmp(name, "read")) {
        begin(&f, &tx);
        if (!strcmp(name, "disjoint")) {
            mica_type_Value page[64], after = 0;
            struct mica_KernelScanResult result;
            sum = 0;
            do {
                result = mica_kernel_scan(&tx, 2, tuple(&f.worker, 0, 0), 0, after, page, 64);
                assert(result.f_status == OK);
                for (uint64_t i = 0; i < result.f_count; ++i, ++sum) {
                    assert(cell(page[i], 0) == (int64_t)(sum / rounds));
                    assert(cell(page[i], 1) == (int64_t)(sum % rounds));
                }
                if (result.f_count) after = page[result.f_count-1];
            } while (result.f_more);
            assert(sum == (uint64_t)threads * rounds);
        } else {
            mica_type_Value found[1];
            struct mica_KernelScanResult result = scan(&tx, 1, 0, 0, 1, found, 1);
            assert(result.f_status == OK && result.f_count == 1 && !result.f_more);
            sum = (uint64_t)cell(found[0], 1);
            assert(sum == (uint64_t)threads * rounds);
        }
        assert(mica_kernel_end(&tx));
    }
    printf("%llu %llu %llu\n", (unsigned long long)elapsed, (unsigned long long)sum, (unsigned long long)conflicts);
    destroy(&f);
    return 0;
}
static void query_indexes(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelNode *tree = NULL;
    for (unsigned i = 0; i < 257; ++i) {
        mica_type_Value row = tuple(&f.worker, (i * 97) % 257, i % 7);
        tree = mica_kernel_query_insert(&f.worker, tree, row);
        assert(tree && height(tree) < 12);
        struct mica_KernelNode *same = mica_kernel_query_insert(&f.worker, tree, row);
        assert(same == tree);
    }
    destroy(&f);
}
static void queries(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    declare(&f, 2, SET);
    struct mica_KernelTransaction old, tx;
    begin(&f, &old);
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 1, 10, true) == OK);
    assert(write_pair(&tx, 1, 2, 20, true) == OK);
    assert(write_pair(&tx, 1, 3, 20, true) == OK);
    assert(write_pair(&tx, 2, 10, 100, true) == OK);
    assert(write_pair(&tx, 2, 20, 200, true) == OK);
    assert(write_pair(&tx, 2, 20, 201, true) == OK);
    mica_type_Value first = query_columns(&f.worker, (int64_t[]){0}, 1);
    mica_type_Value second = query_columns(&f.worker, (int64_t[]){1}, 1);
    mica_type_Value empty = query_columns(&f.worker, NULL, 0);
    struct mica_KernelQuery *left = query_scan(&f, 1), *right = query_scan(&f, 2);
    struct mica_KernelQuery *join = mica_kernel_query_join(&f.worker, left, right, second, first);
    assert(query_count(&tx, join, 4) == 5);
    assert(query_count(&old, join, 4) == 0);
    assert(query_count(&tx, mica_kernel_query_semi(&f.worker, left, right, second, first), 2) == 3);
    assert(query_count(&tx, mica_kernel_query_anti(&f.worker, left, right, second, first), 2) == 0);
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, left, right, empty, empty), 4) == 9);
    assert(query_count(&tx, mica_kernel_query_project(&f.worker, left, empty), 0) == 1);
    assert(query_count(&old, mica_kernel_query_project(&f.worker, left, empty), 0) == 0);
    assert(query_count(&tx, mica_kernel_query_project(&f.worker, left, second), 1) == 2);
    assert(query_count(&tx, mica_kernel_query_union(&f.worker, left, left), 2) == 3);
    assert(query_count(&tx, mica_kernel_query_difference(&f.worker, left, left), 2) == 0);
    assert(query_count(&tx, mica_kernel_query_union(&f.worker, left, right), 2) == 6);
    mica_type_Value repeat = query_columns(&f.worker, (int64_t[]){3, 0, 3}, 3);
    struct mica_KernelQuery *project = mica_kernel_query_project(&f.worker, join, repeat);
    struct mica_KernelQueryResult result = mica_kernel_query_execute(&tx, project);
    assert(result.f_status == OK && result.f_arity == 3 && mica_kernel_width(result.f_rows) == 5);
    mica_type_Value row = mica_kernel_cell(result.f_rows, 0);
    assert(cell(row, 0) == 100 && cell(row, 1) == 1 && cell(row, 2) == 100);
    // Query plans and materialized results remain valid independently of a transaction.
    struct mica_MemoryRoot plan_root = {.f_pointer = (uint8_t *)project}, result_root = {0};
    assert(mica_memory_root_push(&f.worker, &plan_root));
    assert(mica_memory_root_push(&f.worker, &result_root));
    mica_value_root_set(&result_root, result.f_rows);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_query_execute(&tx, project).f_status == CLOSED);
    assert(mica_memory_safepoint(&f.worker, true));
    project = (struct mica_KernelQuery *)plan_root.f_pointer;
    assert(query_count(&old, project, 3) == 0);
    assert(mica_kernel_width(mica_value_root_get(&result_root)) == 5);
    assert(mica_memory_root_pop(&f.worker, &result_root));
    assert(mica_memory_root_pop(&f.worker, &plan_root));
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_end(&old));
    begin(&f, &tx);
    left = query_scan(&f, 1); right = query_scan(&f, 2);
    first = query_columns(&f.worker, (int64_t[]){0}, 1);
    second = query_columns(&f.worker, (int64_t[]){1}, 1);
    assert(write_pair(&tx, 1, 2, 20, false) == OK);
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, left, right, second, first), 4) == 3);
    // Invalid plans fail even when inputs are empty; no unchecked column reads.
    mica_type_Value invalid = query_columns(&f.worker, (int64_t[]){-1}, 1);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_project(&f.worker, left, invalid)).f_status == SCHEMA);
    invalid = query_columns(&f.worker, (int64_t[]){2}, 1);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_project(&f.worker, left, invalid)).f_status == SCHEMA);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_project(&f.worker, left, integer(0))).f_status == SCHEMA);
    assert(mica_kernel_query_execute(&tx, NULL).f_status == SCHEMA);
    assert(mica_kernel_query_execute(&tx, query_scan(&f, 999)).f_status == UNKNOWN);
    struct mica_KernelQuery *one_column = mica_kernel_query_project(&f.worker, left, first);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_union(&f.worker, left, one_column)).f_status == ARITY);
    empty = query_columns(&f.worker, NULL, 0);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_join(&f.worker, left, right, first, empty)).f_status == SCHEMA);
    // Input headings are canonicalized by the relation value constructor.
    uint32_t heading[] = {2, 1};
    mica_type_Value cells[] = {integer(9), integer(7)};
    struct mica_ValueTuple input_row = {.f_data = cells, .f_arity = 2};
    struct mica_ValueResult input = mica_value_relation(&f.worker, heading, 2, &input_row, 1);
    assert(input.f_ok);
    result = mica_kernel_query_execute(&tx, mica_kernel_query_input(&f.worker, input.f_value));
    assert(result.f_status == OK && result.f_arity == 2);
    row = mica_kernel_cell(result.f_rows, 0);
    assert(cell(row, 0) == 7 && cell(row, 1) == 9);
    // Exact value equality does not merge numerically equal integer/float keys.
    cells[0] = mica_value_float(1.0f).f_value; cells[1] = integer(1);
    input = mica_value_relation(&f.worker, heading, 2, &input_row, 1);
    assert(input.f_ok);
    assert(query_count(&tx, mica_kernel_query_semi(&f.worker, left,
        mica_kernel_query_input(&f.worker, input.f_value), first, second), 2) == 0);
    // More than one scan page, including a secondary-index scan.
    for (int64_t i = 4; i < 150; ++i) assert(write_pair(&tx, 1, i, 20, true) == OK);
    struct mica_KernelQuery *bound = mica_kernel_query_scan(&f.worker, 1, tuple(&f.worker, 0, 20), 2);
    assert(query_count(&tx, bound, 2) == 147);
    // Repeated key positions and key order are positional, not a column mask.
    mica_type_Value key_left = query_columns(&f.worker, (int64_t[]){1, 0, 1}, 3);
    mica_type_Value key_right = query_columns(&f.worker, (int64_t[]){1, 0, 1}, 3);
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, left, left, key_left, key_right), 4) == 148);
    key_right = query_columns(&f.worker, (int64_t[]){0, 1, 0}, 3);
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, left, left, key_left, key_right), 4) == 1);
    // Bounded plan depth also rejects cycles supplied by a malformed native caller.
    one_column->f_left = one_column;
    assert(mica_kernel_query_execute(&tx, one_column).f_status == SCHEMA);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    // Exhaust allocation partway through a large result. Failure publishes no
    // partial result, leaves staged writes intact, and does not poison a retry.
    struct mica_KernelQuery *product = mica_kernel_query_join(&f.worker, left, left, empty, empty);
    struct mica_MemoryRoot bound_root = {.f_pointer = (uint8_t *)bound};
    assert(mica_memory_root_push(&f.worker, &bound_root));
    atomic_store(&allocation_budget, 0);
    result = mica_kernel_query_execute(&tx, product);
    atomic_store(&allocation_budget, -1);
    assert(result.f_status == OOM && result.f_rows == 0);
    assert(mica_memory_safepoint(&f.worker, true));
    assert(query_count(&tx, (struct mica_KernelQuery *)bound_root.f_pointer, 2) == 147);
    assert(mica_memory_root_pop(&f.worker, &bound_root));
#endif
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "trace")) return trace();
    if (argc == 5 && !strcmp(argv[1], "bench")) return benchmark(argv[2], (unsigned)strtoul(argv[3], NULL, 10), (unsigned)strtoul(argv[4], NULL, 10));
    assert(argc == 1);
    query_indexes();
    queries();
    basics();
    indexes_and_gc();
    conflicts();
    prepared_publication_rebases();
    small_commit_copies_changed_paths();
    concurrency();
    allocation_failures();
    return 0;
}
