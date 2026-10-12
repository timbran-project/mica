// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void subscription_requests(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction competing = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot pattern = {0}, delivery = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_kernel_begin(&kernel, &worker, &competing, integer(7)) == OK);
    assert(mica_kernel_declare(&competing, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&competing) == OK && mica_kernel_end(&competing));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&hub, &worker, 4);
    assert(caps.f_status == OK);
    assert(mica_memory_root_push(&worker, &pattern));
    // A heap string in the pattern must survive both staging and publication.
    uint8_t bytes[128];
    memset(bytes, 's', sizeof(bytes));
    struct mica_ValueResult text = mica_value_string(&worker, bytes, sizeof(bytes));
    assert(text.f_ok);
    mica_type_Value cells[] = {text.f_value, integer(0)};
    mica_value_root_set(&pattern, mailbox_receivers(&worker, cells, 2));
    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_receiver,
        1, 1, mica_value_root_get(&pattern), 1, false, false, 0, 2).f_status == PERMISSION);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        0, 1, mica_value_root_get(&pattern), 1, false, false, 0, 2).f_status == SCHEMA);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 99, mica_value_root_get(&pattern), 1, false, false, 0, 2).f_status == UNKNOWN);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 1, integer(0), 1, false, false, 0, 2).f_status == ARITY);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 1, mica_value_root_get(&pattern), 4, false, false, 0, 2).f_status == SCHEMA);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 1, mica_value_root_get(&pattern), 1, true, true, 0, 2).f_status == SCHEMA);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 1, mica_value_root_get(&pattern), 1, false, false, 0, 0).f_status == SCHEMA);
    assert(mica_task_subscription_cancel(&worker, &attempt, integer(0)) == PERMISSION);
    assert(!attempt.f_effects.f_pointer);

    struct mica_TaskSubscriptionResult registration = mica_task_subscription_request(&worker, &attempt,
        &hub, caps.f_sender, 1, 1, mica_value_root_get(&pattern), 1, false, true, UINT64_MAX, 2);
    assert(registration.f_status == OK && mica_value_as_capability(registration.f_capability).f_ok);
    mica_type_Value discarded = registration.f_capability;
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskEffect *effect = (void *)attempt.f_effects.f_pointer;
    assert(effect && effect->f_subscription && !effect->f_next);
    assert(effect->f_subscription->f_cursor == UINT64_MAX);
    assert(effect->f_subscription->f_capability == discarded);
    // A real conflict discards this request and its identity, without opening
    // a stream or consuming any mailbox slots.
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
    assert(mica_kernel_begin(&kernel, &worker, &competing, integer(7)) == OK);
    assert(mica_kernel_write(&competing, 1, pair(&worker, 1, 3), true) == OK);
    assert(mica_kernel_commit(&competing) == OK && mica_kernel_end(&competing));
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == CONFLICT);
    assert(mica_task_detach(&task, &worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(!attempt.f_effects.f_pointer);
    assert(mica_task_attach(&task, &worker, &attempt) == OK);
    registration = mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        2, 1, mica_value_root_get(&pattern), 1, true, false, 0, 2);
    assert(registration.f_status == OK && registration.f_capability != discarded);
    assert(mica_task_stage(&worker, &attempt.f_effects, 1, integer(7), integer(42)) == OK);
    assert(mica_task_subscription_cancel(&worker, &attempt, registration.f_capability) == OK);
    for (uint64_t i = 0; i <= hub.f_mask; ++i) {
        for (struct mica_TaskMailbox *mailbox = hub.f_buckets[i]; mailbox; mailbox = mailbox->f_next)
            assert(!mailbox->f_streams && !mailbox->f_empty_streams && !mailbox->f_count);
    }
    assert(mica_task_request(&attempt, 1) == OK);
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == OK);
    assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    effect = boundary->f_effects;
    struct mica_TaskSubscriptionRequest *request = effect->f_subscription;
    assert(effect->f_kind == 3 && request && !request->f_cancel);
    assert(request->f_capability == registration.f_capability && request->f_sender == caps.f_sender);
    assert(request->f_subject == 2 && request->f_relation == 1 && request->f_mask == 1);
    assert(request->f_initial && !request->f_resume && request->f_capacity == 2);
    assert(mica_value_string_length(mailbox_list(request->f_pattern, 2)->f_data[0]).f_number == sizeof(bytes));
    effect = effect->f_next;
    assert(effect->f_kind == 1 && !effect->f_subscription && effect->f_payload == integer(42));
    effect = effect->f_next;
    assert(effect->f_kind == 3 && effect->f_subscription->f_cancel && !effect->f_next);
    assert(effect->f_subscription->f_capability == registration.f_capability);
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(mica_task_release(&task, &worker));

    // A catalogue registration has no relation binding and requires the
    // task's root authority. Aborting it must leave no active stream.
    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    registration = mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        3, 0, mailbox_receivers(&worker, NULL, 0), 0, true, false, 0, 2);
    assert(registration.f_status == OK);
    assert(mica_task_finish(&task, &worker, &attempt, CANCELLED) == CANCELLED);
    assert(!task.f_committed && !task.f_prepared.f_registration);
    assert(mica_task_release(&task, &worker));

    // Registration derives authority from the image, never from its sender
    // capability or an ordinary actor argument.
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(8), integer(0), integer(0), NULL, NULL, 3, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8, NULL) == OK);
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        3, 0, mailbox_receivers(&worker, NULL, 0), 0, true, false, 0, 2).f_status == PERMISSION);
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 1, mica_value_root_get(&pattern), 1, true, false, 0, 2).f_status == PERMISSION);
    assert(!attempt.f_effects.f_pointer);
    assert(mica_memory_leave(&worker));
    assert(mica_task_cancel(&task));
    assert(mica_memory_enter(&worker));
    assert(mica_task_subscription_cancel(&worker, &attempt, registration.f_capability) == CANCELLED);
    assert(mica_task_finish(&task, &worker, &attempt, CANCELLED) == CANCELLED);
    assert(mica_task_release(&task, &worker));

    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    attempt.f_remaining = 0;
    assert(mica_task_subscription_request(&worker, &attempt, &hub, caps.f_sender,
        1, 1, mica_value_root_get(&pattern), 1, false, false, 0, 2).f_status == LIMIT);
    assert(!attempt.f_effects.f_pointer);
    assert(mica_task_finish(&task, &worker, &attempt, LIMIT) == LIMIT);
    assert(mica_task_release(&task, &worker));
    assert(mica_memory_root_pop(&worker, &pattern));
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && !heap.f_retained);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    assert(mica_task_resources_stop());
}
