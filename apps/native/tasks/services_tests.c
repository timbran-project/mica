// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Included by tests.c after the shared fixture helpers.
static void service_delivery(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_MemoryRoot image = {0};
    struct mica_TaskServiceProbe probe = {0};
    struct mica_TaskServices services = {
        .f_context = (void *)&probe,
        .f_emit = mica_task_test_service_emit,
        .f_send = mica_task_test_service_send,
        .f_subscribe = mica_task_test_service_subscribe,
        .f_spawn = mica_task_test_service_spawn,
    };
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_memory_root_push(&worker, &image));
    assert(mica_memory_root_push(&worker, &probe.f_last));
    uint8_t bytes[256];
    memset(bytes, 'd', sizeof(bytes));
    struct mica_ValueResult text = mica_value_string(&worker, bytes, sizeof(bytes));
    assert(text.f_ok);
    text = mica_value_string_slice(&worker, text.f_value, 16, 64);
    assert(text.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(7), text.f_value, integer(42), NULL, NULL, 3, NULL);
    assert(image.f_pointer);
    for (unsigned scenario = 0; scenario < 4; ++scenario) {
        struct mica_TaskControl task = {0};
        struct mica_TaskAttempt attempt = {0};
        probe.f_calls = probe.f_delivered = probe.f_order = 0;
        probe.f_fail_at = scenario == 1 ? 3 : 0;
        services.f_send = scenario == 2 ? NULL : mica_task_test_service_send;
        assert(mica_task_init(&task, &worker, &kernel, &image, 100, 1, 8) == OK);
        assert(mica_task_claim(&task, &worker, &attempt) == OK);
        if (scenario) assert(mica_kernel_declare(&attempt.f_transaction, 100 + scenario, 100 + scenario, 2, 0, 0, 0) == OK);
        for (uint64_t kind = 1; kind <= 4; ++kind) {
            struct mica_TaskImage *current = (void *)attempt.f_current.f_pointer;
            mica_type_Value cells[] = {integer(kind), current->f_environment};
            struct mica_ValueResult payload = mica_value_list(&worker, cells, 2);
            assert(payload.f_ok);
            assert(mica_task_stage(&worker, &attempt.f_effects, kind, integer(7), payload.f_value) == OK);
        }
        assert(mica_task_dispatch(&task, &worker, &services) == CLOSED);
        assert(probe.f_calls == 0);
        if (!scenario) {
            // A read-only task still captures the view selected at commit,
            // which can differ from the snapshot where its attempt began.
            struct mica_KernelTransaction concurrent = {0};
            assert(mica_kernel_begin(&kernel, &worker, &concurrent, integer(7)) == OK);
            assert(mica_kernel_declare(&concurrent, 50, 50, 2, 0, 0, 0) == OK);
            assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
            assert(mica_kernel_version(&attempt.f_transaction) == 0);
        }
        assert(mica_task_request(&attempt, 1) == OK);
        assert(mica_task_prepare(&task, &worker, &attempt) == OK);
        assert(mica_task_commit(&task, &worker, &attempt) == OK);
        uint64_t committed_version = mica_kernel_version(&attempt.f_transaction);
        if (!scenario) assert(committed_version == 1);
        assert(task.f_committed_view.f_registration);
        // Inject a post-publication error. Its code must not imply retry or
        // prevent delivery of the transaction's already committed effects.
        if (scenario == 3) task.f_status = CONFLICT;
        // Delivery cannot run until the owning attempt has released its roots.
        assert(mica_task_dispatch(&task, &worker, &services) == CLOSED);
        assert(probe.f_calls == 0);
        assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
        // Overwrite the history slot and release the attempt before delivery.
        // The retained view must survive independently, including a no-op commit.
        for (unsigned update = 0; update < 3; ++update) {
            struct mica_KernelTransaction concurrent = {0};
            uint64_t id = 200 + scenario * 3 + update;
            assert(mica_kernel_begin(&kernel, &worker, &concurrent, integer(7)) == OK);
            assert(mica_kernel_declare(&concurrent, id, id, 2, 0, 0, 0) == OK);
            assert(mica_kernel_commit(&concurrent) == OK && mica_kernel_end(&concurrent));
        }
        struct mica_KernelHistoryResult history = mica_kernel_history_next(&kernel, &worker, committed_version);
        assert(history.f_status == OK && history.f_gap && history.f_latest == committed_version + 3);
        assert(mica_memory_safepoint(&worker, true));
        struct mica_KernelSnapshot *captured = (void *)task.f_committed_view.f_pointer;
        assert(captured && captured->f_version == committed_version);
        for (struct mica_KernelRelation *relation = captured->f_relations; relation; relation = relation->f_next)
            assert(relation->f_id < 200 + scenario * 3);
        uint64_t expected = scenario == 0 ? OK : scenario == 1 ? PERMISSION : scenario == 2 ? UNKNOWN : CONFLICT;
        if (scenario == 3) {
            struct mica_TaskQueue queue = {0};
            assert(mica_task_queue_init(&queue, 1) == OK);
            assert(mica_task_queue_push(&queue, 0) == OK);
            mica_task_queue_close(&queue);
            // Execution rejects this already published task. Queue draining
            // must still deliver its boundary without attempting replay.
            struct mica_TaskDrainResult drained = mica_task_drain(&queue, &worker, &task, 1, &services);
            assert(drained.f_status == OK && drained.f_taken == 1 && drained.f_completed == 0);
            assert(mica_task_queue_release(&queue));
        } else {
            assert(mica_task_dispatch(&task, &worker, &services) == expected);
        }
        assert(task.f_committed && task.f_status == expected);
        assert(probe.f_snapshot_version == committed_version);
        assert(!task.f_committed_view.f_registration && !task.f_committed_view.f_pointer);
        assert(task.f_state == (scenario == 0 ? COMPLETED : FAILED));
        uint64_t delivered = scenario == 0 || scenario == 3 ? 4 : 3;
        assert(probe.f_delivered == delivered);
        assert(probe.f_calls == (scenario == 2 ? 3 : 4));
        uint64_t order = 0;
        for (uint64_t kind = 1; kind <= 4; ++kind) {
            if ((scenario == 1 && kind == 3) || (scenario == 2 && kind == 2)) continue;
            order = order * 64 + kind;
        }
        assert(probe.f_order == order);
        assert(mica_memory_safepoint(&worker, true));
        const struct mica_HeapList *last = mica_value_as_list(mica_value_root_get(&probe.f_last)).f_header;
        assert(last && last->f_length == 2);
        assert(last->f_data[0] == integer(4));
        assert(mica_value_string_length(last->f_data[1]).f_number == 48);
        uint64_t calls = probe.f_calls;
        assert(mica_task_dispatch(&task, &worker, &services) == CLOSED);
        assert(probe.f_calls == calls);
        assert(mica_task_execute(&task, &worker) == CLOSED);
        assert(mica_task_release(&task, &worker));
    }
    assert(mica_memory_root_pop(&worker, &probe.f_last));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}
