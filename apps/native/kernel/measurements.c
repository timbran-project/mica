// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Included by tests.c. Compiler hooks are enabled only in the diagnostic build;
// ordinary measurements run the same generated module without instrumentation.
enum { PROFILE_CATALOG, PROFILE_RULES, PROFILE_DERIVE, PROFILE_PUBLISH, PROFILE_GC, PROFILE_SHARE, PROFILE_VIEWS, PROFILE_AUTHORITY, PROFILE_AUTHORITY_CHANGES, PROFILE_COUNT };
struct rule_profile {
    uint64_t start[PROFILE_COUNT], ns[PROFILE_COUNT], calls[PROFILE_COUNT], max_ns[PROFILE_COUNT];
    uint64_t held_at, lock_ns, lock_max_ns, locks;
};
static _Thread_local struct rule_profile rule_profile;
static _Atomic bool rule_profile_enabled;
static _Atomic(uint8_t *) rule_profile_mutex;
static int profile_index(void *function) {
    uintptr_t address = (uintptr_t)function;
    if (address == (uintptr_t)mica_kernel_prepare_catalog) return PROFILE_CATALOG;
    if (address == (uintptr_t)mica_kernel_rules_rebase) return PROFILE_RULES;
    if (address == (uintptr_t)mica_kernel_rule_maintain) return PROFILE_DERIVE;
    if (address == (uintptr_t)mica_kernel_try_publish) return PROFILE_PUBLISH;
    if (address == (uintptr_t)mica_memory_collect_stopped) return PROFILE_GC;
    if (address == (uintptr_t)mica_memory_share) return PROFILE_SHARE;
    if (address == (uintptr_t)mica_kernel_authority_views) return PROFILE_VIEWS;
    if (address == (uintptr_t)mica_kernel_authority_validate) return PROFILE_AUTHORITY;
    if (address == (uintptr_t)mica_kernel_authority_changes) return PROFILE_AUTHORITY_CHANGES;
    return -1;
}
void __cyg_profile_func_enter(void *function, void *caller) {
    (void)caller;
    if (!atomic_load(&rule_profile_enabled)) return;
    int index = profile_index(function);
    if (index < 0) return;
    if (index == PROFILE_CATALOG || index == PROFILE_RULES || index == PROFILE_DERIVE || index >= PROFILE_VIEWS)
        assert(!rule_profile.held_at); // Preparation must never hold the publication lock.
    assert(!rule_profile.start[index]);
    rule_profile.start[index] = nanos();
    ++rule_profile.calls[index];
}
void __cyg_profile_func_exit(void *function, void *caller) {
    (void)caller;
    if (!atomic_load(&rule_profile_enabled)) return;
    int index = profile_index(function);
    if (index < 0) return;
    assert(rule_profile.start[index]);
    uint64_t elapsed = nanos() - rule_profile.start[index];
    rule_profile.ns[index] += elapsed;
    if (elapsed > rule_profile.max_ns[index]) rule_profile.max_ns[index] = elapsed;
    rule_profile.start[index] = 0;
}
#ifdef MICA_KERNEL_PROFILE
void __real_mica_foreign_memory_lock(uint8_t *pointer);
void __real_mica_foreign_memory_unlock(uint8_t *pointer);
void __wrap_mica_foreign_memory_lock(uint8_t *pointer) {
    __real_mica_foreign_memory_lock(pointer);
    if (atomic_load(&rule_profile_enabled) && pointer == atomic_load(&rule_profile_mutex)) {
        assert(!rule_profile.held_at);
        rule_profile.held_at = nanos();
    }
}
void __wrap_mica_foreign_memory_unlock(uint8_t *pointer) {
    if (atomic_load(&rule_profile_enabled) && pointer == atomic_load(&rule_profile_mutex)) {
        assert(rule_profile.held_at);
        uint64_t elapsed = nanos() - rule_profile.held_at;
        rule_profile.lock_ns += elapsed;
        if (elapsed > rule_profile.lock_max_ns) rule_profile.lock_max_ns = elapsed;
        ++rule_profile.locks;
        rule_profile.held_at = 0;
    }
    __real_mica_foreign_memory_unlock(pointer);
}
#endif
static void profile_begin(struct fixture *f) {
    memset(&rule_profile, 0, sizeof(rule_profile));
    atomic_store(&rule_profile_mutex, f->kernel.f_mutex);
    atomic_store(&rule_profile_enabled, true);
}
static struct rule_profile profile_end(void) {
    atomic_store(&rule_profile_enabled, false);
    assert(!rule_profile.held_at);
    return rule_profile;
}
static uint64_t measurement_tree_count(struct mica_KernelNode *node) {
    return node ? 1 + measurement_tree_count(node->f_left) + measurement_tree_count(node->f_right) : 0;
}
static void measurement_tree_equal(struct mica_KernelNode *actual, struct mica_KernelNode *expected) {
    assert(measurement_tree_count(actual) == measurement_tree_count(expected));
    if (!actual) return;
    assert(mica_kernel_find(expected, actual->f_row, 0, false));
    // Subtree shapes can differ; compare logical membership against the whole tree.
    struct mica_KernelNode *stack[128];
    unsigned length = 0;
    stack[length++] = actual;
    while (length) {
        struct mica_KernelNode *node = stack[--length];
        assert(mica_kernel_find(expected, node->f_row, 0, false));
        assert(length + 2 <= 128);
        if (node->f_left) stack[length++] = node->f_left;
        if (node->f_right) stack[length++] = node->f_right;
    }
}
static void measurement_closure(struct mica_KernelTransaction *tx, unsigned edge, unsigned path) {
    assert(install_edge(tx, edge, path, edge, false) == OK);
    int head[] = {0, 2}, left[] = {0, 1}, right[] = {1, 2};
    struct mica_KernelRuleAtom *atoms = mica_kernel_rule_atom(tx->f_worker, edge, false, oracle_native_terms(tx->f_worker, right), NULL);
    atoms = mica_kernel_rule_atom(tx->f_worker, path, false, oracle_native_terms(tx->f_worker, left), atoms);
    assert(mica_kernel_rule_add(tx, path, fixture_rule(tx->f_worker, path, oracle_native_terms(tx->f_worker, head), atoms, NULL), true) == OK);
}
static void measurement_limits(struct mica_KernelTransaction *tx) {
    assert(mica_kernel_set_limits(tx, 50000000, 1000000, 100000, 256 * 1024 * 1024) == OK);
}
static void measurement_report(const char *mode, unsigned n, unsigned sample, uint64_t elapsed,
    const struct rule_profile *profile, const struct mica_KernelRuleEvaluation *evaluation,
    uint64_t before_bytes, uint64_t after_bytes) {
#ifdef MICA_KERNEL_PROFILE
    assert(profile->calls[PROFILE_CATALOG] == 1 && profile->calls[PROFILE_RULES] == 1);
    assert(profile->calls[PROFILE_DERIVE] == 1 && profile->calls[PROFILE_PUBLISH] == 1);
    assert(profile->locks == 2);
#endif
    printf("{\"workload\":\"rules-%s\",\"edges\":%u,\"sample\":%u,\"instrumented\":%s,"
        "\"commit_ns\":%llu,\"validation_ns\":%llu,\"derivation_ns\":%llu,\"publication_ns\":%llu,\"share_ns\":%llu,"
        "\"lock_hold_ns\":%llu,\"max_lock_hold_ns\":%llu,\"locks\":%llu,\"collection_ns\":%llu,\"collections\":%llu,"
        "\"candidate_builds\":%llu,\"max_collection_ns\":%llu,\"derivations\":%llu,\"probes\":%llu,\"evaluation_steps\":%llu,\"rounds\":%llu,\"rows_cleared\":%llu,"
        "\"reused_components\":%llu,\"extended_components\":%llu,\"recomputed_components\":%llu,"
        "\"allocated_before\":%llu,\"allocated_after\":%llu}\n",
        mode, n, sample, profile->calls[PROFILE_PUBLISH] ? "true" : "false",
        (unsigned long long)elapsed,
        (unsigned long long)(profile->ns[PROFILE_CATALOG] + profile->ns[PROFILE_RULES] + profile->ns[PROFILE_VIEWS] + profile->ns[PROFILE_AUTHORITY] + profile->ns[PROFILE_AUTHORITY_CHANGES]),
        (unsigned long long)profile->ns[PROFILE_DERIVE], (unsigned long long)profile->ns[PROFILE_PUBLISH],
        (unsigned long long)profile->ns[PROFILE_SHARE], (unsigned long long)profile->lock_ns,
        (unsigned long long)profile->lock_max_ns, (unsigned long long)profile->locks,
        (unsigned long long)profile->ns[PROFILE_GC], (unsigned long long)profile->calls[PROFILE_GC],
        (unsigned long long)profile->calls[PROFILE_CATALOG], (unsigned long long)profile->max_ns[PROFILE_GC],
        (unsigned long long)profile->calls[PROFILE_DERIVE], (unsigned long long)evaluation->f_probes,
        (unsigned long long)evaluation->f_steps, (unsigned long long)evaluation->f_rounds, (unsigned long long)evaluation->f_cleared,
        (unsigned long long)evaluation->f_reused, (unsigned long long)evaluation->f_extended,
        (unsigned long long)evaluation->f_recomputed, (unsigned long long)before_bytes, (unsigned long long)after_bytes);
}
static void measurement_reference(struct mica_KernelTransaction *tx, unsigned relations) {
    struct mica_KernelRuleEvaluationResult full = mica_kernel_rule_evaluate(tx);
    assert(full.f_status == OK);
    struct mica_KernelRuleEvaluation *maintained = mica_kernel_work(tx)->f_base->f_evaluation;
    for (unsigned id = 1; id <= relations; ++id) {
        struct mica_KernelRuleRows *actual = mica_kernel_rule_rows_index_lookup(maintained->f_rows, id);
        struct mica_KernelRuleRows *expected = mica_kernel_rule_rows_index_lookup(full.f_evaluation->f_rows, id);
        measurement_tree_equal(actual->f_all, expected->f_all);
        measurement_tree_equal(actual->f_derived, expected->f_derived);
    }
}
static void measurement_updates(unsigned n, unsigned samples) {
    struct fixture f;
    init(&f);
    assert(mica_memory_set_capacity(&f.heap, 256 * 1024 * 1024));
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 4) == OK);
    struct mica_KernelTransaction tx, old;
    begin(&f, &tx);
    for (unsigned id = 1; id <= 4; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    measurement_closure(&tx, 1, 2);
    measurement_closure(&tx, 3, 4);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &old);
    begin(&f, &tx);
    measurement_limits(&tx);
    profile_begin(&f);
    uint64_t start = nanos();
    for (unsigned i = 0; i < n; ++i) {
        assert(write_pair(&tx, 1, i, i + 1, true) == OK);
        assert(write_pair(&tx, 3, i, i + 1, true) == OK);
        if (i % 8 == 0) assert(count(&old, 2) == 0);
    }
    uint64_t staging_ns = nanos() - start;
    assert(rule_profile.calls[PROFILE_DERIVE] == 0);
    uint64_t before = f.heap.f_allocated;
    start = nanos();
    assert(mica_kernel_commit(&tx) == OK);
    uint64_t elapsed = nanos() - start;
    struct rule_profile profile = profile_end();
    assert(profile.calls[PROFILE_DERIVE] <= 1);
    measurement_report("bulk", n, 0, elapsed, &profile, mica_kernel_work(&tx)->f_candidate->f_evaluation, before, f.heap.f_allocated);
    printf("{\"workload\":\"rules-bulk-stage\",\"edges\":%u,\"staging_ns\":%llu}\n", n, (unsigned long long)staging_ns);
    assert(count(&old, 2) == 0 && mica_kernel_end(&tx) && mica_kernel_end(&old));
    for (unsigned deletion = 0; deletion < 2; ++deletion) {
        for (unsigned sample = 0; sample < samples; ++sample) {
            assert(mica_memory_safepoint(&f.worker, true));
            begin(&f, &tx);
            measurement_limits(&tx);
            int64_t a = deletion ? n / 2 : n, b = a + 1;
            assert(write_pair(&tx, 1, a, b, !deletion) == OK);
            before = f.heap.f_allocated;
            profile_begin(&f);
            start = nanos();
            assert(mica_kernel_commit(&tx) == OK);
            elapsed = nanos() - start;
            profile = profile_end();
            struct mica_KernelWork *work = mica_kernel_work(&tx);
            struct mica_KernelRuleEvaluation *evaluation = work->f_candidate->f_evaluation;
            struct mica_KernelRuleRows *other = mica_kernel_rule_rows_index_lookup(evaluation->f_rows, 4);
            struct mica_KernelRuleRows *prior = mica_kernel_rule_rows_index_lookup(work->f_base->f_evaluation->f_rows, 4);
            assert(other->f_all == prior->f_all && other->f_derived == prior->f_derived);
            measurement_report(deletion ? "delete" : "add", n, sample, elapsed, &profile, evaluation, before, f.heap.f_allocated);
            assert(mica_kernel_end(&tx));
            begin(&f, &tx);
            measurement_limits(&tx);
            measurement_reference(&tx, 4);
            assert(write_pair(&tx, 1, a, b, deletion) == OK);
            assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        }
    }
    destroy(&f);
}
struct measurement_cancellation { struct mica_KernelTransaction *tx; unsigned allocations; };
static bool measurement_cancel(uint8_t *context, uint64_t bytes, uint64_t steps) {
    (void)steps;
    struct measurement_cancellation *request = (void *)context;
    if (bytes && request->allocations && !--request->allocations)
        assert(mica_kernel_cancel(request->tx));
    return true;
}
// Keep a historical closure while discarding cancelled and superseded candidates.
// Root release and collection are measured separately; release does not free memory.
static void measurement_retention(void) {
    struct fixture f;
    init(&f);
    assert(mica_memory_set_capacity(&f.heap, 64 * 1024 * 1024));
    assert(mica_kernel_set_retention_limit(&f.kernel, &f.worker, 3) == OK);
    struct mica_KernelTransaction tx, old, winner, refused = {0};
    begin(&f, &tx);
    for (unsigned id = 1; id <= 3; ++id)
        assert(mica_kernel_declare(&tx, id, id, 2, SET, 0, 0) == OK);
    measurement_closure(&tx, 1, 2);
    for (unsigned i = 0; i < 32; ++i) assert(write_pair(&tx, 1, i, i + 1, true) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    begin(&f, &old);
    assert(count(&old, 2) == 528);
    for (unsigned mode = 0; mode < 3; ++mode) {
        assert(mica_memory_safepoint(&f.worker, true));
        uint64_t baseline = f.heap.f_retained;
        begin(&f, &tx);
        measurement_limits(&tx);
        assert(write_pair(&tx, 1, 32, 33, true) == OK);
        if (mode == 2) {
            struct measurement_cancellation request = {.tx = &tx, .allocations = 20};
            struct mica_MemoryControl control = {.f_context = (void *)&request, .f_check = measurement_cancel};
            mica_memory_control_push(&f.worker, &control);
            assert(mica_kernel_commit(&tx) == CANCELLED && !request.allocations);
            assert(f.worker.f_control == &control && !mica_kernel_work(&tx)->f_candidate);
            mica_memory_control_pop(&f.worker);
        } else {
            struct mica_KernelBudget budget = operation_budget(&tx);
            assert(mica_kernel_prepare(&tx, &budget) == OK);
            struct mica_KernelWork *work = mica_kernel_work(&tx);
            work->f_candidate = (void *)mica_memory_share(&f.worker, (void *)work->f_candidate);
            assert(work->f_candidate);
            if (mode == 1) {
                begin(&f, &winner);
                assert(mica_kernel_begin(&f.kernel, &f.worker, &refused, integer(7)) == LIMIT);
                assert(write_pair(&winner, 3, 0, 1, true) == OK);
                assert(mica_kernel_commit(&winner) == OK && mica_kernel_end(&winner));
            } else {
                assert(mica_kernel_cancel(&tx));
            }
            assert(!mica_kernel_try_publish(&tx));
        }
        profile_begin(&f);
        uint64_t start = nanos();
        assert(mica_memory_safepoint(&f.worker, true));
        uint64_t held_pause = nanos() - start;
        uint64_t held = f.heap.f_retained;
        assert(mica_kernel_end(&tx));
        assert(f.heap.f_retained == held);
        start = nanos();
        assert(mica_memory_safepoint(&f.worker, true));
        uint64_t released_pause = nanos() - start;
        struct rule_profile profile = profile_end();
        assert(f.heap.f_retained < held);
        assert(count(&old, 2) == 528);
        begin(&f, &tx);
        assert(count(&tx, 2) == 528 && mica_kernel_end(&tx));
        printf("{\"workload\":\"rules-retention\",\"candidate\":\"%s\",\"baseline_bytes\":%llu,"
            "\"held_bytes\":%llu,\"released_bytes\":%llu,\"held_safepoint_ns\":%llu,"
            "\"released_safepoint_ns\":%llu,\"collector_ns\":%llu,\"retention_limit\":3,\"capacity_bytes\":67108864}\n",
            (const char *[]){"cancelled", "superseded", "cancelled-during-preparation"}[mode], (unsigned long long)baseline, (unsigned long long)held,
            (unsigned long long)f.heap.f_retained, (unsigned long long)held_pause,
            (unsigned long long)released_pause, (unsigned long long)profile.ns[PROFILE_GC]);
    }
    begin(&f, &tx);
    assert(write_pair(&tx, 1, 16, 17, false) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t history_bytes = f.heap.f_retained;
    assert(count(&old, 2) == 528 && mica_kernel_end(&old));
    assert(f.heap.f_retained == history_bytes);
    uint64_t start = nanos();
    assert(mica_memory_safepoint(&f.worker, true));
    uint64_t pause = nanos() - start;
    assert(f.heap.f_retained < history_bytes);
    printf("{\"workload\":\"rules-history\",\"held_bytes\":%llu,\"released_bytes\":%llu,\"safepoint_ns\":%llu}\n",
        (unsigned long long)history_bytes, (unsigned long long)f.heap.f_retained, (unsigned long long)pause);
    begin(&f, &tx);
    assert(count(&tx, 2) == 256 && mica_kernel_end(&tx));
    destroy(&f);
}
static int rule_measurements(unsigned samples) {
    assert(samples > 0 && samples <= 100);
    for (unsigned n = 32; n <= 128; n *= 2) measurement_updates(n, samples);
    measurement_retention();
    for (unsigned sample = 0; sample < samples; ++sample) {
        uint64_t start = nanos();
        uint64_t conflicts = rule_concurrency();
        printf("{\"workload\":\"rules-contention\",\"threads\":4,\"commits\":80,\"sample\":%u,\"elapsed_ns\":%llu,\"conflicts\":%llu}\n",
            sample, (unsigned long long)(nanos() - start), (unsigned long long)conflicts);
    }
    return 0;
}
