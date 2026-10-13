// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static mica_type_Value subscription_register_subject(struct mica_MemoryWorker *worker,
        struct mica_Kernel *kernel, struct mica_TaskMailboxHub *mailboxes,
        struct mica_TaskSubscriptionHub *hub, mica_type_Value sender, uint64_t subject,
        uint64_t relation, bool initial, bool resume, uint64_t cursor) {
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_TaskServices services = {.f_context = (void *)hub, .f_subscribe = mica_task_subscription_service};
    mailbox_task(worker, kernel, &task, false);
    assert(mica_task_claim(&task, worker, &attempt) == OK);
    mica_type_Value pattern = subject == 3 ? mailbox_receivers(worker, NULL, 0) : pair(worker, 0, 0);
    struct mica_TaskSubscriptionResult result = mica_task_subscription_request(worker, &attempt,
        mailboxes, sender, subject, relation, pattern, 0, initial, resume, cursor, 4);
    assert(result.f_status == OK);
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(&task, worker, &attempt) == OK);
    assert(mica_task_commit(&task, worker, &attempt) == OK);
    assert(mica_task_finish(&task, worker, &attempt, OK) == OK);
    assert(mica_task_dispatch(&task, worker, &services) == OK);
    assert(mica_task_release(&task, worker));
    return result.f_capability;
}
static void subscription_take_message(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_TaskMailboxHub *hub, mica_type_Value receiver, struct mica_MemoryRoot *message) {
    struct mica_TaskControl task = {0};
    struct mica_MemoryRoot receivers = {0};
    assert(mica_memory_root_push(worker, &receivers));
    mica_value_root_set(&receivers, mailbox_receivers(worker, &receiver, 1));
    mailbox_task(worker, kernel, &task, true);
    struct mica_TaskMailboxRead read = mica_task_mailbox_receive(hub, worker,
        mica_value_root_get(&receivers), &task, task.f_generation);
    assert(read.f_status == OK && read.f_count == 1 && task.f_state == RUNNABLE);
    struct mica_TaskImage *image = (void *)task.f_image.f_pointer;
    const struct mica_HeapList *groups = mailbox_list(image->f_input, 1);
    const struct mica_HeapList *group = mailbox_list(groups->f_data[0], 2);
    const struct mica_HeapList *messages = mailbox_list(group->f_data[1], 1);
    mica_value_root_set(message, messages->f_data[0]);
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &receivers));
    assert(mica_memory_safepoint(worker, true));
}
static void subscription_pump_once(struct mica_TaskSubscriptionHub *hub,
        struct mica_MemoryWorker *worker, mica_type_Value capability, bool progress) {
    struct mica_KernelBudget budget = subscription_budget();
    struct mica_TaskSubscriptionPump result = mica_task_subscription_pump(hub, worker,
        mica_value_as_capability(capability).f_number >> 1, &budget);
    assert(result.f_status == OK && result.f_progress == progress && !worker->f_control);
}
static void subscription_cancel_now(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_TaskSubscriptionHub *hub, mica_type_Value capability) {
    struct mica_TaskControl task = {0};
    struct mica_TaskServices services = {.f_context = (void *)hub, .f_subscribe = mica_task_subscription_service};
    subscription_prepare_cancel(worker, kernel, &task, capability, true, integer(7));
    assert(mica_task_dispatch(&task, worker, &services) == OK);
    assert(mica_task_release(&task, worker));
}
static void subscription_write(struct mica_Kernel *kernel, struct mica_MemoryWorker *worker,
        uint64_t relation, int64_t key, int64_t value, bool asserted) {
    struct mica_KernelTransaction transaction = {0};
    assert(mica_kernel_begin(kernel, worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_write(&transaction, relation, pair(worker, key, value), asserted) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
}
static void subscription_pump(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0};
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub mailboxes = {0};
    struct mica_TaskSubscriptionHub hub = {0};
    struct mica_MemoryRoot message = {0};
    struct mica_KernelTransaction transaction = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 4));
    assert(mica_memory_root_push(&worker, &message));
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 1, 1, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&transaction, 2, 2, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&transaction, 10, 10, 1, 0, 0, 0) == OK);
    assert(mica_kernel_write(&transaction, 10, mailbox_receivers(&worker, (mica_type_Value[]){integer(7)}, 1), true) == OK);
    assert(mica_kernel_write(&transaction, 1, pair(&worker, 1, 10), true) == OK);
    assert(mica_kernel_rule_add(&transaction, 1, subscription_test_rule(&worker), true) == OK);
    assert(mica_kernel_set_authority_policy(&transaction, 10, 0, 0, 0, 0, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    assert(mica_task_mailbox_hub_init(&mailboxes, &worker, 1) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&mailboxes, &worker, 16);
    assert(caps.f_status == OK);
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, 8) == OK);
    mica_type_Value capability = subscription_register_subject(&worker, &kernel, &mailboxes,
        &hub, caps.f_sender, 2, 2, true, false, 0);
    uint64_t initial_cursor = subscription_entry(&hub, capability)->f_cursor;
    subscription_write(&kernel, &worker, 1, 2, 20, true);
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    mica_type_Value value = mica_value_root_get(&message);
    assert(subscription_message_field(&symbols, value, "subject") == subscription_symbol(&symbols, "snapshot"));
    assert(mica_value_as_int(subscription_message_field(&symbols, value, "cursor")).f_number == (int64_t)initial_cursor);
    const struct mica_HeapList *rows = mailbox_list(subscription_message_field(&symbols, value, "assertions"), 1);
    assert(mailbox_list(rows->f_data[0], 2)->f_data[0] == integer(1));
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    value = mica_value_root_get(&message);
    rows = mailbox_list(subscription_message_field(&symbols, value, "assertions"), 1);
    assert(mailbox_list(rows->f_data[0], 2)->f_data[0] == integer(2));
    subscription_pump_once(&hub, &worker, capability, false);
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 20, 20, 1, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_pump_once(&hub, &worker, capability, false);
    subscription_write(&kernel, &worker, 1, 1, 10, false);
    uint64_t cursor = subscription_entry(&hub, capability)->f_cursor;
    struct mica_KernelBudget budget = subscription_budget();
    budget.f_steps = 0;
    struct mica_TaskSubscriptionPump limited = mica_task_subscription_pump(&hub, &worker,
        mica_value_as_capability(capability).f_number >> 1, &budget);
    assert(limited.f_status == LIMIT && !limited.f_progress && !worker.f_control);
    assert(subscription_entry(&hub, capability)->f_cursor == cursor && !subscription_entry(&hub, capability)->f_busy);
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    value = mica_value_root_get(&message);
    (void)mailbox_list(subscription_message_field(&symbols, value, "assertions"), 0);
    rows = mailbox_list(subscription_message_field(&symbols, value, "retractions"), 1);
    assert(mailbox_list(rows->f_data[0], 2)->f_data[0] == integer(1));
    subscription_cancel_now(&worker, &kernel, &hub, capability);
    capability = subscription_register_subject(&worker, &kernel, &mailboxes, &hub, caps.f_sender, 1, 1, false, true, 0);
    for (int64_t i = 0; i < 5; ++i) subscription_write(&kernel, &worker, 1, 100 + i, i, true);
    subscription_pump_once(&hub, &worker, capability, true);
    assert(!subscription_entry(&hub, capability));
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    assert(subscription_message_field(&symbols, mica_value_root_get(&message), "kind") == subscription_symbol(&symbols, "resynchronize"));
    capability = subscription_register_subject(&worker, &kernel, &mailboxes, &hub, caps.f_sender, 1, 1, true, false, 0);
    initial_cursor = subscription_entry(&hub, capability)->f_cursor;
    for (int64_t i = 0; i < 5; ++i) subscription_write(&kernel, &worker, 1, 200 + i, i, true);
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    value = mica_value_root_get(&message);
    assert(mica_value_as_int(subscription_message_field(&symbols, value, "cursor")).f_number == (int64_t)initial_cursor);
    (void)mailbox_list(subscription_message_field(&symbols, value, "assertions"), 6);
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    assert(subscription_message_field(&symbols, mica_value_root_get(&message), "kind") == subscription_symbol(&symbols, "resynchronize"));
    capability = subscription_register_subject(&worker, &kernel, &mailboxes, &hub, caps.f_sender, 3, 0, true, false, 0);
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    value = mica_value_root_get(&message);
    assert(subscription_message_field(&symbols, value, "kind") == subscription_symbol(&symbols, "snapshot"));
    (void)mailbox_list(subscription_message_field(&symbols, value, "entries"), 5);
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_rule_remove(&transaction, 1) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    subscription_pump_once(&hub, &worker, capability, true);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    value = subscription_message_field(&symbols, mica_value_root_get(&message), "entries");
    (void)mailbox_list(value, 1);
    assert(subscription_catalogue_has(&symbols, value, "rule_removed", "rule", 1));
    subscription_cancel_now(&worker, &kernel, &hub, capability);
    // Even former root authority is refreshed before an initial snapshot is disclosed.
    capability = subscription_register_subject(&worker, &kernel, &mailboxes, &hub, caps.f_sender, 1, 1, true, false, 0);
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_write(&transaction, 10, mailbox_receivers(&worker, (mica_type_Value[]){integer(7)}, 1), false) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    subscription_pump_once(&hub, &worker, capability, true);
    assert(!hub.f_count);
    subscription_take_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    assert(subscription_message_field(&symbols, mica_value_root_get(&message), "kind") == subscription_symbol(&symbols, "revoked"));
    assert(mica_task_subscription_hub_close(&hub, &worker) == OK);
    assert(mica_task_subscription_hub_release(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_shutdown(&mailboxes, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&mailboxes, &worker));
    assert(mica_memory_root_pop(&worker, &message));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    assert(mica_task_resources_stop());
    mica_value_symbol_table_release(&symbols);
}

struct subscription_event_job {
    struct mica_MemoryHeap *heap;
    struct mica_TaskSubscriptionHub *hub;
    struct mica_KernelBudget limits;
    uint64_t status;
};
static void *subscription_event_thread(void *opaque) {
    struct subscription_event_job *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, job->heap, 4096));
    assert(mica_memory_enter(&worker));
    job->status = mica_task_subscription_events(job->hub, &worker, &job->limits);
    assert(!worker.f_roots && !worker.f_control);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void subscription_wait_message(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_TaskMailboxHub *hub, mica_type_Value receiver, struct mica_MemoryRoot *message) {
    struct mica_TaskControl task = {0};
    struct mica_MemoryRoot receivers = {0};
    assert(mica_memory_root_push(worker, &receivers));
    mica_value_root_set(&receivers, mailbox_receivers(worker, &receiver, 1));
    mailbox_task(worker, kernel, &task, true);
    uint64_t deadline = mica_foreign_task_clock_now() + UINT64_C(10000000000);
    for (;;) {
        struct mica_TaskMailboxRead read = mica_task_mailbox_receive(hub, worker,
            mica_value_root_get(&receivers), &task, task.f_generation);
        assert(read.f_status == OK);
        if (read.f_count) { assert(read.f_count == 1); break; }
        assert(mica_foreign_task_clock_now() < deadline);
        assert(mica_memory_safepoint(worker, false));
    }
    struct mica_TaskImage *image = (void *)task.f_image.f_pointer;
    const struct mica_HeapList *groups = mailbox_list(image->f_input, 1);
    const struct mica_HeapList *group = mailbox_list(groups->f_data[0], 2);
    const struct mica_HeapList *messages = mailbox_list(group->f_data[1], 1);
    mica_value_root_set(message, messages->f_data[0]);
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &receivers));
    assert(mica_memory_safepoint(worker, true));
}
static void subscription_events(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0};
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub mailboxes = {0};
    struct mica_TaskSubscriptionHub hub = {0};
    struct mica_MemoryRoot message = {0};
    struct mica_KernelTransaction transaction = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 4));
    assert(mica_memory_root_push(&worker, &message));
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 1, 1, 2, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    assert(mica_task_mailbox_hub_init(&mailboxes, &worker, 1) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&mailboxes, &worker, 8);
    assert(caps.f_status == OK);
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, 4) == OK);
    struct mica_KernelBudget limits = subscription_budget();
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    atomic_store(&allocation_budget, 0);
    assert(mica_task_subscription_events(&hub, &worker, &limits) == OOM);
    atomic_store(&allocation_budget, -1);
    assert(!hub.f_events && !kernel.f_notify && !kernel.f_notify_context);
#endif
    mica_type_Value capability = subscription_register_subject(&worker, &kernel, &mailboxes,
        &hub, caps.f_sender, 1, 1, true, false, 0);
    uint64_t cursor = subscription_entry(&hub, capability)->f_cursor;
    limits.f_steps = 0;
    assert(mica_task_subscription_events(&hub, &worker, &limits) == LIMIT);
    assert(!hub.f_events && !kernel.f_notify && !kernel.f_notify_context);
    assert(subscription_entry(&hub, capability)->f_cursor == cursor);
    assert(subscription_entry(&hub, capability)->f_initial && !subscription_entry(&hub, capability)->f_busy);
    limits = subscription_budget();
    // Hold a claim while the event worker inspects it. Releasing the claim must
    // notify that worker even though no world commit follows the release.
    uint64_t id = mica_value_as_capability(capability).f_number >> 1;
    struct mica_TaskSubscriptionClaim claim = mica_task_subscription_claim(&hub, &worker, id);
    assert(claim.f_status == OK && claim.f_ready);
    struct subscription_event_job job = {.heap = &heap, .hub = &hub, .limits = limits};
    pthread_t thread;
    assert(mica_memory_leave(&worker));
    assert(!pthread_create(&thread, NULL, subscription_event_thread, &job));
    assert(mica_memory_enter(&worker));
    uint64_t deadline = mica_foreign_task_clock_now() + UINT64_C(10000000000);
    for (;;) {
        assert(mica_memory_leave(&worker));
        mica_foreign_memory_lock(hub.f_mutex);
        bool requested = subscription_entry(&hub, capability)->f_wake;
        mica_foreign_memory_unlock(hub.f_mutex);
        assert(mica_memory_enter(&worker));
        if (requested) break;
        assert(mica_foreign_task_clock_now() < deadline);
        assert(mica_memory_safepoint(&worker, false));
    }
    assert(mica_task_subscription_events(&hub, &worker, &limits) == CLOSED);
    assert(mica_kernel_set_notify(&kernel, &worker, NULL, hub.f_event, mica_task_subscription_signal) == CONFLICT);
    assert(mica_task_subscription_unclaim(&hub, &worker, id, claim.f_claim) == OK);
    subscription_wait_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    (void)mailbox_list(subscription_message_field(&symbols, mica_value_root_get(&message), "assertions"), 0);
    for (int64_t i = 0; i < 16; ++i) {
        // Direct kernel commits, without a task service call, wake delivery.
        subscription_write(&kernel, &worker, 1, i, i + 100, true);
        assert(mica_memory_safepoint(&worker, true));
        subscription_wait_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
        mica_type_Value batch = mica_value_root_get(&message);
        const struct mica_HeapList *rows = mailbox_list(subscription_message_field(&symbols, batch, "assertions"), 1);
        assert(mailbox_list(rows->f_data[0], 2)->f_data[0] == integer(i));
        assert(mica_value_as_int(subscription_message_field(&symbols, batch, "cursor")).f_number == (int64_t)++cursor);
    }
    subscription_cancel_now(&worker, &kernel, &hub, capability);
    // Admission itself wakes a parked dispatcher. This read-only registration
    // produces no world commit notification to hide a missing admission signal.
    capability = subscription_register_subject(&worker, &kernel, &mailboxes,
        &hub, caps.f_sender, 1, 1, true, false, 0);
    subscription_wait_message(&worker, &kernel, &mailboxes, caps.f_receiver, &message);
    (void)mailbox_list(subscription_message_field(&symbols, mica_value_root_get(&message), "assertions"), 16);
    subscription_cancel_now(&worker, &kernel, &hub, capability);
    // An idle dispatcher must wake for close and detach before event storage is freed.
    assert(mica_task_subscription_hub_close(&hub, &worker) == OK);
    assert(mica_memory_leave(&worker));
    assert(!pthread_join(thread, NULL));
    assert(job.status == OK && !hub.f_events && !kernel.f_notify && !kernel.f_notify_context);
    assert(mica_memory_enter(&worker));
    assert(mica_task_subscription_hub_release(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_shutdown(&mailboxes, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&mailboxes, &worker));
    assert(mica_memory_root_pop(&worker, &message));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    assert(mica_task_resources_stop());
    mica_value_symbol_table_release(&symbols);
}
