// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

// This fixture drives the generated queue operations. Integer payloads stand
// for row batches and markers; subscription message encoding is a separate layer.
static void stream_receive(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
                           struct mica_TaskMailboxHub *hub, mica_type_Value receiver,
                           const int64_t *expected, uint64_t count) {
    struct mica_TaskControl task = {0};
    struct mica_MemoryRoot receivers = {0};
    assert(mica_memory_root_push(worker, &receivers));
    mica_value_root_set(&receivers, mailbox_receivers(worker, &receiver, 1));
    mailbox_task(worker, kernel, &task, true);
    struct mica_TaskMailboxRead result = mica_task_mailbox_receive(hub, worker,
        mica_value_root_get(&receivers), &task, task.f_generation);
    assert(result.f_status == OK && result.f_count == count);
    assert(task.f_state == RUNNABLE);
    assert(mica_memory_safepoint(worker, true));
    struct mica_TaskImage *image = (void *)task.f_image.f_pointer;
    const struct mica_HeapList *groups = mailbox_list(image->f_input, 1);
    const struct mica_HeapList *group = mailbox_list(groups->f_data[0], 2);
    assert(group->f_data[0] == receiver);
    const struct mica_HeapList *messages = mailbox_list(group->f_data[1], count);
    for (uint64_t i = 0; i < count; ++i) assert(messages->f_data[i] == integer(expected[i]));
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &receivers));
}
static void stream_send(struct mica_TaskMailboxHub *hub, struct mica_MemoryWorker *worker,
                        mica_type_Value sender, uint64_t id, int64_t payload, bool terminal, bool replaced) {
    struct mica_TaskMailboxDelivery result = mica_task_mailbox_stream_send(hub, worker,
        sender, id, integer(payload), integer(99), terminal);
    assert(result.f_status == OK && result.f_replaced == replaced);
}
struct stream_writer {
    struct mica_MemoryHeap *heap;
    struct mica_TaskMailboxHub *hub;
    mica_type_Value sender;
    struct mica_TaskMailboxDelivery result;
};
static void *stream_write_thread(void *opaque) {
    struct stream_writer *job = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, job->heap, 4096));
    assert(mica_memory_enter(&worker));
    job->result = mica_task_mailbox_stream_send(job->hub, &worker, job->sender, 4, integer(44), integer(99), false);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void mailbox_streams(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 2) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&hub, &worker, 5);
    assert(caps.f_status == OK);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_receiver, 1, 2) == PERMISSION);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 0, 2) == SCHEMA);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 1, 0) == SCHEMA);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    atomic_store(&allocation_budget, 0);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 1, 2) == OOM);
    atomic_store(&allocation_budget, -1);
#endif
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 1, 2) == OK);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 1, 2) == SCHEMA);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 2, 2) == OK);
    assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(10)) == OK);
    stream_send(&hub, &worker, caps.f_sender, 1, 11, false, false);
    assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(20)) == OK);
    stream_send(&hub, &worker, caps.f_sender, 1, 12, false, false);
    assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(30)) == LIMIT);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 3, 1) == LIMIT);
    stream_send(&hub, &worker, caps.f_sender, 2, 21, false, false);
#ifdef MICA_MEMORY_TEST_ALLOCATOR
    struct mica_MemoryRoot marker = {0};
    assert(mica_memory_root_push(&worker, &marker));
    uint8_t *bytes = malloc(200000);
    assert(bytes);
    memset(bytes, 'x', 200000);
    struct mica_ValueResult text = mica_value_string(&worker, bytes, 200000);
    free(bytes);
    assert(text.f_ok);
    mica_value_root_set(&marker, text.f_value);
    atomic_store(&allocation_budget, 0);
    struct mica_TaskMailboxDelivery failed = mica_task_mailbox_stream_send(&hub, &worker,
        caps.f_sender, 1, integer(13), mica_value_root_get(&marker), false);
    atomic_store(&allocation_budget, -1);
    assert(failed.f_status == OOM && !failed.f_replaced);
    assert(mica_memory_root_pop(&worker, &marker));
#endif
    // Replacement removes only stream 1; unrelated FIFO messages survive.
    stream_send(&hub, &worker, caps.f_sender, 1, 13, false, true);
    assert(mica_task_mailbox_stream_send(&hub, &worker, caps.f_sender, 1, integer(14), integer(99), false).f_status == CLOSED);
    // Explicit revocation uses its own marker, not the overflow marker.
    stream_send(&hub, &worker, caps.f_sender, 2, 88, true, true);
    stream_receive(&worker, &kernel, &hub, caps.f_receiver, (int64_t[]){10, 20, 99, 88}, 4);
    assert(mica_task_mailbox_stream_close(&hub, &worker, caps.f_sender, 1) == OK);
    assert(mica_task_mailbox_stream_close(&hub, &worker, caps.f_sender, 2) == OK);
    assert(mica_task_mailbox_stream_close(&hub, &worker, caps.f_sender, 2) == PERMISSION);

    // Cancelling a stream keeps already delivered messages in the mailbox.
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 3, 1) == OK);
    stream_send(&hub, &worker, caps.f_sender, 3, 33, false, false);
    assert(mica_task_mailbox_stream_close(&hub, &worker, caps.f_sender, 3) == OK);
    for (int64_t i = 0; i < 4; ++i) assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(i)) == OK);
    assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(4)) == LIMIT);
    stream_receive(&worker, &kernel, &hub, caps.f_receiver, (int64_t[]){33, 0, 1, 2, 3}, 5);

    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 4, 1) == OK);
    struct stream_writer jobs[2] = {{.heap = &heap, .hub = &hub, .sender = caps.f_sender},
                                    {.heap = &heap, .hub = &hub, .sender = caps.f_sender}};
    pthread_t threads[2];
    assert(mica_memory_leave(&worker));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_create(&threads[i], NULL, stream_write_thread, &jobs[i]));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_memory_enter(&worker));
    assert(jobs[0].result.f_status == OK && jobs[1].result.f_status == OK);
    assert(jobs[0].result.f_replaced != jobs[1].result.f_replaced);
    stream_receive(&worker, &kernel, &hub, caps.f_receiver, (int64_t[]){99}, 1);
    // Closing the mailbox owns all remaining native stream records.
    assert(mica_task_mailbox_close(&hub, &worker, caps.f_receiver) == OK);
    // Global capacity can overflow before the stream's own budget is reached.
    caps = mica_task_mailbox_create(&hub, &worker, 2);
    assert(caps.f_status == OK);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 6, 5) == OK);
    assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(1)) == OK);
    stream_send(&hub, &worker, caps.f_sender, 6, 66, false, false);
    stream_send(&hub, &worker, caps.f_sender, 6, 67, false, true);
    stream_receive(&worker, &kernel, &hub, caps.f_receiver, (int64_t[]){1, 99}, 2);
    assert(mica_task_mailbox_close(&hub, &worker, caps.f_receiver) == OK);
    caps = mica_task_mailbox_create(&hub, &worker, 1);
    assert(caps.f_status == OK);
    assert(mica_task_mailbox_stream_open(&hub, &worker, caps.f_sender, 5, 1) == OK);
    assert(mica_task_mailbox_send(&hub, &worker, caps.f_sender, integer(1)) == LIMIT);
    stream_send(&hub, &worker, caps.f_sender, 5, 88, true, true);
    stream_receive(&worker, &kernel, &hub, caps.f_receiver, (int64_t[]){88}, 1);
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    assert(mica_task_resources_stop());
}
