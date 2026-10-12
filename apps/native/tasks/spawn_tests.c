// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

struct spawn_fixture {
    struct call_fixture call;
    struct mica_TaskMailboxHub hub;
    struct mica_TaskScheduler scheduler;
    struct mica_TaskSpawnProbe probe;
    struct mica_TaskServices services;
    struct mica_TaskRuntime runtime;
};
static void spawn_fixture_init(struct spawn_fixture *f, uint64_t capacity) {
    call_fixture_init(&f->call, mica_task_test_call_value);
    struct mica_MemoryWorker *worker = &f->call.worker;
    assert(mica_task_mailbox_hub_init(&f->hub, worker, 2) == OK);
    assert(mica_task_scheduler_init(&f->scheduler, worker, &f->hub, capacity) == OK);
    f->probe.f_mutex = mica_foreign_memory_sync_create();
    assert(f->probe.f_mutex);
    f->runtime = (struct mica_TaskRuntime){.f_symbols = &f->call.symbols,
        .f_mailboxes = &f->hub, .f_spawn = &f->probe.f_configuration, .f_mailbox_capacity = 64};
    assert(mica_task_spawn_context_init(&f->probe.f_configuration, worker, &f->scheduler,
        mica_task_test_call_vocabulary(), 702, 703, 701, 704,
        mica_value_root_get(&f->call.prototypes), 10000, 3, 8, &f->runtime) == OK);
    f->services = (struct mica_TaskServices){.f_context = (void *)&f->probe,
        .f_emit = mica_task_test_spawn_emit, .f_spawn = mica_task_test_spawn_service};
    ((struct mica_TaskImage *)f->call.image.f_pointer)->f_entry = mica_task_test_spawn;
}
static void spawn_parent(struct spawn_fixture *f, struct mica_TaskControl *task) {
    assert(mica_task_init(task, &f->call.worker, &f->call.kernel, &f->call.image,
        10000, 3, 8, &f->runtime) == OK);
}
static void spawn_fixture_release(struct spawn_fixture *f) {
    struct mica_MemoryWorker *worker = &f->call.worker;
    assert(mica_memory_leave(worker));
    mica_task_scheduler_stop(&f->scheduler);
    assert(mica_memory_enter(worker));
    assert(mica_task_scheduler_release(&f->scheduler, worker) == OK);
    assert(mica_task_spawn_context_release(&f->probe.f_configuration, worker) == OK);
    assert(mica_task_mailbox_hub_shutdown(&f->hub, worker) == OK);
    assert(mica_task_mailbox_hub_release(&f->hub, worker));
    mica_foreign_memory_sync_destroy(f->probe.f_mutex);
    call_fixture_release(&f->call);
}
static void spawn_publish(struct spawn_fixture *f, struct mica_TaskControl *parent,
        struct mica_TaskAttempt *attempt) {
    assert(mica_task_prepare(parent, &f->call.worker, attempt) == OK);
    assert(mica_task_commit(parent, &f->call.worker, attempt) == OK);
    assert(mica_task_finish(parent, &f->call.worker, attempt, OK) == OK);
}
static void spawn_child(struct spawn_fixture *f, uint64_t expected) {
    struct mica_MemoryWorker *worker = &f->call.worker;
    struct mica_TaskQueueResult selected = mica_task_scheduler_take(&f->scheduler, worker);
    assert(selected.f_status == OK);
    struct mica_TaskControl *child = f->scheduler.f_slots[selected.f_id].f_task;
    assert(child->f_runtime == &f->runtime);
    assert(mica_memory_safepoint(worker, true));
    assert(mica_task_execute(child, worker) == expected);
    struct mica_MemoryRoot boundary = {0};
    if (expected == OK) {
        assert(mica_memory_acquire(worker, &child->f_prepared, &boundary));
        struct mica_TaskBoundary *b = (void *)boundary.f_pointer;
        assert(b->f_kind == 1 && b->f_image->f_input == integer(42));
        assert(!b->f_image->f_pending && !b->f_image->f_caller);
        assert(b->f_effects && b->f_effects->f_payload == integer(42) && !b->f_effects->f_next);
        assert(mica_task_dispatch(child, worker, &f->services) == OK);
    } else {
        assert(!child->f_committed && child->f_state == FAILED);
    }
    assert(mica_task_scheduler_settle(&f->scheduler, worker, selected.f_id, (void *)boundary.f_pointer) == OK);
    if (expected == OK) assert(mica_memory_root_pop(worker, &boundary));
    assert(!mica_task_scheduler_inspect(&f->scheduler, worker).f_count);
}
static uint64_t spawn_reply(struct spawn_fixture *f, struct mica_TaskControl *parent) {
    assert(parent->f_state == RUNNABLE);
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_acquire(&f->call.worker, &parent->f_image, &image));
    struct mica_IntResult reply = mica_value_as_int(((struct mica_TaskImage *)image.f_pointer)->f_input);
    assert(reply.f_ok && reply.f_number > 0);
    uint64_t id = reply.f_number;
    assert(mica_memory_root_pop(&f->call.worker, &image));
    return id;
}
static void spawn_transactions(void) {
    struct spawn_fixture f = {0};
    spawn_fixture_init(&f, 2);
    struct mica_TaskControl parent = {0};
    struct mica_TaskAttempt attempt = {0};
    spawn_parent(&f, &parent);
    assert(mica_task_claim(&parent, &f.call.worker, &attempt) == OK);
    assert(mica_task_test_spawn(&attempt) == OK);
    assert(attempt.f_effects.f_pointer && !f.scheduler.f_count);
    assert(mica_task_finish(&parent, &f.call.worker, &attempt, CANCELLED) == CANCELLED);
    assert(!f.scheduler.f_count && mica_task_dispatch(&parent, &f.call.worker, &f.services) == CLOSED);
    assert(mica_task_release(&parent, &f.call.worker));

    spawn_parent(&f, &parent);
    assert(mica_task_claim(&parent, &f.call.worker, &attempt) == OK);
    // A real read conflict discards the request. Only the replay is admitted.
    mica_type_Value cells[] = {dispatch_identity(601), dispatch_identity(8001)}, rows[2];
    assert(mica_kernel_scan(&attempt.f_transaction, 704, mailbox_receivers(&f.call.worker, cells, 2), 0, 0, rows, 2).f_status == OK);
    assert(mica_task_test_spawn(&attempt) == OK);
    struct mica_KernelTransaction rival = {0};
    assert(mica_kernel_begin(&f.call.kernel, &f.call.worker, &rival, integer(7)) == OK);
    call_binding(&rival, 8001, false);
    call_binding(&rival, 8002, true);
    call_body(&rival, 8002, mica_task_test_call_value, NULL);
    assert(mica_kernel_commit(&rival) == OK && mica_kernel_end(&rival));
    assert(mica_kernel_commit(&attempt.f_transaction) == CONFLICT);
    assert(!f.scheduler.f_count);
    assert(mica_task_detach(&parent, &f.call.worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(mica_task_attach(&parent, &f.call.worker, &attempt) == OK);
    assert(!attempt.f_effects.f_pointer && attempt.f_runtime == &f.runtime);
    assert(mica_task_test_spawn(&attempt) == OK);
    spawn_publish(&f, &parent, &attempt);
    assert(mica_memory_safepoint(&f.call.worker, true));
    assert(!f.scheduler.f_count);
    assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == OK);
    uint64_t id = spawn_reply(&f, &parent);
    assert(f.scheduler.f_count == 1 && mica_task_scheduler_find(&f.scheduler, id));
    assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == CLOSED);
    spawn_child(&f, OK);
    assert(mica_task_execute(&parent, &f.call.worker) == OK);
    assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == OK);
    assert(f.probe.f_emitted == 2 && mica_task_release(&parent, &f.call.worker));
    spawn_fixture_release(&f);
}

static void spawn_failures(void) {
    // Every injected allocation failure leaves either no child or exactly one
    // independently owned child. A failed reply must never replay admission.
    bool failed_before = false, failed_after = false, succeeded = false;
    for (unsigned managed = 0; managed < 2; ++managed) {
        succeeded = false;
        for (int64_t fail = 0; fail < (managed ? 4096 : 4) && !succeeded; fail += managed ? 32 : 1) {
            struct spawn_fixture f = {0};
            spawn_fixture_init(&f, 1);
            struct mica_TaskControl parent = {0};
            spawn_parent(&f, &parent);
            assert(mica_task_execute(&parent, &f.call.worker) == OK);
            struct mica_KernelBudget budget = {UINT64_MAX, UINT64_MAX, UINT64_MAX, (uint64_t)fail, OK, NULL};
            struct mica_MemoryControl control = {.f_context = (void *)&budget, .f_check = mica_kernel_memory_check};
            if (managed) mica_memory_control_push(&f.call.worker, &control);
            else atomic_store(&allocation_budget, fail);
            uint64_t status = mica_task_dispatch(&parent, &f.call.worker, &f.services);
            if (managed) mica_memory_control_pop(&f.call.worker);
            else atomic_store(&allocation_budget, -1);
            assert(status == OK || status == OOM);
            assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == CLOSED);
            if (status == OK) {
                succeeded = true;
                assert(f.scheduler.f_count == 1 && spawn_reply(&f, &parent));
            } else if (f.scheduler.f_count) {
                failed_after = true;
                assert(f.scheduler.f_count == 1 && parent.f_state == FAILED);
            } else {
                failed_before = true;
                assert(parent.f_state == FAILED);
            }
            if (f.scheduler.f_count) spawn_child(&f, OK);
            assert(mica_task_release(&parent, &f.call.worker));
            spawn_fixture_release(&f);
        }
        assert(succeeded);
    }
    assert(failed_before && failed_after && succeeded);
}

static void spawn_concurrent(void) {
    struct spawn_fixture f = {0};
    spawn_fixture_init(&f, 8);
    struct mica_TaskControl parents[3] = {{0}};
    uint64_t ids[3];
    for (unsigned i = 0; i < 3; ++i) {
        spawn_parent(&f, &parents[i]);
        struct mica_TaskAdmission admitted = mica_task_scheduler_submit(&f.scheduler, &f.call.worker, &parents[i], false, 0);
        assert(admitted.f_status == OK);
        ids[i] = admitted.f_id;
    }
    assert(mica_memory_leave(&f.call.worker));
    struct scheduler_runner runners[2] = {{0}};
    pthread_t threads[2];
    for (unsigned i = 0; i < 2; ++i) {
        runners[i] = (struct scheduler_runner){.heap = &f.call.heap, .scheduler = &f.scheduler, .services = &f.services};
        assert(!pthread_create(&threads[i], NULL, scheduler_thread, &runners[i]));
    }
    assert(mica_memory_enter(&f.call.worker));
    while (mica_task_scheduler_inspect(&f.scheduler, &f.call.worker).f_remaining) {
        assert(mica_memory_safepoint(&f.call.worker, true));
    }
    assert(mica_memory_leave(&f.call.worker));
    mica_task_scheduler_stop(&f.scheduler);
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_memory_enter(&f.call.worker));
    assert(f.probe.f_emitted == 6);
    assert(runners[0].result.f_status == OK && runners[1].result.f_status == OK);
    assert(runners[0].result.f_taken + runners[1].result.f_taken == 9);
    for (unsigned i = 0; i < 3; ++i) {
        assert(parents[i].f_state == COMPLETED);
        assert(mica_task_scheduler_withdraw(&f.scheduler, &f.call.worker, ids[i]) == OK);
        assert(mica_task_release(&parents[i], &f.call.worker));
    }
    spawn_fixture_release(&f);
}

static void spawn_late_resolution(bool revoke) {
    struct spawn_fixture f = {0};
    spawn_fixture_init(&f, 2);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f.call.kernel, &f.call.worker, &tx, integer(7)) == OK);
    if (revoke) {
        assert(mica_kernel_declare(&tx, 11, 11, 1, 0, 0, 0) == OK);
        assert(mica_kernel_declare(&tx, 12, 12, 2, 0, 0, 0) == OK);
        assert(mica_kernel_declare(&tx, 13, 13, 2, 0, 0, 0) == OK);
        mica_type_Value owner = integer(7), right[] = {integer(8), f.call.selector};
        assert(mica_kernel_write(&tx, 11, mailbox_receivers(&f.call.worker, &owner, 1), true) == OK);
        assert(mica_kernel_write(&tx, 12, mailbox_receivers(&f.call.worker, right, 2), true) == OK);
        for (uint64_t id = 701; id <= 704; ++id)
            assert(mica_kernel_write(&tx, 13, permission(&f.call.worker, 8, id), true) == OK);
        assert(mica_kernel_set_authority_policy(&tx, 11, 0, 0, 13, 0, 0, 12) == OK);
    } else {
        call_body(&tx, 8001, mica_task_test_call_wait, NULL);
    }
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    struct mica_TaskImage *image = (void *)f.call.image.f_pointer;
    image->f_tenant = integer(revoke ? 8 : 7);
    image->f_input = integer(1); // One nanosecond: still enters the delayed queue.
    struct mica_TaskControl parent = {0};
    spawn_parent(&f, &parent);
    assert(mica_task_execute(&parent, &f.call.worker) == OK);
    assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == OK);
    uint64_t id = spawn_reply(&f, &parent);
    struct mica_TaskSlot *slot = mica_task_scheduler_find(&f.scheduler, id);
    assert(slot && slot->f_state == 5 && !f.scheduler.f_queue.f_count);
    assert(mica_kernel_begin(&f.call.kernel, &f.call.worker, &tx, integer(7)) == OK);
    if (revoke) {
        mica_type_Value right[] = {integer(8), f.call.selector};
        assert(mica_kernel_write(&tx, 12, mailbox_receivers(&f.call.worker, right, 2), false) == OK);
    } else {
        call_binding(&tx, 8001, false);
        call_binding(&tx, 8002, true);
        call_body(&tx, 8002, mica_task_test_call_value, NULL);
        assert(mica_kernel_program_remove(&tx, 8001) == OK);
    }
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_memory_safepoint(&f.call.worker, true));
    assert(mica_task_scheduler_poll(&f.scheduler, &f.call.worker).f_status == OK);
    assert(slot->f_state == 1);
    spawn_child(&f, revoke ? PERMISSION : OK);
    assert(mica_task_release(&parent, &f.call.worker));
    spawn_fixture_release(&f);
}

static void spawn_admission_failures(void) {
    for (unsigned closed = 0; closed < 2; ++closed) {
        struct spawn_fixture f = {0};
        spawn_fixture_init(&f, 1);
        struct mica_TaskControl parent = {0}, other = {0};
        spawn_parent(&f, &parent);
        assert(mica_task_execute(&parent, &f.call.worker) == OK);
        uint64_t id = 0;
        if (closed) {
            assert(mica_memory_leave(&f.call.worker));
            mica_task_scheduler_stop(&f.scheduler);
            assert(mica_memory_enter(&f.call.worker));
        } else {
            spawn_parent(&f, &other);
            struct mica_TaskAdmission admitted = mica_task_scheduler_submit(&f.scheduler, &f.call.worker, &other, false, 0);
            assert(admitted.f_status == OK);
            id = admitted.f_id;
        }
        assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == (closed ? CLOSED : LIMIT));
        assert(parent.f_state == FAILED);
        assert(f.scheduler.f_count == (closed ? 0 : 1));
        assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == CLOSED);
        assert(mica_task_release(&parent, &f.call.worker));
        if (!closed) {
            assert(mica_memory_leave(&f.call.worker));
            mica_task_scheduler_stop(&f.scheduler);
            assert(mica_memory_enter(&f.call.worker));
            // Stop cancels controls; the worker still settles queued slots
            // before a caller can withdraw its terminal, unowned control.
            struct mica_TaskQueueResult queued = mica_task_scheduler_take(&f.scheduler, &f.call.worker);
            assert(queued.f_status == OK && other.f_state == TASK_CANCELLED);
            assert(mica_task_scheduler_settle(&f.scheduler, &f.call.worker, queued.f_id, NULL) == OK);
            assert(mica_task_scheduler_withdraw(&f.scheduler, &f.call.worker, id) == OK);
            assert(mica_task_release(&other, &f.call.worker));
        }
        spawn_fixture_release(&f);
    }
}

static void spawn_cancel_and_limits(void) {
    for (unsigned mode = 0; mode < 3; ++mode) {
        struct spawn_fixture f = {0};
        spawn_fixture_init(&f, 1);
        if (mode == 1) f.probe.f_configuration.f_remaining = 8;
        struct mica_TaskControl parent = {0};
        spawn_parent(&f, &parent);
        assert(mica_task_execute(&parent, &f.call.worker) == OK);
        if (mode == 0) assert(mica_task_cancel(&parent));
        if (mode == 2) {
            struct mica_KernelTransaction tx = {0};
            assert(mica_kernel_begin(&f.call.kernel, &f.call.worker, &tx, integer(7)) == OK);
            mica_type_Value selector[] = {dispatch_identity(601), f.call.selector};
            assert(mica_kernel_write(&tx, 702, mailbox_receivers(&f.call.worker, selector, 2), false) == OK);
            assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
        }
        assert(mica_task_dispatch(&parent, &f.call.worker, &f.services) == (mode == 0 ? CLOSED : OK));
        assert(f.scheduler.f_count == 1);
        if (mode == 0) assert(parent.f_state == TASK_CANCELLED);
        // Cancellation after publication cannot retract the child's admission.
        spawn_child(&f, mode == 0 ? OK : mode == 1 ? LIMIT : 15);
        assert(mica_task_release(&parent, &f.call.worker));
        spawn_fixture_release(&f);
    }
}
