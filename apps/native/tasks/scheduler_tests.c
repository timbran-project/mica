// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

struct scheduler_runner {
    struct mica_MemoryHeap *heap;
    struct mica_TaskScheduler *scheduler;
    struct mica_TaskServices *services;
    bool events;
    uint64_t status;
    struct mica_TaskDrainResult result;
};
static void *scheduler_thread(void *argument) {
    struct scheduler_runner *runner = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, runner->heap, 4096));
    assert(mica_memory_enter(&worker));
    if (runner->events) runner->status = mica_task_scheduler_events(runner->scheduler, &worker);
    else runner->result = mica_task_scheduler_worker(runner->scheduler, &worker, runner->services);
    assert(!worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

enum scheduler_reply { SCHEDULER_CANCEL, SCHEDULER_EXTERNAL, SCHEDULER_MAILBOX };

static void scheduled_tasks(enum scheduler_reply reply) {
    enum { TASKS = 4, WORKERS = 2 };
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskControl tasks[TASKS] = {{0}};
    struct mica_TaskScheduler scheduler = {0}, duplicate = {0};
    struct mica_TaskMailboxCaps caps[TASKS];
    uint64_t ids[TASKS];
    struct mica_TaskSchedulerProbe *probe = mica_task_scheduler_probe();
    struct mica_TaskServices services = {.f_context = (void *)probe, .f_emit = mica_task_test_scheduler_emit};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_task_mailbox_hub_init(&hub, &worker, TASKS) == OK);
    *probe = (struct mica_TaskSchedulerProbe){.f_hub = &hub, .f_mutex = mica_foreign_memory_sync_create()};
    assert(probe->f_mutex);
    for (unsigned i = 0; i < TASKS; ++i) {
        caps[i] = mica_task_mailbox_create(&hub, &worker, 4);
        assert(caps[i].f_status == OK);
        struct mica_MemoryRoot captures = {0}, image = {0};
        assert(mica_memory_root_push(&worker, &captures));
        assert(mica_memory_root_push(&worker, &image));
        mica_value_root_set(&captures, mailbox_receivers(&worker, &caps[i].f_receiver, 1));
        int64_t timeout = i == 1 ? 0 : i == 2 ? 20000000 : -1;
        mica_type_Value environment[] = {mica_value_root_get(&captures), integer(timeout)};
        mica_value_root_set(&captures, mailbox_receivers(&worker, environment, 2));
        image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_receive, NULL,
            integer(7), mica_value_root_get(&captures), integer(0), NULL, NULL, 3, NULL);
        assert(image.f_pointer);
        assert(mica_task_init(&tasks[i], &worker, &kernel, &image, 1000, 2, 8) == OK);
        assert(mica_memory_root_pop(&worker, &image));
        assert(mica_memory_root_pop(&worker, &captures));
    }
    atomic_store(&allocation_budget, 0);
    assert(mica_task_scheduler_init(&scheduler, &worker, &hub, TASKS) == OOM);
    atomic_store(&allocation_budget, -1);
    assert(!scheduler.f_mutex && !scheduler.f_slots && !hub.f_event);
    assert(mica_task_scheduler_init(&scheduler, &worker, &hub, TASKS) == OK);
    assert(mica_task_scheduler_init(&duplicate, &worker, &hub, TASKS) == CLOSED);
    assert(!duplicate.f_mutex && !duplicate.f_slots);
    for (unsigned i = 0; i < TASKS; ++i) {
        struct mica_TaskAdmission admitted = mica_task_scheduler_submit(&scheduler, &worker, &tasks[i], false, 0);
        assert(admitted.f_status == OK && admitted.f_id);
        ids[i] = admitted.f_id;
    }
    assert(!mica_task_release(&tasks[0], &worker));
    assert(mica_task_scheduler_release(&scheduler, &worker) == CLOSED);
    assert(mica_task_mailbox_send(&hub, &worker, caps[0].f_sender, integer(42)) == OK);
    uint64_t started = mica_foreign_task_clock_now();
    assert(mica_memory_leave(&worker));
    pthread_t threads[WORKERS + 1];
    struct scheduler_runner runners[WORKERS + 1] = {0};
    for (unsigned i = 0; i <= WORKERS; ++i) {
        runners[i] = (struct scheduler_runner){.heap = &heap, .scheduler = &scheduler,
            .services = &services, .events = i == WORKERS};
        assert(!pthread_create(&threads[i], NULL, scheduler_thread, &runners[i]));
    }
    assert(mica_memory_enter(&worker));
    for (;;) {
        struct mica_TaskObservation observation = mica_task_observe(&tasks[3], &worker);
        assert(observation.f_status == OK);
        assert(observation.f_state != FAILED);
        if (observation.f_state == WAITING) break;
        assert(mica_memory_safepoint(&worker, false));
    }
    // Both queue waiters and the event thread can park while collection runs.
    assert(mica_memory_safepoint(&worker, true));
    if (reply == SCHEDULER_EXTERNAL) {
        struct mica_ValueResult empty = mica_value_list(&worker, NULL, 0);
        assert(empty.f_ok);
        assert(mica_task_scheduler_wake(&scheduler, &worker, ids[3], 1, empty.f_value) == OK);
        assert(mica_task_scheduler_wake(&scheduler, &worker, ids[3], 1, integer(99)) == CLOSED);
        assert(mica_memory_leave(&worker));
    } else if (reply == SCHEDULER_MAILBOX) {
        // Send after suspension. The hub must notify the generated event loop,
        // whether registration has finished or races this send.
        assert(mica_task_mailbox_send(&hub, &worker, caps[3].f_sender, integer(43)) == OK);
        assert(mica_memory_safepoint(&worker, true));
        assert(mica_memory_leave(&worker));
    } else {
        assert(mica_memory_leave(&worker));
        assert(mica_task_scheduler_cancel(&scheduler, ids[3]));
    }
    assert(mica_memory_enter(&worker));
    for (;;) {
        struct mica_TaskSchedulerObservation observed = mica_task_scheduler_inspect(&scheduler, &worker);
        assert(observed.f_status == OK && !observed.f_closed);
        if (!observed.f_remaining) break;
        assert(mica_memory_safepoint(&worker, false));
    }
    assert(mica_memory_leave(&worker));
    mica_task_scheduler_stop(&scheduler);
    for (unsigned i = 0; i <= WORKERS; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_foreign_task_clock_now() - started >= UINT64_C(20000000));
    uint64_t taken = 0;
    for (unsigned i = 0; i < WORKERS; ++i) {
        assert(runners[i].result.f_status == OK);
        taken += runners[i].result.f_taken;
    }
    assert(runners[WORKERS].status == OK && taken == (reply == SCHEDULER_CANCEL ? 7 : 8));
    assert(probe->f_delivered == (reply == SCHEDULER_CANCEL ? 3 : 4));
    assert(probe->f_empty == (reply == SCHEDULER_EXTERNAL ? 3 : 2));
    assert(!scheduler.f_remaining && !scheduler.f_workers && !scheduler.f_events);
    for (unsigned i = 0; i < 3; ++i) assert(tasks[i].f_state == COMPLETED && tasks[i].f_committed);
    assert(tasks[3].f_state == (reply == SCHEDULER_CANCEL ? TASK_CANCELLED : COMPLETED));
    mica_task_scheduler_stop(&scheduler);
    assert(mica_memory_enter(&worker));
    assert(mica_task_scheduler_release(&scheduler, &worker) == OK);
    for (unsigned i = 0; i < TASKS; ++i) {
        assert(mica_task_release(&tasks[i], &worker));
        mailbox_task(&worker, &kernel, &tasks[i], false);
    }
    // Stopping before any worker starts must also release reserved queue
    // entries. No worker or event loop is required to clean up cancelled work.
    assert(mica_task_scheduler_init(&scheduler, &worker, &hub, TASKS) == OK);
    for (unsigned i = 0; i < TASKS; ++i) {
        struct mica_TaskAdmission admitted = mica_task_scheduler_submit(&scheduler, &worker, &tasks[i], false, 0);
        assert(admitted.f_status == OK && admitted.f_id > ids[i]);
        ids[i] = admitted.f_id;
    }
    struct mica_TaskQueueResult selected = mica_task_scheduler_take(&scheduler, &worker);
    assert(selected.f_status == OK && selected.f_id == 0);
    struct mica_TaskAttempt attempt = {0};
    assert(mica_task_claim(&tasks[0], &worker, &attempt) == OK);
    struct mica_ValueResult emitted = mica_value_list(&worker, NULL, 0);
    assert(emitted.f_ok);
    assert(mica_task_stage(&worker, &attempt.f_effects, 1, integer(7), emitted.f_value) == OK);
    assert(mica_task_request(&attempt, 2) == OK);
    assert(mica_task_prepare(&tasks[0], &worker, &attempt) == OK);
    assert(mica_task_commit(&tasks[0], &worker, &attempt) == OK);
    // Force shutdown between publication and attempt retirement. It loses the
    // transaction cancellation race, but must still prevent a later wait.
    assert(mica_memory_leave(&worker));
    mica_task_scheduler_stop(&scheduler);
    assert(mica_memory_enter(&worker));
    assert(tasks[0].f_committed && !tasks[0].f_cancel_requested);
    assert(mica_task_finish(&tasks[0], &worker, &attempt, OK) == OK);
    struct mica_MemoryRoot boundary = {0};
    assert(mica_memory_acquire(&worker, &tasks[0].f_prepared, &boundary));
    uint64_t deliveries = probe->f_delivered;
    assert(mica_task_dispatch(&tasks[0], &worker, &services) == OK);
    assert(probe->f_delivered == deliveries + 1);
    assert(mica_task_scheduler_settle(&scheduler, &worker, 0, (void *)boundary.f_pointer) == OK);
    assert(tasks[0].f_state == TASK_CANCELLED);
    assert(mica_memory_root_pop(&worker, &boundary));
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(!mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_scheduler_release(&scheduler, &worker) == OK);
    assert(!hub.f_event);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    for (unsigned i = 0; i < TASKS; ++i) {
        assert(tasks[i].f_state == TASK_CANCELLED && tasks[i].f_committed == (i == 0));
        assert(mica_task_release(&tasks[i], &worker));
    }
    mica_foreign_memory_sync_destroy(probe->f_mutex);
    *probe = (struct mica_TaskSchedulerProbe){0};
    assert(mica_task_resources_stop());
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void scheduler_complete_task(struct mica_MemoryWorker *worker,
                                    struct mica_Kernel *kernel,
                                    struct mica_TaskControl *task) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_task_test_complete, NULL,
        integer(7), integer(0), integer(0), NULL, NULL, 3, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(task, worker, kernel, &image, 1000, 2, 8) == OK);
    assert(mica_memory_root_pop(worker, &image));
}

static struct mica_TaskSchedulerObservation scheduler_idle(
    struct mica_TaskScheduler *scheduler, struct mica_MemoryWorker *worker) {
    uint64_t deadline = mica_foreign_task_clock_now() + UINT64_C(30000000000);
    for (;;) {
        struct mica_TaskSchedulerObservation observed = mica_task_scheduler_inspect(scheduler, worker);
        assert(observed.f_status == OK && !observed.f_closed);
        if (!observed.f_remaining) return observed;
        assert(mica_foreign_task_clock_now() < deadline);
        assert(mica_memory_safepoint(worker, false));
    }
}

// Admission runs while native workers are alive. Slots can be reused, but a
// public ID never names a later occupant. C only orchestrates generated APIs.
static void scheduler_admission(void) {
    enum { CAPACITY = 2, WORKERS = 2 };
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskScheduler scheduler = {0};
    struct mica_TaskServices services = {0};
    struct mica_TaskControl first = {0}, second = {0}, rejected = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 4));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    bool initialized = false;
    for (long budget = 0; budget < 16; ++budget) {
        atomic_store(&allocation_budget, budget);
        uint64_t status = mica_task_scheduler_init(&scheduler, &worker, &hub, CAPACITY);
        atomic_store(&allocation_budget, -1);
        if (status == OK) {
            initialized = true;
            break;
        }
        assert(status == OOM);
        assert(!scheduler.f_mutex && !scheduler.f_slots && !scheduler.f_buckets);
        assert(!hub.f_event);
    }
    assert(initialized);
    assert(mica_memory_leave(&worker));
    pthread_t threads[WORKERS + 1];
    struct scheduler_runner runners[WORKERS + 1] = {0};
    for (unsigned i = 0; i <= WORKERS; ++i) {
        runners[i] = (struct scheduler_runner){.heap = &heap, .scheduler = &scheduler,
            .services = &services, .events = i == WORKERS};
        assert(!pthread_create(&threads[i], NULL, scheduler_thread, &runners[i]));
    }
    assert(mica_memory_enter(&worker));
    uint64_t join_deadline = mica_foreign_task_clock_now() + UINT64_C(30000000000);
    for (;;) {
        struct mica_TaskSchedulerObservation observed = scheduler_idle(&scheduler, &worker);
        assert(!observed.f_count);
        if (observed.f_workers == WORKERS && observed.f_events) break;
        assert(mica_foreign_task_clock_now() < join_deadline);
        assert(mica_memory_safepoint(&worker, false));
    }
    // An empty scheduler stays open, and its parked threads do not block GC.
    assert(mica_memory_safepoint(&worker, true));
    scheduler_complete_task(&worker, &kernel, &first);
    scheduler_complete_task(&worker, &kernel, &second);
    scheduler_complete_task(&worker, &kernel, &rejected);
    const uint64_t distant = UINT64_C(3600000000000);
    struct mica_TaskAdmission a = mica_task_scheduler_submit(&scheduler, &worker, &first, false, distant);
    assert(a.f_status == OK && a.f_id);
    assert(mica_task_scheduler_submit(&scheduler, &worker, &first, false, 0).f_status == CLOSED);
    struct mica_TaskAdmission b = mica_task_scheduler_submit(&scheduler, &worker, &second, false, distant);
    assert(b.f_status == OK && b.f_id > a.f_id);
    assert(mica_task_scheduler_submit(&scheduler, &worker, &rejected, false, 0).f_status == LIMIT);
    assert(!rejected.f_scheduler && mica_task_release(&rejected, &worker));
    assert(mica_task_scheduler_withdraw(&scheduler, &worker, a.f_id) == CLOSED);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_observe(&first, &worker).f_state == RUNNABLE);
    assert(mica_task_observe(&second, &worker).f_state == RUNNABLE);
    assert(mica_memory_leave(&worker));
    assert(mica_task_scheduler_cancel(&scheduler, a.f_id));
    assert(mica_task_scheduler_cancel(&scheduler, b.f_id));
    assert(mica_memory_enter(&worker));
    assert(scheduler_idle(&scheduler, &worker).f_count == CAPACITY);
    assert(mica_task_observe(&first, &worker).f_state == TASK_CANCELLED);
    assert(mica_task_observe(&second, &worker).f_state == TASK_CANCELLED);
    assert(mica_task_scheduler_withdraw(&scheduler, &worker, a.f_id) == OK);
    assert(mica_task_scheduler_withdraw(&scheduler, &worker, b.f_id) == OK);
    assert(mica_task_release(&first, &worker) && mica_task_release(&second, &worker));

    scheduler_complete_task(&worker, &kernel, &first);
    uint64_t started = mica_foreign_task_clock_now();
    struct mica_TaskAdmission delayed = mica_task_scheduler_submit(&scheduler, &worker, &first, false, UINT64_C(20000000));
    assert(delayed.f_status == OK && delayed.f_id > b.f_id);
    assert(mica_memory_leave(&worker));
    assert(!mica_task_scheduler_cancel(&scheduler, a.f_id));
    assert(!mica_task_scheduler_cancel(&scheduler, b.f_id));
    assert(mica_memory_enter(&worker));
    assert(mica_task_scheduler_wake(&scheduler, &worker, a.f_id, 1, integer(0)) == CLOSED);
    assert(scheduler_idle(&scheduler, &worker).f_count == 1);
    assert(mica_foreign_task_clock_now() - started >= UINT64_C(20000000));
    struct mica_TaskObservation completed = mica_task_observe(&first, &worker);
    assert(completed.f_state == COMPLETED && completed.f_committed && completed.f_outcome == OK);
    assert(mica_task_scheduler_withdraw(&scheduler, &worker, delayed.f_id) == OK);
    assert(mica_task_release(&first, &worker));

    // Owned controls retire automatically after execution or delayed cancellation.
    // Repeated admissions exceed capacity without retaining controls or roots.
    uint64_t previous = delayed.f_id;
    for (unsigned i = 0; i < 12; ++i) {
        struct mica_TaskControl *owned = (void *)mica_foreign_memory_platform_allocate(sizeof(*owned));
        assert(owned);
        *owned = (struct mica_TaskControl){0};
        scheduler_complete_task(&worker, &kernel, owned);
        struct mica_TaskAdmission admitted = mica_task_scheduler_submit(&scheduler, &worker, owned, true, i % 2 ? distant : 0);
        assert(admitted.f_status == OK && admitted.f_id > previous);
        previous = admitted.f_id;
        if (i % 2) {
            assert(mica_memory_leave(&worker));
            assert(mica_task_scheduler_cancel(&scheduler, admitted.f_id));
            assert(mica_memory_enter(&worker));
        }
        assert(!scheduler_idle(&scheduler, &worker).f_count);
        assert(mica_memory_safepoint(&worker, true));
        assert(mica_task_scheduler_withdraw(&scheduler, &worker, admitted.f_id) == CLOSED);
    }
    assert(mica_memory_leave(&worker));
    mica_task_scheduler_stop(&scheduler);
    for (unsigned i = 0; i <= WORKERS; ++i) assert(!pthread_join(threads[i], NULL));
    uint64_t executed = 0;
    for (unsigned i = 0; i < WORKERS; ++i) {
        assert(runners[i].result.f_status == OK);
        assert(runners[i].result.f_taken == runners[i].result.f_completed);
        executed += runners[i].result.f_taken;
    }
    assert(executed == 7 && runners[WORKERS].status == OK);
    assert(mica_memory_enter(&worker));
    scheduler_complete_task(&worker, &kernel, &rejected);
    assert(mica_task_scheduler_submit(&scheduler, &worker, &rejected, false, 0).f_status == CLOSED);
    assert(!rejected.f_scheduler && mica_task_release(&rejected, &worker));
    assert(mica_task_scheduler_release(&scheduler, &worker) == OK);
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_resources_stop());
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct scheduler_stop_race {
    struct mica_TaskScheduler *scheduler;
    _Atomic bool ready;
    _Atomic bool go;
};
static void *scheduler_stop_thread(void *argument) {
    struct scheduler_stop_race *race = argument;
    atomic_store(&race->ready, true);
    while (!atomic_load(&race->go)) sched_yield();
    mica_task_scheduler_stop(race->scheduler);
    return NULL;
}

static void scheduler_shutdown_admission(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    for (unsigned i = 0; i < 32; ++i) {
        struct mica_TaskScheduler scheduler = {0};
        struct mica_TaskControl task = {0};
        scheduler_complete_task(&worker, &kernel, &task);
        assert(mica_task_scheduler_init(&scheduler, &worker, &hub, 1) == OK);
        struct scheduler_stop_race race = {.scheduler = &scheduler};
        pthread_t thread;
        assert(mica_memory_leave(&worker));
        assert(!pthread_create(&thread, NULL, scheduler_stop_thread, &race));
        while (!atomic_load(&race.ready)) sched_yield();
        assert(mica_memory_enter(&worker));
        atomic_store(&race.go, true);
        struct mica_TaskAdmission admitted = mica_task_scheduler_submit(&scheduler, &worker, &task, false, 0);
        assert(admitted.f_status == OK || admitted.f_status == CLOSED);
        assert(mica_memory_leave(&worker));
        assert(!pthread_join(thread, NULL));
        assert(mica_memory_enter(&worker));
        struct mica_TaskObservation observed = mica_task_observe(&task, &worker);
        assert(observed.f_status == OK && !observed.f_committed);
        if (admitted.f_status == OK) {
            assert(task.f_scheduler == (void *)&scheduler && observed.f_state == TASK_CANCELLED);
        } else {
            assert(!task.f_scheduler && observed.f_state == RUNNABLE);
        }
        assert(mica_task_scheduler_release(&scheduler, &worker) == OK);
        assert(mica_task_release(&task, &worker));
        assert(mica_memory_safepoint(&worker, true));
    }
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_resources_stop());
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static uint64_t scheduler_suspend_task(struct mica_MemoryWorker *worker,
    struct mica_Kernel *kernel, struct mica_TaskScheduler *scheduler,
    struct mica_TaskControl *task, int64_t delay, uint64_t slot) {
    struct mica_MemoryRoot image = {0}, boundary = {0};
    struct mica_TaskServices services = {0};
    assert(mica_memory_root_push(worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_task_test_suspend, NULL,
        integer(7), integer(delay), integer(0), NULL, NULL, 1, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(task, worker, kernel, &image, 100, 2, 8) == OK);
    assert(mica_memory_root_pop(worker, &image));
    struct mica_TaskAdmission admitted = mica_task_scheduler_submit(scheduler, worker, task, false, 0);
    assert(admitted.f_status == OK);
    struct mica_TaskQueueResult next = mica_task_scheduler_take(scheduler, worker);
    assert(next.f_status == OK && next.f_id == slot);
    assert(mica_task_execute(task, worker) == OK);
    assert(mica_memory_acquire(worker, &task->f_prepared, &boundary));
    assert(mica_task_dispatch(task, worker, &services) == OK);
    assert(mica_task_scheduler_settle(scheduler, worker, slot, (void *)boundary.f_pointer) == OK);
    assert(mica_memory_root_pop(worker, &boundary));
    assert(task->f_committed && task->f_generation == 1);
    return admitted.f_id;
}

static void scheduled_timers(void) {
    enum { TASKS = 5, WORKERS = 2 };
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskScheduler scheduler = {0};
    struct mica_TaskControl tasks[TASKS] = {{0}};
    struct mica_TaskServices services = {0};
    uint64_t ids[TASKS];
    int64_t delays[TASKS] = {0, 20000000, -1, -1, -2};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    assert(mica_task_scheduler_init(&scheduler, &worker, &hub, TASKS) == OK);
    uint64_t started = mica_foreign_task_clock_now();
    for (unsigned i = 0; i < TASKS; ++i) {
        ids[i] = scheduler_suspend_task(&worker, &kernel, &scheduler, &tasks[i], delays[i], i);
        if (i == 4) {
            // UINT64_MAX is the explicit no-deadline sentinel. Other values
            // that overflow the monotonic deadline fail after publication.
            assert(tasks[i].f_state == FAILED && tasks[i].f_status == LIMIT);
        } else {
            assert(tasks[i].f_state == WAITING);
            struct mica_TaskWaitRequest *request = (void *)scheduler.f_slots[i].f_request.f_pointer;
            assert(request && !request->f_mailbox && mica_value_is_unit(request->f_reply));
        }
    }
    // No mailbox exists. Timer registration and firing must not attempt a
    // receive, even for the zero-duration request. All requests survive GC.
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    assert(mica_task_scheduler_cancel(&scheduler, ids[3]));
    pthread_t threads[WORKERS + 1];
    struct scheduler_runner runners[WORKERS + 1] = {0};
    for (unsigned i = 0; i <= WORKERS; ++i) {
        runners[i] = (struct scheduler_runner){.heap = &heap, .scheduler = &scheduler,
            .services = &services, .events = i == WORKERS};
        assert(!pthread_create(&threads[i], NULL, scheduler_thread, &runners[i]));
    }
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskObservation observation = mica_task_observe(&tasks[2], &worker);
    assert(observation.f_status == OK && observation.f_state == WAITING);
    struct mica_ValueTuple tuple = {0};
    struct mica_ValueResult unit = mica_value_relation(&worker, NULL, 0, &tuple, 1);
    assert(unit.f_ok);
    assert(mica_task_scheduler_wake(&scheduler, &worker, ids[2], 1, unit.f_value) == OK);
    assert(mica_task_scheduler_wake(&scheduler, &worker, ids[2], 1, integer(99)) == CLOSED);
    for (;;) {
        struct mica_TaskSchedulerObservation observed = mica_task_scheduler_inspect(&scheduler, &worker);
        assert(observed.f_status == OK && !observed.f_closed);
        if (!observed.f_remaining) break;
        assert(mica_memory_safepoint(&worker, false));
    }
    assert(mica_memory_leave(&worker));
    mica_task_scheduler_stop(&scheduler);
    for (unsigned i = 0; i <= WORKERS; ++i) assert(!pthread_join(threads[i], NULL));
    assert(mica_foreign_task_clock_now() - started >= UINT64_C(20000000));
    uint64_t taken = 0;
    for (unsigned i = 0; i < WORKERS; ++i) {
        assert(runners[i].result.f_status == OK);
        taken += runners[i].result.f_taken;
    }
    assert(runners[WORKERS].status == OK && taken == 3);
    for (unsigned i = 0; i < 3; ++i) assert(tasks[i].f_state == COMPLETED && tasks[i].f_committed);
    assert(tasks[3].f_state == TASK_CANCELLED);
    assert(tasks[4].f_state == FAILED && tasks[4].f_status == LIMIT);
    assert(mica_memory_enter(&worker));
    assert(mica_task_scheduler_release(&scheduler, &worker) == OK);
    for (unsigned i = 0; i < TASKS; ++i) assert(mica_task_release(&tasks[i], &worker));
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_resources_stop());
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0 && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct timer_race {
    struct mica_MemoryHeap *heap;
    struct mica_TaskScheduler *scheduler;
    uint64_t id;
    bool cancel;
    _Atomic bool ready;
    _Atomic bool go;
    uint64_t status;
};

static void *timer_race_thread(void *argument) {
    struct timer_race *race = argument;
    atomic_store(&race->ready, true);
    while (!atomic_load(&race->go)) sched_yield();
    if (race->cancel) {
        assert(mica_task_scheduler_cancel(race->scheduler, race->id));
        return NULL;
    }
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot reply = {0};
    struct mica_ValueTuple tuple = {0};
    assert(mica_memory_worker_init(&worker, race->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_root_push(&worker, &reply));
    struct mica_ValueResult unit = mica_value_relation(&worker, NULL, 0, &tuple, 1);
    assert(unit.f_ok);
    mica_value_root_set(&reply, unit.f_value);
    assert(mica_memory_safepoint(&worker, true));
    race->status = mica_task_scheduler_wake(race->scheduler, &worker, race->id, 1,
        mica_value_root_get(&reply));
    assert(mica_memory_root_pop(&worker, &reply));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void scheduled_timer_races(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskServices services = {0};
    assert(mica_task_resources_start());
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_task_mailbox_hub_init(&hub, &worker, 1) == OK);
    for (unsigned i = 0; i < 64; ++i) {
        struct mica_TaskScheduler scheduler = {0};
        struct mica_TaskControl task = {0};
        assert(mica_task_scheduler_init(&scheduler, &worker, &hub, 1) == OK);
        uint64_t id = scheduler_suspend_task(&worker, &kernel, &scheduler, &task, 0, 0);
        assert(task.f_state == WAITING);
        assert(mica_memory_safepoint(&worker, true));
        struct timer_race race = {.heap = &heap, .scheduler = &scheduler,
            .id = id, .cancel = i % 2 == 0};
        pthread_t thread;
        assert(mica_memory_leave(&worker));
        assert(!pthread_create(&thread, NULL, timer_race_thread, &race));
        while (!atomic_load(&race.ready)) sched_yield();
        assert(mica_memory_enter(&worker));
        atomic_store(&race.go, true);
        assert(mica_memory_safepoint(&worker, true));
        assert(mica_task_scheduler_poll(&scheduler, &worker).f_status == OK);
        assert(mica_memory_leave(&worker));
        assert(!pthread_join(thread, NULL));
        assert(mica_memory_enter(&worker));
        assert(mica_task_scheduler_poll(&scheduler, &worker).f_status == OK);
        // Both contenders have finished. A wait can enqueue only one segment,
        // and its retained request must be retired regardless of the winner.
        assert(!scheduler.f_slots[0].f_request.f_pointer);
        assert(scheduler.f_queue.f_count <= 1 && task.f_generation == 1);
        assert(mica_task_scheduler_wake(&scheduler, &worker, id, 1, integer(99)) == CLOSED);
        if (race.cancel) {
            assert(task.f_state == TASK_CANCELLED);
        } else {
            assert(race.status == OK || race.status == CLOSED);
            assert(task.f_state == RUNNABLE && scheduler.f_queue.f_count == 1);
        }
        if (scheduler.f_queue.f_count) {
            struct mica_TaskQueueResult next = mica_task_scheduler_take(&scheduler, &worker);
            assert(next.f_status == OK && next.f_id == 0);
            if (!race.cancel) {
                assert(mica_task_execute(&task, &worker) == OK);
                assert(mica_task_dispatch(&task, &worker, &services) == OK);
                assert(task.f_state == COMPLETED);
            }
            assert(mica_task_scheduler_settle(&scheduler, &worker, 0, NULL) == OK);
        }
        assert(!scheduler.f_remaining && !scheduler.f_queue.f_count);
        assert(mica_memory_leave(&worker));
        mica_task_scheduler_stop(&scheduler);
        assert(mica_memory_enter(&worker));
        assert(mica_task_scheduler_release(&scheduler, &worker) == OK);
        assert(mica_task_release(&task, &worker));
        assert(mica_memory_safepoint(&worker, true));
    }
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == OK);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_task_resources_stop());
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}
