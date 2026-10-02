// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef MICA_MEMORY_TEST_ALLOCATOR
static _Atomic long allocation_budget = -1;
static _Atomic long cancellation_allocation = -1;
static _Atomic(struct mica_KernelTransaction *) allocation_transaction;
uint8_t *mica_foreign_memory_platform_allocate(uint64_t size) {
    long until_cancel = atomic_load(&cancellation_allocation);
    if (until_cancel > 0 && atomic_fetch_sub(&cancellation_allocation, 1) == 1)
        assert(mica_kernel_cancel(atomic_load(&allocation_transaction)));
    long remaining = atomic_load(&allocation_budget);
    if (remaining == 0) return NULL;
    if (remaining > 0) atomic_fetch_sub(&allocation_budget, 1);
    return malloc(size);
}
void mica_foreign_memory_platform_release(uint8_t *pointer) { free(pointer); }
#endif

// These values are the public status and conflict-policy codes.
enum { OK, UNKNOWN, ARITY, NONPERSISTENT, KEY, CONFLICT, NAME, SCHEMA, OOM, CLOSED, LIMIT, CANCELLED, PERMISSION };
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
    assert(mica_kernel_init(&f->kernel, &f->worker, integer(7)));
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
    assert(mica_kernel_begin(&f->kernel, &f->worker, tx, integer(7)) == OK);
}
static void declare(struct fixture *f, uint64_t id, uint64_t policy) {
    struct mica_KernelTransaction tx = {0};
    begin(f, &tx);
    assert(mica_kernel_declare(&tx, id, id, 2, policy, policy == FUNCTIONAL ? 1 : 0, 2) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_end(&tx));
}
static struct mica_KernelBudget operation_budget(struct mica_KernelTransaction *tx) {
    struct mica_KernelWork *work = mica_kernel_work(tx);
    return (struct mica_KernelBudget){.f_steps = work->f_rule_limit, .f_rows = work->f_rows_limit,
        .f_rounds = work->f_rounds_limit, .f_bytes = work->f_bytes_limit, .f_signal = tx->f_signal};
}
static uint64_t refresh(struct mica_KernelTransaction *tx) {
    struct mica_KernelBudget budget = operation_budget(tx);
    return mica_kernel_rule_refresh(tx, &budget);
}
static uint64_t depend(struct mica_KernelTransaction *tx, uint64_t id) {
    struct mica_KernelBudget budget = operation_budget(tx);
    return mica_kernel_rule_depend(tx, id, &budget);
}
static struct mica_KernelQueryResult derived_rows(struct mica_MemoryWorker *worker, struct mica_KernelRuleEvaluation *evaluation, uint64_t id, bool derived, uint64_t steps) {
    struct mica_KernelBudget budget = {.f_steps = steps, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX};
    return mica_kernel_rule_rows(worker, evaluation, id, derived, &budget);
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
    assert(mica_kernel_query_execute(&old, NULL).f_status == CLOSED);
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
    assert(mica_kernel_query_execute(&old, NULL).f_status == CLOSED);
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
    struct mica_KernelBudget budget = {.f_steps = mica_kernel_work(&a)->f_rule_limit, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_signal = a.f_signal};
    mica_foreign_memory_lock(f.kernel.f_mutex);
    assert(mica_kernel_prepare(&a, &budget) == OK);
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
            assert(mica_kernel_begin(&p->fixture->kernel, &worker, &tx, integer(7)) == OK);
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

// Build complete, range-restricted self-recursive definitions for catalogue
// fixtures. Constructor failure is part of the same allocation-failure test.
static struct mica_KernelRuleDef *self_rule(struct mica_MemoryWorker *worker, uint64_t head, unsigned arity) {
    struct mica_KernelRuleTerm *terms = NULL;
    for (unsigned i = arity; i > 0; --i) {
        terms = mica_kernel_rule_variable(worker, i - 1, terms);
        if (!terms) return NULL;
    }
    struct mica_KernelRuleAtom *atom = mica_kernel_rule_atom(worker, head, false, terms, NULL);
    if (!atom) return NULL;
    struct mica_ValueResult source = mica_value_string(worker, (const uint8_t *)"", 0);
    if (!source.f_ok) return NULL;
    return mica_kernel_rule_definition(worker, head, terms, atom, NULL, integer(7), source.f_value);
}
static uint64_t stage_self_rule(struct mica_KernelTransaction *tx, uint64_t id, uint64_t head, unsigned arity, bool active, bool replace) {
    struct mica_KernelRuleDef *definition = NULL;
    if (tx->f_worker && mica_kernel_work(tx) && !mica_kernel_work(tx)->f_done) {
        definition = self_rule(tx->f_worker, head, arity);
        if (!definition) return OOM;
    }
    return replace ? mica_kernel_rule_update(tx, id, definition, active) : mica_kernel_rule_add(tx, id, definition, active);
}
static uint64_t add_rule(struct mica_KernelTransaction *tx, uint64_t id, uint64_t head, unsigned arity, bool active) {
    return stage_self_rule(tx, id, head, arity, active, false);
}
static uint64_t update_rule(struct mica_KernelTransaction *tx, uint64_t id, uint64_t head, unsigned arity, bool active) {
    return stage_self_rule(tx, id, head, arity, active, true);
}

static void expect_rule(struct mica_KernelTransaction *tx, uint64_t id, uint64_t head, bool active) {
    struct mica_KernelRuleResult result = mica_kernel_rule(tx, id);
    assert(result.f_status == OK && result.f_rule);
    assert(result.f_rule->f_id == id && result.f_rule->f_head == head);
    assert(result.f_rule->f_arity == 2 && result.f_rule->f_active == active);
}
static uint64_t rule_count(struct mica_KernelTransaction *tx) {
    struct mica_KernelRuleResult result = mica_kernel_rules(tx);
    assert(result.f_status == OK);
    uint64_t n = 0;
    for (struct mica_KernelRule *rule = result.f_rule; rule; rule = rule->f_next) {
        for (struct mica_KernelRule *next = rule->f_next; next; next = next->f_next)
            assert(rule->f_id != next->f_id);
        assert(++n < 4096);
    }
    return n;
}
static struct mica_KernelRuleTerm *rule_pair(struct mica_MemoryWorker *worker) {
    struct mica_KernelRuleTerm *last = mica_kernel_rule_variable(worker, 1, NULL);
    assert(last);
    struct mica_KernelRuleTerm *first = mica_kernel_rule_variable(worker, 0, last);
    assert(first);
    return first;
}
static struct mica_KernelRuleAtom *rule_atom_pair(struct mica_MemoryWorker *worker, uint64_t relation, bool negative, struct mica_KernelRuleAtom *next) {
    struct mica_KernelRuleAtom *atom = mica_kernel_rule_atom(worker, relation, negative, rule_pair(worker), next);
    assert(atom);
    return atom;
}
static struct mica_KernelRuleDef *fixture_rule(struct mica_MemoryWorker *worker, uint64_t head, struct mica_KernelRuleTerm *terms, struct mica_KernelRuleAtom *atoms, struct mica_KernelRuleGuard *guards) {
    struct mica_ValueResult source = mica_value_string(worker, (const uint8_t *)"fixture", 7);
    assert(source.f_ok);
    struct mica_KernelRuleDef *definition = mica_kernel_rule_definition(worker, head, terms, atoms, guards, integer(7), source.f_value);
    assert(definition);
    return definition;
}
static uint64_t install_edge(struct mica_KernelTransaction *tx, uint64_t id, uint64_t head, uint64_t source, bool negative) {
    struct mica_MemoryWorker *worker = tx->f_worker;
    struct mica_KernelRuleAtom *atoms = rule_atom_pair(worker, source, negative, NULL);
    if (negative) atoms = rule_atom_pair(worker, 2, false, atoms);
    struct mica_KernelRuleDef *definition = fixture_rule(worker, head, rule_pair(worker), atoms, NULL);
    return mica_kernel_rule_add(tx, id, definition, true);
}
static void rule_definition_validation(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    declare(&f, 2, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    for (unsigned invalid = 0; invalid < 9; ++invalid) {
        struct mica_KernelRuleTerm *head = rule_pair(&f.worker);
        struct mica_KernelRuleTerm *body = rule_pair(&f.worker);
        struct mica_KernelRuleGuard *guard = NULL;
        bool negative = false;
        uint64_t relation = 2;
        if (invalid == 0) head = mica_kernel_rule_variable(&f.worker, 99, head->f_next);
        if (invalid == 1) head = mica_kernel_rule_hole(&f.worker, head->f_next);
        if (invalid == 2) { negative = true; body = mica_kernel_rule_hole(&f.worker, body->f_next); }
        if (invalid == 3) { negative = true; }
        if (invalid == 4) body = body->f_next;
        if (invalid == 5) relation = 999;
        if (invalid >= 6) {
            struct mica_KernelRuleTerm *operand = mica_kernel_rule_variable(&f.worker, invalid == 6 ? 99 : 0, NULL);
            if (invalid == 7) operand = mica_kernel_rule_hole(&f.worker, NULL);
            guard = mica_kernel_rule_guard(&f.worker, invalid == 8 ? 6 : 0, operand, mica_kernel_rule_constant(&f.worker, integer(0), NULL), NULL);
            assert(guard);
        }
        struct mica_KernelRuleAtom *atom = mica_kernel_rule_atom(&f.worker, relation, negative, body, NULL);
        assert(atom);
        struct mica_KernelRuleDef *definition = fixture_rule(&f.worker, 1, head, atom, guard);
        assert(mica_kernel_rule_add(&tx, 1, definition, true) == (invalid == 5 ? UNKNOWN : SCHEMA));
        assert(f.worker.f_roots == parent);
        assert(mica_kernel_rule(&tx, 1).f_status == UNKNOWN);
    }
    // Negation may precede the positive binding; repeated variables and holes
    // are valid in positive atoms. Guards filter bindings and never bind them.
    struct mica_KernelRuleTerm *head = mica_kernel_rule_variable(&f.worker, 0, mica_kernel_rule_variable(&f.worker, 0, NULL));
    struct mica_KernelRuleTerm *positive = mica_kernel_rule_variable(&f.worker, 0, mica_kernel_rule_hole(&f.worker, NULL));
    struct mica_KernelRuleAtom *atoms = mica_kernel_rule_atom(&f.worker, 2, false, positive, NULL);
    atoms = mica_kernel_rule_atom(&f.worker, 2, true, head, atoms);
    struct mica_KernelRuleGuard *guards = NULL;
    for (unsigned op = 0; op < 6; ++op) {
        guards = mica_kernel_rule_guard(&f.worker, op, mica_kernel_rule_variable(&f.worker, 0, NULL), mica_kernel_rule_constant(&f.worker, integer(5), NULL), guards);
        assert(guards);
    }
    struct mica_KernelRuleDef *definition = fixture_rule(&f.worker, 1, head, atoms, guards);
    assert(mica_kernel_rule_add(&tx, 1, definition, true) == OK);
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_KernelRuleResult found = mica_kernel_rule(&tx, 1);
    assert(found.f_status == OK && found.f_rule->f_definition->f_guards);
    assert(mica_value_string_length(found.f_rule->f_definition->f_source).f_number == 7);
    assert(mica_value_as_int(found.f_rule->f_definition->f_tenant).f_number == 7);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    found = mica_kernel_rule(&tx, 1);
    assert(found.f_status == OK && found.f_rule->f_definition->f_atoms->f_negative);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}
static struct mica_KernelRuleVertex *planned_vertex(struct mica_KernelRulePlan *plan, uint64_t id) {
    struct mica_KernelRuleVertex *vertex = mica_kernel_rule_vertex_index_lookup(plan->f_index, id);
    assert(vertex && vertex->f_component);
    return vertex;
}
static void rule_dependency_planning(void) {
    struct fixture f;
    init(&f);
    for (unsigned id = 1; id <= 5; ++id) declare(&f, id, SET);
    struct mica_KernelTransaction tx, a, b;
    begin(&f, &tx);
    assert(install_edge(&tx, 1, 1, 2, false) == OK);
    assert(install_edge(&tx, 2, 2, 1, false) == OK);
    assert(install_edge(&tx, 3, 3, 1, false) == OK);
    assert(install_edge(&tx, 4, 3, 4, true) == OK);
    struct mica_KernelRulePlanResult planned = mica_kernel_rule_plan(&tx);
    assert(planned.f_status == OK && planned.f_plan);
    uint64_t component = planned_vertex(planned.f_plan, 1)->f_component;
    assert(component == planned_vertex(planned.f_plan, 2)->f_component);
    assert(component < planned_vertex(planned.f_plan, 3)->f_component);
    assert(planned_vertex(planned.f_plan, 4)->f_component < planned_vertex(planned.f_plan, 3)->f_component);
    assert(install_edge(&tx, 5, 4, 3, true) == SCHEMA);
    assert(mica_kernel_rule(&tx, 5).f_status == UNKNOWN);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    // Each concurrent edit is stratified alone; their union is not.
    begin(&f, &a);
    begin(&f, &b);
    assert(install_edge(&a, 6, 4, 5, true) == OK);
    assert(install_edge(&b, 7, 5, 3, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT);
    assert(mica_kernel_work(&a)->f_candidate == NULL);
    assert(mica_kernel_end(&a));
    // A new rule feeding an indirectly queried head invalidates the consumer.
    begin(&f, &a);
    begin(&f, &b);
    assert(depend(&a, 3) == OK);
    assert(install_edge(&b, 8, 4, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT);
    assert(mica_kernel_work(&a)->f_conflict_relation == 4);
    assert(mica_kernel_end(&a));
    // Unrelated heads do not force task retry.
    begin(&f, &a);
    begin(&f, &b);
    assert(depend(&a, 1) == OK);
    assert(install_edge(&b, 9, 5, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    // Inactive rules participate in structural validation, but do not change
    // the active head generation. Activation must check stratification again.
    begin(&f, &a);
    begin(&f, &b);
    assert(depend(&a, 1) == OK);
    struct mica_KernelRuleAtom *inactive_atoms = rule_atom_pair(&f.worker, 1, true, NULL);
    inactive_atoms = rule_atom_pair(&f.worker, 2, false, inactive_atoms);
    struct mica_KernelRuleDef *inactive = fixture_rule(&f.worker, 1, rule_pair(&f.worker), inactive_atoms, NULL);
    assert(mica_kernel_rule_add(&b, 90, inactive, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    begin(&f, &tx);
    struct mica_KernelRuleResult inactive_read = mica_kernel_rule(&tx, 90);
    assert(inactive_read.f_status == OK);
    assert(mica_kernel_rule_update(&tx, 90, inactive_read.f_rule->f_definition, true) == SCHEMA);
    assert(!mica_kernel_rule(&tx, 90).f_rule->f_active);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(mica_kernel_set_limits(&tx, 0, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(install_edge(&tx, 10, 5, 2, false) == LIMIT);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_rule(&tx, 10).f_status == UNKNOWN);
    assert(install_edge(&tx, 10, 5, 2, false) == OK);
    assert(mica_kernel_set_limits(&tx, 0, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(mica_kernel_work(&tx)->f_status == LIMIT && mica_kernel_work(&tx)->f_candidate == NULL);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}

static void rule_work_limits(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx, observer;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 4; ++id) assert(mica_kernel_rule(&tx, id).f_status == UNKNOWN);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    // Point reads need validation even when there are no writes or head reads.
    bool completed = false;
    for (unsigned limit = 0; limit < 64; ++limit) {
        assert(mica_kernel_set_limits(&tx, limit, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        uint64_t status = mica_kernel_commit(&tx);
        assert(f.worker.f_roots == parent && !f.worker.f_control);
        if (status == OK) {
            assert(limit >= 4); // All four catalogue observations were charged.
            completed = true;
            break;
        }
        assert(status == LIMIT && mica_kernel_work(&tx)->f_status == LIMIT);
        assert(!mica_kernel_work(&tx)->f_done && !mica_kernel_work(&tx)->f_candidate);
    }
    assert(completed && mica_kernel_end(&tx));

    begin(&f, &tx);
    for (unsigned id = 100; id < 164; ++id) assert(mica_kernel_rule(&tx, id).f_status == UNKNOWN);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(add_rule(&tx, 1, 1, 2, false) == OK);
    parent = f.worker.f_roots;
    // Catalogue validation and allocation share one allowance. This cannot
    // cover preparation, 65 observations, and definition validation.
    assert(mica_kernel_set_limits(&tx, 71, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(f.worker.f_roots == parent);
    assert(mica_kernel_work(&tx)->f_status == LIMIT);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_deltas(&tx));
    assert(!mica_kernel_work(&tx)->f_done && mica_kernel_view(&tx, 1));
    expect_rule(&tx, 1, 1, false);
    begin(&f, &observer);
    assert(!mica_kernel_view(&observer, 1));
    assert(mica_kernel_rule(&observer, 1).f_status == UNKNOWN);
    assert(mica_kernel_end(&observer));
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}

static void rule_catalogue(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction closed = {0}, tx, old, observer;
    assert(mica_kernel_rule(&closed, 1).f_status == CLOSED);
    assert(mica_kernel_rules(&closed).f_status == CLOSED);
    assert(add_rule(&closed, 1, 1, 2, true) == CLOSED);
    assert(update_rule(&closed, 1, 1, 2, false) == CLOSED);
    assert(mica_kernel_rule_remove(&closed, 1) == CLOSED);
    begin(&f, &old);
    begin(&f, &tx);
    assert(add_rule(&tx, 10, 1, 2, true) == UNKNOWN);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 2, 2, 2, SET, 0, 0) == OK);
    assert(add_rule(&tx, 10, 1, 1, true) == SCHEMA);
    assert(add_rule(&tx, 10, 1, 2, true) == OK);
    assert(add_rule(&tx, 10, 1, 2, true) == SCHEMA);
    expect_rule(&tx, 10, 1, true);
    assert(update_rule(&tx, 10, 2, 2, false) == OK);
    expect_rule(&tx, 10, 2, false);
    assert(update_rule(&tx, 10, 999, 2, true) == UNKNOWN);
    assert(update_rule(&tx, 10, 1, 3, true) == SCHEMA);
    expect_rule(&tx, 10, 2, false);
    assert(mica_kernel_rule_remove(&tx, 10) == OK);
    assert(mica_kernel_rule(&tx, 10).f_status == UNKNOWN && rule_count(&tx) == 0);
    assert(mica_kernel_rule_remove(&tx, 10) == UNKNOWN);
    assert(update_rule(&tx, 10, 1, 2, true) == UNKNOWN);
    assert(add_rule(&tx, 10, 1, 2, true) == OK);
    assert(add_rule(&tx, 20, 2, 2, false) == OK);
    assert(write_pair(&tx, 1, 4, 5, true) == OK);
    assert(mica_memory_safepoint(&f.worker, true));
    expect_rule(&tx, 10, 1, true);
    assert(rule_count(&tx) == 2 && rule_count(&old) == 0);
    assert(mica_kernel_commit(&tx) == OK);
    assert(add_rule(&tx, 30, 1, 2, true) == CLOSED);
    assert(mica_kernel_rule(&tx, 10).f_status == CLOSED);
    assert(mica_kernel_end(&tx));
    assert(rule_count(&old) == 0 && mica_kernel_view(&old, 1) == NULL);
    assert(mica_kernel_end(&old));
    begin(&f, &observer);
    expect_rule(&observer, 10, 1, true);
    expect_rule(&observer, 20, 2, false);
    assert(count(&observer, 1) == 1);
    begin(&f, &tx);
    assert(update_rule(&tx, 10, 2, 2, false) == OK);
    assert(mica_kernel_rule_remove(&tx, 20) == OK);
    assert(write_pair(&tx, 1, 4, 5, false) == OK);
    assert(mica_kernel_end(&tx)); // Rollback discards catalogue and tuple changes.
    begin(&f, &tx);
    expect_rule(&tx, 10, 1, true);
    assert(rule_count(&tx) == 2 && count(&tx, 1) == 1);
    assert(mica_kernel_rule_remove(&tx, 10) == OK);
    assert(mica_kernel_rule_remove(&tx, 20) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    expect_rule(&observer, 20, 2, false); // Retained snapshot survives removal and GC.
    assert(mica_kernel_end(&observer));
    begin(&f, &tx);
    assert(rule_count(&tx) == 0);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}

static void rule_conflicts(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction a, b, observer;
    // Disjoint additions rebase without discarding either catalogue change.
    begin(&f, &b); begin(&f, &a);
    assert(add_rule(&a, 10, 1, 2, true) == OK);
    assert(add_rule(&b, 20, 1, 2, true) == OK);
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    begin(&f, &b); begin(&f, &a);
    assert(update_rule(&a, 10, 1, 2, false) == OK);
    assert(update_rule(&b, 10, 1, 2, false) == OK);
    assert(write_pair(&b, 1, 99, 99, true) == OK);
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    assert(mica_kernel_commit(&b) == CONFLICT);
    assert(mica_kernel_work(&b)->f_status == CONFLICT && mica_kernel_work(&b)->f_conflict_rule == 10);
    assert(mica_kernel_work(&b)->f_candidate == NULL && mica_kernel_deltas(&b) == NULL);
    assert(mica_kernel_end(&b));
    begin(&f, &observer);
    assert(count(&observer, 1) == 0 && rule_count(&observer) == 2);
    assert(mica_kernel_end(&observer));

    // Exact reads, missing-id reads, and whole-catalogue reads all validate,
    // including read-only commits that otherwise take the unchanged fast path.
    for (unsigned kind = 0; kind < 3; ++kind) {
        begin(&f, &a); begin(&f, &b);
        uint64_t id = 30 + kind;
        if (kind == 0) expect_rule(&a, 10, 1, false);
        else if (kind == 1) assert(mica_kernel_rule(&a, id).f_status == UNKNOWN);
        else assert(rule_count(&a) == 3);
        if (kind == 0) assert(update_rule(&b, 10, 1, 2, true) == OK);
        else assert(add_rule(&b, id, 1, 2, true) == OK);
        assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
        assert(mica_kernel_commit(&a) == CONFLICT && mica_kernel_end(&a));
    }
    // An unchanged rule's revision survives prefix copying for another id.
    begin(&f, &a); begin(&f, &b);
    expect_rule(&a, 10, 1, true);
    assert(update_rule(&b, 20, 1, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    // Delete and recreate the same definition must not hide an intervening change.
    begin(&f, &a); begin(&f, &b);
    expect_rule(&a, 10, 1, true);
    assert(mica_kernel_rule_remove(&b, 10) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    begin(&f, &b);
    assert(add_rule(&b, 10, 1, 2, true) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT && mica_kernel_end(&a));
    // Simultaneous definitions of a previously missing id conflict.
    begin(&f, &a); begin(&f, &b);
    assert(add_rule(&a, 100, 1, 2, true) == OK);
    assert(add_rule(&b, 100, 1, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT && mica_kernel_end(&a));
    destroy(&f);
}

static void rule_allocation_failures(void) {
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    // Exhaust storage during staging rather than commit. The failed edit must
    // preserve the preceding definition, and a later collection must trace it.
    struct fixture staging;
    init(&staging);
    declare(&staging, 1, SET);
    struct mica_KernelTransaction draft;
    begin(&staging, &draft);
    assert(add_rule(&draft, 1, 1, 2, true) == OK);
    bool active = true, exhausted = false;
    atomic_store(&allocation_budget, 0);
    for (unsigned i = 0; i < 4096; ++i) {
        uint64_t status = update_rule(&draft, 1, 1, 2, !active);
        assert(status == OK || status == OOM);
        if (status == OOM) { exhausted = true; break; }
        active = !active;
    }
    atomic_store(&allocation_budget, -1);
    assert(exhausted);
    expect_rule(&draft, 1, 1, active);
    assert(rule_count(&draft) == 1);
    assert(mica_memory_safepoint(&staging.worker, true));
    expect_rule(&draft, 1, 1, active);
    assert(mica_kernel_commit(&draft) == OK && mica_kernel_end(&draft));
    destroy(&staging);
    // Fail edits at both ends and the middle of an existing catalogue. Neither
    // an incomplete persistent path nor its observation may replace the draft.
    for (unsigned key = 0; key < 3; ++key) {
        struct fixture f;
        init(&f);
        declare(&f, 1, SET);
        struct mica_KernelTransaction tx, retained;
        begin(&f, &tx);
        for (unsigned i = 0; i < 63; ++i) assert(add_rule(&tx, i, 1, 2, true) == OK);
        assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        begin(&f, &retained);
        begin(&f, &tx);
        uint64_t id = (uint64_t[]){0, 31, 62}[key];
        bool previous = true, failed = false;
        struct mica_MemoryRoot *parent = f.worker.f_roots;
        atomic_store(&allocation_budget, 0);
        for (unsigned i = 0; i < 4096; ++i) {
            uint64_t status = update_rule(&tx, id, 1, 2, !previous);
            assert(f.worker.f_roots == parent);
            assert(status == OK || status == OOM);
            if (status == OOM) { failed = true; break; }
            previous = !previous;
        }
        atomic_store(&allocation_budget, -1);
        assert(failed);
        assert(mica_memory_safepoint(&f.worker, true));
        for (unsigned i = 0; i < 63; ++i) {
            expect_rule(&tx, i, 1, i == id ? previous : true);
            expect_rule(&retained, i, 1, true);
        }
        assert(rule_count(&tx) == 63 && rule_count(&retained) == 63);
        assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        expect_rule(&retained, id, 1, true);
        assert(mica_kernel_end(&retained));
        destroy(&f);
    }
    unsigned failures = 0;
    bool succeeded = false;
    for (long budget = 0; budget < 64 && !succeeded; ++budget) {
        struct fixture f;
        init(&f);
        declare(&f, 1, SET);
        struct mica_KernelTransaction tx, observer;
        begin(&f, &tx);
        for (unsigned i = 0; i < 48; ++i) assert(add_rule(&tx, i, 1, 2, true) == OK);
        assert(write_pair(&tx, 1, 1, 2, true) == OK);
        atomic_store(&allocation_budget, budget);
        uint64_t status = mica_kernel_commit(&tx);
        atomic_store(&allocation_budget, -1);
        assert(status == OK || status == OOM);
        if (status == OOM) {
            ++failures;
            assert(mica_kernel_work(&tx)->f_status == OOM);
            assert(mica_kernel_work(&tx)->f_candidate == NULL && mica_kernel_deltas(&tx) == NULL);
            assert(mica_memory_safepoint(&f.worker, true));
            assert(rule_count(&tx) == 48);
            begin(&f, &observer);
            assert(rule_count(&observer) == 0 && count(&observer, 1) == 0);
            assert(mica_kernel_end(&observer));
            assert(mica_kernel_commit(&tx) == OK);
        } else succeeded = true;
        assert(mica_kernel_end(&tx));
        begin(&f, &tx);
        assert(rule_count(&tx) == 48 && count(&tx, 1) == 1);
        assert(mica_kernel_end(&tx));
        destroy(&f);
    }
    assert(failures > 0 && succeeded);
#endif
}

static void rule_prepared_publication(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction a, b;
    begin(&f, &a);
    assert(add_rule(&a, 1, 1, 2, true) == OK);
    struct mica_KernelBudget budget = {.f_steps = mica_kernel_work(&a)->f_rule_limit, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_signal = a.f_signal};
    mica_foreign_memory_lock(f.kernel.f_mutex);
    assert(mica_kernel_prepare(&a, &budget) == OK); // No publication lock needed to prepare rules.
    mica_foreign_memory_unlock(f.kernel.f_mutex);
    struct mica_KernelWork *work = mica_kernel_work(&a);
    work->f_candidate = (void *)mica_memory_share(&f.worker, (void *)work->f_candidate);
    assert(work->f_candidate);
    begin(&f, &b);
    assert(add_rule(&b, 2, 1, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(!mica_kernel_try_publish(&a));
    assert(mica_memory_safepoint(&f.worker, true));
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    begin(&f, &a);
    expect_rule(&a, 1, 1, true);
    expect_rule(&a, 2, 1, false);
    uint64_t version = mica_kernel_version(&a);
    assert(mica_kernel_commit(&a) == OK && mica_kernel_version(&a) == version);
    assert(mica_kernel_end(&a));
    destroy(&f);
}

struct rule_parallel { struct fixture *fixture; unsigned id; uint64_t conflicts; };
static void *rule_parallel_worker(void *opaque) {
    struct rule_parallel *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, &job->fixture->heap, 4096));
    assert(mica_memory_enter(&worker));
    for (unsigned round = 0; round < 20; ++round) {
        bool committed = false;
        for (unsigned attempt = 0; attempt < 1000 && !committed; ++attempt) {
            struct mica_KernelTransaction tx = {0};
            assert(mica_kernel_begin(&job->fixture->kernel, &worker, &tx, integer(7)) == OK);
            // Reading and toggling one shared definition forces task retries.
            struct mica_KernelRuleResult current = mica_kernel_rule(&tx, 1);
            assert(current.f_status == OK);
            bool active = current.f_rule->f_active;
            assert(update_rule(&tx, 1, 1, 2, !active) == OK);
            uint64_t id = 100 + job->id * 20 + round;
            assert(add_rule(&tx, id, 1, 2, true) == OK);
            assert(write_pair(&tx, 1, job->id, round, true) == OK);
            uint64_t status = mica_kernel_commit(&tx);
            assert(status == OK || status == CONFLICT);
            committed = status == OK;
            job->conflicts += status == CONFLICT;
            assert(mica_kernel_end(&tx));
            if (round % 7 == 0) assert(mica_memory_safepoint(&worker, true));
        }
        assert(committed);
    }
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static uint64_t rule_concurrency(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    assert(add_rule(&tx, 1, 1, 2, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    enum { THREADS = 4 };
    pthread_t threads[THREADS];
    struct rule_parallel jobs[THREADS];
    assert(mica_memory_leave(&f.worker));
    for (unsigned i = 0; i < THREADS; ++i) {
        jobs[i] = (struct rule_parallel){.fixture = &f, .id = i};
        assert(!pthread_create(&threads[i], NULL, rule_parallel_worker, &jobs[i]));
    }
    for (unsigned i = 0; i < THREADS; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_memory_enter(&f.worker));
    begin(&f, &tx);
    expect_rule(&tx, 1, 1, false); // An even number of committed toggles.
    assert(rule_count(&tx) == 1 + THREADS * 20);
    assert(count(&tx, 1) == THREADS * 20);
    assert(mica_kernel_end(&tx));
    destroy(&f);
    uint64_t conflicts = 0;
    for (unsigned i = 0; i < THREADS; ++i) conflicts += jobs[i].conflicts;
    return conflicts;
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
    struct mica_MemoryRoot *roots = tx->f_worker->f_roots;
    struct mica_KernelQueryResult result = mica_kernel_query_execute(tx, query);
    assert(tx->f_worker->f_roots == roots);
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
static void oracle_install(struct mica_KernelTransaction *tx);
static int trace(bool rules) {
    struct fixture f;
    init(&f);
    if (rules) {
        struct mica_KernelTransaction setup;
        begin(&f, &setup);
        oracle_install(&setup);
        assert(mica_kernel_commit(&setup) == OK && mica_kernel_end(&setup));
    } else {
        declare(&f, 1, FUNCTIONAL);
        declare(&f, 2, SET);
    }
    struct mica_KernelTransaction transactions[2] = {{0}};
    char op;
    unsigned slot;
    unsigned long long id, mask;
    long long a, b;
    while (scanf(" %c %u %llu %lld %lld %llu", &op, &slot, &id, &a, &b, &mask) == 6) {
        assert(slot < 2);
        struct mica_KernelTransaction *tx = &transactions[slot];
        uint64_t status = OK;
        if (op == 'b') status = mica_kernel_begin(&f.kernel, &f.worker, tx, integer(7));
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
            if (rules) {
                printf("c %llu\n", (unsigned long long)status);
                continue;
            }
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
    struct mica_KernelTransaction closed = {0};
    assert(mica_kernel_query_execute(&closed, NULL).f_status == CLOSED);
    begin(&f, &old);
    begin(&f, &tx);
    enum {
        Q_first, Q_second, Q_empty, Q_repeat, Q_invalid, Q_key_left, Q_key_right,
        Q_left, Q_right, Q_join, Q_project, Q_one_column, Q_bound, Q_product, QUERY_ROOT_COUNT
    };
    struct mica_MemoryRoot held[QUERY_ROOT_COUNT] = {{0}};
    for (unsigned i = 0; i < QUERY_ROOT_COUNT; ++i) assert(mica_memory_root_push(&f.worker, &held[i]));
#define QUERY_VALUE(slot) mica_value_root_get(&held[slot])
#define QUERY_PLAN(slot) ((struct mica_KernelQuery *)held[slot].f_pointer)

    assert(write_pair(&tx, 1, 1, 10, true) == OK);
    assert(write_pair(&tx, 1, 2, 20, true) == OK);
    assert(write_pair(&tx, 1, 3, 20, true) == OK);
    assert(write_pair(&tx, 2, 10, 100, true) == OK);
    assert(write_pair(&tx, 2, 20, 200, true) == OK);
    assert(write_pair(&tx, 2, 20, 201, true) == OK);
    mica_value_root_set(&held[Q_first], query_columns(&f.worker, (int64_t[]){0}, 1));
    mica_value_root_set(&held[Q_second], query_columns(&f.worker, (int64_t[]){1}, 1));
    mica_value_root_set(&held[Q_empty], query_columns(&f.worker, NULL, 0));
    held[Q_left].f_pointer = (uint8_t *)(query_scan(&f, 1));
    held[Q_right].f_pointer = (uint8_t *)(query_scan(&f, 2));
    held[Q_join].f_pointer = (uint8_t *)(mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right), QUERY_VALUE(Q_second), QUERY_VALUE(Q_first)));
    assert(query_count(&tx, QUERY_PLAN(Q_join), 4) == 5);
    assert(query_count(&old, QUERY_PLAN(Q_join), 4) == 0);
    assert(query_count(&tx, mica_kernel_query_semi(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right), QUERY_VALUE(Q_second), QUERY_VALUE(Q_first)), 2) == 3);
    assert(query_count(&tx, mica_kernel_query_anti(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right), QUERY_VALUE(Q_second), QUERY_VALUE(Q_first)), 2) == 0);
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right), QUERY_VALUE(Q_empty), QUERY_VALUE(Q_empty)), 4) == 9);
    assert(query_count(&tx, mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), QUERY_VALUE(Q_empty)), 0) == 1);
    assert(query_count(&old, mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), QUERY_VALUE(Q_empty)), 0) == 0);
    assert(query_count(&tx, mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), QUERY_VALUE(Q_second)), 1) == 2);
    assert(query_count(&tx, mica_kernel_query_union(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_left)), 2) == 3);
    assert(query_count(&tx, mica_kernel_query_difference(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_left)), 2) == 0);
    assert(query_count(&tx, mica_kernel_query_union(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right)), 2) == 6);
    mica_value_root_set(&held[Q_repeat], query_columns(&f.worker, (int64_t[]){3, 0, 3}, 3));
    held[Q_project].f_pointer = (uint8_t *)(mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_join), QUERY_VALUE(Q_repeat)));
    struct mica_KernelQueryResult result = mica_kernel_query_execute(&tx, QUERY_PLAN(Q_project));
    assert(result.f_status == OK && result.f_arity == 3 && mica_kernel_width(result.f_rows) == 5);
    mica_type_Value row = mica_kernel_cell(result.f_rows, 0);
    assert(cell(row, 0) == 100 && cell(row, 1) == 1 && cell(row, 2) == 100);
    // Query plans and materialized results remain valid independently of a transaction.
    struct mica_MemoryRoot plan_root = {.f_pointer = (uint8_t *)QUERY_PLAN(Q_project)}, result_root = {0};
    assert(mica_memory_root_push(&f.worker, &plan_root));
    assert(mica_memory_root_push(&f.worker, &result_root));
    mica_value_root_set(&result_root, result.f_rows);
    assert(mica_kernel_commit(&tx) == OK);
    assert(mica_kernel_query_execute(&tx, QUERY_PLAN(Q_project)).f_status == CLOSED);
    assert(mica_memory_safepoint(&f.worker, true));
    held[Q_project].f_pointer = (uint8_t *)((struct mica_KernelQuery *)plan_root.f_pointer);
    assert(query_count(&old, QUERY_PLAN(Q_project), 3) == 0);
    assert(mica_kernel_width(mica_value_root_get(&result_root)) == 5);
    assert(mica_memory_root_pop(&f.worker, &result_root));
    assert(mica_memory_root_pop(&f.worker, &plan_root));
    for (unsigned i = QUERY_ROOT_COUNT; i > 0; --i) assert(mica_memory_root_pop(&f.worker, &held[i-1]));
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_end(&old));
    assert(mica_kernel_query_execute(&old, NULL).f_status == CLOSED);
    begin(&f, &tx);
    for (unsigned i = 0; i < QUERY_ROOT_COUNT; ++i) assert(mica_memory_root_push(&f.worker, &held[i]));
    held[Q_left].f_pointer = (uint8_t *)(query_scan(&f, 1)); held[Q_right].f_pointer = (uint8_t *)(query_scan(&f, 2));
    mica_value_root_set(&held[Q_first], query_columns(&f.worker, (int64_t[]){0}, 1));
    mica_value_root_set(&held[Q_second], query_columns(&f.worker, (int64_t[]){1}, 1));
    assert(write_pair(&tx, 1, 2, 20, false) == OK);
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right), QUERY_VALUE(Q_second), QUERY_VALUE(Q_first)), 4) == 3);
    // Invalid plans fail even when inputs are empty; no unchecked column reads.
    mica_value_root_set(&held[Q_invalid], query_columns(&f.worker, (int64_t[]){-1}, 1));
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), QUERY_VALUE(Q_invalid))).f_status == SCHEMA);
    mica_value_root_set(&held[Q_invalid], query_columns(&f.worker, (int64_t[]){2}, 1));
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), QUERY_VALUE(Q_invalid))).f_status == SCHEMA);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), integer(0))).f_status == SCHEMA);
    assert(mica_kernel_query_execute(&tx, NULL).f_status == SCHEMA);
    assert(mica_kernel_query_execute(&tx, query_scan(&f, 999)).f_status == UNKNOWN);
    held[Q_one_column].f_pointer = (uint8_t *)(mica_kernel_query_project(&f.worker, QUERY_PLAN(Q_left), QUERY_VALUE(Q_first)));
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_union(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_one_column))).f_status == ARITY);
    mica_value_root_set(&held[Q_empty], query_columns(&f.worker, NULL, 0));
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_right), QUERY_VALUE(Q_first), QUERY_VALUE(Q_empty))).f_status == SCHEMA);
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
    assert(query_count(&tx, mica_kernel_query_semi(&f.worker, QUERY_PLAN(Q_left),
        mica_kernel_query_input(&f.worker, input.f_value), QUERY_VALUE(Q_first), QUERY_VALUE(Q_second)), 2) == 0);
    // More than one scan page, including a secondary-index scan.
    for (int64_t i = 4; i < 150; ++i) assert(write_pair(&tx, 1, i, 20, true) == OK);
    held[Q_bound].f_pointer = (uint8_t *)(mica_kernel_query_scan(&f.worker, 1, tuple(&f.worker, 0, 20), 2));
    assert(query_count(&tx, QUERY_PLAN(Q_bound), 2) == 147);
    // Scan bindings and join bindings must agree. A contradictory probe is
    // empty for join/semi, and retains the left row for anti.
    assert(query_count(&tx, mica_kernel_query_semi(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_bound), QUERY_VALUE(Q_second), QUERY_VALUE(Q_second)), 2) == 147);
    assert(query_count(&tx, mica_kernel_query_anti(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_bound), QUERY_VALUE(Q_second), QUERY_VALUE(Q_second)), 2) == 1);
    assert(mica_kernel_query_execute(&tx, mica_kernel_query_scan(&f.worker, 1, tuple(&f.worker, 0, 0), 4)).f_status == SCHEMA);
    // Repeated key positions and key order are positional, not a column mask.
    mica_value_root_set(&held[Q_key_left], query_columns(&f.worker, (int64_t[]){1, 0, 1}, 3));
    mica_value_root_set(&held[Q_key_right], query_columns(&f.worker, (int64_t[]){1, 0, 1}, 3));
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_left), QUERY_VALUE(Q_key_left), QUERY_VALUE(Q_key_right)), 4) == 148);
    mica_value_root_set(&held[Q_key_right], query_columns(&f.worker, (int64_t[]){0, 1, 0}, 3));
    assert(query_count(&tx, mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_left), QUERY_VALUE(Q_key_left), QUERY_VALUE(Q_key_right)), 4) == 1);
    // Bounded plan depth also rejects cycles supplied by a malformed native caller.
    QUERY_PLAN(Q_one_column)->f_left = QUERY_PLAN(Q_one_column);
    assert(mica_kernel_query_execute(&tx, QUERY_PLAN(Q_one_column)).f_status == SCHEMA);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    // Exhaust allocation partway through a large result. Failure publishes no
    // partial result, leaves staged writes intact, and does not poison a retry.
    held[Q_product].f_pointer = (uint8_t *)(mica_kernel_query_join(&f.worker, QUERY_PLAN(Q_left), QUERY_PLAN(Q_left), QUERY_VALUE(Q_empty), QUERY_VALUE(Q_empty)));
    struct mica_MemoryRoot bound_root = {.f_pointer = (uint8_t *)QUERY_PLAN(Q_bound)};
    assert(mica_memory_root_push(&f.worker, &bound_root));
    atomic_store(&allocation_budget, 0);
    result = mica_kernel_query_execute(&tx, QUERY_PLAN(Q_product));
    atomic_store(&allocation_budget, -1);
    assert(result.f_status == OOM && result.f_rows == 0);
    assert(mica_memory_safepoint(&f.worker, true));
    assert(query_count(&tx, (struct mica_KernelQuery *)bound_root.f_pointer, 2) == 147);
    assert(mica_memory_root_pop(&f.worker, &bound_root));
#endif
    for (unsigned i = QUERY_ROOT_COUNT; i > 0; --i) assert(mica_memory_root_pop(&f.worker, &held[i-1]));
    assert(mica_kernel_end(&tx));
    destroy(&f);
#undef QUERY_VALUE
#undef QUERY_PLAN
}

// A collector on another native thread must make progress during a large
// query. Tiny nurseries also force relocation while joins retain live rows.
struct collection_job {
    struct mica_MemoryHeap *heap;
    _Atomic bool ready;
};
static void *collect_repeatedly(void *opaque) {
    struct collection_job *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, job->heap, 4096));
    assert(mica_memory_enter(&worker));
    atomic_store(&job->ready, true);
    for (unsigned i = 0; i < 16; ++i) assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void query_collection(bool indexed) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (int64_t i = 0; i < 128; ++i) assert(write_pair(&tx, 1, i, i % 8, true) == OK);
    struct mica_KernelQuery *scan_plan = query_scan(&f, 1);
    mica_type_Value empty = query_columns(&f.worker, NULL, 0);
    struct mica_KernelQuery *right = scan_plan;
    if (!indexed) {
        // Force the general join path through a materialized projection.
        mica_type_Value identity = query_columns(&f.worker, (int64_t[]){0, 1}, 2);
        right = mica_kernel_query_project(&f.worker, right, identity);
    }
    struct mica_MemoryRoot plan = {.f_pointer = (uint8_t *)mica_kernel_query_join(&f.worker, scan_plan, right, empty, empty)};
    assert(mica_memory_root_push(&f.worker, &plan));
    uint64_t epoch = f.heap.f_epoch;
    struct collection_job job = {.heap = &f.heap};
    pthread_t collector;
    assert(!pthread_create(&collector, NULL, collect_repeatedly, &job));
    while (!atomic_load(&job.ready)) sched_yield();
    // Observe the request under the heap mutex before entering the query.
    // The collector is waiting for this active worker to reach an internal poll.
    for (;;) {
        mica_foreign_memory_lock(f.heap.f_mutex);
        bool requested = f.heap.f_collecting;
        mica_foreign_memory_unlock(f.heap.f_mutex);
        if (requested) break;
        sched_yield();
    }
    assert(query_count(&tx, (struct mica_KernelQuery *)plan.f_pointer, 4) == 128 * 128);
    assert(f.heap.f_epoch > epoch);
    assert(f.worker.f_roots == &plan);
    assert(mica_memory_root_pop(&f.worker, &plan));
    assert(mica_kernel_end(&tx));
    assert(mica_memory_leave(&f.worker));
    assert(!pthread_join(collector, NULL));
    assert(mica_memory_enter(&f.worker));
    destroy(&f);
}

// Check balance and ordering after mixed insertions and removals. The height
// bound is also a regression gate against catalogue-size native stack growth.
static uint64_t rule_tree_check(struct mica_KernelRuleIndex *node, uint64_t low, uint64_t high) {
    if (!node) return 0;
    uint64_t id = node->f_entry->f_id;
    assert(low <= id && id < high);
    uint64_t left = rule_tree_check(node->f_left, low, id);
    uint64_t right = rule_tree_check(node->f_right, id + 1, high);
    assert(left <= right + 1 && right <= left + 1);
    assert(node->f_height == 1 + (left > right ? left : right));
    return node->f_height;
}
static void rule_index_edits(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    // Multiplication permutes the ids and exercises both double rotations.
    for (unsigned i = 0; i < 1024; ++i) {
        assert(add_rule(&tx, (i * 307) % 1024, 1, 2, true) == OK);
        assert(rule_tree_check(mica_kernel_work(&tx)->f_rule_index, 0, UINT64_MAX) <= 15);
    }
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    for (unsigned i = 0; i < 1024; ++i) {
        assert(mica_kernel_rule_remove(&tx, (i * 701) % 1024) == OK);
        rule_tree_check(mica_kernel_work(&tx)->f_rule_index, 0, UINT64_MAX);
    }
    assert(rule_count(&tx) == 0);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}

static void rule_collection(bool preparing) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned i = 0; i < 1024; ++i) assert(add_rule(&tx, i, 1, 2, true) == OK);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    uint64_t epoch = f.heap.f_epoch;
    struct collection_job job = {.heap = &f.heap};
    pthread_t collector;
    assert(!pthread_create(&collector, NULL, collect_repeatedly, &job));
    while (!atomic_load(&job.ready)) sched_yield();
    for (;;) {
        mica_foreign_memory_lock(f.heap.f_mutex);
        bool requested = f.heap.f_collecting;
        mica_foreign_memory_unlock(f.heap.f_mutex);
        if (requested) break;
        sched_yield();
    }
    struct mica_KernelBudget budget = {.f_steps = mica_kernel_work(&tx)->f_rule_limit, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_signal = tx.f_signal};
    if (preparing) assert(mica_kernel_prepare(&tx, &budget) == OK);
    else assert(rule_count(&tx) == 1024);
    assert(f.heap.f_epoch > epoch && f.worker.f_roots == parent);
    assert(mica_memory_leave(&f.worker));
    assert(!pthread_join(collector, NULL));
    assert(mica_memory_enter(&f.worker));
    assert(rule_count(&tx) == 1024);
    for (unsigned i = 0; i < 1024; ++i) expect_rule(&tx, i, 1, true);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}

// Build the graph directly to isolate SCC stack depth from catalogue building.
// Collection during DFS must retain both the active frames and Tarjan stack.
static void rule_deep_planning(void) {
    enum { VERTICES = 10000 };
    struct fixture f;
    init(&f);
    struct mica_KernelRuleVertex *vertices = NULL;
    for (unsigned i = 0; i < VERTICES; ++i) {
        struct mica_KernelRuleEdge *edge = NULL;
        if (vertices) {
            edge = mica_kernel_new_RuleEdge(&f.worker, NULL, vertices, i % 2 != 0);
            assert(edge);
        }
        vertices = mica_kernel_new_RuleVertex(&f.worker, vertices, i, 0, edge, NULL, 0, 0, false, 0);
        assert(vertices);
    }
    struct mica_KernelRulePlan *plan = mica_kernel_new_RulePlan(&f.worker, vertices, NULL, NULL, NULL, 0);
    assert(plan);
    struct mica_MemoryRoot root = {.f_pointer = (uint8_t *)plan};
    assert(mica_memory_root_push(&f.worker, &root));
    uint64_t epoch = f.heap.f_epoch;
    struct collection_job job = {.heap = &f.heap};
    pthread_t collector;
    assert(!pthread_create(&collector, NULL, collect_repeatedly, &job));
    while (!atomic_load(&job.ready)) sched_yield();
    for (;;) {
        mica_foreign_memory_lock(f.heap.f_mutex);
        bool requested = f.heap.f_collecting;
        mica_foreign_memory_unlock(f.heap.f_mutex);
        if (requested) break;
        sched_yield();
    }
    struct mica_KernelBudget budget = {.f_steps = VERTICES * 16, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX};
    assert(mica_kernel_rule_stratify(&f.worker, (struct mica_KernelRulePlan *)root.f_pointer, &budget) == OK);
    assert(f.heap.f_epoch > epoch && f.worker.f_roots == &root);
    plan = (struct mica_KernelRulePlan *)root.f_pointer;
    assert(plan->f_count == VERTICES);
    uint64_t id = 1;
    for (struct mica_KernelRuleComponent *component = plan->f_components; component; component = component->f_next) {
        assert(component->f_id == id++ && !component->f_recursive);
        assert(component->f_members && !component->f_members->f_next);
        assert(component->f_members->f_vertex->f_id == component->f_id - 1);
    }
    assert(id == VERTICES + 1);
    assert(mica_memory_root_pop(&f.worker, &root));
    assert(mica_memory_leave(&f.worker));
    assert(!pthread_join(collector, NULL));
    assert(mica_memory_enter(&f.worker));
    destroy(&f);
}

// A small reachability oracle is independent of the generated DFS algorithm.
static void rule_graph_oracle(void) {
    enum { N = 8 };
    struct fixture f;
    init(&f);
    uint64_t random = 19;
    for (unsigned sample = 0; sample < 64; ++sample) {
        bool edge[N][N] = {{false}}, negative[N][N] = {{false}}, reachable[N][N] = {{false}};
        struct mica_KernelRuleVertex *vertices[N];
        struct mica_KernelRuleVertex *head = NULL;
        for (unsigned i = 0; i < N; ++i) {
            vertices[i] = mica_kernel_new_RuleVertex(&f.worker, head, i, 0, NULL, NULL, 0, 0, false, 0);
            assert(vertices[i]);
            head = vertices[i];
        }
        for (unsigned i = 0; i < N; ++i) {
            reachable[i][i] = true;
            for (unsigned j = 0; j < N; ++j) {
                random ^= random << 13; random ^= random >> 7; random ^= random << 17;
                edge[i][j] = random % 7 == 0;
                negative[i][j] = edge[i][j] && sample % 3 && (random >> 8) % 3 == 0;
                if (!edge[i][j]) continue;
                reachable[i][j] = true;
                vertices[i]->f_edges = mica_kernel_new_RuleEdge(&f.worker, vertices[i]->f_edges, vertices[j], negative[i][j]);
                assert(vertices[i]->f_edges);
            }
        }
        for (unsigned k = 0; k < N; ++k)
            for (unsigned i = 0; i < N; ++i)
                for (unsigned j = 0; j < N; ++j)
                    reachable[i][j] |= reachable[i][k] && reachable[k][j];
        bool invalid = false;
        for (unsigned i = 0; i < N; ++i)
            for (unsigned j = 0; j < N; ++j)
                invalid |= negative[i][j] && reachable[j][i];
        struct mica_KernelRulePlan *plan = mica_kernel_new_RulePlan(&f.worker, head, NULL, NULL, NULL, 0);
        assert(plan);
        struct mica_MemoryRoot root = {.f_pointer = (uint8_t *)plan};
        assert(mica_memory_root_push(&f.worker, &root));
        struct mica_KernelBudget budget = {.f_steps = 10000, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX};
        uint64_t status = mica_kernel_rule_stratify(&f.worker, plan, &budget);
        assert(status == (invalid ? SCHEMA : OK));
        if (!invalid) {
            plan = (struct mica_KernelRulePlan *)root.f_pointer;
            for (struct mica_KernelRuleVertex *v = plan->f_vertices; v; v = v->f_next) vertices[v->f_id] = v;
            for (unsigned i = 0; i < N; ++i) {
                for (unsigned j = 0; j < N; ++j) {
                    bool same = vertices[i]->f_component == vertices[j]->f_component;
                    assert(same == (reachable[i][j] && reachable[j][i]));
                    if (edge[i][j] && !same) assert(vertices[j]->f_component < vertices[i]->f_component);
                }
            }
        }
        assert(f.worker.f_roots == &root);
        assert(mica_memory_root_pop(&f.worker, &root));
        assert(mica_memory_safepoint(&f.worker, true));
    }
    destroy(&f);
}

// Independent finite-domain oracle: enumerate substitutions, then repeatedly
// apply all rules in each stratum. It uses no native binding or SCC machinery.
enum { RULE_DOMAIN = 6, RULE_RELATIONS = 16, RULE_HOLE = -100 };
struct oracle_atom { unsigned relation; bool negative; int term[2]; };
struct oracle_rule {
    unsigned head, stratum;
    int term[2];
    unsigned count;
    struct oracle_atom atoms[2];
    int guard; // -1, or one of == != < <= > >= against integer 1.
};
static const struct oracle_rule oracle_rules[] = {
    {2, 0, {0, 1}, 1, {{1, false, {0, 1}}}, -1},
    {2, 0, {0, 2}, 2, {{2, false, {0, 1}}, {1, false, {1, 2}}}, -1},
    {2, 0, {0, 2}, 2, {{2, false, {0, 1}}, {2, false, {1, 2}}}, -1},
    {3, 0, {0, 1}, 1, {{2, false, {0, 1}}}, -1},
    {2, 0, {0, 1}, 1, {{3, false, {0, 1}}}, -1},
    {5, 1, {0, 1}, 2, {{4, true, {0, 1}}, {2, false, {0, 1}}}, -1},
    {6, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 2},
    {7, 0, {0, 0}, 1, {{1, false, {0, 0}}}, -1},
    {4, 0, {0, 1}, 1, {{7, false, {0, 1}}}, -1},
    {8, 0, {-3, 0}, 1, {{1, false, {0, -2}}}, -1},
    {9, 0, {0, 0}, 1, {{1, false, {0, RULE_HOLE}}}, -1},
    {2, 0, {0, 1}, 1, {{2, false, {0, 1}}}, -1},
    {2, 0, {0, 1}, 1, {{1, false, {0, 1}}}, -1},
    {10, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 0},
    {11, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 1},
    {12, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 2},
    {13, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 3},
    {14, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 4},
    {15, 1, {0, 1}, 1, {{5, false, {0, 1}}}, 5},
};
static unsigned oracle_term(int term, const unsigned *binding) {
    return term < 0 ? (unsigned)(-term - 1) : binding[term];
}
static uint64_t oracle_bit(unsigned x, unsigned y) {
    return UINT64_C(1) << (x * RULE_DOMAIN + y);
}
static bool oracle_matches(const struct oracle_atom *atom, const unsigned *binding, uint64_t rows) {
    for (unsigned x = 0; x < RULE_DOMAIN; ++x)
        for (unsigned y = 0; y < RULE_DOMAIN; ++y)
            if ((rows & oracle_bit(x, y)) &&
                (atom->term[0] == RULE_HOLE || oracle_term(atom->term[0], binding) == x) &&
                (atom->term[1] == RULE_HOLE || oracle_term(atom->term[1], binding) == y)) return true;
    return false;
}
static void oracle_evaluate(const uint64_t *stored, uint64_t *all, uint64_t *derived) {
    memcpy(all, stored, RULE_RELATIONS * sizeof(*all));
    memset(derived, 0, RULE_RELATIONS * sizeof(*derived));
    for (unsigned stratum = 0; stratum < 2; ++stratum) {
        bool changed;
        do {
            changed = false;
            for (unsigned r = 0; r < sizeof(oracle_rules) / sizeof(*oracle_rules); ++r) {
                const struct oracle_rule *rule = &oracle_rules[r];
                if (rule->stratum != stratum) continue;
                for (unsigned assignment = 0; assignment < RULE_DOMAIN * RULE_DOMAIN * RULE_DOMAIN; ++assignment) {
                    unsigned binding[] = {assignment % RULE_DOMAIN, assignment / RULE_DOMAIN % RULE_DOMAIN, assignment / (RULE_DOMAIN * RULE_DOMAIN)};
                    bool match = true;
                    for (unsigned a = 0; a < rule->count; ++a) {
                        const struct oracle_atom *atom = &rule->atoms[a];
                        match &= oracle_matches(atom, binding, all[atom->relation]) != atom->negative;
                    }
                    if (rule->guard >= 0) {
                        bool guards[] = {binding[0] == 1, binding[0] != 1, binding[0] < 1, binding[0] <= 1, binding[0] > 1, binding[0] >= 1};
                        match &= guards[rule->guard];
                    }
                    if (!match) continue;
                    uint64_t bit = oracle_bit(oracle_term(rule->term[0], binding), oracle_term(rule->term[1], binding));
                    derived[rule->head] |= bit;
                    changed |= !(all[rule->head] & bit);
                    all[rule->head] |= bit;
                }
            }
        } while (changed);
    }
}
static struct mica_KernelRuleTerm *oracle_native_term(struct mica_MemoryWorker *worker, int term, struct mica_KernelRuleTerm *next) {
    struct mica_KernelRuleTerm *result;
    if (term == RULE_HOLE) result = mica_kernel_rule_hole(worker, next);
    else if (term < 0) result = mica_kernel_rule_constant(worker, integer(-term - 1), next);
    else result = mica_kernel_rule_variable(worker, (uint64_t)term, next);
    assert(result);
    return result;
}
static struct mica_KernelRuleTerm *oracle_native_terms(struct mica_MemoryWorker *worker, const int *terms) {
    return oracle_native_term(worker, terms[0], oracle_native_term(worker, terms[1], NULL));
}
static void oracle_install(struct mica_KernelTransaction *tx) {
    for (unsigned id = 1; id < RULE_RELATIONS; ++id)
        assert(mica_kernel_declare(tx, id, id, 2, SET, 0, 0) == OK);
    for (unsigned r = 0; r < sizeof(oracle_rules) / sizeof(*oracle_rules); ++r) {
        const struct oracle_rule *rule = &oracle_rules[r];
        struct mica_KernelRuleAtom *atoms = NULL;
        for (unsigned a = rule->count; a > 0; --a) {
            const struct oracle_atom *atom = &rule->atoms[a - 1];
            atoms = mica_kernel_rule_atom(tx->f_worker, atom->relation, atom->negative, oracle_native_terms(tx->f_worker, atom->term), atoms);
            assert(atoms);
        }
        struct mica_KernelRuleGuard *guard = NULL;
        if (rule->guard >= 0) {
            guard = mica_kernel_rule_guard(tx->f_worker, (uint64_t)rule->guard,
                oracle_native_term(tx->f_worker, 0, NULL), oracle_native_term(tx->f_worker, -2, NULL), NULL);
            assert(guard);
        }
        struct mica_KernelRuleDef *definition = fixture_rule(tx->f_worker, rule->head, oracle_native_terms(tx->f_worker, rule->term), atoms, guard);
        assert(mica_kernel_rule_add(tx, r + 1, definition, true) == OK);
    }
}
static void oracle_check(struct mica_MemoryWorker *worker, struct mica_MemoryRoot *root, const uint64_t *stored) {
    uint64_t all[RULE_RELATIONS], derived[RULE_RELATIONS];
    oracle_evaluate(stored, all, derived);
    for (unsigned id = 1; id < RULE_RELATIONS; ++id) {
        struct mica_KernelRuleEvaluation *evaluation = (struct mica_KernelRuleEvaluation *)root->f_pointer;
        struct mica_KernelRuleRows *captured = mica_kernel_rule_rows_index_lookup(evaluation->f_rows, id);
        // Completed results must not retain per-round scratch indexes.
        assert(!evaluation->f_executables && captured && !captured->f_delta && !captured->f_pending && !captured->f_previous && !captured->f_added);
        for (unsigned mode = 0; mode < 2; ++mode) {
            struct mica_KernelQueryResult result = derived_rows(worker, (struct mica_KernelRuleEvaluation *)root->f_pointer, id, mode != 0, UINT64_MAX);
            assert(result.f_status == OK && result.f_arity == 2);
            uint64_t found = 0;
            for (uint64_t i = 0; i < mica_kernel_width(result.f_rows); ++i) {
                mica_type_Value row = mica_kernel_cell(result.f_rows, i);
                unsigned x = (unsigned)cell(row, 0), y = (unsigned)cell(row, 1);
                assert(x < RULE_DOMAIN && y < RULE_DOMAIN);
                uint64_t bit = oracle_bit(x, y);
                assert(!(found & bit));
                found |= bit;
            }
            assert(found == (mode ? derived[id] : all[id]));
        }
    }
}
static void rule_evaluation(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    oracle_install(&tx);
    // The same evaluator handles staged definitions and declarations.
    assert(write_pair(&tx, 1, 0, 1, true) == OK);
    struct mica_KernelRuleEvaluationResult evaluated = mica_kernel_rule_evaluate(&tx);
    assert(evaluated.f_status == OK && evaluated.f_evaluation);
    struct mica_MemoryRoot root = {.f_pointer = (uint8_t *)evaluated.f_evaluation};
    assert(mica_memory_root_push(&f.worker, &root));
    uint64_t stored[RULE_RELATIONS] = {0};
    stored[1] = oracle_bit(0, 1);
    oracle_check(&f.worker, &root, stored);
    assert(mica_memory_root_pop(&f.worker, &root));
    assert(write_pair(&tx, 1, 0, 1, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    uint64_t random = 321;
    for (unsigned sample = 0; sample < 24; ++sample) {
        memset(stored, 0, sizeof(stored));
        begin(&f, &tx);
        for (unsigned x = 0; x < RULE_DOMAIN; ++x) {
            for (unsigned y = 0; y < RULE_DOMAIN; ++y) {
                random ^= random << 13; random ^= random >> 7; random ^= random << 17;
                for (unsigned r = 0; r < 3; ++r) {
                    unsigned id = (unsigned[]){1, 2, 4}[r];
                    if (((random >> (r * 8)) & 7) != 0) continue;
                    stored[id] |= oracle_bit(x, y);
                    assert(write_pair(&tx, id, x, y, true) == OK);
                }
            }
        }
        struct collection_job job = {.heap = &f.heap};
        pthread_t collector;
        uint64_t epoch = f.heap.f_epoch;
        if (sample == 0) {
            assert(!pthread_create(&collector, NULL, collect_repeatedly, &job));
            while (!atomic_load(&job.ready)) sched_yield();
            for (;;) {
                mica_foreign_memory_lock(f.heap.f_mutex);
                bool requested = f.heap.f_collecting;
                mica_foreign_memory_unlock(f.heap.f_mutex);
                if (requested) break;
                sched_yield();
            }
        }
        evaluated = mica_kernel_rule_evaluate(&tx);
        assert(evaluated.f_status == OK && evaluated.f_evaluation);
        root.f_pointer = (uint8_t *)evaluated.f_evaluation;
        assert(mica_memory_root_push(&f.worker, &root));
        if (sample == 0) {
            assert(f.heap.f_epoch > epoch);
            assert(mica_memory_leave(&f.worker));
            assert(!pthread_join(collector, NULL));
            assert(mica_memory_enter(&f.worker));
        }
        assert(mica_memory_safepoint(&f.worker, true));
        oracle_check(&f.worker, &root, stored);
        // A fresh evaluation must not mutate an earlier retained result.
        uint64_t previous[RULE_RELATIONS];
        memcpy(previous, stored, sizeof(previous));
        for (unsigned x = 0; x < RULE_DOMAIN; ++x)
            for (unsigned y = 0; y < RULE_DOMAIN; ++y)
                if (stored[1] & oracle_bit(x, y)) assert(write_pair(&tx, 1, x, y, false) == OK);
        stored[1] = 0;
        evaluated = mica_kernel_rule_evaluate(&tx);
        assert(evaluated.f_status == OK);
        struct mica_MemoryRoot after = {.f_pointer = (uint8_t *)evaluated.f_evaluation};
        assert(mica_memory_root_push(&f.worker, &after));
        oracle_check(&f.worker, &after, stored);
        oracle_check(&f.worker, &root, previous);
        assert(mica_memory_root_pop(&f.worker, &after));
        assert(mica_memory_root_pop(&f.worker, &root));
        assert(mica_kernel_end(&tx));
    }
    // Resource exhaustion returns no partial result and keeps the draft usable.
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 0, 1, true) == OK);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    for (uint64_t limit = 0; limit < 4096; limit = limit ? limit * 2 : 1) {
        assert(mica_kernel_set_limits(&tx, limit, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        evaluated = mica_kernel_rule_evaluate(&tx);
        assert(evaluated.f_status == LIMIT || evaluated.f_status == OK);
        if (evaluated.f_status == LIMIT) assert(!evaluated.f_evaluation);
        assert(f.worker.f_roots == parent && !mica_kernel_work(&tx)->f_done);
        assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        assert(count(&tx, 1) == 1);
    }
    assert(mica_kernel_end(&tx));
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 0, 1, true) == OK);
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_MemoryRoot held[4096] = {{0}};
    unsigned retained_count = 0;
    bool failed = false;
    atomic_store(&allocation_budget, 0);
    while (retained_count < 4096) {
        struct mica_MemoryRoot *before = f.worker.f_roots;
        evaluated = mica_kernel_rule_evaluate(&tx);
        assert(f.worker.f_roots == before);
        if (evaluated.f_status == OOM) {
            assert(!evaluated.f_evaluation);
            failed = true;
            break;
        }
        assert(evaluated.f_status == OK);
        held[retained_count].f_pointer = (uint8_t *)evaluated.f_evaluation;
        assert(mica_memory_root_push(&f.worker, &held[retained_count++]));
    }
    atomic_store(&allocation_budget, -1);
    assert(failed && !mica_kernel_work(&tx)->f_done && count(&tx, 1) == 1);
    while (retained_count) assert(mica_memory_root_pop(&f.worker, &held[--retained_count]));
    assert(mica_kernel_rule_evaluate(&tx).f_status == OK);
    assert(mica_kernel_end(&tx));
#endif
    struct mica_KernelTransaction closed = {0};
    assert(mica_kernel_rule_evaluate(&closed).f_status == CLOSED);
    destroy(&f);
}

static void rule_evaluation_capture(void) {
    struct fixture f;
    init(&f);
    struct mica_MemoryRoot retained = {0};
    assert(mica_memory_root_push(&f.worker, &retained));
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 256; id > 0; --id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    assert(install_edge(&tx, 1, 256, 255, false) == OK);
    assert(write_pair(&tx, 255, 1, 2, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    // Catalogue visits consume allowance even when most relations are unrelated.
    // The two rule dependencies are at the end of the committed catalogue.
    assert(mica_kernel_set_limits(&tx, 128, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    struct mica_KernelRuleEvaluationResult result = mica_kernel_rule_evaluate(&tx);
    assert(result.f_status == LIMIT && !result.f_evaluation && f.worker.f_roots == parent);
    assert(mica_kernel_set_limits(&tx, 65536, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(count(&tx, 255) == 1 && count(&tx, 256) == 1);
    assert(!mica_kernel_index(mica_kernel_view(&tx, 256), 0));
    result = mica_kernel_rule_evaluate(&tx);
    assert(result.f_status == OK);
    retained.f_pointer = (uint8_t *)result.f_evaluation;
    // Draft facts override captured base relations. New unrelated declarations
    // remain accessible without being part of the dependency graph.
    assert(write_pair(&tx, 255, 1, 2, false) == OK);
    assert(write_pair(&tx, 255, 3, 4, true) == OK);
    assert(mica_kernel_declare(&tx, 257, 257, 2, SET, 0, 0) == OK);
    assert(write_pair(&tx, 257, 5, 6, true) == OK);
    result = mica_kernel_rule_evaluate(&tx);
    assert(result.f_status == OK);
    struct mica_MemoryRoot draft = {.f_pointer = (uint8_t *)result.f_evaluation};
    assert(mica_memory_root_push(&f.worker, &draft));
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_KernelQueryResult rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)draft.f_pointer, 256, true, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 1);
    assert(cell(mica_kernel_cell(rows.f_rows, 0), 0) == 3 && cell(mica_kernel_cell(rows.f_rows, 0), 1) == 4);
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)draft.f_pointer, 257, false, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 1);
    assert(cell(mica_kernel_cell(rows.f_rows, 0), 0) == 5 && cell(mica_kernel_cell(rows.f_rows, 0), 1) == 6);
    assert(mica_memory_root_pop(&f.worker, &draft));
    assert(mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 256, true, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 1);
    assert(cell(mica_kernel_cell(rows.f_rows, 0), 0) == 1 && cell(mica_kernel_cell(rows.f_rows, 0), 1) == 2);
    assert(mica_memory_root_pop(&f.worker, &retained));
    destroy(&f);
}

static void rule_evaluation_values(void) {
    struct fixture f;
    init(&f);
    struct mica_MemoryRoot retained = {0};
    assert(mica_memory_root_push(&f.worker, &retained));
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 4; ++id)
        assert(mica_kernel_declare(&tx, id, id, id == 3 ? 0 : 2, SET, 0, 0) == OK);
    mica_type_Value cells[] = {mica_value_float(1.0f).f_value, integer(2)};
    struct mica_ValueResult row = mica_value_list(&f.worker, cells, 2);
    assert(row.f_ok && mica_kernel_write(&tx, 1, row.f_value, true) == OK);
    assert(write_pair(&tx, 1, 1, 3, true) == OK);
    struct mica_KernelRuleGuard *guard = mica_kernel_rule_guard(&f.worker, 0,
        oracle_native_term(&f.worker, 0, NULL), mica_kernel_rule_constant(&f.worker, integer(1), NULL), NULL);
    assert(guard);
    struct mica_KernelRuleDef *definition = fixture_rule(&f.worker, 2, rule_pair(&f.worker), rule_atom_pair(&f.worker, 1, false, NULL), guard);
    assert(mica_kernel_rule_add(&tx, 1, definition, true) == OK);
    // An empty body derives the unit relation containing one zero-column row.
    definition = fixture_rule(&f.worker, 3, NULL, NULL, NULL);
    assert(mica_kernel_rule_add(&tx, 2, definition, true) == OK);
    // Atom constants retain canonical numeric kinds, unlike guard equality.
    int pattern[] = {-2, 1};
    struct mica_KernelRuleAtom *atom = mica_kernel_rule_atom(&f.worker, 1, false, oracle_native_terms(&f.worker, pattern), NULL);
    assert(atom);
    definition = fixture_rule(&f.worker, 4, oracle_native_terms(&f.worker, pattern), atom, NULL);
    assert(mica_kernel_rule_add(&tx, 3, definition, true) == OK);
    struct mica_KernelRuleEvaluationResult result = mica_kernel_rule_evaluate(&tx);
    assert(result.f_status == OK);
    retained.f_pointer = (uint8_t *)result.f_evaluation;
    assert(mica_kernel_end(&tx)); // Rollback leaves a rooted result usable.
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_KernelQueryResult rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 2, false, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 2);
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 3, true, UINT64_MAX);
    assert(rows.f_status == OK && rows.f_arity == 0 && mica_kernel_width(rows.f_rows) == 1);
    assert(mica_kernel_width(mica_kernel_cell(rows.f_rows, 0)) == 0);
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 4, false, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 1);
    assert(cell(mica_kernel_cell(rows.f_rows, 0), 0) == 1 && cell(mica_kernel_cell(rows.f_rows, 0), 1) == 3);
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 2, false, 0);
    assert(rows.f_status == LIMIT && !rows.f_rows);
    begin(&f, &tx);
    assert(!mica_kernel_view(&tx, 1) && mica_kernel_rule(&tx, 1).f_status == UNKNOWN);
    assert(mica_kernel_declare(&tx, 7, 7, 2, SET, 0, 0) == OK);
    assert(write_pair(&tx, 7, 4, 5, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    result = mica_kernel_rule_evaluate(&tx);
    assert(result.f_status == OK);
    retained.f_pointer = (uint8_t *)result.f_evaluation;
    assert(write_pair(&tx, 7, 4, 5, false) == OK);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    // A relation outside the rule graph still has its captured stored rows.
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 7, false, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 1);
    rows = derived_rows(&f.worker, (struct mica_KernelRuleEvaluation *)retained.f_pointer, 7, true, UINT64_MAX);
    assert(rows.f_status == OK && mica_kernel_width(rows.f_rows) == 0);
    assert(mica_memory_root_pop(&f.worker, &retained));
    destroy(&f);
}

// Compare maintained draft and committed rows against the independent oracle.
static void maintained_oracle_check(struct mica_KernelTransaction *tx, const uint64_t *stored) {
    assert(refresh(tx) == OK);
    struct mica_MemoryRoot root = {.f_pointer = (uint8_t *)mica_kernel_work(tx)->f_evaluation};
    assert(mica_memory_root_push(tx->f_worker, &root));
    oracle_check(tx->f_worker, &root, stored);
    uint64_t all[RULE_RELATIONS], derived[RULE_RELATIONS];
    oracle_evaluate(stored, all, derived);
    for (unsigned id = 1; id < RULE_RELATIONS; ++id) {
        unsigned expected = (unsigned)__builtin_popcountll(all[id]);
        assert(count(tx, id) == expected);
        struct mica_KernelQuery *query = mica_kernel_query_scan(tx->f_worker, id, tuple(tx->f_worker, 0, 0), 0);
        assert(query && query_count(tx, query, 2) == expected);
    }
    assert(mica_memory_root_pop(tx->f_worker, &root));
}
static void rule_maintenance(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    oracle_install(&tx);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    uint64_t stored[RULE_RELATIONS] = {0};
    uint64_t random = 7919;
    for (unsigned sample = 0; sample < 32; ++sample) {
        begin(&f, &tx);
        for (unsigned edit = 0; edit < 3; ++edit) {
            random ^= random << 13; random ^= random >> 7; random ^= random << 17;
            unsigned id = (unsigned[]){1, 2, 4}[edit];
            unsigned x = random % RULE_DOMAIN, y = random / RULE_DOMAIN % RULE_DOMAIN;
            uint64_t bit = oracle_bit(x, y);
            bool asserted = !(stored[id] & bit);
            assert(write_pair(&tx, id, x, y, asserted) == OK);
            stored[id] ^= bit;
            maintained_oracle_check(&tx, stored);
        }
        assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        begin(&f, &tx);
        maintained_oracle_check(&tx, stored);
        assert(mica_kernel_end(&tx));
    }
    destroy(&f);
}
static void rule_maintenance_catalogue(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 7; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    assert(install_edge(&tx, 1, 2, 1, false) == OK);
    assert(install_edge(&tx, 2, 2, 3, false) == OK);
    assert(install_edge(&tx, 3, 3, 2, false) == OK);
    struct mica_KernelRuleAtom *atoms = rule_atom_pair(&f.worker, 2, false, rule_atom_pair(&f.worker, 4, true, NULL));
    assert(mica_kernel_rule_add(&tx, 4, fixture_rule(&f.worker, 5, rule_pair(&f.worker), atoms, NULL), true) == OK);
    assert(install_edge(&tx, 6, 7, 6, false) == OK); // Independent component.
    assert(write_pair(&tx, 1, 10, 20, true) == OK);
    assert(write_pair(&tx, 6, 30, 40, true) == OK);
    assert(count(&tx, 5) == 1 && count(&tx, 7) == 1);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 11, 21, true) == OK);
    assert(count(&tx, 5) == 2);
    for (unsigned kind = 0; kind < 3; ++kind) {
        mica_type_Value positions = query_columns(&f.worker, (int64_t[]){0}, 1);
        struct mica_KernelQuery *left = query_scan(&f, 2), *right = query_scan(&f, 5);
        struct mica_KernelQuery *query = kind == 0 ? mica_kernel_query_join(&f.worker, left, right, positions, positions) :
            kind == 1 ? mica_kernel_query_semi(&f.worker, left, right, positions, positions) :
            mica_kernel_query_anti(&f.worker, left, right, positions, positions);
        assert(query_count(&tx, query, kind == 0 ? 4 : 2) == (kind == 2 ? 0 : 2));
    }
    struct mica_KernelRuleEvaluation *evaluation = mica_kernel_work(&tx)->f_evaluation;
    assert(evaluation->f_extended > 0 && evaluation->f_reused >= 2);
    struct mica_KernelRuleRows *base = mica_kernel_rule_rows_index_lookup(mica_kernel_work(&tx)->f_base->f_evaluation->f_rows, 7);
    struct mica_KernelRuleRows *current = mica_kernel_rule_rows_index_lookup(evaluation->f_rows, 7);
    assert(base->f_all == current->f_all && base->f_derived == current->f_derived);
    // Remove support from a mutually recursive component, including old edges.
    assert(mica_kernel_rule_remove(&tx, 1) == OK);
    assert(count(&tx, 2) == 0 && count(&tx, 3) == 0 && count(&tx, 5) == 0);
    assert(install_edge(&tx, 1, 2, 1, false) == OK);
    assert(install_edge(&tx, 5, 2, 1, false) == OK);
    assert(count(&tx, 2) == 2);
    assert(mica_kernel_rule_remove(&tx, 1) == OK);
    assert(count(&tx, 2) == 2); // Surviving rule still supports both rows.
    struct mica_KernelRuleDef *definition = fixture_rule(&f.worker, 2, rule_pair(&f.worker), rule_atom_pair(&f.worker, 1, false, NULL), NULL);
    assert(mica_kernel_rule_update(&tx, 5, definition, false) == OK);
    assert(count(&tx, 2) == 0 && count(&tx, 3) == 0);
    assert(install_edge(&tx, 1, 2, 1, false) == OK);
    definition = fixture_rule(&f.worker, 4, rule_pair(&f.worker), rule_atom_pair(&f.worker, 1, false, NULL), NULL);
    assert(mica_kernel_rule_update(&tx, 5, definition, true) == OK);
    assert(count(&tx, 5) == 0); // A new rule can invalidate negated consumers.
    assert(mica_kernel_rule_remove(&tx, 5) == OK);
    assert(count(&tx, 4) == 0 && count(&tx, 5) == 2);
    assert(mica_kernel_work(&tx)->f_evaluation->f_cleared >= 2);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 2) == 1 && count(&tx, 5) == 1); // Rollback kept the published closure.
    assert(mica_kernel_end(&tx));
    destroy(&f);
}
static void rule_maintenance_conflicts(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx, reader, writer;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 3; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    assert(install_edge(&tx, 1, 2, 1, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &reader);
    assert(count(&reader, 2) == 0);
    begin(&f, &writer);
    assert(write_pair(&writer, 1, 1, 2, true) == OK);
    assert(mica_kernel_commit(&writer) == OK && mica_kernel_end(&writer));
    assert(count(&reader, 2) == 0); // Existing readers retain their snapshot.
    assert(mica_kernel_commit(&reader) == CONFLICT);
    assert(mica_kernel_end(&reader));
    // Explicit closure results also consume fact inputs, including unruled ones.
    begin(&f, &reader);
    assert(mica_kernel_rule_evaluate(&reader).f_status == OK);
    begin(&f, &writer);
    assert(write_pair(&writer, 3, 9, 10, true) == OK);
    assert(mica_kernel_commit(&writer) == OK && mica_kernel_end(&writer));
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    begin(&f, &writer);
    assert(write_pair(&writer, 3, 9, 10, false) == OK);
    assert(mica_kernel_commit(&writer) == OK && mica_kernel_end(&writer));
    begin(&f, &reader);
    assert(count(&reader, 2) == 1);
    assert(write_pair(&reader, 3, 3, 4, true) == OK);
    begin(&f, &writer);
    assert(write_pair(&writer, 3, 5, 6, true) == OK);
    assert(mica_kernel_commit(&writer) == OK && mica_kernel_end(&writer));
    assert(mica_kernel_commit(&reader) == OK && mica_kernel_end(&reader));
    begin(&f, &tx);
    assert(count(&tx, 2) == 1 && count(&tx, 3) == 2);
    assert(write_pair(&tx, 1, 7, 8, true) == OK);
    assert(mica_kernel_set_limits(&tx, 0, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    struct mica_KernelRuleEvaluation *complete = mica_kernel_work(&tx)->f_evaluation;
    mica_type_Value buffer[4];
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    assert(scan(&tx, 2, 0, 0, 0, buffer, 4).f_status == LIMIT);
    assert(f.worker.f_roots == parent && mica_kernel_work(&tx)->f_evaluation == complete);
    assert(mica_kernel_work(&tx)->f_dirty);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(count(&tx, 2) == 2);
    assert(mica_kernel_set_limits(&tx, 0, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(!mica_kernel_work(&tx)->f_done && !mica_kernel_work(&tx)->f_candidate);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 1) == 1 && count(&tx, 2) == 1);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

static void *cancel_transaction(void *opaque) {
    assert(mica_kernel_cancel(opaque));
    return NULL;
}
struct cancellation_race {
    struct mica_KernelTransaction *tx;
    _Atomic bool ready;
    _Atomic bool go;
    bool won;
};
static void *race_cancellation(void *opaque) {
    struct cancellation_race *race = opaque;
    atomic_store(&race->ready, true);
    while (!atomic_load(&race->go)) sched_yield();
    race->won = mica_kernel_cancel(race->tx);
    return NULL;
}
static void rule_execution_controls(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 2; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    assert(install_edge(&tx, 1, 2, 1, false) == OK);
    assert(write_pair(&tx, 1, 1, 2, true) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(!mica_kernel_cancel(&tx)); // Publication already won.
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 3, 4, true) == OK);
    struct mica_KernelRuleEvaluation *previous = mica_kernel_work(&tx)->f_evaluation;
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, 0, UINT64_MAX, UINT64_MAX) == OK);
    assert(refresh(&tx) == LIMIT);
    assert(mica_kernel_work(&tx)->f_evaluation == previous);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, 0, UINT64_MAX) == OK);
    assert(refresh(&tx) == LIMIT);
    assert(mica_kernel_work(&tx)->f_evaluation == previous);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(refresh(&tx) == OK);
    pthread_t canceller;
    assert(!pthread_create(&canceller, NULL, cancel_transaction, &tx));
    assert(!pthread_join(canceller, NULL));
    assert(mica_kernel_rule_evaluate(&tx).f_status == CANCELLED);
    assert(mica_kernel_commit(&tx) == CANCELLED);
    assert(!mica_kernel_work(&tx)->f_done && !mica_kernel_work(&tx)->f_candidate);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 1) == 1 && count(&tx, 2) == 1);
    assert(write_pair(&tx, 1, 5, 6, true) == OK);
    struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_signal = tx.f_signal};
    assert(mica_kernel_prepare(&tx, &budget) == OK);
    assert(mica_kernel_cancel(&tx)); // Candidate is complete, but not published.
    assert(!mica_kernel_try_publish(&tx));
    assert(mica_kernel_work(&tx)->f_status == CANCELLED && !mica_kernel_work(&tx)->f_done);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 2) == 1);
    assert(mica_kernel_cancel(&tx));
    assert(mica_kernel_commit(&tx) == CANCELLED); // Read-only fast path too.
    assert(mica_kernel_end(&tx));
    for (unsigned attempt = 0; attempt < 32; ++attempt) {
        begin(&f, &tx);
        struct cancellation_race race = {.tx = &tx};
        assert(!pthread_create(&canceller, NULL, race_cancellation, &race));
        while (!atomic_load(&race.ready)) sched_yield();
        atomic_store(&race.go, true);
        uint64_t status = mica_kernel_commit(&tx);
        assert(!pthread_join(canceller, NULL));
        assert(status == (race.won ? CANCELLED : OK));
        assert(mica_kernel_work(&tx)->f_done == !race.won);
        assert(mica_kernel_end(&tx));
    }
    destroy(&f);
}

// Measure one operation's logical work, then enforce that same allowance
// across its children. A per-child budget reset would incorrectly succeed.
static void query_execution_controls(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 2; ++id) {
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
        for (int64_t i = 0; i < 24; ++i) assert(write_pair(&tx, id, i, i % 4, true) == OK);
    }
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    for (unsigned kind = 0; kind < 8; ++kind) {
        struct mica_MemoryRoot plan = {.f_pointer = (uint8_t *)trace_query(&f, 1, 0, 0, kind)};
        assert(mica_memory_root_push(&f.worker, &plan));
        // Prime immutable draft metadata so both runs measure the same state.
        assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        assert(mica_kernel_query_execute(&tx, (struct mica_KernelQuery *)plan.f_pointer).f_status == OK);
        struct mica_KernelBudget budget = operation_budget(&tx);
        struct mica_MemoryControl control = {.f_context = (uint8_t *)&budget, .f_check = mica_kernel_memory_check};
        mica_memory_control_push(&f.worker, &control);
        assert(mica_kernel_query_evaluate(&tx, (struct mica_KernelQuery *)plan.f_pointer, 0, &budget).f_status == OK);
        mica_memory_control_pop(&f.worker);
        assert(!f.worker.f_control);
        uint64_t steps = UINT64_MAX - budget.f_steps;
        assert(steps > 1);
        assert(mica_kernel_set_limits(&tx, steps - 1, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        assert(mica_kernel_query_execute(&tx, (struct mica_KernelQuery *)plan.f_pointer).f_status == LIMIT);
        assert(f.worker.f_roots == &plan);
        assert(mica_kernel_set_limits(&tx, steps, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        assert(mica_kernel_query_execute(&tx, (struct mica_KernelQuery *)plan.f_pointer).f_status == OK);
        assert(mica_kernel_set_limits(&tx, UINT64_MAX, 0, UINT64_MAX, UINT64_MAX) == OK);
        assert(mica_kernel_query_execute(&tx, (struct mica_KernelQuery *)plan.f_pointer).f_status == LIMIT);
        assert(f.worker.f_roots == &plan);
        assert(mica_memory_root_pop(&f.worker, &plan));
    }
    mica_type_Value rows[24];
    for (unsigned i = 0; i < 24; ++i) rows[i] = integer(-1);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, 4, UINT64_MAX, UINT64_MAX) == OK);
    struct mica_KernelScanResult result = scan(&tx, 1, 0, 0, 0, rows, 24);
    assert(result.f_status == LIMIT && result.f_count == 0);
    for (unsigned i = 0; i < 24; ++i) assert(rows[i] == integer(-1));
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    struct mica_KernelBudget failed = operation_budget(&tx);
    failed.f_status = LIMIT;
    assert(mica_kernel_rule_refresh(&tx, &failed) == LIMIT); // Cached view must not hide failure.
    assert(mica_kernel_prepare(&tx, &failed) == LIMIT);
    assert(!mica_kernel_work(&tx)->f_candidate);
    assert(mica_kernel_cancel(&tx));
    assert(scan(&tx, 1, 0, 0, 0, rows, 24).f_status == CANCELLED);
    for (unsigned i = 0; i < 24; ++i) assert(rows[i] == integer(-1));
    assert(mica_kernel_query_execute(&tx, query_scan(&f, 1)).f_status == CANCELLED);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

// Cancel at an actual allocation after the join starts. The allocator still
// succeeds: cancellation must propagate as CANCELLED, rather than OOM.
static void query_mid_join_cancellation(void) {
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    mica_type_Value tuples[128];
    for (unsigned i = 0; i < 128; ++i) tuples[i] = tuple(&f.worker, i, i % 8);
    struct mica_ValueResult rows = mica_value_list(&f.worker, tuples, 128);
    assert(rows.f_ok);
    mica_type_Value empty = query_columns(&f.worker, NULL, 0);
    struct mica_KernelBudget budget = operation_budget(&tx);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    atomic_store(&allocation_transaction, &tx);
    atomic_store(&cancellation_allocation, 3);
    struct mica_KernelQueryResult result = mica_kernel_query_join_rows(&f.worker,
        rows.f_value, rows.f_value, 2, 2, empty, empty, &budget);
    assert(atomic_load(&cancellation_allocation) == 0);
    atomic_store(&allocation_transaction, NULL);
    atomic_store(&cancellation_allocation, -1);
    assert(result.f_status == CANCELLED && !result.f_rows);
    assert(budget.f_status == CANCELLED && budget.f_rows < UINT64_MAX);
    assert(f.worker.f_roots == parent);
    assert(mica_kernel_commit(&tx) == CANCELLED);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_done);
    assert(mica_kernel_end(&tx));
    destroy(&f);
#endif
}

// Pages must retain every selected tuple while collection relocates the tree.
// Caller storage is populated only after the last safepoint in each scan.
static void scan_collection(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (int64_t i = 0; i < 512; ++i) assert(write_pair(&tx, 1, i, i % 8, true) == OK);
    assert(refresh(&tx) == OK);
    struct collection_job job = {.heap = &f.heap};
    pthread_t collector;
    assert(!pthread_create(&collector, NULL, collect_repeatedly, &job));
    while (!atomic_load(&job.ready)) sched_yield();
    for (;;) {
        mica_foreign_memory_lock(f.heap.f_mutex);
        bool requested = f.heap.f_collecting;
        mica_foreign_memory_unlock(f.heap.f_mutex);
        if (requested) break;
        sched_yield();
    }
    uint64_t total = 0;
    mica_type_Value after = 0;
    for (;;) {
        mica_type_Value rows[73];
        struct mica_MemoryRoot *roots = f.worker.f_roots;
        struct mica_KernelScanResult page = mica_kernel_scan(&tx, 1, tuple(&f.worker, 0, 0), 0, after, rows, 73);
        assert(page.f_status == OK && f.worker.f_roots == roots);
        for (uint64_t i = 0; i < page.f_count; ++i) {
            assert(cell(rows[i], 0) == (int64_t)total);
            assert(cell(rows[i], 1) == (int64_t)(total % 8));
            ++total;
        }
        if (!page.f_more) break;
        assert(page.f_count == 73);
        after = rows[page.f_count - 1];
    }
    assert(total == 512);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_leave(&f.worker));
    assert(!pthread_join(collector, NULL));
    assert(mica_memory_enter(&f.worker));
    destroy(&f);
}

static void preparation_controls(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx, observer, concurrent;
    begin(&f, &observer);
    begin(&f, &tx);
    for (int64_t i = 0; i < 512; ++i) assert(write_pair(&tx, 1, i, i % 8, true) == OK);
    assert(mica_kernel_set_limits(&tx, 32, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(f.worker.f_roots == parent);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_deltas);
    assert(mica_kernel_work(&tx)->f_status == LIMIT && !mica_kernel_work(&tx)->f_done);
    assert(count(&observer, 1) == 0);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_kernel_end(&observer));
    begin(&f, &tx);
    for (int64_t i = 512; i < 1024; ++i) assert(write_pair(&tx, 1, i, i % 8, true) == OK);
    begin(&f, &concurrent);
    assert(write_pair(&concurrent, 1, 2000, 1, true) == OK);
    assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
    // Validation now traverses the changed input tree before replay starts.
    assert(mica_kernel_set_limits(&tx, 16, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(mica_kernel_work(&tx)->f_status == LIMIT);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_deltas);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 1) == 1025);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

static void preparation_collection(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 64; ++id) {
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 2) == OK);
        assert(write_pair(&tx, id, id, id, true) == OK);
    }
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    for (int64_t i = 0; i < 512; ++i) assert(write_pair(&tx, 64, i, i + 1, true) == OK);
    struct collection_job job = {.heap = &f.heap};
    pthread_t collector;
    assert(!pthread_create(&collector, NULL, collect_repeatedly, &job));
    while (!atomic_load(&job.ready)) sched_yield();
    for (;;) {
        mica_foreign_memory_lock(f.heap.f_mutex);
        bool requested = f.heap.f_collecting;
        mica_foreign_memory_unlock(f.heap.f_mutex);
        if (requested) break;
        sched_yield();
    }
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    assert(mica_kernel_commit(&tx) == OK);
    assert(f.worker.f_roots == parent);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_leave(&f.worker));
    assert(!pthread_join(collector, NULL));
    assert(mica_memory_enter(&f.worker));
    begin(&f, &tx);
    assert(count(&tx, 64) == 513 && count(&tx, 63) == 1);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

static void preparation_cancellation(void) {
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (int64_t i = 0; i < 2048; ++i) assert(write_pair(&tx, 1, i, i % 8, true) == OK);
    // Complete any pending promotion before arming the allocation trigger.
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_KernelBudget budget = operation_budget(&tx);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    atomic_store(&allocation_transaction, &tx);
    atomic_store(&cancellation_allocation, 3);
    assert(mica_kernel_prepare(&tx, &budget) == CANCELLED);
    assert(atomic_load(&cancellation_allocation) == 0);
    atomic_store(&allocation_transaction, NULL);
    atomic_store(&cancellation_allocation, -1);
    assert(f.worker.f_roots == parent);
    assert(mica_kernel_work(&tx)->f_status == CANCELLED);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_deltas);
    assert(mica_kernel_commit(&tx) == CANCELLED && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 1) == 0);
    assert(mica_kernel_end(&tx));
    destroy(&f);
#endif
}

// Trigger after observable evaluation progress, independent of page reuse.
struct round_cancellation {
    struct mica_KernelTransaction *transaction;
    struct mica_KernelBudget *budget;
    uint64_t rows;
    bool cancelled;
};
static bool cancel_after_round_progress(uint8_t *context, uint64_t bytes, uint64_t steps) {
    struct round_cancellation *control = (void *)context;
    (void)bytes;
    (void)steps;
    if (!control->cancelled && control->budget->f_rows < control->rows) {
        assert(mica_kernel_cancel(control->transaction));
        control->cancelled = true;
    }
    return true;
}
static void fixpoint_cancellation(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 2; ++id) assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    assert(install_edge(&tx, 1, 2, 1, false) == OK);
    assert(add_rule(&tx, 2, 2, 2, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    for (int64_t i = 0; i < 512; ++i) assert(write_pair(&tx, 1, i, i + 1, true) == OK);
    struct mica_KernelBudget budget = operation_budget(&tx);
    struct mica_KernelWork *work = mica_kernel_work(&tx);
    struct mica_KernelRuleEvaluationResult captured = mica_kernel_rule_capture(&f.worker,
        work->f_base->f_relations, work->f_changes, work->f_rule_plan, &budget);
    assert(captured.f_status == OK);
    struct mica_MemoryRoot evaluation = {.f_pointer = (uint8_t *)captured.f_evaluation};
    assert(mica_memory_root_push(&f.worker, &evaluation));
    for (unsigned id = 1; id <= 2; ++id) {
        struct mica_KernelRuleResult rule = mica_kernel_rule(&tx, id);
        assert(rule.f_status == OK);
        assert(mica_kernel_rule_lower(&f.worker, (struct mica_KernelRuleEvaluation *)evaluation.f_pointer, rule.f_rule, &budget) == OK);
    }
    struct mica_KernelRuleComponent *component = mica_kernel_work(&tx)->f_rule_plan->f_components;
    while (component && !component->f_recursive) component = component->f_next;
    assert(component);
    struct mica_MemoryRoot round = {.f_pointer = (uint8_t *)component};
    assert(mica_memory_root_push(&f.worker, &round));
    assert(mica_memory_safepoint(&f.worker, true));
    struct round_cancellation state = {.transaction = &tx, .budget = &budget, .rows = budget.f_rows};
    struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = cancel_after_round_progress};
    mica_memory_control_push(&f.worker, &control);
    assert(mica_kernel_rule_round(&f.worker, (struct mica_KernelRuleEvaluation *)evaluation.f_pointer,
        (struct mica_KernelRuleComponent *)round.f_pointer, false, false, &budget) == CANCELLED);
    assert(state.cancelled && f.worker.f_control == &control);
    mica_memory_control_pop(&f.worker);
    assert(f.worker.f_roots == &round && budget.f_status == CANCELLED);
    assert(budget.f_rows < state.rows);
    assert(mica_memory_root_pop(&f.worker, &round));
    assert(mica_memory_root_pop(&f.worker, &evaluation));
    assert(mica_kernel_commit(&tx) == CANCELLED && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 1) == 0 && count(&tx, 2) == 0);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

struct publication_cancellation {
    struct mica_KernelTransaction *transaction;
    uint64_t remaining;
    bool cancelled;
};
static bool cancel_publication(uint8_t *context, uint64_t bytes, uint64_t steps) {
    struct publication_cancellation *control = (void *)context;
    (void)bytes;
    (void)steps;
    if (!control->cancelled && mica_kernel_work(control->transaction)->f_candidate && !--control->remaining) {
        assert(mica_kernel_cancel(control->transaction));
        control->cancelled = true;
    }
    return true; // The kernel's own budget reports the cancellation reason.
}
static void publication_cancellation(void) {
    unsigned cancelled = 0;
    bool published = false;
    for (unsigned checkpoint = 1; checkpoint < 1024; ++checkpoint) {
        struct fixture f;
        init(&f);
        declare(&f, 1, SET);
        struct mica_KernelTransaction tx;
        begin(&f, &tx);
        assert(write_pair(&tx, 1, 10, 20, true) == OK);
        struct publication_cancellation state = {.transaction = &tx, .remaining = checkpoint};
        struct mica_MemoryControl control = {.f_context = (void *)&state, .f_check = cancel_publication};
        struct mica_MemoryRoot *roots = f.worker.f_roots;
        mica_memory_control_push(&f.worker, &control);
        uint64_t status = mica_kernel_commit(&tx);
        assert(f.worker.f_control == &control && f.worker.f_roots == roots);
        mica_memory_control_pop(&f.worker);
        if (state.cancelled) {
            assert(status == CANCELLED && mica_kernel_work(&tx)->f_status == CANCELLED);
            assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_deltas);
            assert(!mica_kernel_work(&tx)->f_done);
            ++cancelled;
        } else {
            assert(status == OK);
            published = true;
        }
        assert(mica_kernel_end(&tx));
        begin(&f, &tx);
        assert(count(&tx, 1) == (published ? 1 : 0));
        assert(mica_kernel_end(&tx));
        destroy(&f);
        if (published) break;
    }
    assert(published && cancelled > 5);
}

static void allocation_controls(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, 0) == OK);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == LIMIT);
    assert(!f.worker.f_control && !mica_kernel_view(&tx, 1));
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    struct mica_KernelBudget outer = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_bytes = 0};
    struct mica_MemoryControl parent = {.f_context = (void *)&outer, .f_check = mica_kernel_memory_check};
    mica_memory_control_push(&f.worker, &parent);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == LIMIT);
    assert(f.worker.f_control == &parent && outer.f_status == LIMIT);
    mica_memory_control_pop(&f.worker);
    assert(!mica_kernel_view(&tx, 1));
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(write_pair(&tx, 1, 1, 2, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(!f.worker.f_control);

    begin(&f, &tx);
    struct mica_KernelRuleDef *definition = self_rule(&f.worker, 1, 2);
    assert(definition);
    mica_type_Value row = tuple(&f.worker, 2, 3);
    struct mica_KernelQuery *query = query_scan(&f, 1);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, 0) == OK);
    assert(mica_kernel_rule_add(&tx, 1, definition, true) == LIMIT);
    assert(!f.worker.f_control && !mica_kernel_work(&tx)->f_rule_changes);
    assert(mica_kernel_write(&tx, 1, row, true) == LIMIT);
    assert(!f.worker.f_control && !mica_kernel_work(&tx)->f_changes);
    assert(mica_kernel_query_execute(&tx, query).f_status == LIMIT);
    assert(!f.worker.f_control);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(count(&tx, 1) == 1);
    assert(write_pair(&tx, 1, 2, 3, true) == OK);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, 0) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(!f.worker.f_control && mica_kernel_work(&tx)->f_status == LIMIT);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_deltas);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(count(&tx, 1) == 1);
    assert(mica_kernel_end(&tx));

    // The byte allowance includes headers and alignment, with overflow checked.
    uint64_t extent = (sizeof(struct mica_MemoryObject) + 13 + 7) & ~UINT64_C(7);
    struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
        .f_rounds = UINT64_MAX, .f_bytes = extent};
    assert(mica_kernel_memory_check((void *)&budget, 13, 1));
    assert(budget.f_bytes == 0 && budget.f_status == OK);
    assert(!mica_kernel_memory_check((void *)&budget, 1, 1));
    assert(budget.f_status == LIMIT);
    budget.f_bytes = UINT64_MAX;
    budget.f_status = OK;
    assert(!mica_kernel_memory_check((void *)&budget, UINT64_MAX, 0));
    assert(budget.f_status == LIMIT && budget.f_bytes == UINT64_MAX);
    destroy(&f);
}

static void snapshot_admission(void) {
    struct fixture f;
    init(&f);
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 1) == OK);
    struct mica_KernelTransaction tx, refused = {0};
    begin(&f, &tx);
    assert(f.kernel.f_retained_transactions == 1);
    assert(!mica_kernel_release(&f.kernel, &f.worker));
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 0) == LIMIT);
    assert(f.kernel.f_retention_limit == 1);
    assert(mica_kernel_begin(&f.kernel, &f.worker, &refused, integer(7)) == LIMIT);
    assert(!refused.f_worker && !refused.f_signal && !refused.f_root.f_registration);
    assert(f.worker.f_roots == &tx.f_root && !f.worker.f_control);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(write_pair(&tx, 1, 1, 2, true) == OK);
    assert(mica_kernel_commit(&tx) == OK);
    assert(f.kernel.f_retained_transactions == 1); // Commit still retains the transaction's roots.
    assert(mica_kernel_begin(&f.kernel, &f.worker, &refused, integer(7)) == LIMIT);
    uint64_t allocated = f.heap.f_allocated;
    assert(mica_kernel_end(&tx));
    assert(!f.kernel.f_retained_transactions && f.heap.f_allocated == allocated);
    begin(&f, &tx);
    assert(count(&tx, 1) == 1);
    assert(mica_kernel_cancel(&tx));
    assert(mica_kernel_begin(&f.kernel, &f.worker, &refused, integer(7)) == LIMIT);
    assert(mica_kernel_end(&tx));
    assert(!f.kernel.f_retained_transactions);
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 0) == OK);
    assert(mica_kernel_begin(&f.kernel, &f.worker, &refused, integer(7)) == LIMIT);
    assert(!f.kernel.f_retained_transactions && !refused.f_worker);
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 1) == OK);
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 3, 4, true) == OK);
    assert(mica_kernel_end(&tx)); // Rollback also releases admission.
    begin(&f, &tx);
    assert(count(&tx, 1) == 1 && mica_kernel_end(&tx));
    destroy(&f);
}

static void heap_admission(void) {
    struct fixture f;
    init(&f);
    uint64_t capacity = f.heap.f_allocated + f.heap.f_nursery_bytes;
    assert(mica_memory_set_capacity(&f.heap, capacity));
    uint64_t remaining = f.worker.f_capacity - f.worker.f_used;
    assert(mica_memory_allocate(&f.worker, remaining - sizeof(struct mica_MemoryObject), 0));
    assert(f.worker.f_used == f.worker.f_capacity);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == LIMIT);
    assert(!f.kernel.f_retained_transactions && !tx.f_worker && !f.worker.f_control);
    assert(f.heap.f_allocated + f.heap.f_nursery_bytes == capacity);
    assert(mica_memory_safepoint(&f.worker, true));
    begin(&f, &tx); // Collection reclaimed the unrooted nursery payload.
    assert(mica_kernel_end(&tx));
    destroy(&f);

    init(&f);
    capacity = f.heap.f_allocated + f.heap.f_nursery_bytes;
    assert(mica_memory_set_capacity(&f.heap, capacity));
    begin(&f, &tx);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(write_pair(&tx, 1, 1, 2, true) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(mica_kernel_work(&tx)->f_status == LIMIT && !mica_kernel_work(&tx)->f_done);
    assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_deltas);
    assert(f.kernel.f_retained_transactions == 1 && !f.worker.f_control);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(!mica_kernel_view(&tx, 1));
    assert(mica_kernel_end(&tx));
    destroy(&f);

    init(&f);
    capacity = f.heap.f_allocated + f.heap.f_nursery_bytes;
    assert(mica_memory_set_capacity(&f.heap, capacity));
    begin(&f, &tx);
    assert(!mica_memory_allocate(&f.worker, 90000, 0));
    assert(f.heap.f_pressure);
    assert(mica_kernel_rule(&tx, 9).f_status == LIMIT); // Collection failed before catalogue access.
    assert(f.heap.f_collection_limited && f.worker.f_roots == &tx.f_root && !f.worker.f_control);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx); // Released roots no longer require promotion.
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

static void retained_snapshot_reclamation(void) {
    struct fixture f;
    init(&f);
    declare(&f, 1, SET);
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 4) == OK);
    struct mica_KernelTransaction history[3], writer;
    for (unsigned version = 0; version < 3; ++version) {
        begin(&f, &history[version]);
        assert(count(&history[version], 1) == version * 32);
        begin(&f, &writer);
        for (unsigned row = version * 32; row < (version + 1) * 32; ++row)
            assert(write_pair(&writer, 1, row, row + 1, true) == OK);
        assert(mica_kernel_commit(&writer) == OK && mica_kernel_end(&writer));
    }
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t retained = f.heap.f_retained;
    for (unsigned version = 0; version < 3; ++version)
        assert(count(&history[version], 1) == version * 32);
    for (unsigned version = 3; version > 0; --version)
        assert(mica_kernel_end(&history[version - 1]));
    assert(!f.kernel.f_retained_transactions && f.heap.f_retained == retained);
    assert(mica_memory_safepoint(&f.worker, true));
    assert(f.heap.f_retained < retained);
    begin(&f, &writer);
    assert(count(&writer, 1) == 96 && mica_kernel_end(&writer));
    destroy(&f);
}

struct snapshot_admission_job {
    struct fixture *fixture;
    _Atomic unsigned attempted, admitted;
    _Atomic bool release;
};
static void *admit_snapshot(void *opaque) {
    struct snapshot_admission_job *job = opaque;
    struct mica_MemoryWorker worker = {0};
    struct mica_KernelTransaction tx = {0};
    assert(mica_memory_worker_init(&worker, &job->fixture->heap, 8192));
    assert(mica_memory_enter(&worker));
    uint64_t status = mica_kernel_begin(&job->fixture->kernel, &worker, &tx, integer(7));
    assert(status == OK || status == LIMIT);
    if (status == OK) atomic_fetch_add(&job->admitted, 1);
    assert(mica_memory_leave(&worker));
    atomic_fetch_add(&job->attempted, 1);
    while (!atomic_load(&job->release)) sched_yield();
    if (status == OK) {
        assert(mica_memory_enter(&worker));
        assert(mica_kernel_end(&tx));
        assert(mica_memory_leave(&worker));
    }
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void concurrent_snapshot_admission(void) {
    struct fixture f;
    init(&f);
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 3) == OK);
    assert(mica_memory_leave(&f.worker));
    struct snapshot_admission_job job = {.fixture = &f};
    pthread_t threads[8];
    for (unsigned i = 0; i < 8; ++i) assert(!pthread_create(&threads[i], NULL, admit_snapshot, &job));
    while (atomic_load(&job.attempted) != 8) sched_yield();
    assert(atomic_load(&job.admitted) == 3 && f.kernel.f_retained_transactions == 3);
    atomic_store(&job.release, true);
    for (unsigned i = 0; i < 8; ++i) assert(!pthread_join(threads[i], NULL));
    assert(!f.kernel.f_retained_transactions);
    assert(mica_memory_enter(&f.worker));
    destroy(&f);
}

static mica_type_Value relation_identity(uint64_t id) {
    struct mica_ValueResult result = mica_value_identity(id);
    assert(result.f_ok);
    return result.f_value;
}
static void policy_fact(struct mica_KernelTransaction *tx, uint64_t policy, int64_t tenant, uint64_t relation, bool asserted) {
    mica_type_Value cells[] = {integer(tenant), relation_identity(relation)};
    uint64_t width = policy <= 3 ? 2 : 1;
    struct mica_ValueResult row = mica_value_list(tx->f_worker, cells, width);
    assert(row.f_ok && mica_kernel_write(tx, policy, row.f_value, asserted) == OK);
}
static struct mica_KernelAuthorityResult compile_authority(struct mica_KernelTransaction *tx, int64_t tenant, uint64_t steps) {
    struct mica_KernelBudget budget = operation_budget(tx);
    budget.f_steps = steps;
    struct mica_MemoryControl control = {.f_context = (void *)&budget, .f_check = mica_kernel_memory_check};
    mica_memory_control_push(tx->f_worker, &control);
    struct mica_KernelAuthorityResult result = mica_kernel_authority_compile(tx->f_worker, integer(tenant),
        mica_kernel_view(tx, 6), mica_kernel_view(tx, 4), mica_kernel_view(tx, 5),
        mica_kernel_view(tx, 1), mica_kernel_view(tx, 2), mica_kernel_view(tx, 3), &budget);
    mica_memory_control_pop(tx->f_worker);
    return result;
}
static void authority_projection(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    for (uint64_t id = 1; id <= 6; ++id)
        assert(mica_kernel_declare(&tx, id, id, id <= 3 ? 2 : 1, SET, 0, 0) == OK);
    for (int64_t tenant = 100; tenant < 1124; ++tenant)
        policy_fact(&tx, 1, tenant, 99, true);
    policy_fact(&tx, 1, 7, 10, true);
    policy_fact(&tx, 1, 7, 20, true);
    policy_fact(&tx, 2, 7, 20, true);
    policy_fact(&tx, 3, 7, 20, true);
    policy_fact(&tx, 4, 7, 0, true);
    policy_fact(&tx, 5, 8, 0, true);
    policy_fact(&tx, 6, 9, 0, true);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    struct mica_MemoryRoot *parent = f.worker.f_roots;
    // Only this tenant's indexed prefix consumes the row budget.
    struct mica_KernelAuthorityResult result = compile_authority(&tx, 7, 128);
    assert(result.f_status == OK && result.f_authority && f.worker.f_roots == parent);
    struct mica_KernelAuthority *authority = result.f_authority;
    assert(mica_kernel_authority_allows(authority, 10, 1));
    assert(!mica_kernel_authority_allows(authority, 10, 2));
    assert(!mica_kernel_authority_allows(authority, 10, 4));
    assert(mica_kernel_authority_allows(authority, 20, 7));
    assert(!mica_kernel_authority_allows(authority, 99, 1));
    assert(!mica_kernel_authority_allows(authority, 20, 8));
    assert(!mica_kernel_authority_allows(authority, 20, 0));
    assert(mica_kernel_authority_catalogue(authority) && !mica_kernel_authority_grant(authority));
    struct mica_MemoryRoot cached = {0};
    assert(mica_memory_root_push(&f.worker, &cached));
    cached.f_pointer = (void *)authority;
    assert(mica_memory_safepoint(&f.worker, true));
    authority = (void *)cached.f_pointer;
    assert(mica_kernel_authority_allows(authority, 20, 7));
    assert(mica_value_as_int(authority->f_tenant).f_number == 7);
    policy_fact(&tx, 3, 7, 20, false);
    // The captured context is immutable until the next task boundary.
    result = compile_authority(&tx, 7, 128);
    assert(result.f_status == OK && !mica_kernel_authority_allows(result.f_authority, 20, 4));
    assert(mica_kernel_authority_allows((void *)cached.f_pointer, 20, 4));
    result = compile_authority(&tx, 8, 128);
    assert(result.f_status == OK && mica_kernel_authority_grant(result.f_authority));
    assert(!mica_kernel_authority_catalogue(result.f_authority));
    assert(!mica_kernel_authority_allows(result.f_authority, 20, 1));
    result = compile_authority(&tx, 9, 128);
    assert(result.f_status == OK && mica_kernel_authority_catalogue(result.f_authority));
    assert(mica_kernel_authority_grant(result.f_authority));
    assert(mica_kernel_authority_allows(result.f_authority, 1234, 7));
    assert(!mica_kernel_authority_allows(result.f_authority, 0, 1));
    assert(!mica_kernel_authority_allows(result.f_authority, 20, 8));
    result = compile_authority(&tx, 10, 128);
    assert(result.f_status == OK && !mica_kernel_authority_catalogue(result.f_authority));
    assert(!mica_kernel_authority_allows(result.f_authority, 20, 1));
    unsigned limited = 0;
    for (uint64_t steps = 1; steps < 128; ++steps) {
        result = compile_authority(&tx, 7, steps);
        assert(f.worker.f_roots == &cached);
        if (result.f_status == LIMIT) {
            assert(!result.f_authority);
            ++limited;
        } else {
            assert(result.f_status == OK && mica_kernel_authority_allows(result.f_authority, 10, 1));
        }
    }
    assert(limited > 0);
    result = compile_authority(&tx, 7, 0);
    assert(result.f_status == LIMIT && !result.f_authority && f.worker.f_roots == &cached);
    assert(!mica_kernel_authority_allows(NULL, 20, 1));
    assert(!mica_kernel_authority_catalogue(NULL) && !mica_kernel_authority_grant(NULL));
    // Effective views include rule-derived grants, not just stored rows.
    assert(mica_kernel_declare(&tx, 7, 7, 2, SET, 0, 0) == OK);
    mica_type_Value granted[] = {integer(11), relation_identity(42)};
    struct mica_ValueResult grant_row = mica_value_list(&f.worker, granted, 2);
    assert(grant_row.f_ok && mica_kernel_write(&tx, 7, grant_row.f_value, true) == OK);
    assert(install_edge(&tx, 1, 1, 7, false) == OK && refresh(&tx) == OK);
    struct mica_KernelRelation *effective = mica_kernel_rule_rows_index_lookup(mica_kernel_work(&tx)->f_evaluation->f_rows, 1)->f_view;
    struct mica_KernelBudget budget = operation_budget(&tx);
    budget.f_steps = 128;
    result = mica_kernel_authority_compile(&f.worker, integer(11), NULL, NULL, NULL, effective, NULL, NULL, &budget);
    assert(result.f_status == OK && mica_kernel_authority_allows(result.f_authority, 42, 1));
    assert(!mica_kernel_authority_allows(result.f_authority, 42, 4));
    budget = operation_budget(&tx);
    result = mica_kernel_authority_compile(&f.worker, integer(7), NULL, NULL, NULL, mica_kernel_view(&tx, 4), NULL, NULL, &budget);
    assert(result.f_status == ARITY && !result.f_authority && f.worker.f_roots == &cached);
    // Matching malformed rows fail closed, with no partially built context.
    assert(write_pair(&tx, 1, 7, 50, true) == OK);
    result = compile_authority(&tx, 7, 128);
    assert(result.f_status == SCHEMA && !result.f_authority && f.worker.f_roots == &cached);
    assert(mica_kernel_cancel(&tx));
    result = compile_authority(&tx, 7, 128);
    assert(result.f_status == CANCELLED && !result.f_authority && f.worker.f_roots == &cached);
    assert(mica_memory_root_pop(&f.worker, &cached));
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

// Policy IDs 1..6 are read/write/export/catalogue/grant/root projections.
static void begin_tenant(struct fixture *f, struct mica_KernelTransaction *tx, int64_t tenant) {
    memset(tx, 0, sizeof(*tx));
    assert(mica_kernel_begin(&f->kernel, &f->worker, tx, integer(tenant)) == OK);
}
static struct mica_KernelRuleDef *tenant_definition(struct mica_KernelTransaction *tx, int64_t tenant,
                                                    uint64_t head, uint64_t source, bool negative) {
    struct mica_MemoryWorker *worker = tx->f_worker;
    struct mica_KernelRuleAtom *atoms = rule_atom_pair(worker, source, negative, NULL);
    if (negative) atoms = rule_atom_pair(worker, 14, false, atoms);
    struct mica_ValueResult text = mica_value_string(worker, (const uint8_t *)"policy fixture", 14);
    assert(text.f_ok);
    struct mica_KernelRuleDef *definition = mica_kernel_rule_definition(worker, head,
        rule_pair(worker), atoms, NULL, integer(tenant), text.f_value);
    assert(definition);
    return definition;
}
static uint64_t tenant_edge(struct mica_KernelTransaction *tx, uint64_t id, int64_t tenant,
                            uint64_t head, uint64_t source, bool negative) {
    return mica_kernel_rule_add(tx, id, tenant_definition(tx, tenant, head, source, negative), true);
}
static uint64_t read_status(struct mica_KernelTransaction *tx, uint64_t id) {
    struct mica_KernelBudget budget = operation_budget(tx);
    return mica_kernel_read(tx, id, &budget).f_status;
}
static void configure_policy(struct fixture *f) {
    struct mica_KernelTransaction tx;
    begin(f, &tx);
    for (uint64_t id = 1; id <= 6; ++id)
        assert(mica_kernel_declare(&tx, id, id, id <= 3 ? 2 : 1, SET, 0, 0) == OK);
    for (uint64_t id = 10; id <= 17; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    policy_fact(&tx, 6, 7, 0, true);
    for (int64_t tenant = 8; tenant <= 12; ++tenant) {
        if (tenant != 9) policy_fact(&tx, 4, tenant, 0, true);
    }
    for (uint64_t id = 10; id <= 14; ++id) {
        if (id != 12) policy_fact(&tx, 1, 8, id, true);
    }
    policy_fact(&tx, 2, 8, 10, true);
    policy_fact(&tx, 3, 8, 12, true);
    policy_fact(&tx, 3, 8, 15, true);
    policy_fact(&tx, 1, 9, 12, true);
    policy_fact(&tx, 1, 9, 15, true);
    policy_fact(&tx, 1, 10, 10, true); // Source read alone cannot export.
    policy_fact(&tx, 3, 11, 12, true); // Head export alone cannot read sources.
    policy_fact(&tx, 1, 12, 10, true);
    policy_fact(&tx, 3, 12, 12, true);
    assert(write_pair(&tx, 10, 1, 2, true) == OK);
    assert(write_pair(&tx, 11, 3, 4, true) == OK);
    assert(write_pair(&tx, 13, 1, 2, true) == OK);
    assert(write_pair(&tx, 14, 1, 2, true) == OK);
    assert(write_pair(&tx, 14, 3, 4, true) == OK);
    assert(mica_kernel_set_authority_policy(&tx, 6, 4, 5, 1, 2, 3) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
}
static void inactive_authority(void) {
    struct fixture f;
    init(&f);
    configure_policy(&f);
    struct mica_KernelTransaction tx;
    begin(&f, &tx);
    assert(mica_kernel_rule_add(&tx, 1, tenant_definition(&tx, 8, 12, 10, false), false) == OK);
    policy_fact(&tx, 3, 8, 12, false);
    assert(mica_kernel_commit(&tx) == PERMISSION);
    assert(!mica_kernel_work(&tx)->f_candidate);
    assert(mica_kernel_rule_remove(&tx, 1) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    policy_fact(&tx, 3, 8, 12, true);
    assert(mica_kernel_rule_add(&tx, 1, tenant_definition(&tx, 8, 12, 10, false), false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    // An existing inactive definition does not retain publishing authority.
    begin(&f, &tx);
    policy_fact(&tx, 3, 8, 12, false);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    struct mica_KernelRuleResult rule = mica_kernel_rule(&tx, 1);
    assert(rule.f_status == OK && !rule.f_rule->f_active);
    assert(mica_kernel_rule_update(&tx, 1, rule.f_rule->f_definition, true) == PERMISSION);
    policy_fact(&tx, 3, 8, 12, true);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    // Replacing an inactive definition is another installation, even though
    // it emits no rows. Final-candidate source authorization still applies.
    begin(&f, &tx);
    assert(mica_kernel_rule_update(&tx, 1, tenant_definition(&tx, 8, 12, 11, false), false) == OK);
    policy_fact(&tx, 1, 8, 11, false);
    assert(mica_kernel_commit(&tx) == PERMISSION);
    assert(mica_kernel_rule_remove(&tx, 1) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}
static void transactional_authority(void) {
    struct fixture f;
    init(&f);
    struct mica_KernelTransaction tx, reader, concurrent;
    begin_tenant(&f, &tx, 8);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == PERMISSION);
    assert(mica_kernel_rules(&tx).f_status == PERMISSION);
    assert(read_status(&tx, 1) == PERMISSION);
    assert(mica_kernel_end(&tx));
    // An all-zero configuration revokes implicit bootstrap ownership. Rollback
    // restores the unconfigured world; the draft task's boundary cache is fixed.
    begin(&f, &tx);
    assert(mica_kernel_set_authority_policy(&tx, 0, 0, 0, 0, 0, 0) == OK);
    assert(refresh(&tx) == OK && mica_kernel_work(&tx)->f_authority->f_root);
    assert(mica_kernel_end(&tx));
    configure_policy(&f);
    begin_tenant(&f, &tx, 8);
    assert(count(&tx, 10) == 1);
    assert(read_status(&tx, 12) == PERMISSION);
    assert(write_pair(&tx, 11, 1, 2, true) == PERMISSION);
    assert(mica_kernel_set_authority_policy(&tx, 0, 0, 0, 0, 0, 0) == PERMISSION);
    assert(mica_kernel_declare(&tx, 18, 18, 2, SET, 0, 0) == PERMISSION);
    assert(mica_kernel_rule_evaluate(&tx).f_status == PERMISSION);
    assert(tenant_edge(&tx, 1, 7, 12, 10, false) == PERMISSION);
    assert(tenant_edge(&tx, 1, 8, 16, 10, false) == PERMISSION);
    assert(tenant_edge(&tx, 1, 8, 12, 10, false) == OK);
    assert(tenant_edge(&tx, 2, 8, 15, 13, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin_tenant(&f, &tx, 10);
    assert(tenant_edge(&tx, 3, 10, 12, 10, false) == PERMISSION);
    assert(mica_kernel_rule_remove(&tx, 1) == PERMISSION);
    assert(mica_kernel_end(&tx));
    begin_tenant(&f, &tx, 11);
    assert(tenant_edge(&tx, 3, 11, 12, 10, false) == PERMISSION);
    assert(mica_kernel_end(&tx));
    begin_tenant(&f, &reader, 9);
    assert(mica_kernel_rules(&reader).f_status == PERMISSION);
    assert(read_status(&reader, 10) == PERMISSION);
    struct mica_KernelQueryResult denied = mica_kernel_query_execute(&reader, query_scan(&f, 10));
    assert(denied.f_status == PERMISSION && !denied.f_rows);
    // A fused anti-join must reject an unreadable source, not treat it as empty.
    struct mica_KernelQuery *left = query_scan(&f, 12);
    struct mica_KernelQuery *right = query_scan(&f, 13);
    mica_type_Value positions = query_columns(&f.worker, (int64_t[]){0, 1}, 2);
    struct mica_KernelQuery *anti = mica_kernel_query_anti(&f.worker, left, right, positions, positions);
    denied = mica_kernel_query_execute(&reader, anti);
    assert(denied.f_status == PERMISSION && !denied.f_rows);
    assert(count(&reader, 12) == 1);
    // This reader cannot see the blocked source. Its existing world row still
    // suppresses (1,2), leaving only (3,4) in the negated head.
    mica_type_Value rows[4];
    struct mica_KernelScanResult scanned = scan(&reader, 15, 0, 0, 0, rows, 4);
    assert(scanned.f_status == OK && scanned.f_count == 1 && cell(rows[0], 0) == 3);
    uint64_t policy_revision = mica_kernel_work(&reader)->f_base->f_policy_revision;
    begin(&f, &concurrent);
    assert(write_pair(&concurrent, 17, 5, 6, true) == OK);
    assert(mica_kernel_commit(&concurrent) == OK);
    assert(mica_kernel_work(&concurrent)->f_candidate->f_policy_revision == policy_revision);
    assert(mica_kernel_end(&concurrent));
    assert(mica_kernel_commit(&reader) == OK && mica_kernel_end(&reader));
    // A changed policy invalidates a permission-dependent read-only commit.
    begin_tenant(&f, &reader, 9);
    assert(count(&reader, 12) == 1);
    begin(&f, &concurrent);
    policy_fact(&concurrent, 1, 9, 12, false);
    assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
    assert(mica_memory_safepoint(&f.worker, true));
    assert(count(&reader, 12) == 1); // Immutable historical snapshot and cache.
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    begin_tenant(&f, &reader, 9);
    assert(read_status(&reader, 12) == PERMISSION && mica_kernel_end(&reader));
    // A permission check can precede a rejected operation without recording
    // fact or catalogue dependencies. The unchanged-commit fast path still
    // has to validate the policy generation.
    begin_tenant(&f, &reader, 8);
    assert(mica_kernel_write(&reader, 10, integer(1), true) == ARITY);
    assert(!mica_kernel_work(&reader)->f_changes && !mica_kernel_work(&reader)->f_rule_dependencies);
    begin(&f, &concurrent);
    policy_fact(&concurrent, 2, 8, 10, false);
    assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    // A caught denial also observes policy. A later grant invalidates that
    // observation even when the task has no staged writes.
    begin_tenant(&f, &reader, 9);
    assert(read_status(&reader, 10) == PERMISSION);
    begin(&f, &concurrent);
    policy_fact(&concurrent, 1, 9, 10, true);
    assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    // Catalogue-only access also records policy use (no fact read dependency).
    begin_tenant(&f, &reader, 8);
    assert(mica_kernel_rule_plan(&reader).f_status == OK);
    begin(&f, &concurrent);
    policy_fact(&concurrent, 4, 8, 0, false);
    assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    begin(&f, &tx);
    policy_fact(&tx, 4, 8, 0, true);
    policy_fact(&tx, 1, 9, 12, true);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    // Two tenants independently support the same conclusion.
    begin_tenant(&f, &tx, 12);
    assert(tenant_edge(&tx, 3, 12, 12, 10, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    policy_fact(&tx, 3, 8, 12, false);
    assert(refresh(&tx) == PERMISSION);
    assert(mica_kernel_rule_evaluate(&tx).f_status == PERMISSION);
    assert(mica_kernel_commit(&tx) == PERMISSION);
    assert(!mica_kernel_work(&tx)->f_candidate && mica_kernel_work(&tx)->f_dirty);
    begin_tenant(&f, &reader, 9);
    assert(count(&reader, 12) == 1 && mica_kernel_end(&reader));
    // Repair the private draft by exact deactivation, then publish together.
    struct mica_KernelRuleResult rule = mica_kernel_rule(&tx, 1);
    assert(rule.f_status == OK);
    assert(mica_kernel_rule_update(&tx, 1, rule.f_rule->f_definition, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin_tenant(&f, &reader, 9);
    assert(count(&reader, 12) == 1 && mica_kernel_end(&reader));
    begin(&f, &tx);
    // Revoking the remaining source permission requires removing that rule.
    policy_fact(&tx, 1, 12, 10, false);
    assert(mica_kernel_commit(&tx) == PERMISSION);
    assert(mica_kernel_rule_remove(&tx, 3) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin_tenant(&f, &reader, 9);
    assert(count(&reader, 12) == 0 && mica_kernel_end(&reader));
    // Configuration is transactional even without any extensional writes.
    begin(&f, &tx);
    assert(mica_kernel_set_authority_policy(&tx, 0, 0, 0, 0, 0, 0) == OK);
    assert(mica_kernel_rule_remove(&tx, 2) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(!mica_kernel_work(&tx)->f_authority->f_root);
    assert(mica_kernel_rules(&tx).f_status == PERMISSION && mica_kernel_end(&tx));
    destroy(&f);
}

static bool refuse_authority_allocation(uint8_t *context, uint64_t bytes, uint64_t steps) {
    struct mica_KernelTransaction *tx = (void *)context;
    (void)steps;
    // Work allocation precedes admission. Refuse the subsequent cache build.
    return !bytes || !tx->f_worker || !tx->f_root.f_registration || !tx->f_kernel->f_retained_transactions;
}
static void derived_authority(void) {
    struct fixture f;
    init(&f);
    configure_policy(&f);
    struct mica_KernelTransaction tx, reader = {0};
    struct mica_MemoryControl refused = {.f_context = (void *)&reader, .f_check = refuse_authority_allocation};
    mica_memory_control_push(&f.worker, &refused);
    assert(mica_kernel_begin(&f.kernel, &f.worker, &reader, integer(9)) == LIMIT);
    assert(!reader.f_worker && !reader.f_signal && !reader.f_root.f_registration);
    assert(!f.kernel.f_retained_transactions && !f.worker.f_roots && f.worker.f_control == &refused);
    mica_memory_control_pop(&f.worker);
    begin(&f, &tx);
    assert(mica_kernel_declare(&tx, 18, 18, 2, SET, 0, 0) == OK);
    mica_type_Value cells[] = {integer(14), relation_identity(12)};
    struct mica_ValueResult row = mica_value_list(&f.worker, cells, 2);
    assert(row.f_ok && mica_kernel_write(&tx, 18, row.f_value, true) == OK);
    assert(install_edge(&tx, 100, 1, 18, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin_tenant(&f, &reader, 14);
    assert(count(&reader, 12) == 0);
    begin(&f, &tx);
    row = mica_value_list(&f.worker, cells, 2);
    assert(row.f_ok && mica_kernel_write(&tx, 18, row.f_value, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    begin_tenant(&f, &reader, 14);
    assert(read_status(&reader, 12) == PERMISSION && mica_kernel_end(&reader));
    // A malformed tenant projection fails admission after capture. It must
    // release its retention slot, root, cancellation signal, and memory control.
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 15, 12, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    for (unsigned i = 0; i < 8; ++i) {
        assert(mica_kernel_begin(&f.kernel, &f.worker, &reader, integer(15)) == SCHEMA);
        assert(!reader.f_worker && !reader.f_signal && !reader.f_root.f_registration);
        assert(!f.kernel.f_retained_transactions && !f.worker.f_roots && !f.worker.f_control);
    }
    destroy(&f);
}
static void authority_tenant_collection(void) {
    struct fixture f = {0};
    assert(mica_kernel_heap_init(&f.heap));
    assert(mica_memory_worker_init(&f.worker, &f.heap, 32768));
    assert(mica_memory_enter(&f.worker));
    const char *name = "tenant-with-managed-storage";
    struct mica_ValueResult owner = mica_value_string(&f.worker, (const uint8_t *)name, strlen(name));
    assert(owner.f_ok && mica_kernel_init(&f.kernel, &f.worker, owner.f_value));
    struct mica_ValueResult tenant = mica_value_string(&f.worker, (const uint8_t *)name, strlen(name));
    assert(tenant.f_ok);
    // Admission's first safepoint must root the incoming tenant before moving
    // its nursery payload. The equal owner already lives in the shared heap.
    f.heap.f_pressure = true;
    uint64_t epoch = f.heap.f_epoch;
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, tenant.f_value) == OK);
    assert(f.heap.f_epoch > epoch && f.worker.f_roots == &tx.f_root);
    assert(mica_kernel_work(&tx)->f_authority->f_root);
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_KernelWork *work = mica_kernel_work(&tx);
    struct mica_BoolResult equal = mica_value_equal(work->f_tenant, work->f_base->f_owner);
    assert(equal.f_ok && equal.f_value);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    destroy(&f);
}
static void authority_controls(void) {
    struct fixture f;
    init(&f);
    configure_policy(&f);
    struct mica_KernelTransaction tx, reader;
    // Cancellation and budget failures publish neither grants nor configuration.
    begin(&f, &tx);
    policy_fact(&tx, 1, 9, 10, true);
    assert(mica_kernel_set_authority_policy(&tx, 6, 4, 5, 1, 2, 3) == OK);
    assert(mica_kernel_cancel(&tx));
    assert(mica_kernel_commit(&tx) == CANCELLED && mica_kernel_end(&tx));
    unsigned limited = 0;
    for (uint64_t budget = 1; budget <= 256; budget *= 2) {
        begin(&f, &tx);
        policy_fact(&tx, 1, 9, 10, true);
        assert(mica_kernel_set_limits(&tx, budget, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
        uint64_t status = mica_kernel_commit(&tx);
        assert(status == LIMIT || status == OK);
        if (status == LIMIT) {
            ++limited;
            assert(!mica_kernel_work(&tx)->f_candidate && !mica_kernel_work(&tx)->f_done);
            assert(f.worker.f_roots == &tx.f_root && !f.worker.f_control);
        }
        assert(mica_kernel_end(&tx));
        begin_tenant(&f, &reader, 9);
        assert(read_status(&reader, 10) == (status == OK ? OK : PERMISSION));
        assert(mica_kernel_end(&reader));
        if (status == OK) {
            begin(&f, &tx);
            policy_fact(&tx, 1, 9, 10, false);
            assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        }
    }
    assert(limited > 0);
    begin(&f, &tx);
    assert(mica_kernel_set_authority_policy(&tx, 0, 0, 0, 0, 0, 0) == OK);
    assert(!mica_kernel_work(&tx)->f_changes && !mica_kernel_work(&tx)->f_rule_changes);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(mica_kernel_declare(&tx, 19, 19, 2, SET, 0, 0) == PERMISSION);
    assert(mica_kernel_end(&tx));
    destroy(&f);
}

// Installers below are generated from tests/rule_sources.mica through the
// shared Mica parser. These expectations do not use the native rule evaluator
// as an oracle.
static bool collect_source_allocations(uint8_t *context, uint64_t bytes, uint64_t steps) {
    struct mica_MemoryHeap *heap = (void *)context;
    (void)steps;
    // This fixture has one worker. Request collection after each allocation;
    // the next generated poll must preserve partially constructed definitions.
    if (bytes) heap->f_collection_limit = 0;
    return true;
}
static void source_rules(void) {
    struct fixture f;
    init(&f);
    struct mica_SymbolTable symbols = {0};
    assert(mica_value_symbol_table_init(&symbols));
    struct mica_KernelTransaction tx, old;
    begin(&f, &tx);
    for (uint64_t id = 40; id <= 43; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 44, 44, 8, SET, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 45, 45, 0, SET, 0, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &old);
    begin(&f, &tx);
    assert(mica_kernel_source_0(&tx, &symbols, 100, false, true) == OK);
    assert(mica_kernel_source_1(&tx, &symbols, 101, false, true) == OK);
    assert(mica_kernel_source_2(&tx, &symbols, 102, false, true) == OK);
    assert(write_pair(&tx, 40, 1, 2, true) == OK);
    assert(write_pair(&tx, 40, 2, 3, true) == OK);
    assert(write_pair(&tx, 42, 1, 3, true) == OK);
    assert(count(&tx, 41) == 3 && count(&tx, 43) == 2);
    assert(count(&old, 41) == 0 && rule_count(&old) == 0);
    assert(mica_kernel_end(&tx)); // Discard rules, facts, and draft closure.
    begin(&f, &tx);
    assert(rule_count(&tx) == 0 && count(&tx, 40) == 0);
    // Force collections while source terms and chains are being constructed.
    uint64_t collection_limit = f.heap.f_collection_limit;
    struct mica_MemoryControl collection = {.f_context = (void *)&f.heap, .f_check = collect_source_allocations};
    mica_memory_control_push(&f.worker, &collection);
    uint64_t epoch = f.heap.f_epoch;
    assert(mica_kernel_source_0(&tx, &symbols, 100, false, true) == OK);
    assert(mica_kernel_source_1(&tx, &symbols, 101, false, true) == OK);
    assert(mica_kernel_source_2(&tx, &symbols, 102, false, true) == OK);
    assert(mica_kernel_source_3(&tx, &symbols, 103, false, true) == OK);
    assert(f.heap.f_epoch > epoch + 4 && f.worker.f_control == &collection);
    mica_memory_control_pop(&f.worker);
    f.heap.f_collection_limit = collection_limit;
    assert(write_pair(&tx, 40, 1, 2, true) == OK);
    assert(write_pair(&tx, 40, 2, 3, true) == OK);
    assert(write_pair(&tx, 42, 1, 3, true) == OK);
    struct mica_ValueResult unit = mica_value_list(&f.worker, NULL, 0);
    assert(unit.f_ok && mica_kernel_write(&tx, 45, unit.f_value, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(count(&old, 41) == 0 && mica_kernel_end(&old));
    assert(mica_memory_safepoint(&f.worker, true));
    begin(&f, &tx);
    assert(count(&tx, 41) == 3 && count(&tx, 43) == 2);
    struct mica_KernelRuleResult installed = mica_kernel_rule(&tx, 103);
    assert(installed.f_status == OK && installed.f_rule);
    const struct mica_HeapString *source = mica_value_as_string(installed.f_rule->f_definition->f_source).f_header;
    assert(source && source->f_length > 20);
    // Native identities and interned names are independent of bootstrap IDs.
    mica_type_Value pattern_cells[8] = {0}, rows[2];
    struct mica_ValueResult pattern = mica_value_list(&f.worker, pattern_cells, 8);
    assert(pattern.f_ok);
    struct mica_KernelScanResult found = mica_kernel_scan(&tx, 44, pattern.f_value, 0, 0, rows, 2);
    assert(found.f_status == OK && found.f_count == 1 && !found.f_more);
    assert(cell(rows[0], 0) == -7);
    assert(mica_value_as_float(mica_kernel_cell(rows[0], 1)).f_number == 1.5f);
    assert(mica_value_as_bool(mica_kernel_cell(rows[0], 2)).f_value);
    const struct mica_HeapString *text = mica_value_as_string(mica_kernel_cell(rows[0], 3)).f_header;
    const uint8_t utf8[] = {0xc3, 0xa9, 0xf0, 0x9f, 0x99, 0x82};
    assert(text->f_length == sizeof(utf8) && !memcmp(text->f_data, utf8, sizeof(utf8)));
    const struct mica_HeapBytes *bytes = mica_value_as_bytes(mica_kernel_cell(rows[0], 4)).f_header;
    assert(bytes->f_length == 2 && bytes->f_data[0] == 0 && bytes->f_data[1] == 255);
    struct mica_IdResult symbol = mica_value_as_symbol(mica_kernel_cell(rows[0], 5));
    struct mica_SymbolText name = mica_value_symbol_text(&symbols, (uint32_t)symbol.f_number);
    assert(symbol.f_ok && name.f_ok && name.f_length == 2 && !memcmp(name.f_data, utf8, 2));
    assert(mica_value_as_identity(mica_kernel_cell(rows[0], 6)).f_number == 1234);
    struct mica_IdResult error = mica_value_as_error_code(mica_kernel_cell(rows[0], 7));
    name = mica_value_symbol_text(&symbols, (uint32_t)error.f_number);
    assert(error.f_ok && name.f_ok && name.f_length == 5 && !memcmp(name.f_data, "E_DIV", 5));
    assert(mica_kernel_source_4(&tx, &symbols, 104, false, true) == SCHEMA);
    assert(mica_kernel_source_5(&tx, &symbols, 104, false, true) == SCHEMA);
    assert(mica_kernel_source_6(&tx, &symbols, 104, false, true) == SCHEMA);
    assert(mica_kernel_source_7(&tx, &symbols, 104, false, true) == UNKNOWN);
    assert(rule_count(&tx) == 4);
    assert(mica_kernel_source_0(&tx, &symbols, 100, false, true) == SCHEMA);
    assert(mica_kernel_source_1(&tx, &symbols, 101, true, false) == OK);
    assert(count(&tx, 41) == 2);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(mica_kernel_set_limits(&tx, 0, UINT64_MAX, UINT64_MAX, UINT64_MAX) == OK);
    assert(mica_kernel_source_0(&tx, &symbols, 200, false, true) == LIMIT);
    assert(mica_kernel_end(&tx));
    begin(&f, &tx);
    assert(mica_kernel_cancel(&tx));
    assert(mica_kernel_source_0(&tx, &symbols, 200, false, true) == CANCELLED);
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_source_0(&tx, &symbols, 200, false, true) == CLOSED);
    struct mica_KernelTransaction denied = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &denied, integer(99)) == OK);
    assert(mica_kernel_source_0(&denied, &symbols, 200, false, true) == PERMISSION);
    assert(mica_kernel_end(&denied));
    mica_value_symbol_table_release(&symbols);
    destroy(&f);
}

#include "measurements.c"

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "trace")) return trace(false);
    if (argc == 3 && !strcmp(argv[1], "rule-bench")) return rule_measurements((unsigned)strtoul(argv[2], NULL, 10));
    if (argc == 2 && !strcmp(argv[1], "rules")) return trace(true);
    if (argc == 5 && !strcmp(argv[1], "bench")) return benchmark(argv[2], (unsigned)strtoul(argv[3], NULL, 10), (unsigned)strtoul(argv[4], NULL, 10));
    assert(argc == 1);
    source_rules();
    inactive_authority();
    transactional_authority();
    derived_authority();
    authority_controls();
    authority_tenant_collection();
    authority_projection();
    snapshot_admission();
    heap_admission();
    retained_snapshot_reclamation();
    concurrent_snapshot_admission();
    allocation_controls();
    publication_cancellation();
    rule_catalogue();
    rule_definition_validation();
    rule_dependency_planning();
    rule_work_limits();
    rule_evaluation();
    rule_evaluation_capture();
    rule_evaluation_values();
    rule_maintenance();
    rule_maintenance_catalogue();
    rule_maintenance_conflicts();
    rule_execution_controls();
    preparation_controls();
    preparation_collection();
    preparation_cancellation();
    fixpoint_cancellation();
    query_execution_controls();
    query_mid_join_cancellation();
    scan_collection();
    rule_conflicts();
    rule_allocation_failures();
    rule_prepared_publication();
    rule_concurrency();
    rule_index_edits();
    rule_collection(false);
    rule_collection(true);
    rule_deep_planning();
    rule_graph_oracle();
    query_indexes();
    queries();
    query_collection(true);
    query_collection(false);
    basics();
    indexes_and_gc();
    conflicts();
    prepared_publication_rebases();
    small_commit_copies_changed_paths();
    concurrency();
    allocation_failures();
    return 0;
}
