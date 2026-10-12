// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

// OS orchestration and assertions only. The source module installs the verb,
// performs its writes, captures continuations, spawns, sends, and receives.
#include <assert.h>
#include <pthread.h>
#include <time.h>

static mica_type_Value source_integer(int64_t number) {
    struct mica_ValueResult result = mica_value_int(number);
    assert(result.f_ok);
    return result.f_value;
}
static mica_type_Value source_cell(mica_type_Value list, uint64_t index) {
    struct mica_ValueResult result = mica_value_list_get(list, index);
    assert(result.f_ok);
    return result.f_value;
}
struct source_worker {
    struct mica_MemoryHeap *heap;
    struct mica_TaskScheduler *scheduler;
    struct mica_TaskServices *services;
    bool events;
    uint64_t status;
};
static void *source_thread(void *opaque) {
    struct source_worker *state = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, state->heap, 4096));
    assert(mica_memory_enter(&worker));
    if (state->events) {
        state->status = mica_task_scheduler_events(state->scheduler, &worker);
    } else {
        state->status = mica_task_scheduler_worker(state->scheduler, &worker, state->services).f_status;
    }
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void source_roundtrip(bool force_conflict) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskMailboxHub hub = {0};
    struct mica_TaskScheduler scheduler = {0};
    struct mica_TaskSpawnContext spawn = {0};
    struct mica_TaskDispatchContext dispatch = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0};
    struct mica_MemoryRoot code = {0}, prototypes = {0}, image = {0}, result = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == 0);
    assert(mica_kernel_init(&kernel, &worker, source_integer(7), 1));
    assert(mica_task_code_init(&registry, &worker) == 0);
    assert(mica_task_mailbox_hub_init(&hub, &worker, 16) == 0);
    assert(mica_task_scheduler_init(&scheduler, &worker, &hub, 16) == 0);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &prototypes));
    assert(mica_memory_root_push(&worker, &image));
    assert(mica_memory_root_push(&worker, &result));
    // These generated entries are part of this executable. Its real loader
    // handle is owned and closed by the same code registry used for libraries.
    void *library = dlopen(NULL, RTLD_NOW);
    assert(library);
    assert(mica_task_code_register(&registry, &worker, library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry, (void *)code.f_pointer) == 0);
    mica_type_Value kinds[16];
    for (unsigned i = 0; i < 16; ++i) {
        struct mica_ValueResult kind = mica_value_identity(9001 + i);
        assert(kind.f_ok);
        kinds[i] = kind.f_value;
    }
    struct mica_ValueResult list = mica_value_list(&worker, kinds, 16);
    assert(list.f_ok);
    mica_value_root_set(&prototypes, list.f_value);
    struct mica_TaskRuntime runtime = {.f_symbols = &symbols, .f_mailboxes = &hub,
        .f_spawn = &spawn, .f_dispatch = &dispatch, .f_mailbox_capacity = 64};
    assert(mica_task_dispatch_context_init(&dispatch, &worker, &vocabulary,
        702, 703, 701, 704, mica_value_root_get(&prototypes)) == 0);
    assert(mica_task_spawn_context_init(&spawn, &worker, &scheduler, 100000, 3, 16, &runtime) == 0);
    struct mica_TaskServices services = {.f_context = (void *)&runtime,
        .f_send = mica_task_runtime_send, .f_spawn = mica_task_runtime_spawn};
    struct mica_KernelTransaction setup = {0};
    assert(mica_kernel_begin(&kernel, &worker, &setup, source_integer(7)) == 0);
    for (uint64_t id = 701; id <= 705; ++id) {
        uint64_t arity = id == 701 ? 3 : id == 703 ? 6 : 2;
        assert(mica_kernel_declare(&setup, id, id, arity, id == 705 ? 1 : 0, id == 705 ? 1 : 0, 0) == 0);
    }
    assert(mica_kernel_commit(&setup) == 0 && mica_kernel_end(&setup));
    // Installation is visible to its draft and absent outside it. Aborting
    // discards both method facts and the compiled-program catalogue entry.
    struct mica_KernelTransaction install = {0}, outsider = {0};
    assert(mica_kernel_begin(&kernel, &worker, &install, source_integer(7)) == 0);
    assert(mica_task_source_roundtrip_install(&install, &dispatch, (void *)code.f_pointer, source_integer(7)) == 0);
    assert(mica_kernel_program_get(&install, 8003).f_program);
    assert(mica_kernel_begin(&kernel, &worker, &outsider, source_integer(7)) == 0);
    assert(!mica_kernel_program_get(&outsider, 8003).f_program);
    assert(mica_kernel_end(&outsider) && mica_kernel_end(&install));
    assert(mica_kernel_begin(&kernel, &worker, &install, source_integer(7)) == 0);
    assert(!mica_kernel_program_get(&install, 8003).f_program);
    assert(mica_task_source_roundtrip_install(&install, &dispatch, (void *)code.f_pointer, source_integer(7)) == 0);
    assert(mica_kernel_commit(&install) == 0 && mica_kernel_end(&install));
    list = mica_value_list(&worker, NULL, 0);
    assert(list.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_source_roundtrip_entry_0,
        (void *)code.f_pointer, source_integer(7), list.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl parent = {0};
    assert(mica_task_init(&parent, &worker, &kernel, &image, 100000, 3, 16, &runtime) == 0);
    // Run the initial region here. Final resumption runs on another thread.
    assert(mica_task_execute(&parent, &worker) == 0);
    assert(mica_task_dispatch(&parent, &worker, &services) == 0);
    assert(mica_task_scheduler_submit(&scheduler, &worker, &parent, false, 0).f_status == 0);
    if (force_conflict) {
        // Hold the child until the parent has committed its receive request.
        struct mica_TaskQueueResult child_slot = mica_task_scheduler_take(&scheduler, &worker);
        struct mica_TaskQueueResult parent_slot = mica_task_scheduler_take(&scheduler, &worker);
        assert(child_slot.f_status == 0 && parent_slot.f_status == 0);
        assert(scheduler.f_slots[parent_slot.f_id].f_task == &parent);
        struct mica_TaskControl *child = scheduler.f_slots[child_slot.f_id].f_task;
        assert(child != &parent && child->f_runtime == &runtime);
        assert(mica_task_execute(&parent, &worker) == 0);
        struct mica_MemoryRoot waiting = {0};
        assert(mica_memory_acquire(&worker, &parent.f_prepared, &waiting));
        assert(mica_task_dispatch(&parent, &worker, &services) == 0);
        assert(mica_task_scheduler_settle(&scheduler, &worker, parent_slot.f_id, (void *)waiting.f_pointer) == 0);
        assert(mica_memory_root_pop(&worker, &waiting));
        assert(mica_task_observe(&parent, &worker).f_state == 4);
        // Entering a receive must not run its finalizer before delivery.
        struct mica_KernelTransaction waiting_view = {0};
        assert(mica_kernel_begin(&kernel, &worker, &waiting_view, source_integer(7)) == 0);
        mica_type_Value waiting_cells[] = {source_integer(9), source_integer(1)}, waiting_matches[1];
        struct mica_ValueResult waiting_pattern = mica_value_list(&worker, waiting_cells, 2);
        assert(waiting_pattern.f_ok);
        struct mica_KernelScanResult waiting_rows = mica_kernel_scan(&waiting_view, 705, waiting_pattern.f_value, 3, 0, waiting_matches, 1);
        assert(waiting_rows.f_status == 0 && waiting_rows.f_count == 0);
        assert(mica_kernel_end(&waiting_view));
        struct mica_TaskAttempt attempt = {0};
        assert(mica_task_claim(child, &worker, &attempt) == 0);
        struct mica_TaskImage *entry = (void *)attempt.f_current.f_pointer;
        assert(entry->f_entry(&attempt) == 0);
        assert(attempt.f_effects.f_pointer);
        struct mica_KernelTransaction rival = {0};
        mica_type_Value rival_cells[] = {source_integer(7), source_integer(99)};
        struct mica_MemoryRoot rival_row = {0};
        assert(mica_memory_root_push(&worker, &rival_row));
        list = mica_value_list(&worker, rival_cells, 2);
        assert(list.f_ok);
        mica_value_root_set(&rival_row, list.f_value);
        assert(mica_kernel_begin(&kernel, &worker, &rival, source_integer(7)) == 0);
        assert(mica_kernel_write(&rival, 705, mica_value_root_get(&rival_row), true) == 0);
        assert(mica_kernel_commit(&rival) == 0 && mica_kernel_end(&rival));
        assert(mica_kernel_commit(&attempt.f_transaction) == 5);
        // Remove the competing value, then replay from the child's original
        // invocation. The first attempt's mailbox effect must be discarded.
        assert(mica_kernel_begin(&kernel, &worker, &rival, source_integer(7)) == 0);
        assert(mica_kernel_write(&rival, 705, mica_value_root_get(&rival_row), false) == 0);
        assert(mica_kernel_commit(&rival) == 0 && mica_kernel_end(&rival));
        assert(mica_memory_root_pop(&worker, &rival_row));
        assert(mica_task_detach(child, &worker) == 0);
        assert(mica_task_attempt_retry(&attempt) == 0);
        assert(mica_task_attach(child, &worker, &attempt) == 0);
        assert(!attempt.f_effects.f_pointer && attempt.f_retries == 2);
        assert(mica_memory_safepoint(&worker, true));
        entry = (void *)attempt.f_current.f_pointer;
        assert(entry->f_entry(&attempt) == 0);
        assert(mica_task_prepare(child, &worker, &attempt) == 0);
        assert(mica_task_commit(child, &worker, &attempt) == 0);
        assert(mica_task_finish(child, &worker, &attempt, 0) == 0);
        assert(mica_task_dispatch(child, &worker, &services) == 0);
        assert(mica_task_scheduler_settle(&scheduler, &worker, child_slot.f_id, NULL) == 0);
    }
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_memory_leave(&worker));
    pthread_t threads[3];
    struct source_worker workers[3];
    for (unsigned i = 0; i < 3; ++i) {
        workers[i] = (struct source_worker){&heap, &scheduler, &services, i == 2, UINT64_MAX};
        assert(!pthread_create(&threads[i], NULL, source_thread, &workers[i]));
    }
    assert(mica_memory_enter(&worker));
    struct timespec start, now;
    assert(!clock_gettime(CLOCK_MONOTONIC, &start));
    while (mica_task_scheduler_inspect(&scheduler, &worker).f_remaining) {
        assert(mica_memory_safepoint(&worker, true));
        assert(!clock_gettime(CLOCK_MONOTONIC, &now));
        assert(now.tv_sec - start.tv_sec < 30);
    }
    assert(mica_memory_leave(&worker));
    mica_task_scheduler_stop(&scheduler);
    for (unsigned i = 0; i < 3; ++i) {
        assert(!pthread_join(threads[i], NULL));
        assert(workers[i].status == 0);
    }
    assert(mica_memory_enter(&worker));
    struct mica_TaskObservation observed = mica_task_observe(&parent, &worker);
    assert(observed.f_status == 0 && observed.f_state == 5);
    struct mica_MemoryRoot completed = {0};
    assert(mica_memory_acquire(&worker, &parent.f_image, &completed));
    mica_value_root_set(&result, ((struct mica_TaskImage *)completed.f_pointer)->f_input);
    assert(mica_memory_root_pop(&worker, &completed));
    assert(mica_memory_safepoint(&worker, true));
    mica_type_Value returned = mica_value_root_get(&result);
    assert(mica_value_list_length(returned).f_number == 4);
    assert(source_cell(returned, 3) == source_integer(2));
    assert(mica_value_as_int(source_cell(returned, 0)).f_number > 0);
    mica_type_Value saved = source_cell(returned, 1);
    assert(source_cell(saved, 1) == source_integer(42));
    struct mica_HeapStringResult text = mica_value_as_string(source_cell(saved, 0));
    assert(text.f_ok && text.f_header->f_length == 19);
    assert(!memcmp(text.f_header->f_data, "parent continuation", 19));
    mica_type_Value messages = source_cell(returned, 2);
    assert(mica_value_list_length(messages).f_number == 1);
    mica_type_Value group = source_cell(messages, 0);
    assert(mica_value_as_capability(source_cell(group, 0)).f_ok);
    mica_type_Value payloads = source_cell(group, 1);
    assert(mica_value_list_length(payloads).f_number == 1);
    assert(source_cell(payloads, 0) == source_integer(43));
    assert(mica_kernel_begin(&kernel, &worker, &setup, source_integer(7)) == 0);
    mica_type_Value cells[] = {source_integer(7), source_integer(42)}, rows[2];
    list = mica_value_list(&worker, cells, 2);
    assert(list.f_ok);
    struct mica_KernelScanResult scanned = mica_kernel_scan(&setup, 705, list.f_value, 3, 0, rows, 2);
    assert(scanned.f_status == 0 && scanned.f_count == 1);
    cells[0] = source_integer(8);
    cells[1] = source_integer(43);
    list = mica_value_list(&worker, cells, 2);
    assert(list.f_ok);
    scanned = mica_kernel_scan(&setup, 705, list.f_value, 3, 0, rows, 2);
    assert(scanned.f_status == 0 && scanned.f_count == 1);
    cells[0] = source_integer(9);
    cells[1] = source_integer(1);
    list = mica_value_list(&worker, cells, 2);
    assert(list.f_ok);
    scanned = mica_kernel_scan(&setup, 705, list.f_value, 3, 0, rows, 2);
    assert(scanned.f_status == 0 && scanned.f_count == 1);
    assert(mica_kernel_end(&setup));
    assert(mica_task_scheduler_release(&scheduler, &worker) == 0);
    assert(mica_task_release(&parent, &worker));
    assert(mica_task_spawn_context_release(&spawn, &worker) == 0);
    assert(mica_task_dispatch_context_release(&dispatch, &worker) == 0);
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == 0);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_root_pop(&worker, &result));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_root_pop(&worker, &prototypes));
    assert(mica_memory_root_pop(&worker, &code));
    assert(mica_task_code_close(&registry, &worker) == 0);
    struct mica_TaskCodeReap reaped = mica_task_code_reap(&registry, &worker);
    assert(reaped.f_status == 0 && reaped.f_released == 1);
    assert(mica_task_code_release(&registry, &worker) == 0);
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}
int main(void) {
    assert(mica_task_resources_start());
#ifdef MICA_SOURCE_CONFORMANCE
    source_conformance();
#endif
    source_roundtrip(false);
    source_roundtrip(true);
    assert(mica_task_resources_stop());
    return 0;
}
