// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static struct mica_KernelBudget subscription_budget(void) {
    return (struct mica_KernelBudget){.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
        .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
}

static void subscription_select_request(struct mica_MemoryWorker *worker,
        struct mica_MemoryRoot *root, uint64_t subject, uint64_t relation,
        mica_type_Value pattern, uint64_t mask) {
    root->f_pointer = (void *)mica_task_new_SubscriptionRequest(worker,
        mica_value_capability(2).f_value, integer(7), mica_value_capability(4).f_value,
        subject, relation, pattern, mask, false, false, 0, 8, false);
    assert(root->f_pointer);
}

static struct mica_TaskSubscriptionRows subscription_select(struct mica_MemoryWorker *worker,
        struct mica_MemoryRoot *before, struct mica_MemoryRoot *after,
        struct mica_MemoryRoot *request, uint64_t added, uint64_t removed) {
    struct mica_KernelBudget budget = subscription_budget();
    struct mica_TaskSubscriptionRows result = mica_task_subscription_rows(worker,
        (void *)before->f_pointer, (void *)after->f_pointer, (void *)request->f_pointer, &budget);
    assert(result.f_status == OK && !worker->f_control);
    (void)mailbox_list(result.f_assertions, added);
    (void)mailbox_list(result.f_retractions, removed);
    return result;
}

static void subscription_rows(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction transaction = {0};
    struct mica_MemoryRoot before = {0}, after = {0}, request = {0}, added = {0}, removed = {0};
    struct mica_MemoryRoot absent = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_memory_root_push(&worker, &before));
    assert(mica_memory_root_push(&worker, &after));
    assert(mica_memory_root_push(&worker, &request));
    assert(mica_memory_root_push(&worker, &added));
    assert(mica_memory_root_push(&worker, &removed));
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 1, 1, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&transaction, 2, 2, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&transaction, 3, 3, 0, 0, 0, 0) == OK);
    struct mica_KernelRuleTerm *terms = mica_kernel_rule_variable(&worker, 1, NULL);
    assert(terms);
    terms = mica_kernel_rule_variable(&worker, 0, terms);
    assert(terms);
    struct mica_KernelRuleAtom *atom = mica_kernel_rule_atom(&worker, 1, false, terms, NULL);
    assert(atom);
    struct mica_ValueResult source = mica_value_string(&worker, (const uint8_t *)"", 0);
    assert(source.f_ok);
    struct mica_KernelRuleDef *definition = mica_kernel_rule_definition(&worker, 2, terms,
        atom, NULL, integer(7), source.f_value);
    assert(definition && mica_kernel_rule_add(&transaction, 1, definition, true) == OK);
    for (int64_t i = 0; i < 1024; ++i)
        assert(mica_kernel_write(&transaction, 1, pair(&worker, i, i % 4), true) == OK);
    assert(mica_kernel_write(&transaction, 2, pair(&worker, 8, 0), true) == OK);
    assert(mica_kernel_write(&transaction, 3, mailbox_receivers(&worker, NULL, 0), true) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    // This fixture owns the only kernel worker; copy roots before any safepoint.
    before.f_pointer = kernel.f_current.f_pointer;
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_write(&transaction, 1, pair(&worker, 512, 0), false) == OK);
    assert(mica_kernel_write(&transaction, 1, pair(&worker, 1024, 0), true) == OK);
    assert(mica_kernel_write(&transaction, 2, pair(&worker, 8, 0), false) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    after.f_pointer = kernel.f_current.f_pointer;
    assert(mica_memory_safepoint(&worker, true));
    subscription_select_request(&worker, &request, 1, 1, pair(&worker, 0, 0), 0);
    struct mica_TaskSubscriptionRows result = subscription_select(&worker, &before, &after, &request, 1, 1);
    // A two-row update must skip shared subtrees, including those moved by rotations.
    assert(result.f_visited < 128);
    mica_value_root_set(&added, result.f_assertions);
    mica_value_root_set(&removed, result.f_retractions);
    assert(mica_memory_safepoint(&worker, true));
    const struct mica_HeapList *rows = mailbox_list(mica_value_root_get(&added), 1);
    assert(mailbox_list(rows->f_data[0], 2)->f_data[0] == integer(1024));
    rows = mailbox_list(mica_value_root_get(&removed), 1);
    assert(mailbox_list(rows->f_data[0], 2)->f_data[0] == integer(512));
    result = subscription_select(&worker, &after, &after, &request, 0, 0);
    assert(result.f_visited == 0);
    (void)subscription_select(&worker, &absent, &after, &request, 1024, 0);
    (void)subscription_select(&worker, &after, &absent, &request, 0, 1024);
    subscription_select_request(&worker, &request, 1, 1, pair(&worker, 1024, 0), 1);
    (void)subscription_select(&worker, &before, &after, &request, 1, 0);
    subscription_select_request(&worker, &request, 1, 1, pair(&worker, 0, 0), 2);
    (void)subscription_select(&worker, &before, &after, &request, 1, 1);
    subscription_select_request(&worker, &request, 1, 1, pair(&worker, 0, 3), 2);
    (void)subscription_select(&worker, &before, &after, &request, 0, 0);
    // Removing a stored head fact leaves the logical row supported by its rule.
    subscription_select_request(&worker, &request, 1, 2, pair(&worker, 0, 0), 0);
    (void)subscription_select(&worker, &before, &after, &request, 0, 1);
    subscription_select_request(&worker, &request, 2, 2, pair(&worker, 0, 0), 0);
    (void)subscription_select(&worker, &before, &after, &request, 1, 1);
    (void)subscription_select(&worker, &absent, &after, &request, 1024, 0);
    subscription_select_request(&worker, &request, 1, 3, mailbox_receivers(&worker, NULL, 0), 0);
    result = subscription_select(&worker, &absent, &after, &request, 1, 0);
    (void)mailbox_list(mailbox_list(result.f_assertions, 1)->f_data[0], 0);
    subscription_select_request(&worker, &request, 1, 1, pair(&worker, 0, 0), 0);
    for (unsigned kind = 0; kind < 4; ++kind) {
        struct mica_KernelBudget budget = subscription_budget();
        if (kind == 0) budget.f_steps = 0;
        if (kind == 1) budget.f_rows = 0;
        if (kind == 2) budget.f_bytes = 0;
        if (kind == 3) {
            budget.f_signal = mica_foreign_memory_atomic_create(1);
            assert(budget.f_signal);
        }
        result = mica_task_subscription_rows(&worker, (void *)before.f_pointer,
            (void *)after.f_pointer, (void *)request.f_pointer, &budget);
        assert(result.f_status == (kind == 3 ? CANCELLED : LIMIT));
        assert(!worker.f_control && !result.f_assertions && !result.f_retractions);
        if (budget.f_signal) mica_foreign_memory_atomic_destroy(budget.f_signal);
    }
    // Inner refusal must restore an enclosing control and preserve its limit.
    struct mica_KernelBudget outer = subscription_budget(), inner = subscription_budget();
    outer.f_bytes = 0;
    struct mica_MemoryControl control = {.f_context = (void *)&outer, .f_check = mica_kernel_memory_check};
    mica_memory_control_push(&worker, &control);
    result = mica_task_subscription_rows(&worker, (void *)before.f_pointer,
        (void *)after.f_pointer, (void *)request.f_pointer, &inner);
    assert(result.f_status == LIMIT && outer.f_status == LIMIT && worker.f_control == &control);
    mica_memory_control_pop(&worker);
    (void)subscription_select(&worker, &before, &after, &request, 1, 1);
    // Reusing an attached allowance must not debit allocations twice, including
    // when a different operation's control sits above it.
    assert(mica_memory_safepoint(&worker, true));
    inner = subscription_budget();
    result = mica_task_subscription_rows(&worker, (void *)before.f_pointer,
        (void *)after.f_pointer, (void *)request.f_pointer, &inner);
    assert(result.f_status == OK);
    uint64_t remaining = inner.f_bytes;
    assert(remaining < UINT64_MAX);
    for (unsigned nested = 0; nested < 2; ++nested) {
        assert(mica_memory_safepoint(&worker, true));
        inner = subscription_budget();
        outer = subscription_budget();
        struct mica_MemoryControl shared = {.f_context = (void *)&inner, .f_check = mica_kernel_memory_check};
        mica_memory_control_push(&worker, &shared);
        if (nested) mica_memory_control_push(&worker, &control);
        result = mica_task_subscription_rows(&worker, (void *)before.f_pointer,
            (void *)after.f_pointer, (void *)request.f_pointer, &inner);
        assert(result.f_status == OK && inner.f_bytes == remaining);
        assert(worker.f_control == (nested ? &control : &shared));
        if (nested) mica_memory_control_pop(&worker);
        mica_memory_control_pop(&worker);
        assert(!worker.f_control);
    }
    subscription_select_request(&worker, &request, 1, 1, integer(0), 0);
    inner = subscription_budget();
    assert(mica_task_subscription_rows(&worker, NULL, NULL, (void *)request.f_pointer, &inner).f_status == SCHEMA);
    subscription_select_request(&worker, &request, 1, 1, pair(&worker, 0, 0), 4);
    inner = subscription_budget();
    assert(mica_task_subscription_rows(&worker, (void *)before.f_pointer,
        (void *)after.f_pointer, (void *)request.f_pointer, &inner).f_status == SCHEMA);
    assert(mica_memory_root_pop(&worker, &removed));
    assert(mica_memory_root_pop(&worker, &added));
    assert(mica_memory_root_pop(&worker, &request));
    assert(mica_memory_root_pop(&worker, &after));
    assert(mica_memory_root_pop(&worker, &before));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}
