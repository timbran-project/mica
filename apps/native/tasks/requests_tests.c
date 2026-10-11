// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static uint64_t stage_wait_request(struct mica_TaskMailboxHub *hub,
        struct mica_MemoryWorker *worker, struct mica_TaskAttempt *attempt,
        mica_type_Value receivers, uint64_t timeout, bool timer) {
    if (timer) return mica_task_suspend_request(worker, attempt, timeout);
    return mica_task_mailbox_request(hub, worker, attempt, receivers, timeout);
}

static void wait_requests(bool timer_wait) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction competing = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot receivers = {0}, delivery = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &competing, integer(7)) == OK);
    assert(mica_kernel_declare(&competing, 1, 1, 2, 1, 1, 0) == OK);
    assert(mica_kernel_commit(&competing) == OK);
    assert(mica_kernel_end(&competing));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    struct mica_TaskMailboxCaps caps = mica_task_mailbox_create(&hub, &worker, 4);
    assert(caps.f_status == OK);
    assert(mica_memory_root_push(&worker, &receivers));
    mica_value_root_set(&receivers, mailbox_receivers(&worker, &caps.f_receiver, 1));
    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_task_mailbox_request(&hub, &worker, &attempt, integer(1), 0) == SCHEMA);
    assert(!attempt.f_request.f_pointer && !attempt.f_boundary);
    assert(mica_task_suspend_request(&worker, &attempt, 15) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskWaitRequest *timer = (void *)attempt.f_request.f_pointer;
    assert(timer && !timer->f_mailbox && timer->f_timeout == 15);
    assert(mica_value_is_unit(timer->f_reply));
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 2), true) == OK);
    assert(stage_wait_request(&hub, &worker, &attempt, mica_value_root_get(&receivers), 15, timer_wait) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskWaitRequest *request = (void *)attempt.f_request.f_pointer;
    assert(request && request->f_timeout == 15 && attempt.f_boundary == 2);
    assert(request->f_mailbox == !timer_wait);
    if (timer_wait) {
        assert(mica_value_is_unit(request->f_reply));
    } else {
        assert(mailbox_list(request->f_reply, 0));
        assert(mailbox_list(request->f_receivers, 1)->f_data[0] == caps.f_receiver);
    }

    // A real conflicting publication invalidates both the draft and its wait.
    assert(mica_kernel_begin(&kernel, &worker, &competing, integer(7)) == OK);
    assert(mica_kernel_write(&competing, 1, pair(&worker, 1, 3), true) == OK);
    assert(mica_kernel_commit(&competing) == OK);
    assert(mica_kernel_end(&competing));
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == CONFLICT);
    assert(!task.f_committed && !task.f_prepared.f_registration && !task.f_committed_view.f_registration);
    assert(mica_task_detach(&task, &worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(!attempt.f_request.f_pointer && !attempt.f_boundary);
    assert(mica_task_attach(&task, &worker, &attempt) == OK);
    assert(mica_memory_safepoint(&worker, true));
    assert(stage_wait_request(&hub, &worker, &attempt, mica_value_root_get(&receivers), UINT64_MAX, timer_wait) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 3), false) == OK);
    assert(mica_kernel_write(&attempt.f_transaction, 1, pair(&worker, 1, 4), true) == OK);
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == OK);
    assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    assert(boundary->f_kind == 2 && boundary->f_request);
    assert(boundary->f_request->f_timeout == UINT64_MAX);
    assert(boundary->f_request->f_mailbox == !timer_wait);
    if (timer_wait) assert(mica_value_is_unit(boundary->f_request->f_reply));
    else assert(mailbox_list(boundary->f_request->f_receivers, 1)->f_data[0] == caps.f_receiver);
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(task.f_state == WAITING && task.f_generation == 1);
    struct mica_TaskObservation observed = mica_task_observe(&task, &worker);
    assert(observed.f_status == OK && observed.f_state == WAITING && observed.f_committed);
    assert(mica_task_fail_wait(&task, &worker, 0, PERMISSION) == CLOSED);
    assert(mica_task_fail_wait(&task, &worker, 1, OK) == SCHEMA);
    assert(mica_task_fail_wait(&task, &worker, 1, PERMISSION) == OK);
    observed = mica_task_observe(&task, &worker);
    assert(observed.f_status == OK && observed.f_state == FAILED);
    assert(observed.f_outcome == PERMISSION && observed.f_committed);
    assert(mica_task_wake(&task, &worker, 1, integer(9)) == CLOSED);
    assert(mica_task_fail_wait(&task, &worker, 1, OOM) == CLOSED);
    assert(mica_task_release(&task, &worker));

    // Replacing a wait boundary with an ordinary boundary clears its roots.
    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(stage_wait_request(&hub, &worker, &attempt, mica_value_root_get(&receivers), 0, timer_wait) == OK);
    assert(mica_task_request(&attempt, 1) == OK);
    assert(!attempt.f_request.f_pointer);
    assert(mica_task_finish(&task, &worker, &attempt, CANCELLED) == CANCELLED);
    assert(mica_task_release(&task, &worker));

    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    attempt.f_remaining = 0;
    assert(mica_task_mailbox_request(&hub, &worker, &attempt, mica_value_root_get(&receivers), 0) == LIMIT);
    assert(!attempt.f_request.f_pointer && !attempt.f_boundary);
    assert(mica_task_finish(&task, &worker, &attempt, LIMIT) == LIMIT);
    assert(mica_task_release(&task, &worker));

    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_memory_leave(&worker));
    assert(mica_task_cancel(&task));
    assert(mica_memory_enter(&worker));
    assert(mica_task_mailbox_request(&hub, &worker, &attempt, mica_value_root_get(&receivers), 0) == CANCELLED);
    assert(mica_task_suspend_request(&worker, &attempt, 0) == CANCELLED);
    assert(!attempt.f_request.f_pointer && !attempt.f_boundary);
    assert(mica_task_finish(&task, &worker, &attempt, CANCELLED) == CANCELLED);
    assert(mica_task_release(&task, &worker));
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

static void receive_requests(void) {
    wait_requests(false);
    wait_requests(true);
}
