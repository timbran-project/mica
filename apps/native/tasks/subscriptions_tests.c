// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static struct mica_TaskSubscription *subscription_entry(struct mica_TaskSubscriptionHub *hub,
                                                        mica_type_Value capability) {
    uint64_t id = mica_value_as_capability(capability).f_number >> 1;
    for (uint64_t i = 0; i <= hub->f_mask; ++i)
        for (struct mica_TaskSubscription *entry = hub->f_buckets[i]; entry; entry = entry->f_next)
            if (entry->f_id == id) return entry;
    return NULL;
}

static mica_type_Value subscription_prepare(struct mica_MemoryWorker *worker,
        struct mica_Kernel *kernel, struct mica_TaskMailboxHub *mailboxes,
        struct mica_TaskControl *task, mica_type_Value sender, bool initial, uint64_t cursor) {
    struct mica_TaskAttempt attempt = {0};
    mailbox_task(worker, kernel, task, false);
    assert(mica_task_claim(task, worker, &attempt) == OK);
    struct mica_TaskSubscriptionResult result = mica_task_subscription_request(worker, &attempt,
        mailboxes, sender, 1, 1, pair(worker, 1, 0), 1, initial, cursor != 0, cursor, 2);
    assert(result.f_status == OK);
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(task, worker, &attempt) == OK);
    assert(mica_task_commit(task, worker, &attempt) == OK);
    assert(mica_task_finish(task, worker, &attempt, OK) == OK);
    return result.f_capability;
}

static void subscription_prepare_cancel(struct mica_MemoryWorker *worker,
        struct mica_Kernel *kernel, struct mica_TaskControl *task, mica_type_Value capability,
        bool commit, mica_type_Value tenant) {
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_task_test_entry, NULL,
        tenant, integer(0), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(task, worker, kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_memory_root_pop(worker, &image));
    assert(mica_task_claim(task, worker, &attempt) == OK);
    assert(mica_task_subscription_cancel(worker, &attempt, capability) == OK);
    if (!commit) {
        assert(mica_task_finish(task, worker, &attempt, CANCELLED) == CANCELLED);
        return;
    }
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(task, worker, &attempt) == OK);
    assert(mica_task_commit(task, worker, &attempt) == OK);
    assert(mica_task_finish(task, worker, &attempt, OK) == OK);
}

struct subscription_cancel_job {
    struct mica_MemoryHeap *heap;
    struct mica_TaskControl *task;
    struct mica_TaskServices *services;
    uint64_t status;
};
static void *subscription_cancel_thread(void *opaque) {
    struct subscription_cancel_job *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, job->heap, 4096));
    assert(mica_memory_enter(&worker));
    job->status = mica_task_dispatch(job->task, &worker, job->services);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void subscriptions(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0}, other = {0};
    struct mica_TaskMailboxHub mailboxes = {0};
    struct mica_TaskSubscriptionHub hub = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_TaskSubscriptionVocabulary vocabulary = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskServices services = {.f_context = (void *)&hub, .f_subscribe = mica_task_subscription_service};
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_subscription_vocabulary_init(&symbols, &vocabulary) == OK);
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_kernel_init(&other, &worker, integer(7), 1));
    struct mica_KernelTransaction transaction = {0};
    assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
    assert(mica_kernel_declare(&transaction, 1, 1, 2, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    assert(mica_task_mailbox_hub_init(&mailboxes, &worker, 1) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&mailboxes, &worker, 4);
    assert(caps.f_status == OK);
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, 0) == SCHEMA);
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, UINT64_MAX) == LIMIT);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    atomic_store(&allocation_budget, 0);
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, 2) == OOM);
    atomic_store(&allocation_budget, -1);
    assert(!hub.f_mutex && !hub.f_buckets);
#endif
    assert(mica_task_subscription_hub_init(&hub, &worker, &kernel, &mailboxes, &vocabulary, 2) == OK);
    assert(mica_task_subscription_hub_release(&hub, &worker) == CLOSED);
    mica_type_Value first = subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, true, 0);
    assert(!hub.f_count && !subscription_entry(&hub, first));
    uint64_t version = ((struct mica_KernelSnapshot *)task.f_committed_view.f_pointer)->f_version;
    assert(mica_task_dispatch(&task, &worker, &services) == OK);
    assert(hub.f_count == 1 && task.f_state == COMPLETED);
    assert(mica_task_dispatch(&task, &worker, &services) == CLOSED);
    assert(mica_task_release(&task, &worker));
    // Evict history and destroy the task. Only the registry retains this view.
    for (uint64_t id = 2; id < 5; ++id) {
        assert(mica_kernel_begin(&kernel, &worker, &transaction, integer(7)) == OK);
        assert(mica_kernel_declare(&transaction, id, id, 1, 0, 0, 0) == OK);
        assert(mica_kernel_commit(&transaction) == OK && mica_kernel_end(&transaction));
    }
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskSubscription *entry = subscription_entry(&hub, first);
    assert(entry && entry->f_initial && entry->f_cursor == version);
    assert(((struct mica_KernelSnapshot *)entry->f_view.f_pointer)->f_version == version);
    struct mica_TaskSubscriptionRequest *request = (void *)entry->f_request.f_pointer;
    assert(request->f_capability == first && request->f_tenant == integer(7));
    assert(mailbox_list(request->f_pattern, 2)->f_data[0] == integer(1));

    mica_type_Value second = subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, false, UINT64_MAX);
    assert(mica_task_dispatch(&task, &worker, &services) == OK);
    assert(mica_task_release(&task, &worker));
    entry = subscription_entry(&hub, second);
    assert(entry && !entry->f_initial && !entry->f_view.f_pointer && entry->f_cursor == UINT64_MAX);
    mica_type_Value excess = subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, false, 0);
    assert(mica_task_dispatch(&task, &worker, &services) == LIMIT);
    assert(task.f_committed && task.f_state == FAILED && hub.f_count == 2);
    assert(!subscription_entry(&hub, excess));
    assert(mica_task_release(&task, &worker));

    subscription_prepare_cancel(&worker, &kernel, &task, first, false, integer(7));
    assert(subscription_entry(&hub, first));
    assert(mica_task_release(&task, &worker));
    // A service wired to another world cannot cancel this world's registration.
    subscription_prepare_cancel(&worker, &other, &task, first, true, integer(7));
    assert(mica_task_dispatch(&task, &worker, &services) == PERMISSION);
    assert(subscription_entry(&hub, first));
    assert(mica_task_release(&task, &worker));
    stream_send(&mailboxes, &worker, caps.f_sender, mica_value_as_capability(first).f_number >> 1, 51, false, false);
    // Possession of the capability permits delegated cancellation.
    subscription_prepare_cancel(&worker, &kernel, &task, first, true, integer(8));
    assert(mica_task_dispatch(&task, &worker, &services) == OK);
    assert(!subscription_entry(&hub, first) && hub.f_count == 1);
    assert(mica_task_release(&task, &worker));
    stream_receive(&worker, &kernel, &mailboxes, caps.f_receiver, (int64_t[]){51}, 1);

    struct mica_TaskControl tasks[2] = {0};
    struct subscription_cancel_job jobs[2];
    pthread_t threads[2];
    for (unsigned i = 0; i < 2; ++i) {
        subscription_prepare_cancel(&worker, &kernel, &tasks[i], second, true, integer(7));
        jobs[i] = (struct subscription_cancel_job){.heap = &heap, .task = &tasks[i], .services = &services};
    }
    assert(mica_memory_leave(&worker));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_create(&threads[i], NULL, subscription_cancel_thread, &jobs[i]));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_memory_enter(&worker));
    assert((jobs[0].status == OK && jobs[1].status == PERMISSION) || (jobs[1].status == OK && jobs[0].status == PERMISSION));
    assert(!hub.f_count);
    for (unsigned i = 0; i < 2; ++i) assert(mica_task_release(&tasks[i], &worker));
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    for (long budget = 0; budget < 3; ++budget) {
        mica_type_Value capability = subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, true, 0);
        atomic_store(&allocation_budget, budget);
        uint64_t status = mica_task_dispatch(&task, &worker, &services);
        atomic_store(&allocation_budget, -1);
        assert(status == OK || status == OOM);
        assert(mica_task_release(&task, &worker));
        if (status == OK) {
            subscription_prepare_cancel(&worker, &kernel, &task, capability, true, integer(7));
            assert(mica_task_dispatch(&task, &worker, &services) == OK);
            assert(mica_task_release(&task, &worker));
        }
        assert(!hub.f_count);
        for (uint64_t i = 0; i <= mailboxes.f_mask; ++i)
            for (struct mica_TaskMailbox *mailbox = mailboxes.f_buckets[i]; mailbox; mailbox = mailbox->f_next)
                assert(!mailbox->f_streams && !mailbox->f_empty_streams);
        assert(mica_memory_safepoint(&worker, true));
    }
#endif
    (void)subscription_prepare(&worker, &kernel, &mailboxes, &task, caps.f_sender, true, 0);
    assert(mica_task_dispatch(&task, &worker, &services) == OK);
    assert(mica_task_release(&task, &worker));
    assert(mica_task_mailbox_close(&mailboxes, &worker, caps.f_receiver) == OK);
    assert(mica_task_subscription_hub_close(&hub, &worker) == OK && !hub.f_count);
    assert(mica_task_subscription_hub_close(&hub, &worker) == OK);
    assert(mica_task_subscription_hub_release(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_shutdown(&mailboxes, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&mailboxes, &worker));
    assert(mica_kernel_release(&other, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    assert(mica_task_resources_stop());
    mica_value_symbol_table_release(&symbols);
}
