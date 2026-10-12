// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Included by tests.c after the shared fixture helpers. All queue mutation,
// capability checks, publication, and wake arbitration run in generated code.
static void mailbox_task(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
                         struct mica_TaskControl *task, bool waiting) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_task_test_entry, NULL,
        integer(7), integer(8), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(task, worker, kernel, &image, 10000, 2, 8, NULL) == OK);
    assert(mica_memory_root_pop(worker, &image));
    if (!waiting) return;
    struct mica_TaskAttempt attempt = {0};
    struct mica_TaskServices services = {0};
    assert(mica_task_claim(task, worker, &attempt) == OK);
    assert(mica_task_request(&attempt, 2) == OK);
    assert(mica_task_prepare(task, worker, &attempt) == OK);
    assert(mica_task_commit(task, worker, &attempt) == OK);
    assert(mica_task_finish(task, worker, &attempt, OK) == OK);
    assert(mica_task_dispatch(task, worker, &services) == OK);
    assert(task->f_state == WAITING);
}

static mica_type_Value mailbox_receivers(struct mica_MemoryWorker *worker,
                                        mica_type_Value *values, uint64_t count) {
    struct mica_ValueResult result = mica_value_list(worker, values, count);
    assert(result.f_ok);
    return result.f_value;
}

static const struct mica_HeapList *mailbox_list(mica_type_Value value, uint64_t count) {
    struct mica_HeapListResult list = mica_value_as_list(value);
    assert(list.f_ok && list.f_header->f_length == count);
    return list.f_header;
}

struct mailbox_watch {
    struct mica_MemoryHeap *heap;
    struct mica_TaskMailboxHub *hub;
    uint64_t revision;
    _Atomic bool started;
    struct mica_TaskMailboxChange result;
};
static void *mailbox_watch_thread(void *argument) {
    struct mailbox_watch *watch = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, watch->heap, 4096));
    assert(mica_memory_enter(&worker));
    atomic_store(&watch->started, true);
    watch->result = mica_task_mailbox_changed(watch->hub, &worker, watch->revision);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

struct mailbox_sender {
    struct mica_MemoryHeap *heap;
    struct mica_TaskMailboxHub *hub;
    mica_type_Value sender;
    unsigned count;
};
static void *mailbox_send_thread(void *argument) {
    struct mailbox_sender *sender = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, sender->heap, 4096));
    assert(mica_memory_enter(&worker));
    for (unsigned i = 0; i < sender->count; ++i) {
        uint8_t bytes[256];
        memset(bytes, (int)('a' + i % 26), sizeof(bytes));
        struct mica_ValueResult text = mica_value_string(&worker, bytes, sizeof(bytes));
        assert(text.f_ok);
        text = mica_value_string_slice(&worker, text.f_value, 13, 61);
        assert(text.f_ok);
        assert(mica_task_mailbox_send(sender->hub, &worker, sender->sender, text.f_value) == OK);
        assert(mica_memory_safepoint(&worker, true));
    }
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

struct mailbox_reader {
    struct mica_MemoryHeap *heap;
    struct mica_TaskMailboxHub *hub;
    struct mica_TaskControl *task;
    mica_type_Value receiver;
    _Atomic unsigned *ready;
    _Atomic bool *go;
    struct mica_TaskMailboxRead result;
};
static void *mailbox_receive_thread(void *argument) {
    struct mailbox_reader *reader = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot receivers = {0};
    assert(mica_memory_worker_init(&worker, reader->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &receivers));
    mica_value_root_set(&receivers, mailbox_receivers(&worker, &reader->receiver, 1));
    assert(mica_memory_leave(&worker));
    atomic_fetch_add(reader->ready, 1);
    while (!atomic_load(reader->go)) sched_yield();
    assert(mica_memory_enter(&worker));
    reader->result = mica_task_mailbox_receive(reader->hub, &worker,
        mica_value_root_get(&receivers), reader->task, 1);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_root_pop(&worker, &receivers));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void mailbox_races(struct mica_MemoryHeap *heap, struct mica_MemoryWorker *worker,
                          struct mica_Kernel *kernel, struct mica_TaskMailboxHub *hub,
                          struct mica_TaskMailboxCaps caps) {
    for (unsigned round = 0; round < 32; ++round) {
        struct mica_TaskControl tasks[2] = {{0}};
        _Atomic unsigned ready = 0;
        _Atomic bool go = false;
        pthread_t threads[2];
        struct mailbox_reader readers[2] = {0};
        assert(mica_task_mailbox_send(hub, worker, caps.f_sender, integer(round)) == OK);
        for (unsigned i = 0; i < 2; ++i) {
            mailbox_task(worker, kernel, &tasks[i], true);
            readers[i] = (struct mailbox_reader){.heap = heap, .hub = hub, .task = &tasks[i],
                .receiver = caps.f_receiver, .ready = &ready, .go = &go};
        }
        assert(mica_memory_leave(worker));
        for (unsigned i = 0; i < 2; ++i)
            assert(!pthread_create(&threads[i], NULL, mailbox_receive_thread, &readers[i]));
        while (atomic_load(&ready) < 2) sched_yield();
        atomic_store(&go, true);
        assert(mica_task_cancel(&tasks[0]));
        for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
        assert(mica_memory_enter(worker));
        assert(readers[0].result.f_status == OK || readers[0].result.f_status == CLOSED);
        assert(readers[1].result.f_status == OK);
        assert(readers[0].result.f_count + readers[1].result.f_count == 1);
        assert(mica_memory_safepoint(worker, true));
        for (unsigned i = 0; i < 2; ++i) {
            if (readers[i].result.f_count) {
                struct mica_TaskImage *image = (void *)tasks[i].f_image.f_pointer;
                const struct mica_HeapList *groups = mailbox_list(image->f_input, 1);
                const struct mica_HeapList *group = mailbox_list(groups->f_data[0], 2);
                assert(mailbox_list(group->f_data[1], 1)->f_data[0] == integer(round));
            }
            assert(mica_task_release(&tasks[i], worker));
        }
    }
}

static void mailboxes(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0}, other = {0};
    struct mica_MemoryRoot receivers = {0};
    assert(mica_task_resources_start());
    assert(!mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 3) == OK);
    assert(mica_task_mailbox_hub_init(&other, &worker, 1) == OK);
    assert(!mica_task_resources_stop());
    assert(mica_task_mailbox_create(&hub, &worker, 0).f_status == SCHEMA);
    atomic_store(&allocation_budget, 0);
    assert(mica_task_mailbox_create(&hub, &worker, 1).f_status == OOM);
    atomic_store(&allocation_budget, -1);
    assert(hub.f_count == 0);
    struct mica_TaskMailboxCaps a = mica_task_mailbox_create(&hub, &worker, 128);
    struct mica_TaskMailboxCaps b = mica_task_mailbox_create(&hub, &worker, 2);
    struct mica_TaskMailboxCaps empty = mica_task_mailbox_create(&hub, &worker, 1);
    struct mica_TaskMailboxCaps remote = mica_task_mailbox_create(&other, &worker, 1);
    assert(a.f_status == OK && b.f_status == OK && empty.f_status == OK && remote.f_status == OK);
    assert(mica_task_mailbox_create(&hub, &worker, 1).f_status == LIMIT);
    assert(mica_task_mailbox_send(&hub, &worker, a.f_receiver, integer(1)) == PERMISSION);
    assert(mica_task_mailbox_send(&hub, &worker, remote.f_sender, integer(1)) == PERMISSION);
    assert(mica_task_mailbox_send(&hub, &worker, integer(1), integer(1)) == PERMISSION);
    assert(mica_task_mailbox_close(&hub, &worker, a.f_sender) == PERMISSION);
    assert(mica_memory_root_push(&worker, &receivers));
    mica_type_Value caps[] = {b.f_receiver, empty.f_receiver, a.f_receiver, b.f_receiver};
    mica_value_root_set(&receivers, mailbox_receivers(&worker, caps, 4));

    // A cancelled draft must not send; a committed draft sends exactly once.
    struct mica_TaskServices services = {.f_context = (void *)&hub, .f_send = mica_task_mailbox_service};
    for (unsigned commit = 0; commit < 2; ++commit) {
        struct mica_TaskControl task = {0};
        struct mica_TaskAttempt attempt = {0};
        mailbox_task(&worker, &kernel, &task, false);
        assert(mica_task_claim(&task, &worker, &attempt) == OK);
        assert(mica_task_stage(&worker, &attempt.f_effects, 2, b.f_sender, integer(41)) == OK);
        struct mica_TaskMailboxRead read = mica_task_mailbox_receive(&hub, &worker,
            mica_value_root_get(&receivers), &task, 0);
        assert(read.f_status == OK && read.f_count == 0);
        if (!commit) {
            assert(mica_task_finish(&task, &worker, &attempt, CANCELLED) == CANCELLED);
            assert(mica_task_dispatch(&task, &worker, &services) == CLOSED);
        } else {
            assert(mica_task_request(&attempt, 1) == OK);
            assert(mica_task_prepare(&task, &worker, &attempt) == OK);
            assert(mica_task_commit(&task, &worker, &attempt) == OK);
            assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
            assert(mica_task_dispatch(&task, &worker, &services) == OK);
            assert(mica_task_dispatch(&task, &worker, &services) == CLOSED);
        }
        assert(mica_task_release(&task, &worker));
    }
    assert(mica_task_mailbox_send(&hub, &worker, b.f_sender, integer(42)) == OK);
    assert(mica_task_mailbox_send(&hub, &worker, b.f_sender, integer(43)) == LIMIT);

    // Another worker publishes slices, then exits. The mailbox is their only
    // owner while the receiver is still suspended.
    struct mailbox_sender sender = {&heap, &hub, a.f_sender, 64};
    pthread_t thread;
    assert(mica_memory_leave(&worker));
    assert(!pthread_create(&thread, NULL, mailbox_send_thread, &sender));
    assert(!pthread_join(thread, NULL));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskControl cancelled = {0}, receiver = {0};
    mailbox_task(&worker, &kernel, &cancelled, true);
    mailbox_task(&worker, &kernel, &receiver, true);
    assert(mica_memory_leave(&worker));
    assert(mica_task_cancel(&cancelled));
    assert(mica_memory_enter(&worker));
    struct mica_TaskMailboxRead read = mica_task_mailbox_receive(&hub, &worker,
        mica_value_root_get(&receivers), &cancelled, cancelled.f_generation);
    assert(read.f_status == CLOSED);
    mica_type_Value invalid[] = {b.f_receiver, remote.f_receiver};
    mica_value_root_set(&receivers, mailbox_receivers(&worker, invalid, 2));
    read = mica_task_mailbox_receive(&hub, &worker, mica_value_root_get(&receivers), &receiver, receiver.f_generation);
    assert(read.f_status == PERMISSION && receiver.f_state == WAITING);
    mica_value_root_set(&receivers, mailbox_receivers(&worker, caps, 4));
    read = mica_task_mailbox_receive(&hub, &worker, mica_value_root_get(&receivers), &receiver, 0);
    assert(read.f_status == CLOSED && receiver.f_state == WAITING);
    // Epoch wrap must clear duplicate markers without consuming any message.
    hub.f_read_epoch = UINT64_MAX;
    read = mica_task_mailbox_receive(&hub, &worker, mica_value_root_get(&receivers), &receiver, receiver.f_generation);
    assert(read.f_status == OK && read.f_count == 66 && receiver.f_state == RUNNABLE);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskImage *image = (void *)receiver.f_image.f_pointer;
    const struct mica_HeapList *groups = mailbox_list(image->f_input, 2);
    const struct mica_HeapList *group = mailbox_list(groups->f_data[0], 2);
    assert(group->f_data[0] == b.f_receiver);
    const struct mica_HeapList *messages = mailbox_list(group->f_data[1], 2);
    assert(messages->f_data[0] == integer(41) && messages->f_data[1] == integer(42));
    group = mailbox_list(groups->f_data[1], 2);
    assert(group->f_data[0] == a.f_receiver);
    messages = mailbox_list(group->f_data[1], 64);
    for (unsigned i = 0; i < 64; ++i) {
        assert(mica_value_string_length(messages->f_data[i]).f_number == 48);
        struct mica_HeapStringResult text = mica_value_as_string(messages->f_data[i]);
        assert(text.f_ok && text.f_header->f_data[0] == 'a' + i % 26);
    }
    read = mica_task_mailbox_receive(&hub, &worker, mica_value_root_get(&receivers), &receiver, receiver.f_generation);
    assert(read.f_status == OK && read.f_count == 0);
    assert(mica_task_release(&cancelled, &worker));
    assert(mica_task_release(&receiver, &worker));

    // Force a dedicated large allocation for the drain result. Refusing that
    // allocation cannot consume messages or claim the continuation.
    assert(mica_task_mailbox_close(&hub, &worker, a.f_receiver) == OK);
    struct mica_TaskMailboxCaps large = mica_task_mailbox_create(&hub, &worker, 8193);
    assert(large.f_status == OK);
    mica_value_root_set(&receivers, mailbox_receivers(&worker, &large.f_receiver, 1));
    mailbox_task(&worker, &kernel, &receiver, true);
    for (unsigned i = 0; i < 8193; ++i)
        assert(mica_task_mailbox_send(&hub, &worker, large.f_sender, integer(i)) == OK);
    assert(mica_memory_safepoint(&worker, true));
    atomic_store(&allocation_budget, 0);
    read = mica_task_mailbox_receive(&hub, &worker, mica_value_root_get(&receivers), &receiver, receiver.f_generation);
    atomic_store(&allocation_budget, -1);
    assert(read.f_status == OOM && receiver.f_state == WAITING);
    read = mica_task_mailbox_receive(&hub, &worker, mica_value_root_get(&receivers), &receiver, receiver.f_generation);
    assert(read.f_status == OK && read.f_count == 8193);
    assert(mica_memory_safepoint(&worker, true));
    image = (void *)receiver.f_image.f_pointer;
    groups = mailbox_list(image->f_input, 1);
    group = mailbox_list(groups->f_data[0], 2);
    messages = mailbox_list(group->f_data[1], 8193);
    for (unsigned i = 0; i < 8193; ++i) assert(messages->f_data[i] == integer(i));
    assert(mica_task_release(&receiver, &worker));

    mailbox_races(&heap, &worker, &kernel, &hub, b);

    // A send between the empty receive and watch registration is observed.
    uint64_t revision = read.f_revision;
    assert(mica_task_mailbox_send(&hub, &worker, empty.f_sender, integer(1)) == OK);
    struct mica_TaskMailboxChange changed = mica_task_mailbox_changed(&hub, &worker, revision);
    assert(changed.f_status == OK && changed.f_revision != revision);
    struct mailbox_watch watch = {.heap = &heap, .hub = &hub, .revision = changed.f_revision};
    assert(mica_memory_leave(&worker));
    assert(!pthread_create(&thread, NULL, mailbox_watch_thread, &watch));
    while (!atomic_load(&watch.started)) sched_yield();
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_memory_leave(&worker));
    assert(!pthread_join(thread, NULL));
    assert(watch.result.f_status == CLOSED);
    assert(mica_memory_enter(&worker));
    assert(mica_task_mailbox_send(&hub, &worker, a.f_sender, integer(1)) == CLOSED);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_mailbox_close(&other, &worker, remote.f_receiver) == OK);
    struct mica_TaskMailboxCaps replaced = mica_task_mailbox_create(&other, &worker, 1);
    assert(replaced.f_status == OK && replaced.f_sender != remote.f_sender);
    assert(mica_task_mailbox_send(&other, &worker, remote.f_sender, integer(1)) == PERMISSION);
    assert(mica_task_mailbox_hub_shutdown(&other, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&other, &worker));
    assert(mica_task_resources_stop());
    assert(mica_task_resources_start());
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    struct mica_TaskMailboxCaps restarted = mica_task_mailbox_create(&hub, &worker, 1);
    assert(restarted.f_status == OK && restarted.f_sender != replaced.f_sender);
    assert(mica_task_mailbox_close(&hub, &worker, restarted.f_receiver) == OK);
    // Namespace exhaustion and revision exhaustion cannot reuse capabilities
    // or make a watcher mistake new activity for its previous observation.
    mica_task_resources_state()->f_next = UINT64_C(0x007fffffffffffff);
    struct mica_TaskMailboxCaps last = mica_task_mailbox_create(&hub, &worker, 1);
    assert(last.f_status == OK);
    hub.f_revision = UINT64_MAX;
    assert(mica_task_mailbox_changed(&hub, &worker, UINT64_MAX).f_status == LIMIT);
    assert(mica_task_mailbox_send(&hub, &worker, last.f_sender, integer(1)) == LIMIT);
    assert(mica_task_mailbox_close(&hub, &worker, last.f_receiver) == OK);
    hub.f_revision = 0;
    assert(mica_task_mailbox_create(&hub, &worker, 1).f_status == LIMIT);
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_resources_stop());
    assert(mica_memory_root_pop(&worker, &receivers));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}
