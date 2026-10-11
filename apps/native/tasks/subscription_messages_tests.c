// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static mica_type_Value subscription_symbol(struct mica_SymbolTable *symbols, const char *name) {
    struct mica_IdResult id = mica_value_symbol_intern(symbols, (const uint8_t *)name, strlen(name));
    assert(id.f_ok && id.f_number <= UINT32_MAX);
    struct mica_ValueResult value = mica_value_symbol((uint32_t)id.f_number);
    assert(value.f_ok);
    return value.f_value;
}
static mica_type_Value subscription_message_field(struct mica_SymbolTable *symbols,
                                                  mica_type_Value message, const char *name) {
    struct mica_ValueResult field = mica_value_map_get(message, subscription_symbol(symbols, name));
    assert(field.f_ok);
    return field.f_value;
}

static void subscription_messages(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0}, repeated = {0};
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot rows = {0}, empty = {0}, message = {0};
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == CLOSED);
    assert(mica_value_symbol_table_init(&symbols));
    (void)subscription_symbol(&symbols, "seed-before-protocol-words");
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_subscription_vocabulary_init(&symbols, &repeated) == OK);
    assert(vocabulary.f_kind == repeated.f_kind && vocabulary.f_cursor == repeated.f_cursor);
    assert(vocabulary.f_revoked == repeated.f_revoked);
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &rows));
    assert(mica_memory_root_push(&worker, &empty));
    assert(mica_memory_root_push(&worker, &message));
    mica_value_root_set(&empty, mailbox_receivers(&worker, NULL, 0));
    uint8_t text[128];
    memset(text, 'p', sizeof(text));
    struct mica_ValueResult payload = mica_value_string(&worker, text, sizeof(text));
    assert(payload.f_ok);
    mica_type_Value cells[] = {integer(7), payload.f_value};
    mica_type_Value row = mailbox_receivers(&worker, cells, 2);
    mica_value_root_set(&rows, mailbox_receivers(&worker, &row, 1));
    mica_type_Value capability = mica_value_capability(2).f_value;
    const uint64_t maximum = UINT64_C(0x007fffffffffffff);
    for (unsigned subject = 1; subject <= 2; ++subject) {
        for (unsigned initial = 0; initial < 2; ++initial) {
            struct mica_TaskSubscriptionMessage result = mica_task_subscription_rows_message(&worker,
                &vocabulary, capability, maximum, subject, initial != 0,
                mica_value_root_get(&rows), mica_value_root_get(&empty));
            assert(result.f_status == OK);
            mica_value_root_set(&message, result.f_value);
            assert(mica_memory_safepoint(&worker, true));
            mica_type_Value value = mica_value_root_get(&message);
            assert(subscription_message_field(&symbols, value, "kind") == subscription_symbol(&symbols, "changes"));
            const char *label = initial ? "snapshot" : subject == 1 ? "facts" : "relation";
            assert(subscription_message_field(&symbols, value, "subject") == subscription_symbol(&symbols, label));
            assert(subscription_message_field(&symbols, value, "subscription") == capability);
            assert(mica_value_as_int(subscription_message_field(&symbols, value, "cursor")).f_number == (int64_t)maximum);
            const struct mica_HeapList *assertions = mailbox_list(subscription_message_field(&symbols, value, "assertions"), 1);
            const struct mica_HeapList *asserted = mailbox_list(assertions->f_data[0], 2);
            assert(asserted->f_data[0] == integer(7));
            assert(mica_value_string_length(asserted->f_data[1]).f_number == sizeof(text));
            (void)mailbox_list(subscription_message_field(&symbols, value, "retractions"), 0);
        }
    }
    assert(mica_task_subscription_rows_message(&worker, &vocabulary, capability, 1,
        3, false, mica_value_root_get(&rows), mica_value_root_get(&empty)).f_status == SCHEMA);
    assert(mica_task_subscription_rows_message(&worker, &vocabulary, capability, 1,
        1, false, integer(0), mica_value_root_get(&empty)).f_status == SCHEMA);
    for (unsigned revoked = 0; revoked < 2; ++revoked) {
        struct mica_TaskSubscriptionMessage result = mica_task_subscription_marker(&worker,
            &vocabulary, capability, 42, revoked != 0);
        assert(result.f_status == OK);
        mica_value_root_set(&message, result.f_value);
        assert(mica_memory_safepoint(&worker, true));
        mica_type_Value value = mica_value_root_get(&message);
        assert(subscription_message_field(&symbols, value, "kind") == subscription_symbol(&symbols, revoked ? "revoked" : "resynchronize"));
        assert(subscription_message_field(&symbols, value, "subscription") == capability);
        assert(subscription_message_field(&symbols, value, "cursor") == integer(42));
    }
    assert(mica_task_subscription_marker(&worker, &vocabulary, capability, maximum + 1, false).f_status == LIMIT);
    assert(mica_task_subscription_marker(&worker, &vocabulary, capability, UINT64_MAX, false).f_status == LIMIT);
    for (unsigned initial = 0; initial < 2; ++initial) {
        struct mica_TaskSubscriptionMessage result = mica_task_subscription_catalogue_message(&worker,
            &vocabulary, capability, 7, initial != 0, mica_value_root_get(&empty));
        assert(result.f_status == OK);
        mica_value_root_set(&message, result.f_value);
        assert(mica_memory_safepoint(&worker, true));
        mica_type_Value value = mica_value_root_get(&message);
        assert(subscription_message_field(&symbols, value, "kind") == subscription_symbol(&symbols, initial ? "snapshot" : "changes"));
        assert(subscription_message_field(&symbols, value, "subject") == subscription_symbol(&symbols, "catalogue"));
        assert(subscription_message_field(&symbols, value, "cursor") == integer(7));
        (void)mailbox_list(subscription_message_field(&symbols, value, "entries"), 0);
    }
    assert(mica_task_subscription_catalogue_message(&worker, &vocabulary, capability, 7, false, integer(0)).f_status == SCHEMA);
    assert(mica_memory_root_pop(&worker, &message));
    assert(mica_memory_root_pop(&worker, &empty));
    assert(mica_memory_root_pop(&worker, &rows));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}
