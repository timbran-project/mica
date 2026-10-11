// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

struct subscription_claim_job {
    struct mica_MemoryHeap *heap;
    struct mica_TaskSubscriptionHub *hub;
    uint64_t id, claim;
    bool ready;
};
static void *subscription_claim_thread(void *opaque) {
    struct subscription_claim_job *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, job->heap, 4096));
    assert(mica_memory_enter(&worker));
    struct mica_TaskSubscriptionClaim claim = mica_task_subscription_claim(job->hub, &worker, job->id);
    assert(claim.f_status == OK);
    job->ready = claim.f_ready;
    job->claim = claim.f_claim;
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void subscription_delivery(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub mailboxes = {0};
    struct mica_TaskSubscriptionHub hub = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskServices services = {.f_context = (void *)&hub, .f_subscribe = mica_task_subscription_service};
    struct mica_MemoryRoot request = {0}, view = {0}, payload = {0};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    struct mica_KernelTransaction transaction = {0};
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 1, 1, 2, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    assert(mica_task_mailbox_hub_init(&mailboxes, &worker, 1) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&mailboxes, &worker, 4);
    assert(caps.f_status == OK);
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, 2) == OK);
    mica_type_Value capability = subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, true, 0);
    assert(mica_task_dispatch(&task, &worker, &services) == OK && mica_task_release(&task, &worker));
    uint64_t id = mica_value_as_capability(capability).f_number >> 1;
    struct subscription_claim_job jobs[2] = {{.heap = &heap, .hub = &hub, .id = id},
                                            {.heap = &heap, .hub = &hub, .id = id}};
    pthread_t threads[2];
    assert(mica_memory_leave(&worker));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_create(&threads[i], NULL, subscription_claim_thread, &jobs[i]));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_memory_enter(&worker));
    assert(jobs[0].ready != jobs[1].ready);
    uint64_t previous_claim = jobs[jobs[0].ready ? 0 : 1].claim;
    assert(mica_task_subscription_unclaim(&hub, &worker, id, previous_claim) == OK);
    struct mica_TaskSubscriptionClaim claim = mica_task_subscription_claim(&hub, &worker, id);
    assert(claim.f_status == OK && claim.f_ready && claim.f_claim > previous_claim && claim.f_initial);
    assert(mica_memory_root_push(&worker, &request));
    request.f_pointer = (void *)claim.f_request;
    assert(mica_memory_root_push(&worker, &view));
    view.f_pointer = (void *)claim.f_view;
    assert(mica_memory_root_push(&worker, &payload));
    assert(mica_memory_safepoint(&worker, true));
    assert(((struct mica_TaskSubscriptionRequest *)request.f_pointer)->f_capability == capability);
    assert(((struct mica_KernelSnapshot *)view.f_pointer)->f_version == claim.f_cursor);
    // A delayed cleanup or acceptance cannot consume this pump's claim.
    assert(mica_task_subscription_unclaim(&hub, &worker, id, previous_claim) == OK);
    assert(!mica_task_subscription_claim(&hub, &worker, id).f_ready);
    assert(mica_task_subscription_accept(&hub, &worker, id, previous_claim, claim.f_cursor,
        true, claim.f_cursor, true, integer(41), integer(99), false) == CLOSED);
    assert(!mica_task_subscription_claim(&hub, &worker, id).f_ready);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    uint8_t *bytes = malloc(200000);
    assert(bytes);
    memset(bytes, 'q', 200000);
    struct mica_ValueResult text = mica_value_string(&worker, bytes, 200000);
    free(bytes);
    assert(text.f_ok);
    mica_value_root_set(&payload, text.f_value);
    atomic_store(&allocation_budget, 0);
    uint64_t status = mica_task_subscription_accept(&hub, &worker, id, claim.f_claim, claim.f_cursor,
        true, claim.f_cursor, true, mica_value_root_get(&payload), integer(99), false);
    atomic_store(&allocation_budget, -1);
    assert(status == OOM);
    assert(subscription_entry(&hub, capability)->f_initial);
    assert(subscription_entry(&hub, capability)->f_cursor == claim.f_cursor);
    previous_claim = claim.f_claim;
    claim = mica_task_subscription_claim(&hub, &worker, id);
    assert(claim.f_status == OK && claim.f_ready && claim.f_claim > previous_claim);
#endif
    assert(mica_task_subscription_accept(&hub, &worker, id, claim.f_claim, claim.f_cursor,
        true, claim.f_cursor, true, integer(42), integer(99), false) == OK);
    struct mica_TaskSubscription *entry = subscription_entry(&hub, capability);
    assert(entry && !entry->f_busy && !entry->f_initial && !entry->f_view.f_pointer);
    stream_receive(&worker, &kernel, &mailboxes, caps.f_receiver, (int64_t[]){42}, 1);
    // Cancellation while selection is in progress retires the entry immediately.
    claim = mica_task_subscription_claim(&hub, &worker, id);
    assert(claim.f_status == OK && claim.f_ready);
    subscription_prepare_cancel(&worker, &kernel, &task, capability, true, integer(7));
    assert(mica_task_dispatch(&task, &worker, &services) == OK && mica_task_release(&task, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(((struct mica_TaskSubscriptionRequest *)request.f_pointer)->f_capability == capability);
    assert(mica_task_subscription_accept(&hub, &worker, id, claim.f_claim, claim.f_cursor,
        false, claim.f_cursor, true, integer(43), integer(99), false) == CLOSED);
    assert(mica_task_subscription_unclaim(&hub, &worker, id, claim.f_claim) == OK && !hub.f_count);
    // Overflow accepts only a recovery marker and retires the subscription.
    capability = subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, false, 0);
    assert(mica_task_dispatch(&task, &worker, &services) == OK && mica_task_release(&task, &worker));
    id = mica_value_as_capability(capability).f_number >> 1;
    for (unsigned i = 0; i < 3; ++i) {
        claim = mica_task_subscription_claim(&hub, &worker, id);
        assert(claim.f_status == OK && claim.f_ready);
        assert(mica_task_subscription_accept(&hub, &worker, id, claim.f_claim, claim.f_cursor,
            false, claim.f_cursor, true, integer(50 + i), integer(99), false) == OK);
    }
    assert(!hub.f_count);
    stream_receive(&worker, &kernel, &mailboxes, caps.f_receiver, (int64_t[]){99}, 1);
    assert(mica_memory_root_pop(&worker, &payload));
    assert(mica_memory_root_pop(&worker, &view));
    assert(mica_memory_root_pop(&worker, &request));
    assert(mica_task_subscription_hub_close(&hub, &worker) == OK);
    assert(mica_task_subscription_hub_release(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_shutdown(&mailboxes, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&mailboxes, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    assert(mica_task_resources_stop());
    mica_value_symbol_table_release(&symbols);
}
