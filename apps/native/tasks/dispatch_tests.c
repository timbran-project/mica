// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static struct mica_KernelBudget dispatch_budget(struct mica_KernelTransaction *tx) {
    return (struct mica_KernelBudget){.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
        .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX, .f_signal = tx->f_signal};
}

static void delegation(struct mica_KernelTransaction *tx, int64_t from, int64_t to,
    int64_t rank, bool present) {
    mica_type_Value cells[] = {integer(from), integer(to), integer(rank)};
    assert(mica_kernel_write(tx, 701, mailbox_receivers(tx->f_worker, cells, 3), present) == OK);
}

static void dispatch_reaches(struct mica_KernelTransaction *tx, int64_t from, int64_t to, bool expected) {
    struct mica_KernelBudget budget = dispatch_budget(tx);
    struct mica_KernelRelationResult view = mica_kernel_read(tx, 701, &budget);
    assert(view.f_status == OK);
    struct mica_TaskDispatchMatch match = mica_task_dispatch_reaches(tx->f_worker,
        view.f_relation, integer(from), integer(to), &budget);
    assert(match.f_status == OK && match.f_matched == expected);
}

static void dispatch_prototypes(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction setup = {0}, reader = {0}, draft = {0};
    struct mica_MemoryRoot retained = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_kernel_begin(&kernel, &worker, &setup, integer(7)) == OK);
    assert(mica_kernel_declare(&setup, 701, 701, 3, 0, 0, 1) == OK);
    delegation(&setup, 1, 2, 0, true);
    delegation(&setup, 1, 2, 20, true);
    delegation(&setup, 2, 3, 0, true);
    delegation(&setup, 3, 1, 0, true);
    delegation(&setup, 1, 4, 99, true);
    delegation(&setup, 4, 3, 0, true);
    delegation(&setup, 4, 5, 999, true);
    for (int64_t i = 8; i < 200; ++i) delegation(&setup, i, i + 1, 0, true);
    assert(mica_kernel_commit(&setup) == OK && mica_kernel_end(&setup));
    assert(mica_kernel_begin(&kernel, &worker, &reader, integer(7)) == OK);
    assert(mica_kernel_begin(&kernel, &worker, &draft, integer(7)) == OK);
    dispatch_reaches(&reader, 1, 5, true);
    dispatch_reaches(&reader, 3, 2, true);
    dispatch_reaches(&reader, 1, 6, false);
    dispatch_reaches(&reader, 7, 7, true);
    dispatch_reaches(&reader, 8, 200, true);
    dispatch_reaches(&reader, 200, 8, false);

    // Draft writes affect the next traversal. A retained view and another
    // transaction keep their previous edge sets through collection.
    delegation(&draft, 5, 6, 0, true);
    dispatch_reaches(&draft, 1, 6, true);
    dispatch_reaches(&reader, 1, 6, false);
    struct mica_KernelBudget budget = dispatch_budget(&draft);
    struct mica_KernelRelationResult view = mica_kernel_read(&draft, 701, &budget);
    assert(view.f_status == OK && mica_memory_root_push(&worker, &retained));
    retained.f_pointer = (void *)view.f_relation;
    delegation(&draft, 5, 6, 0, false);
    assert(mica_memory_safepoint(&worker, true));
    dispatch_reaches(&draft, 1, 6, false);
    budget = dispatch_budget(&draft);
    struct mica_TaskDispatchMatch match = mica_task_dispatch_reaches(&worker,
        (void *)retained.f_pointer, integer(1), integer(6), &budget);
    assert(match.f_status == OK && match.f_matched);

    // Failed traversal neither edits the relation nor leaves scratch roots.
    budget = dispatch_budget(&draft);
    budget.f_steps = 0;
    match = mica_task_dispatch_reaches(&worker, (void *)retained.f_pointer, integer(1), integer(6), &budget);
    assert(match.f_status == LIMIT && !match.f_matched);
    budget = dispatch_budget(&draft);
    budget.f_bytes = 0;
    match = mica_task_dispatch_reaches(&worker, (void *)retained.f_pointer, integer(1), integer(6), &budget);
    assert(match.f_status == LIMIT && !match.f_matched);
    budget = dispatch_budget(&draft);
    atomic_store(&allocation_budget, 0);
    match = mica_task_dispatch_reaches(&worker, (void *)retained.f_pointer, integer(8), integer(200), &budget);
    atomic_store(&allocation_budget, -1);
    assert(match.f_status == OOM && !match.f_matched);
    assert(mica_memory_safepoint(&worker, true));
    dispatch_reaches(&draft, 8, 200, true);
    assert(mica_memory_root_pop(&worker, &retained));
    delegation(&draft, 5, 6, 0, true);
    assert(mica_kernel_commit(&draft) == OK && mica_kernel_end(&draft));
    dispatch_reaches(&reader, 1, 6, false);
    assert(mica_kernel_end(&reader));
    assert(mica_kernel_begin(&kernel, &worker, &reader, integer(7)) == OK);
    dispatch_reaches(&reader, 1, 6, true);
    budget = dispatch_budget(&reader);
    view = mica_kernel_read(&reader, 701, &budget);
    assert(view.f_status == OK);
    assert(mica_kernel_cancel(&reader));
    match = mica_task_dispatch_reaches(&worker, view.f_relation, integer(1), integer(6), &budget);
    assert(match.f_status == CANCELLED && !match.f_matched);
    assert(mica_kernel_end(&reader));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static mica_type_Value dispatch_parameter(struct mica_MemoryWorker *worker,
    struct mica_TaskDispatchVocabulary *vocabulary, mica_type_Value role,
    int64_t position, uint32_t mode, mica_type_Value fallback) {
    mica_type_Value cells[] = {integer(700), role,
        mica_value_symbol(vocabulary->f_unrestricted).f_value, integer(position),
        mica_value_symbol(mode).f_value, fallback};
    return mailbox_receivers(worker, cells, 6);
}

static struct mica_TaskDispatchBinding dispatch_bind_parameters(struct mica_MemoryWorker *worker,
    struct mica_TaskDispatchVocabulary *vocabulary, mica_type_Value parameters,
    mica_type_Value roles, struct mica_KernelBudget *budget) {
    struct mica_TaskDispatchSignatureResult signature = mica_task_dispatch_signature(
        worker, vocabulary, integer(700), parameters, budget);
    if (signature.f_status != OK)
        return (struct mica_TaskDispatchBinding){.f_status = signature.f_status};
    return mica_task_dispatch_bind_named(worker, vocabulary, signature.f_signature, roles, budget);
}

static void dispatch_named_bindings(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0}, repeated = {0};
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == CLOSED);
    assert(mica_value_symbol_table_init(&symbols));
    mica_type_Value first = subscription_symbol(&symbols, "first");
    mica_type_Value extra = subscription_symbol(&symbols, "extra");
    mica_type_Value tail = subscription_symbol(&symbols, "tail");
    mica_type_Value ignored = subscription_symbol(&symbols, "ignored");
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_dispatch_vocabulary_init(&symbols, &repeated) == OK);
    assert(vocabulary.f_ready && repeated.f_required == vocabulary.f_required);
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot parameters = {0}, roles = {0}, output = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &parameters));
    assert(mica_memory_root_push(&worker, &roles));
    assert(mica_memory_root_push(&worker, &output));
    uint8_t bytes[128];
    memset(bytes, 'd', sizeof(bytes));
    struct mica_ValueResult fallback = mica_value_string(&worker, bytes, sizeof(bytes));
    assert(fallback.f_ok);
    mica_type_Value rows[] = {
        dispatch_parameter(&worker, &vocabulary, first, 0, vocabulary.f_required, integer(0)),
        dispatch_parameter(&worker, &vocabulary, extra, 1, vocabulary.f_optional, fallback.f_value),
        dispatch_parameter(&worker, &vocabulary, tail, 2, vocabulary.f_rest, integer(0))};
    mica_value_root_set(&parameters, mailbox_receivers(&worker, rows, 3));
    for (unsigned test = 0; test < 4; ++test) {
        struct mica_ValueMapEntry entries[] = {{first, integer(11)}, {extra, integer(12)}, {tail, integer(99)}, {ignored, integer(42)}};
        uint64_t count = test == 0 ? 0 : test == 1 ? 1 : test == 2 ? 4 : 3;
        if (test == 2) {
            mica_type_Value rest[] = {integer(21), integer(22)};
            entries[2].f_value = mailbox_receivers(&worker, rest, 2);
        }
        struct mica_ValueResult map = mica_value_map(&worker, entries, count);
        assert(map.f_ok);
        mica_value_root_set(&roles, map.f_value);
        assert(mica_memory_safepoint(&worker, true));
        struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
            .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
        struct mica_TaskDispatchBinding bound = dispatch_bind_parameters(&worker,
            &vocabulary, mica_value_root_get(&parameters), mica_value_root_get(&roles), &budget);
        assert(bound.f_status == OK && bound.f_applicable == (test == 1 || test == 2));
        if (!bound.f_applicable) continue;
        mica_value_root_set(&output, bound.f_arguments);
        assert(mica_memory_safepoint(&worker, true));
        const struct mica_HeapList *args = mailbox_list(mica_value_root_get(&output), 3);
        assert(args->f_data[0] == integer(11));
        if (test == 1) {
            assert(mica_value_string_length(args->f_data[1]).f_number == sizeof(bytes));
            mailbox_list(args->f_data[2], 0);
        } else {
            assert(args->f_data[1] == integer(12));
            const struct mica_HeapList *rest = mailbox_list(args->f_data[2], 2);
            assert(rest->f_data[0] == integer(21) && rest->f_data[1] == integer(22));
        }
    }
    // Catalogue positions and required/optional/rest ordering are validated
    // independently of the supplied named values.
    for (unsigned test = 0; test < 3; ++test) {
        rows[0] = dispatch_parameter(&worker, &vocabulary, first, test == 0 ? 1 : 0,
            test == 1 ? vocabulary.f_optional : vocabulary.f_rest, integer(1));
        rows[1] = dispatch_parameter(&worker, &vocabulary, extra, 1, vocabulary.f_required, integer(0));
        mica_value_root_set(&parameters, mailbox_receivers(&worker, rows, 2));
        struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
            .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
        struct mica_TaskDispatchBinding bound = dispatch_bind_parameters(&worker,
            &vocabulary, mica_value_root_get(&parameters), mica_value_root_get(&roles), &budget);
        assert(bound.f_status == SCHEMA && !bound.f_applicable);
    }
    assert(mica_memory_root_pop(&worker, &output));
    assert(mica_memory_root_pop(&worker, &roles));
    assert(mica_memory_root_pop(&worker, &parameters));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}

static mica_type_Value dispatch_identity(uint64_t number) {
    struct mica_ValueResult value = mica_value_identity(number);
    assert(value.f_ok);
    return value.f_value;
}

static void dispatch_identity_edge(struct mica_KernelTransaction *tx, uint64_t from, uint64_t to) {
    mica_type_Value cells[] = {dispatch_identity(from), dispatch_identity(to), integer(99)};
    assert(mica_kernel_write(tx, 701, mailbox_receivers(tx->f_worker, cells, 3), true) == OK);
}

static void dispatch_matching(struct mica_KernelTransaction *tx,
    struct mica_TaskDispatchVocabulary *vocabulary, struct mica_MemoryRoot *view,
    struct mica_MemoryRoot *prototypes, struct mica_MemoryRoot *cache,
    mica_type_Value value, mica_type_Value restriction, bool expected, bool implication) {
    struct mica_KernelBudget budget = dispatch_budget(tx);
    struct mica_TaskDispatchMatch result;
    if (implication) result = mica_task_dispatch_implies(tx->f_worker, vocabulary,
        (void *)view->f_pointer, mica_value_root_get(prototypes), cache, value, restriction, &budget);
    else result = mica_task_dispatch_matches(tx->f_worker, vocabulary,
        (void *)view->f_pointer, mica_value_root_get(prototypes), cache, value, restriction, &budget);
    assert(result.f_status == OK && result.f_matched == expected);
    assert(mica_memory_safepoint(tx->f_worker, true));
}

static void dispatch_restrictions(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0};
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction tx = {0};
    struct mica_MemoryRoot prototypes = {0}, view = {0}, cache = {0}, actual = {0}, restriction = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_memory_root_push(&worker, &prototypes));
    assert(mica_memory_root_push(&worker, &view));
    assert(mica_memory_root_push(&worker, &cache));
    assert(mica_memory_root_push(&worker, &actual));
    assert(mica_memory_root_push(&worker, &restriction));
    mica_type_Value kinds[16];
    for (unsigned tag = 1; tag <= 16; ++tag) kinds[tag - 1] = dispatch_identity(9000 + tag);
    mica_value_root_set(&prototypes, mailbox_receivers(&worker, kinds, 16));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 701, 701, 3, 0, 0, 1) == OK);
    dispatch_identity_edge(&tx, 100, 200);
    dispatch_identity_edge(&tx, 200, 300);
    dispatch_identity_edge(&tx, 300, 200);
    dispatch_identity_edge(&tx, 9007, 400); // This world's string prototype.
    dispatch_identity_edge(&tx, 9014, 500); // Frob prototype.
    dispatch_identity_edge(&tx, 9004, 600); // Identity prototype.
    for (uint64_t i = 1000; i < 1200; ++i) dispatch_identity_edge(&tx, i, i + 1);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    struct mica_KernelBudget budget = dispatch_budget(&tx);
    struct mica_KernelRelationResult read = mica_kernel_read(&tx, 701, &budget);
    assert(read.f_status == OK);
    view.f_pointer = (void *)read.f_relation;
    mica_type_Value any = mica_value_symbol(vocabulary.f_unrestricted).f_value;
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, integer(99), any, true, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, dispatch_identity(100), dispatch_identity(300), true, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, dispatch_identity(100), dispatch_identity(600), true, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, dispatch_identity(200), dispatch_identity(300), true, true);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, dispatch_identity(300), dispatch_identity(200), true, true);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, any, dispatch_identity(200), false, true);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, dispatch_identity(200), any, true, true);
    uint8_t bytes[128];
    memset(bytes, 's', sizeof(bytes));
    struct mica_ValueResult value = mica_value_string(&worker, bytes, sizeof(bytes));
    assert(value.f_ok);
    mica_value_root_set(&actual, value.f_value);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, mica_value_root_get(&actual), dispatch_identity(400), true, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, mica_value_root_get(&actual), dispatch_identity(600), false, false);
    value = mica_value_frob(&worker, 100, mica_value_root_get(&actual));
    assert(value.f_ok);
    mica_value_root_set(&actual, value.f_value);
    value = mica_value_frob(&worker, 200, mica_value_symbol(vocabulary.f_frob_only).f_value);
    assert(value.f_ok);
    mica_value_root_set(&restriction, value.f_value);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, mica_value_root_get(&actual), mica_value_root_get(&restriction), true, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, dispatch_identity(100), mica_value_root_get(&restriction), false, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, mica_value_root_get(&actual), dispatch_identity(500), true, false);
    dispatch_matching(&tx, &vocabulary, &view, &prototypes, &cache, mica_value_root_get(&actual), dispatch_identity(600), false, false);

    // A limited, incomplete traversal cannot install a partial cache entry.
    cache.f_pointer = NULL;
    budget = dispatch_budget(&tx);
    budget.f_steps = 64;
    struct mica_TaskDispatchMatch match = mica_task_dispatch_cached_reaches(&worker,
        (void *)view.f_pointer, &cache, dispatch_identity(1000), dispatch_identity(1200), &budget);
    assert(match.f_status == LIMIT && !match.f_matched && !cache.f_pointer);
    budget = dispatch_budget(&tx);
    match = mica_task_dispatch_cached_reaches(&worker, (void *)view.f_pointer, &cache,
        dispatch_identity(1000), dispatch_identity(1200), &budget);
    assert(match.f_status == OK && match.f_matched);
    assert(mica_memory_safepoint(&worker, true));
    assert(cache.f_pointer && !((struct mica_TaskDispatchReach *)cache.f_pointer)->f_next);
    budget = dispatch_budget(&tx);
    budget.f_steps = 64;
    match = mica_task_dispatch_cached_reaches(&worker, (void *)view.f_pointer, &cache,
        dispatch_identity(1000), dispatch_identity(1200), &budget);
    assert(match.f_status == OK && match.f_matched);
    // Retaining a cache across a draft write must not reuse the previous view.
    dispatch_identity_edge(&tx, 1200, 1300);
    budget = dispatch_budget(&tx);
    read = mica_kernel_read(&tx, 701, &budget);
    assert(read.f_status == OK);
    match = mica_task_dispatch_cached_reaches(&worker, read.f_relation, &cache,
        dispatch_identity(1000), dispatch_identity(1300), &budget);
    assert(match.f_status == OK && match.f_matched);
    assert(mica_memory_safepoint(&worker, true));
    budget = dispatch_budget(&tx);
    match = mica_task_dispatch_cached_reaches(&worker, (void *)view.f_pointer, &cache,
        dispatch_identity(1000), dispatch_identity(1300), &budget);
    assert(match.f_status == OK && !match.f_matched);
    assert(mica_kernel_cancel(&tx));
    budget = dispatch_budget(&tx);
    match = mica_task_dispatch_cached_reaches(&worker, (void *)view.f_pointer, &cache,
        dispatch_identity(1000), dispatch_identity(1200), &budget);
    assert(match.f_status == CANCELLED && !match.f_matched);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(&worker, &restriction));
    assert(mica_memory_root_pop(&worker, &actual));
    assert(mica_memory_root_pop(&worker, &cache));
    assert(mica_memory_root_pop(&worker, &view));
    assert(mica_memory_root_pop(&worker, &prototypes));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}

static void dispatch_compare_signatures(struct mica_KernelTransaction *tx,
    struct mica_TaskDispatchVocabulary *vocabulary, struct mica_MemoryRoot *view,
    struct mica_MemoryRoot *prototypes, struct mica_MemoryRoot *cache,
    mica_type_Value left_parameters, mica_type_Value right_parameters,
    bool left_stricter, bool right_stricter) {
    struct mica_MemoryWorker *worker = tx->f_worker;
    struct mica_MemoryRoot left_rows = {0}, right_rows = {0}, left = {0}, right = {0};
    assert(mica_memory_root_push(worker, &left_rows));
    assert(mica_memory_root_push(worker, &right_rows));
    assert(mica_memory_root_push(worker, &left));
    assert(mica_memory_root_push(worker, &right));
    mica_value_root_set(&left_rows, left_parameters);
    mica_value_root_set(&right_rows, right_parameters);
    struct mica_KernelBudget budget = dispatch_budget(tx);
    struct mica_TaskDispatchSignatureResult built = mica_task_dispatch_signature(worker,
        vocabulary, integer(700), mica_value_root_get(&left_rows), &budget);
    assert(built.f_status == OK && built.f_signature);
    left.f_pointer = (void *)built.f_signature;
    assert(mica_memory_safepoint(worker, true));
    built = mica_task_dispatch_signature(worker, vocabulary, integer(700),
        mica_value_root_get(&right_rows), &budget);
    assert(built.f_status == OK && built.f_signature);
    right.f_pointer = (void *)built.f_signature;
    assert(mica_memory_safepoint(worker, true));
    for (unsigned reverse = 0; reverse < 2; ++reverse) {
        budget = dispatch_budget(tx);
        struct mica_TaskDispatchMatch match = mica_task_dispatch_more_specific(worker, vocabulary,
            (void *)view->f_pointer, mica_value_root_get(prototypes), cache,
            (void *)(reverse ? right.f_pointer : left.f_pointer),
            (void *)(reverse ? left.f_pointer : right.f_pointer), &budget);
        assert(match.f_status == OK && match.f_matched == (reverse ? right_stricter : left_stricter));
        assert(mica_memory_safepoint(worker, true));
    }
    budget = dispatch_budget(tx);
    budget.f_steps = 0;
    struct mica_TaskDispatchMatch limited = mica_task_dispatch_more_specific(worker, vocabulary,
        (void *)view->f_pointer, mica_value_root_get(prototypes), cache,
        (void *)left.f_pointer, (void *)right.f_pointer, &budget);
    assert(limited.f_status == LIMIT && !limited.f_matched);
    assert(mica_memory_root_pop(worker, &right));
    assert(mica_memory_root_pop(worker, &left));
    assert(mica_memory_root_pop(worker, &right_rows));
    assert(mica_memory_root_pop(worker, &left_rows));
}

static mica_type_Value dispatch_restricted_parameter(struct mica_MemoryWorker *worker,
    mica_type_Value role, int64_t position, uint32_t mode, mica_type_Value restriction) {
    mica_type_Value cells[] = {integer(700), role, restriction, integer(position),
        mica_value_symbol(mode).f_value, integer(0)};
    return mailbox_receivers(worker, cells, 6);
}

static void dispatch_signatures(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == OK);
    mica_type_Value tail = subscription_symbol(&symbols, "tail");
    mica_type_Value extra = subscription_symbol(&symbols, "extra");
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction tx = {0};
    struct mica_MemoryRoot prototypes = {0}, view = {0}, cache = {0}, rows = {0}, roles = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_memory_root_push(&worker, &prototypes));
    assert(mica_memory_root_push(&worker, &view));
    assert(mica_memory_root_push(&worker, &cache));
    assert(mica_memory_root_push(&worker, &rows));
    assert(mica_memory_root_push(&worker, &roles));
    mica_type_Value kinds[16];
    for (unsigned tag = 1; tag <= 16; ++tag) kinds[tag - 1] = dispatch_identity(9000 + tag);
    mica_value_root_set(&prototypes, mailbox_receivers(&worker, kinds, 16));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 701, 701, 3, 0, 0, 1) == OK);
    dispatch_identity_edge(&tx, 100, 200);
    dispatch_identity_edge(&tx, 200, 300);
    dispatch_identity_edge(&tx, 300, 200);
    struct mica_KernelBudget budget = dispatch_budget(&tx);
    struct mica_KernelRelationResult relation = mica_kernel_read(&tx, 701, &budget);
    assert(relation.f_status == OK);
    view.f_pointer = (void *)relation.f_relation;
    mica_type_Value any = mica_value_symbol(vocabulary.f_unrestricted).f_value;

    // Narrowing is independent of declaration/scan order and method identity.
    // Equivalent cyclic restrictions establish no strict ordering.
    for (unsigned scenario = 0; scenario < 7; ++scenario) {
        uint32_t left_mode = scenario == 0 ? vocabulary.f_rest : vocabulary.f_required;
        uint32_t right_mode = scenario < 2 ? vocabulary.f_optional : vocabulary.f_required;
        mica_type_Value left_restriction = scenario == 2 ? dispatch_identity(100) :
            scenario == 3 ? dispatch_identity(200) : any;
        mica_type_Value right_restriction = scenario == 2 ? dispatch_identity(200) :
            scenario == 3 ? dispatch_identity(300) : any;
        mica_type_Value left_cells[] = {
            dispatch_restricted_parameter(&worker, tail, 0, left_mode, left_restriction),
            dispatch_restricted_parameter(&worker, extra, 1,
                scenario == 5 ? vocabulary.f_optional : vocabulary.f_required, any)};
        mica_type_Value right_cells[] = {dispatch_restricted_parameter(&worker, tail, 0,
            right_mode, right_restriction)};
        if (scenario == 6) {
            left_cells[0] = dispatch_restricted_parameter(&worker, tail, 1, vocabulary.f_required, any);
            left_cells[1] = dispatch_restricted_parameter(&worker, extra, 0, vocabulary.f_required, any);
        }
        mica_type_Value left_parameters = mailbox_receivers(&worker, left_cells, scenario >= 4 ? 2 : 1);
        mica_type_Value right_parameters = mailbox_receivers(&worker, right_cells, 1);
        dispatch_compare_signatures(&tx, &vocabulary, &view, &prototypes, &cache,
            left_parameters, right_parameters, scenario != 3 && scenario != 5, false);
    }

    // The agreed example: lists and omission bind both branches and rest wins;
    // a scalar binds only optional. Test binding and comparison separately.
    for (unsigned call = 0; call < 3; ++call) {
        mica_type_Value list_cells[] = {integer(1), integer(2)};
        struct mica_ValueMapEntry supplied = {tail, call == 1 ? integer(42) : mailbox_receivers(&worker, list_cells, 2)};
        struct mica_ValueResult map = mica_value_map(&worker, &supplied, call == 2 ? 0 : 1);
        assert(map.f_ok);
        mica_value_root_set(&roles, map.f_value);
        for (unsigned optional = 0; optional < 2; ++optional) {
            mica_type_Value fallback = mailbox_receivers(&worker, NULL, 0);
            mica_type_Value row = dispatch_parameter(&worker, &vocabulary, tail, 0,
                optional ? vocabulary.f_optional : vocabulary.f_rest, fallback);
            mica_value_root_set(&rows, mailbox_receivers(&worker, &row, 1));
            budget = dispatch_budget(&tx);
            struct mica_TaskDispatchBinding bound = dispatch_bind_parameters(&worker, &vocabulary,
                mica_value_root_get(&rows), mica_value_root_get(&roles), &budget);
            assert(bound.f_status == OK && bound.f_applicable == (optional || call != 1));
        }
    }
    // Arbitrary scan order is normalized before arguments are bound.
    mica_type_Value shuffled[] = {
        dispatch_parameter(&worker, &vocabulary, tail, 1, vocabulary.f_required, integer(0)),
        dispatch_parameter(&worker, &vocabulary, extra, 0, vocabulary.f_required, integer(0))};
    mica_value_root_set(&rows, mailbox_receivers(&worker, shuffled, 2));
    struct mica_ValueMapEntry entries[] = {{tail, integer(12)}, {extra, integer(34)}};
    struct mica_ValueResult map = mica_value_map(&worker, entries, 2);
    assert(map.f_ok);
    mica_value_root_set(&roles, map.f_value);
    budget = dispatch_budget(&tx);
    struct mica_TaskDispatchBinding ordered = dispatch_bind_parameters(&worker, &vocabulary,
        mica_value_root_get(&rows), mica_value_root_get(&roles), &budget);
    assert(ordered.f_status == OK && ordered.f_applicable);
    const struct mica_HeapList *arguments = mailbox_list(ordered.f_arguments, 2);
    assert(arguments->f_data[0] == integer(34) && arguments->f_data[1] == integer(12));
    mica_value_root_set(&rows, mailbox_receivers(&worker, NULL, 0));
    budget = dispatch_budget(&tx);
    ordered = dispatch_bind_parameters(&worker, &vocabulary,
        mica_value_root_get(&rows), mica_value_root_get(&roles), &budget);
    assert(ordered.f_status == OK && ordered.f_applicable);
    mailbox_list(ordered.f_arguments, 0);
    // The ordered array exceeds both nursery and pooled object sizes, so
    // failure injection reaches a dedicated platform allocation.
    mica_type_Value oversized[8192];
    for (unsigned i = 0; i < 8192; ++i) oversized[i] = integer(0);
    mica_value_root_set(&rows, mailbox_receivers(&worker, oversized, 8192));
    budget = dispatch_budget(&tx);
    atomic_store(&allocation_budget, 0);
    struct mica_TaskDispatchSignatureResult exhausted = mica_task_dispatch_signature(&worker,
        &vocabulary, integer(700), mica_value_root_get(&rows), &budget);
    atomic_store(&allocation_budget, -1);
    assert(exhausted.f_status == OOM && !exhausted.f_signature);
    // Reject duplicate roles, negative/out-of-range/duplicate positions,
    // foreign method rows, malformed rows, and an unknown mode atomically.
    for (unsigned invalid = 0; invalid < 7; ++invalid) {
        mica_type_Value first = dispatch_parameter(&worker, &vocabulary, tail,
            invalid == 1 ? -1 : invalid == 2 ? 2 : 0, vocabulary.f_required, integer(0));
        mica_type_Value cells[] = {integer(invalid == 4 ? 701 : 700), invalid == 0 ? tail : extra,
            any, integer(invalid == 3 ? 0 : 1),
            invalid == 6 ? extra : mica_value_symbol(vocabulary.f_optional).f_value, integer(0)};
        mica_type_Value second = invalid == 5 ? integer(4) : mailbox_receivers(&worker, cells, 6);
        mica_type_Value input[] = {second, first};
        mica_value_root_set(&rows, mailbox_receivers(&worker, input, 2));
        budget = dispatch_budget(&tx);
        struct mica_TaskDispatchSignatureResult result = mica_task_dispatch_signature(&worker,
            &vocabulary, integer(700), mica_value_root_get(&rows), &budget);
        assert(result.f_status == SCHEMA && !result.f_signature);
        assert(mica_memory_safepoint(&worker, true));
    }
    budget = dispatch_budget(&tx);
    budget.f_bytes = 0;
    struct mica_TaskDispatchSignatureResult failed = mica_task_dispatch_signature(&worker,
        &vocabulary, integer(700), mica_value_root_get(&rows), &budget);
    assert(failed.f_status == LIMIT && !failed.f_signature);
    assert(mica_kernel_cancel(&tx));
    budget = dispatch_budget(&tx);
    failed = mica_task_dispatch_signature(&worker, &vocabulary, integer(700),
        mica_value_root_get(&rows), &budget);
    assert(failed.f_status == CANCELLED && !failed.f_signature);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(&worker, &roles));
    assert(mica_memory_root_pop(&worker, &rows));
    assert(mica_memory_root_pop(&worker, &cache));
    assert(mica_memory_root_pop(&worker, &view));
    assert(mica_memory_root_pop(&worker, &prototypes));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}

static void dispatch_method(struct mica_KernelTransaction *tx,
    uint64_t id, mica_type_Value selector,
    mica_type_Value role, uint32_t mode, mica_type_Value restriction) {
    mica_type_Value method = dispatch_identity(id);
    mica_type_Value selector_row[] = {method, selector};
    assert(mica_kernel_write(tx, 702, mailbox_receivers(tx->f_worker, selector_row, 2), true) == OK);
    mica_type_Value row[] = {method, role, restriction, integer(0), mica_value_symbol(mode).f_value,
        mailbox_receivers(tx->f_worker, NULL, 0)};
    assert(mica_kernel_write(tx, 703, mailbox_receivers(tx->f_worker, row, 6), true) == OK);
}

static void dispatch_resolution(struct mica_KernelTransaction *tx,
    struct mica_TaskDispatchVocabulary *vocabulary, struct mica_MemoryRoot *prototypes,
    mica_type_Value selector, struct mica_MemoryRoot *roles, uint64_t count, uint64_t method) {
    struct mica_KernelBudget budget = dispatch_budget(tx);
    struct mica_TaskDispatchSelection result = mica_task_dispatch_resolve(tx, vocabulary,
        702, 703, 701, mica_value_root_get(prototypes), selector, mica_value_root_get(roles), &budget);
    assert(result.f_status == OK && result.f_count == count);
    if (count == 1) {
        assert(result.f_method == dispatch_identity(method));
        struct mica_MemoryRoot arguments = {0};
        assert(mica_memory_root_push(tx->f_worker, &arguments));
        mica_value_root_set(&arguments, result.f_arguments);
        assert(mica_memory_safepoint(tx->f_worker, true));
        mailbox_list(mica_value_root_get(&arguments), 1);
        assert(mica_memory_root_pop(tx->f_worker, &arguments));
    } else {
        assert(!result.f_method && !result.f_arguments);
    }
    assert(mica_memory_safepoint(tx->f_worker, true));
}

static void dispatch_selection(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == OK);
    mica_type_Value tail = subscription_symbol(&symbols, "tail");
    mica_type_Value choose = subscription_symbol(&symbols, "choose");
    mica_type_Value restricted = subscription_symbol(&symbols, "restricted");
    mica_type_Value missing = subscription_symbol(&symbols, "missing");
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction tx = {0}, reader = {0};
    struct mica_MemoryRoot prototypes = {0}, roles = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_memory_root_push(&worker, &prototypes));
    assert(mica_memory_root_push(&worker, &roles));
    mica_type_Value kinds[16];
    for (unsigned tag = 1; tag <= 16; ++tag) kinds[tag - 1] = dispatch_identity(9000 + tag);
    mica_value_root_set(&prototypes, mailbox_receivers(&worker, kinds, 16));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 701, 701, 3, 0, 0, 1) == OK);
    assert(mica_kernel_declare(&tx, 702, 702, 2, 0, 0, 2) == OK);
    assert(mica_kernel_declare(&tx, 703, 703, 6, 0, 0, 1) == OK);
    mica_type_Value any = mica_value_symbol(vocabulary.f_unrestricted).f_value;
    dispatch_method(&tx, 100, choose, tail, vocabulary.f_rest, any);
    dispatch_method(&tx, 101, choose, tail, vocabulary.f_optional, any);
    dispatch_method(&tx, 102, restricted, tail, vocabulary.f_optional, dispatch_identity(500));
    for (unsigned call = 0; call < 3; ++call) {
        mica_type_Value cells[] = {integer(1), integer(2)};
        struct mica_ValueMapEntry entry = {tail, call == 1 ? integer(42) : mailbox_receivers(&worker, cells, 2)};
        struct mica_ValueResult map = mica_value_map(&worker, &entry, call == 2 ? 0 : 1);
        assert(map.f_ok);
        mica_value_root_set(&roles, map.f_value);
        dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 1, call == 1 ? 101 : 100);
    }
    dispatch_resolution(&tx, &vocabulary, &prototypes, missing, &roles, 0, 0);
    // Defaults participate in applicability. The empty-list default starts
    // outside #500; a draft delegation of the list prototype makes it match.
    dispatch_resolution(&tx, &vocabulary, &prototypes, restricted, &roles, 0, 0);
    dispatch_identity_edge(&tx, 9009, 500);
    dispatch_resolution(&tx, &vocabulary, &prototypes, restricted, &roles, 1, 102);
    dispatch_method(&tx, 103, choose, tail, vocabulary.f_rest, any);
    dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 2, 0);
    mica_type_Value remove[] = {dispatch_identity(103), choose};
    assert(mica_kernel_write(&tx, 702, mailbox_receivers(&worker, remove, 2), false) == OK);
    dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 1, 100);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));

    // A snapshot retains its selection while a concurrent catalogue changes.
    // Committing that reader detects its stale catalogue observation.
    assert(mica_kernel_begin(&kernel, &worker, &reader, integer(7)) == OK);
    dispatch_resolution(&reader, &vocabulary, &prototypes, choose, &roles, 1, 100);
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    remove[0] = dispatch_identity(100);
    assert(mica_kernel_write(&tx, 702, mailbox_receivers(&worker, remove, 2), false) == OK);
    dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 1, 101);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    dispatch_resolution(&reader, &vocabulary, &prototypes, choose, &roles, 1, 100);
    assert(mica_kernel_commit(&reader) == CONFLICT && mica_kernel_end(&reader));
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 1, 101);
    assert(mica_kernel_write(&tx, 702, mailbox_receivers(&worker, remove, 2), true) == OK);
    dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 1, 100);
    assert(mica_kernel_end(&tx)); // Roll back the restoration.
    assert(mica_kernel_begin(&kernel, &worker, &tx, integer(7)) == OK);
    dispatch_resolution(&tx, &vocabulary, &prototypes, choose, &roles, 1, 101);
    struct mica_KernelBudget budget = dispatch_budget(&tx);
    budget.f_steps = 0;
    struct mica_TaskDispatchSelection failed = mica_task_dispatch_resolve(&tx, &vocabulary,
        702, 703, 701, mica_value_root_get(&prototypes), choose, mica_value_root_get(&roles), &budget);
    assert(failed.f_status == LIMIT && !failed.f_count && !failed.f_method && !failed.f_arguments);
    assert(mica_kernel_cancel(&tx));
    budget = dispatch_budget(&tx);
    failed = mica_task_dispatch_resolve(&tx, &vocabulary, 702, 703, 701,
        mica_value_root_get(&prototypes), choose, mica_value_root_get(&roles), &budget);
    assert(failed.f_status == CANCELLED && !failed.f_count && !failed.f_method && !failed.f_arguments);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(&worker, &roles));
    assert(mica_memory_root_pop(&worker, &prototypes));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}
