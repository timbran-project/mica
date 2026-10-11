// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static struct mica_KernelRuleDef *subscription_test_rule(struct mica_MemoryWorker *worker) {
    struct mica_MemoryRoot terms = {0}, atom = {0}, source = {0};
    assert(mica_memory_root_push(worker, &terms));
    assert(mica_memory_root_push(worker, &atom));
    assert(mica_memory_root_push(worker, &source));
    terms.f_pointer = (void *)mica_kernel_rule_variable(worker, 1, NULL);
    assert(terms.f_pointer);
    terms.f_pointer = (void *)mica_kernel_rule_variable(worker, 0, (void *)terms.f_pointer);
    assert(terms.f_pointer);
    atom.f_pointer = (void *)mica_kernel_rule_atom(worker, 1, false, (void *)terms.f_pointer, NULL);
    assert(atom.f_pointer);
    struct mica_ValueResult text = mica_value_string(worker, (const uint8_t *)"", 0);
    assert(text.f_ok);
    mica_value_root_set(&source, text.f_value);
    struct mica_KernelRuleDef *definition = mica_kernel_rule_definition(worker, 2,
        (void *)terms.f_pointer, (void *)atom.f_pointer, NULL, integer(7), mica_value_root_get(&source));
    assert(definition);
    assert(mica_memory_root_pop(worker, &source));
    assert(mica_memory_root_pop(worker, &atom));
    assert(mica_memory_root_pop(worker, &terms));
    return definition;
}
static bool subscription_catalogue_has(struct mica_SymbolTable *symbols, mica_type_Value entries,
        const char *kind, const char *key, uint64_t id) {
    struct mica_HeapListResult list = mica_value_as_list(entries);
    assert(list.f_ok);
    struct mica_ValueResult identity = mica_value_identity(id);
    assert(identity.f_ok);
    for (uint64_t i = 0; i < list.f_header->f_length; ++i) {
        mica_type_Value entry = list.f_header->f_data[i];
        if (subscription_message_field(symbols, entry, "kind") == subscription_symbol(symbols, kind)
            && subscription_message_field(symbols, entry, key) == identity.f_value) return true;
    }
    return false;
}
static void subscription_catalogue(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0};
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction transaction = {0};
    struct mica_MemoryRoot before = {0}, after = {0}, entries = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == OK);
    mica_type_Value names[3] = {subscription_symbol(&symbols, "Source"),
        subscription_symbol(&symbols, "Derived"), subscription_symbol(&symbols, "Flag")};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_memory_root_push(&worker, &before));
    assert(mica_memory_root_push(&worker, &after));
    assert(mica_memory_root_push(&worker, &entries));
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    for (uint64_t i = 0; i < 2; ++i)
        assert(mica_kernel_declare(&transaction, i + 1, mica_value_as_symbol(names[i]).f_number, 2, 0, 0, 0) == OK);
    assert(mica_kernel_rule_add(&transaction, 1, subscription_test_rule(&worker), true) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    before.f_pointer = kernel.f_current.f_pointer;
    struct mica_KernelBudget budget = subscription_budget();
    struct mica_TaskSubscriptionEntries result = mica_task_subscription_catalogue_entries(&worker, NULL,
        (void *)before.f_pointer, &vocabulary, &budget);
    assert(result.f_status == OK && !worker.f_control);
    mica_value_root_set(&entries, result.f_entries);
    assert(mica_memory_safepoint(&worker, true));
    const struct mica_HeapList *rows = mailbox_list(mica_value_root_get(&entries), 3);
    assert(subscription_message_field(&symbols, rows->f_data[0], "name") == names[0]);
    assert(subscription_message_field(&symbols, rows->f_data[1], "name") == names[1]);
    assert(subscription_catalogue_has(&symbols, mica_value_root_get(&entries), "rule_installed", "rule", 1));
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 3, mica_value_as_symbol(names[2]).f_number, 0, 0, 0, 0) == OK);
    assert(mica_kernel_rule_update(&transaction, 1, subscription_test_rule(&worker), false) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    after.f_pointer = kernel.f_current.f_pointer;
    budget = subscription_budget();
    result = mica_task_subscription_catalogue_entries(&worker, (void *)before.f_pointer,
        (void *)after.f_pointer, &vocabulary, &budget);
    assert(result.f_status == OK);
    mica_value_root_set(&entries, result.f_entries);
    assert(mica_memory_safepoint(&worker, true));
    (void)mailbox_list(mica_value_root_get(&entries), 2);
    assert(subscription_catalogue_has(&symbols, mica_value_root_get(&entries), "relation_created", "relation", 3));
    assert(subscription_catalogue_has(&symbols, mica_value_root_get(&entries), "rule_disabled", "rule", 1));
    before.f_pointer = after.f_pointer;
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_rule_remove(&transaction, 1) == OK);
    assert(mica_kernel_rule_add(&transaction, 2, subscription_test_rule(&worker), true) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    after.f_pointer = kernel.f_current.f_pointer;
    budget = subscription_budget();
    result = mica_task_subscription_catalogue_entries(&worker, (void *)before.f_pointer,
        (void *)after.f_pointer, &vocabulary, &budget);
    assert(result.f_status == OK);
    mica_value_root_set(&entries, result.f_entries);
    assert(mica_memory_safepoint(&worker, true));
    (void)mailbox_list(mica_value_root_get(&entries), 2);
    assert(subscription_catalogue_has(&symbols, mica_value_root_get(&entries), "rule_removed", "rule", 1));
    assert(subscription_catalogue_has(&symbols, mica_value_root_get(&entries), "rule_installed", "rule", 2));
    budget = subscription_budget();
    result = mica_task_subscription_catalogue_entries(&worker, (void *)after.f_pointer,
        (void *)after.f_pointer, &vocabulary, &budget);
    assert(result.f_status == OK);
    (void)mailbox_list(result.f_entries, 0);
    for (unsigned kind = 0; kind < 4; ++kind) {
        budget = subscription_budget();
        if (kind == 0) budget.f_steps = 0;
        if (kind == 1) budget.f_rows = 0;
        if (kind == 2) budget.f_bytes = 0;
        if (kind == 3) {
            budget.f_signal = mica_foreign_memory_atomic_create(1);
            assert(budget.f_signal);
        }
        result = mica_task_subscription_catalogue_entries(&worker, (void *)before.f_pointer,
            (void *)after.f_pointer, &vocabulary, &budget);
        assert(result.f_status == (kind == 3 ? CANCELLED : LIMIT) && !worker.f_control);
        if (budget.f_signal) mica_foreign_memory_atomic_destroy(budget.f_signal);
    }
    assert(mica_memory_root_pop(&worker, &entries));
    assert(mica_memory_root_pop(&worker, &after));
    assert(mica_memory_root_pop(&worker, &before));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}
