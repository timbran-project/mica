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
uint8_t *mica_foreign_memory_platform_allocate(uint64_t size) {
    long remaining = atomic_load(&allocation_budget);
    if (remaining == 0) return NULL;
    if (remaining > 0) atomic_fetch_sub(&allocation_budget, 1);
    return malloc(size);
}
void mica_foreign_memory_platform_release(uint8_t *pointer) { free(pointer); }
#endif

// These values are the public status and conflict-policy codes.
enum { OK, UNKNOWN, ARITY, NONPERSISTENT, KEY, CONFLICT, NAME, SCHEMA, OOM, CLOSED, LIMIT };
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
    uint64_t budget = mica_kernel_work(&a)->f_rule_limit;
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
    return mica_kernel_rule_definition(worker, head, terms, atom, NULL, integer(1), source.f_value);
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
    assert(mica_kernel_rule_depend(&a, 3) == OK);
    assert(install_edge(&b, 8, 4, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == CONFLICT);
    assert(mica_kernel_work(&a)->f_conflict_relation == 4);
    assert(mica_kernel_end(&a));
    // Unrelated heads do not force task retry.
    begin(&f, &a);
    begin(&f, &b);
    assert(mica_kernel_rule_depend(&a, 1) == OK);
    assert(install_edge(&b, 9, 5, 2, false) == OK);
    assert(mica_kernel_commit(&b) == OK && mica_kernel_end(&b));
    assert(mica_kernel_commit(&a) == OK && mica_kernel_end(&a));
    // Inactive rules participate in structural validation, but do not change
    // the active head generation. Activation must check stratification again.
    begin(&f, &a);
    begin(&f, &b);
    assert(mica_kernel_rule_depend(&a, 1) == OK);
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
    assert(mica_kernel_rule_set_limit(&tx, 0) == OK);
    assert(install_edge(&tx, 10, 5, 2, false) == LIMIT);
    assert(mica_kernel_rule(&tx, 10).f_status == UNKNOWN);
    assert(mica_kernel_rule_set_limit(&tx, UINT64_MAX) == OK);
    assert(install_edge(&tx, 10, 5, 2, false) == OK);
    assert(mica_kernel_rule_set_limit(&tx, 0) == OK);
    assert(mica_kernel_commit(&tx) == LIMIT);
    assert(mica_kernel_work(&tx)->f_status == LIMIT && mica_kernel_work(&tx)->f_candidate == NULL);
    assert(mica_kernel_rule_set_limit(&tx, UINT64_MAX) == OK);
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
    for (unsigned limit = 0; limit < 4; ++limit) {
        assert(mica_kernel_rule_set_limit(&tx, limit) == OK);
        assert(mica_kernel_commit(&tx) == LIMIT);
        assert(f.worker.f_roots == parent);
        assert(mica_kernel_work(&tx)->f_status == LIMIT);
        assert(!mica_kernel_work(&tx)->f_done && !mica_kernel_work(&tx)->f_candidate);
    }
    assert(mica_kernel_rule_set_limit(&tx, 4) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));

    begin(&f, &tx);
    for (unsigned id = 100; id < 164; ++id) assert(mica_kernel_rule(&tx, id).f_status == UNKNOWN);
    assert(mica_kernel_declare(&tx, 1, 1, 2, SET, 0, 0) == OK);
    assert(add_rule(&tx, 1, 1, 2, false) == OK);
    parent = f.worker.f_roots;
    // Schema preparation, its dependency, and 65 catalogue observations leave
    // four steps of this allowance. The definition cannot finish validation.
    // Resetting the allowance at compilation incorrectly makes this succeed.
    assert(mica_kernel_rule_set_limit(&tx, 71) == OK);
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
    assert(mica_kernel_rule_set_limit(&tx, UINT64_MAX) == OK);
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
    uint64_t budget = mica_kernel_work(&a)->f_rule_limit;
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

struct rule_parallel { struct fixture *fixture; unsigned id; };
static void *rule_parallel_worker(void *opaque) {
    struct rule_parallel *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, &job->fixture->heap, 4096));
    assert(mica_memory_enter(&worker));
    for (unsigned round = 0; round < 20; ++round) {
        bool committed = false;
        for (unsigned attempt = 0; attempt < 1000 && !committed; ++attempt) {
            struct mica_KernelTransaction tx = {0};
            assert(mica_kernel_begin(&job->fixture->kernel, &worker, &tx) == OK);
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
            assert(mica_kernel_end(&tx));
            if (round % 7 == 0) assert(mica_memory_safepoint(&worker, true));
        }
        assert(committed);
    }
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void rule_concurrency(void) {
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
    uint64_t budget = mica_kernel_work(&tx)->f_rule_limit;
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
    uint64_t budget = VERTICES * 16;
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
        uint64_t budget = 10000;
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

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "trace")) return trace();
    if (argc == 5 && !strcmp(argv[1], "bench")) return benchmark(argv[2], (unsigned)strtoul(argv[3], NULL, 10), (unsigned)strtoul(argv[4], NULL, 10));
    assert(argc == 1);
    rule_catalogue();
    rule_definition_validation();
    rule_dependency_planning();
    rule_work_limits();
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
