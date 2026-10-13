// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

struct invocation_resume {
    struct mica_MemoryHeap *heap;
    struct mica_Kernel *kernel;
    struct mica_TaskControl *task;
    mica_type_Value selector;
    mica_type_Value capability;
    uint64_t epoch;
};

static void *invocation_resume_worker(void *opaque) {
    struct invocation_resume *resume = opaque;
    struct mica_MemoryWorker worker = {0};
    struct mica_TaskAttempt attempt = {0};
    assert(mica_memory_worker_init(&worker, resume->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_task_claim(resume->task, &worker, &attempt) == OK);
    assert(attempt.f_epoch != resume->epoch);
    assert(mica_memory_safepoint(&worker, true));
    // Fresh authority still grants this selector, but the preceding segment's
    // token cannot survive a scheduler boundary, even on another native thread.
    assert(mica_task_invoke_authorize(&worker, &attempt, resume->selector) == OK);
    assert(mica_task_invoke_check(&worker, &attempt, resume->selector, resume->capability) == PERMISSION);
    struct mica_KernelBudget budget = dispatch_budget(&attempt.f_transaction);
    struct mica_TaskInvocationCapability minted = mica_task_invoke_mint(&worker, &attempt, resume->selector, &budget);
    assert(minted.f_status == OK && minted.f_capability != resume->capability);
    assert(mica_task_invoke_check(&worker, &attempt, resume->selector, minted.f_capability) == OK);
    assert(mica_task_finish(resume->task, &worker, &attempt, CLOSED) == CLOSED);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void invocation_authority(void) {
    struct mica_SymbolTable symbols = {0};
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_KernelTransaction owner = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot image = {0};
    assert(mica_value_symbol_table_init(&symbols));
    mica_type_Value allowed = subscription_symbol(&symbols, "allowed");
    mica_type_Value denied = subscription_symbol(&symbols, "denied");
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    assert(mica_kernel_declare(&owner, 1, 1, 1, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&owner, 2, 2, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&owner, 3, 3, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&owner, 4, 4, 2, 0, 0, 0) == OK);
    mica_type_Value root = integer(7);
    assert(mica_kernel_write(&owner, 1, mailbox_receivers(&worker, &root, 1), true) == OK);
    mica_type_Value right[] = {integer(8), allowed};
    assert(mica_kernel_write(&owner, 2, mailbox_receivers(&worker, right, 2), true) == OK);
    // The same numeric payload in relation and selector namespaces is harmless.
    assert(mica_kernel_write(&owner, 3, permission(&worker, 8, mica_value_as_symbol(denied).f_number), true) == OK);
    assert(mica_kernel_write(&owner, 4, permission(&worker, 8, 77), true) == OK);
    assert(mica_kernel_set_authority_policy(&owner, 1, 0, 0, 3, 0, 0, 2, 4) == OK);
    assert(mica_kernel_commit(&owner) == OK && mica_kernel_end(&owner));
    assert(mica_memory_root_push(&worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(&worker, mica_task_test_entry, NULL,
        integer(8), integer(0), integer(0), NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(&task, &worker, &kernel, &image, 1000, 2, 8, NULL) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    uint64_t epoch = attempt.f_epoch;
    mica_type_Value emit_target = mica_value_identity(77).f_value;
    assert(mica_task_emit(&worker, &attempt, emit_target, integer(42)) == OK);
    assert(attempt.f_effects.f_pointer);
    assert(mica_task_emit(&worker, &attempt, mica_value_identity(78).f_value, integer(42)) == PERMISSION);
    assert(mica_task_emit(&worker, &attempt, integer(77), integer(42)) == SCHEMA);
    uint64_t remaining = attempt.f_remaining;
    attempt.f_remaining = 0;
    assert(mica_task_emit(&worker, &attempt, emit_target, integer(42)) == LIMIT);
    attempt.f_remaining = remaining;
    struct mica_MemoryRoot unpublished = {0};
    assert(mica_task_delivery(&task, &worker, &unpublished) == CLOSED);
    assert(epoch && !mica_task_resources_stop());
    assert(mica_task_invoke_authorize(&worker, &attempt, allowed) == OK);
    assert(mica_task_invoke_authorize(&worker, &attempt, denied) == PERMISSION);
    assert(mica_task_invoke_authorize(&worker, &attempt, integer(1)) == PERMISSION);
    struct mica_KernelAuthority *authority = (void *)attempt.f_authority.f_pointer;
    assert(!mica_kernel_authority_allows(authority, mica_value_as_symbol(allowed).f_number, 1));
    struct mica_KernelBudget budget = dispatch_budget(&attempt.f_transaction);
    struct mica_TaskInvocationCapability minted = mica_task_invoke_mint(&worker, &attempt, allowed, &budget);
    assert(minted.f_status == OK && !worker.f_control);
    mica_type_Value capability = minted.f_capability;
    assert(!mica_value_is_persistable(capability));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_invoke_check(&worker, &attempt, allowed, capability) == OK);
    assert(mica_task_invoke_check(&worker, &attempt, denied, capability) == PERMISSION);
    assert(mica_task_invoke_check(&worker, &attempt, allowed, integer(1)) == PERMISSION);
    mica_type_Value forged = mica_value_capability(mica_value_as_capability(capability).f_number + 1).f_value;
    assert(mica_task_invoke_check(&worker, &attempt, allowed, forged) == PERMISSION);
    budget = dispatch_budget(&attempt.f_transaction);
    budget.f_bytes = 0;
    minted = mica_task_invoke_mint(&worker, &attempt, allowed, &budget);
    assert(minted.f_status == LIMIT && !minted.f_capability && !worker.f_control);
    assert(mica_task_invoke_check(&worker, &attempt, allowed, capability) == OK);
    budget = dispatch_budget(&attempt.f_transaction);
    minted = mica_task_invoke_mint(&worker, &attempt, denied, &budget);
    assert(minted.f_status == PERMISSION && !minted.f_capability);

    // Revocation conflicts with the checked policy dependency. Conflict replay
    // retains segment authority and epoch; a scheduled resumption refreshes both.
    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    assert(mica_kernel_write(&owner, 2, mailbox_receivers(&worker, right, 2), false) == OK);
    assert(mica_kernel_write(&owner, 4, permission(&worker, 8, 77), false) == OK);
    assert(mica_kernel_commit(&owner) == OK && mica_kernel_end(&owner));
    assert(mica_kernel_commit(&attempt.f_transaction) == CONFLICT);
    assert(mica_task_detach(&task, &worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(mica_task_attach(&task, &worker, &attempt) == OK);
    assert(attempt.f_epoch == epoch && !attempt.f_effects.f_pointer);
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_invoke_check(&worker, &attempt, allowed, capability) == OK);
    assert(mica_task_finish(&task, &worker, &attempt, CLOSED) == CLOSED);
    assert(mica_task_release(&task, &worker));
    assert(mica_task_init(&task, &worker, &kernel, &image, 1000, 2, 8, NULL) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(attempt.f_epoch != epoch);
    assert(mica_task_emit(&worker, &attempt, emit_target, integer(42)) == PERMISSION);
    assert(mica_task_invoke_authorize(&worker, &attempt, allowed) == PERMISSION);
    assert(mica_task_invoke_check(&worker, &attempt, allowed, capability) == PERMISSION);
    assert(mica_task_finish(&task, &worker, &attempt, CLOSED) == CLOSED);
    assert(mica_task_release(&task, &worker));

    assert(mica_kernel_begin(&kernel, &worker, &owner, integer(7)) == OK);
    assert(mica_kernel_write(&owner, 2, mailbox_receivers(&worker, right, 2), true) == OK);
    assert(mica_kernel_commit(&owner) == OK && mica_kernel_end(&owner));
    assert(mica_task_init(&task, &worker, &kernel, &image, 1000, 2, 8, NULL) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    budget = dispatch_budget(&attempt.f_transaction);
    minted = mica_task_invoke_mint(&worker, &attempt, allowed, &budget);
    assert(minted.f_status == OK);
    struct invocation_resume resume = {&heap, &kernel, &task, allowed, minted.f_capability, attempt.f_epoch};
    assert(mica_task_suspend_request(&worker, &attempt, 1) == OK);
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == OK);
    assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(&task, &worker, &delivery) == OK);
    assert(mica_memory_root_pop(&worker, &delivery));
    assert(mica_task_acknowledge(&task, &worker, OK) == OK);
    assert(task.f_state == WAITING);
    assert(mica_task_wake(&task, &worker, task.f_generation, integer(0)) == OK);
    assert(mica_memory_leave(&worker));
    pthread_t thread;
    assert(!pthread_create(&thread, NULL, invocation_resume_worker, &resume));
    assert(!pthread_join(thread, NULL));
    assert(mica_memory_enter(&worker));
    assert(mica_task_release(&task, &worker));

    assert(mica_task_init(&task, &worker, &kernel, &image, 1000, 2, 8, NULL) == OK);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    assert(mica_kernel_cancel(&attempt.f_transaction));
    assert(mica_task_invoke_check(&worker, &attempt, allowed, capability) == CANCELLED);
    assert(mica_task_finish(&task, &worker, &attempt, CANCELLED) == CANCELLED);
    assert(mica_task_release(&task, &worker));
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
}
