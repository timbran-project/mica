// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static mica_type_Value subscription_symbol(struct mica_SymbolTable *symbols, const char *name) {
    struct mica_IdResult id = mica_value_symbol_intern(symbols, (const uint8_t *)name, strlen(name), NULL);
    assert(id.f_ok);
    return mica_value_symbol(id.f_number).f_value;
}
static struct mica_TaskSubscription *subscription_entry(struct mica_TaskSubscriptionHub *hub,
        mica_type_Value capability) {
    uint64_t id = mica_value_as_capability(capability).f_number >> 1;
    for (uint64_t i = 0; i <= hub->f_mask; ++i)
        for (struct mica_TaskSubscription *entry = hub->f_buckets[i]; entry; entry = entry->f_next)
            if (entry->f_id == id) return entry;
    return NULL;
}
static mica_type_Value subscription_run(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime, struct mica_TaskSubscriptionHub *hub,
        const char *mode, mica_type_Value sender, mica_type_Value target, uint64_t cursor, bool succeeds) {
    struct mica_MemoryRoot image = {0}, completed = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskServices services = {.f_context = (void *)hub, .f_subscribe = mica_task_subscription_service};
    assert(mica_memory_root_push(worker, &image));
    mica_type_Value values[] = {subscription_symbol(runtime->f_symbols, mode), sender, target, source_integer((int64_t)cursor)};
    struct mica_ValueResult arguments = mica_value_list(worker, values, 4);
    assert(arguments.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_2_0,
        (void *)code->f_pointer, source_integer(7), arguments.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, worker, kernel, &image, 100000, 3, 16, runtime) == 0);
    uint64_t before = hub->f_count;
    uint64_t status = mica_task_execute(&task, worker);
    assert(status == (succeeds ? 0 : 14));
    // Neither successful execution/commit nor failed execution delivers effects.
    assert(hub->f_count == before);
    mica_type_Value result = 0;
    if (succeeds) {
        assert(mica_task_dispatch(&task, worker, &services) == 0);
        assert(mica_memory_acquire(worker, &task.f_image, &completed));
        result = ((struct mica_TaskImage *)completed.f_pointer)->f_input;
        assert(mica_memory_root_pop(worker, &completed));
    }
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &image));
    return result; // The caller inspects this result before the next safepoint.
}
static mica_type_Value subscription_arguments(struct mica_MemoryWorker *worker,
        struct mica_SymbolTable *symbols, mica_type_Value sender, const char *name) {
    uint32_t column = (uint32_t)mica_value_as_symbol(subscription_symbol(symbols, "value")).f_number;
    struct mica_ValueResult none = mica_value_relation(worker, &column, 1, NULL, 0);
    assert(none.f_ok);
    mica_type_Value target = subscription_symbol(symbols, name);
    struct mica_ValueTuple tuple = {.f_data = &target, .f_arity = 1};
    struct mica_ValueResult option = mica_value_relation(worker, &column, 1, &tuple, 1);
    assert(option.f_ok);
    mica_type_Value holes[] = {none.f_value, none.f_value};
    struct mica_ValueResult bindings = mica_value_list(worker, holes, 2);
    assert(bindings.f_ok);
    mica_type_Value cells[] = {sender, subscription_symbol(symbols, "facts"), option.f_value,
        bindings.f_value, subscription_symbol(symbols, "changes")};
    struct mica_ValueResult arguments = mica_value_list(worker, cells, 5);
    assert(arguments.f_ok);
    return arguments.f_value;
}
static void subscription_decode_controls(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols, mica_type_Value sender) {
    struct mica_KernelTransaction tx = {0};
    struct mica_MemoryRoot arguments = {0};
    assert(mica_memory_root_push(worker, &arguments));
    mica_value_root_set(&arguments, subscription_arguments(worker, symbols, sender, "NativeResult"));
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 1};
    struct mica_source_roundtrip_SubscriptionArgs decoded = mica_source_roundtrip_subscription_decode(
        worker, &attempt, &tx, (void *)code->f_pointer, mica_value_root_get(&arguments));
    assert(decoded.f_status == 10 && !decoded.f_shape && !decoded.f_pattern && attempt.f_remaining <= 1);
    attempt.f_remaining = 100000;
    mica_kernel_work(&tx)->f_bytes_limit = 0;
    decoded = mica_source_roundtrip_subscription_decode(worker, &attempt, &tx,
        (void *)code->f_pointer, mica_value_root_get(&arguments));
    assert(decoded.f_status == 10 && !decoded.f_shape && !decoded.f_pattern);
    mica_kernel_work(&tx)->f_bytes_limit = UINT64_MAX;
    assert(mica_kernel_cancel(&tx));
    decoded = mica_source_roundtrip_subscription_decode(worker, &attempt, &tx,
        (void *)code->f_pointer, mica_value_root_get(&arguments));
    assert(decoded.f_status == 11 && !decoded.f_shape && !decoded.f_pattern);
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(8)) == 0);
    decoded = mica_source_roundtrip_subscription_decode(worker, &attempt, &tx,
        (void *)code->f_pointer, mica_value_root_get(&arguments));
    assert(decoded.f_status == 12 && !decoded.f_shape);
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    uint64_t name = mica_value_as_symbol(subscription_symbol(symbols, "DraftSubscriptionRelation")).f_number;
    assert(mica_kernel_declare(&tx, 1000, name, 2, 0, 0, 0) == 0);
    mica_value_root_set(&arguments, subscription_arguments(worker, symbols, sender, "DraftSubscriptionRelation"));
    decoded = mica_source_roundtrip_subscription_decode(worker, &attempt, &tx,
        (void *)code->f_pointer, mica_value_root_get(&arguments));
    assert(!decoded.f_status && !decoded.f_shape && decoded.f_relation == 1000 && !decoded.f_mask);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &arguments));
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    struct mica_TaskMailboxHub mailboxes = {0};
    struct mica_TaskSubscriptionHub hub = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0};
    assert(mica_task_subscription_vocabulary_init(symbols, &vocabulary) == 0);
    assert(mica_task_mailbox_hub_init(&mailboxes, worker, 1) == 0);
    assert(mica_task_subscription_hub_init(&hub, worker, kernel, &mailboxes, &vocabulary, 8) == 0);
    struct mica_TaskRuntime runtime = {.f_symbols = symbols, .f_mailboxes = &mailboxes};
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&mailboxes, worker, 16);
    assert(caps.f_status == 0);
    subscription_decode_controls(worker, kernel, code, symbols, caps.f_sender);
    assert(subscription_run(worker, kernel, code, &runtime, &hub, "validate", caps.f_sender, 0, 0, true) == mica_value_bool(true));
    assert(!hub.f_count);
    mica_type_Value identity = mica_value_identity(705).f_value;
    mica_type_Value name = subscription_symbol(symbols, "NativeResult");
    uint64_t collection_limit = worker->f_heap->f_collection_limit;
    worker->f_heap->f_collection_limit = 0;
    uint64_t collections = worker->f_heap->f_collections;
    for (unsigned named = 0; named < 2; ++named) {
        mica_type_Value target = named ? name : identity;
        subscription_run(worker, kernel, code, &runtime, &hub, "rollback", caps.f_sender, target, 0, false);
        assert(!hub.f_count);
        mica_type_Value cap = subscription_run(worker, kernel, code, &runtime, &hub, "snapshot", caps.f_sender, target, 0, true);
        struct mica_TaskSubscription *entry = subscription_entry(&hub, cap);
        assert(entry && entry->f_initial && hub.f_count == 1);
        struct mica_TaskSubscriptionRequest *request = (void *)entry->f_request.f_pointer;
        assert(request->f_subject == 1 && request->f_relation == 705 && request->f_mask == 1 && request->f_capacity == 64);
        assert(mica_kernel_cell(request->f_pattern, 0) == source_integer(1));
        struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
        struct mica_TaskSubscriptionPump pumped = mica_task_subscription_pump(&hub, worker,
            mica_value_as_capability(cap).f_number >> 1, &budget);
        assert(pumped.f_status == 0 && pumped.f_progress);
        subscription_run(worker, kernel, code, &runtime, &hub, "cancel_rollback", cap, target, 0, false);
        assert(subscription_entry(&hub, cap) && hub.f_count == 1);
        mica_type_Value cancelled = subscription_run(worker, kernel, code, &runtime, &hub, "cancel", cap, target, 0, true);
        assert(mica_value_is_unit(cancelled) && !hub.f_count);
    }
    mica_type_Value cap = subscription_run(worker, kernel, code, &runtime, &hub, "resume", caps.f_sender, identity, 1, true);
    struct mica_TaskSubscription *entry = subscription_entry(&hub, cap);
    assert(entry);
    struct mica_TaskSubscriptionRequest *request = (void *)entry->f_request.f_pointer;
    assert(!entry->f_initial && request->f_resume && request->f_cursor == 1 && request->f_capacity == 4);
    assert(request->f_subject == 2 && request->f_mask == 0);
    subscription_run(worker, kernel, code, &runtime, &hub, "cancel", cap, identity, 0, true);
    cap = subscription_run(worker, kernel, code, &runtime, &hub, "catalogue", caps.f_sender, identity, 0, true);
    entry = subscription_entry(&hub, cap);
    request = (void *)entry->f_request.f_pointer;
    assert(request->f_subject == 3 && !request->f_relation && !mica_kernel_width(request->f_pattern));
    subscription_run(worker, kernel, code, &runtime, &hub, "cancel", cap, identity, 0, true);
    assert(!hub.f_count && worker->f_heap->f_collections > collections);
    worker->f_heap->f_collection_limit = collection_limit;
    assert(mica_task_subscription_hub_close(&hub, worker) == 0);
    assert(mica_task_subscription_hub_release(&hub, worker) == 0);
    assert(mica_task_mailbox_hub_shutdown(&mailboxes, worker) == 0);
    assert(mica_task_mailbox_hub_release(&mailboxes, worker));
}
