// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

struct call_fixture {
    struct mica_SymbolTable symbols;
    struct mica_MemoryHeap heap;
    struct mica_MemoryWorker worker;
    struct mica_Kernel kernel;
    struct mica_MemoryRoot prototypes, roles, image;
    mica_type_Value selector, alias, role;
};

static void call_binding(struct mica_KernelTransaction *tx, uint64_t artifact, bool present) {
    mica_type_Value cells[] = {dispatch_identity(601), dispatch_identity(artifact)};
    assert(mica_kernel_write(tx, 704, mailbox_receivers(tx->f_worker, cells, 2), present) == OK);
}
static void call_body(struct mica_KernelTransaction *tx, uint64_t artifact,
        mica_function_type_TaskEntry entry, struct mica_TaskCodeRef *code) {
    struct mica_MemoryRoot body = {0};
    assert(mica_memory_root_push(tx->f_worker, &body));
    body.f_pointer = (void *)mica_task_new_CompiledProgram(tx->f_worker, entry, code);
    assert(body.f_pointer);
    assert(mica_kernel_program_put(tx, artifact, integer(7), body.f_pointer) == OK);
    assert(mica_memory_root_pop(tx->f_worker, &body));
}
static void call_fixture_init(struct call_fixture *f, mica_function_type_TaskEntry entry) {
    assert(mica_value_symbol_table_init(&f->symbols));
    assert(mica_task_dispatch_vocabulary_init(&f->symbols, mica_task_test_call_vocabulary()) == OK);
    f->selector = subscription_symbol(&f->symbols, "call");
    f->alias = subscription_symbol(&f->symbols, "alias");
    f->role = subscription_symbol(&f->symbols, "argument");
    assert(mica_task_heap_init(&f->heap));
    assert(mica_memory_worker_init(&f->worker, &f->heap, 4096));
    assert(mica_memory_enter(&f->worker));
    assert(mica_kernel_init(&f->kernel, &f->worker, integer(7), 1));
    assert(mica_memory_root_push(&f->worker, &f->prototypes));
    assert(mica_memory_root_push(&f->worker, &f->roles));
    assert(mica_memory_root_push(&f->worker, &f->image));
    mica_type_Value kinds[16];
    for (unsigned i = 0; i < 16; ++i) kinds[i] = dispatch_identity(9001 + i);
    mica_value_root_set(&f->prototypes, mailbox_receivers(&f->worker, kinds, 16));
    struct mica_ValueMapEntry argument = {f->role, integer(42)};
    struct mica_ValueResult roles = mica_value_map(&f->worker, &argument, 1);
    assert(roles.f_ok);
    mica_value_root_set(&f->roles, roles.f_value);
    mica_type_Value environment[] = {f->selector, mica_value_root_get(&f->roles), mica_value_root_get(&f->prototypes)};
    mica_type_Value captured = mailbox_receivers(&f->worker, environment, 3);
    f->image.f_pointer = (void *)mica_task_new_Image(&f->worker, mica_task_test_call_invoke,
        NULL, integer(7), captured, integer(0), NULL, NULL, 0, NULL, NULL);
    assert(f->image.f_pointer);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f->kernel, &f->worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 701, 701, 3, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 702, 702, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 703, 703, 6, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 704, 704, 2, 0, 0, 0) == OK);
    mica_type_Value selector[] = {dispatch_identity(601), f->selector};
    assert(mica_kernel_write(&tx, 702, mailbox_receivers(&f->worker, selector, 2), true) == OK);
    selector[1] = f->alias;
    assert(mica_kernel_write(&tx, 702, mailbox_receivers(&f->worker, selector, 2), true) == OK);
    struct mica_TaskDispatchVocabulary *vocabulary = mica_task_test_call_vocabulary();
    mica_type_Value parameter[] = {dispatch_identity(601), f->role,
        mica_value_symbol(vocabulary->f_unrestricted).f_value, integer(0),
        mica_value_symbol(vocabulary->f_required).f_value, integer(0)};
    assert(mica_kernel_write(&tx, 703, mailbox_receivers(&f->worker, parameter, 6), true) == OK);
    call_binding(&tx, 8001, true);
    call_body(&tx, 8001, entry, NULL);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
}
static void call_fixture_release(struct call_fixture *f) {
    assert(mica_memory_root_pop(&f->worker, &f->image));
    assert(mica_memory_root_pop(&f->worker, &f->roles));
    assert(mica_memory_root_pop(&f->worker, &f->prototypes));
    assert(mica_kernel_release(&f->kernel, &f->worker));
    assert(mica_memory_safepoint(&f->worker, true));
    assert(!f->heap.f_retained && !f->worker.f_roots);
    assert(mica_memory_leave(&f->worker));
    assert(mica_memory_worker_release(&f->worker));
    assert(mica_memory_heap_release(&f->heap));
    mica_value_symbol_table_release(&f->symbols);
}
static void call_task_init(struct call_fixture *f, struct mica_TaskControl *task, uint64_t depth) {
    assert(mica_task_init(task, &f->worker, &f->kernel, &f->image, 10000, 3, depth) == OK);
}
static struct mica_TaskDispatchSelection call_select(struct call_fixture *f,
        struct mica_TaskAttempt *attempt, mica_type_Value selector,
        struct mica_TaskImage *continuation, struct mica_KernelBudget *budget) {
    return mica_task_invoke(attempt->f_worker, attempt, mica_task_test_call_vocabulary(),
        702, 703, 701, 704, mica_value_root_get(&f->prototypes), selector,
        mica_value_root_get(&f->roles), continuation, budget);
}
static void call_acknowledge(struct mica_TaskControl *task, struct mica_MemoryWorker *worker, bool returned) {
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(task, worker, &delivery) == OK);
    assert(mica_memory_safepoint(worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    if (returned) {
        assert(boundary->f_kind == 1 && boundary->f_image->f_input == integer(42));
        assert(!boundary->f_image->f_caller && !boundary->f_image->f_depth);
        struct mica_TaskEffect *effect = boundary->f_effects;
        for (unsigned i = 0; i < 2; ++i) {
            assert(effect && effect->f_kind == 1 && effect->f_payload == integer(42));
            effect = effect->f_next;
        }
        assert(!effect);
    }
    assert(mica_memory_root_pop(worker, &delivery));
    assert(mica_task_acknowledge(task, worker, OK) == OK);
}
struct call_resume {
    struct mica_MemoryHeap *heap;
    struct mica_TaskControl *task;
};
static void *call_resume_worker(void *opaque) {
    struct call_resume *f = opaque;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, f->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_task_wake(f->task, &worker, f->task->f_generation, integer(99)) == OK);
    assert(mica_task_execute(f->task, &worker) == OK);
    call_acknowledge(f->task, &worker, true);
    assert(mica_task_observe(f->task, &worker).f_state == COMPLETED);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void method_calls(void) {
    struct call_fixture f = {0};
    call_fixture_init(&f, mica_task_test_call_value);
    struct mica_TaskControl task = {0};
    call_task_init(&f, &task, 8);
    assert(mica_task_execute(&task, &f.worker) == OK);
    call_acknowledge(&task, &f.worker, true);
    assert(mica_task_release(&task, &f.worker));
    call_task_init(&f, &task, 0);
    assert(mica_task_execute(&task, &f.worker) == LIMIT);
    assert(!task.f_committed && task.f_state == FAILED);
    assert(mica_task_release(&task, &f.worker));

    // A captured callee returns to its captured caller on another worker,
    // despite replacement/removal of the method's current catalogue binding.
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    call_body(&tx, 8001, mica_task_test_call_wait, NULL);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    call_task_init(&f, &task, 8);
    assert(mica_task_execute(&task, &f.worker) == OK);
    call_acknowledge(&task, &f.worker, false);
    assert(task.f_state == WAITING);
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    call_binding(&tx, 8001, false);
    call_binding(&tx, 8002, true);
    call_body(&tx, 8002, mica_task_test_call_value, NULL);
    assert(mica_kernel_program_remove(&tx, 8001) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    struct call_resume resume = {&f.heap, &task};
    assert(mica_memory_leave(&f.worker));
    pthread_t thread;
    assert(!pthread_create(&thread, NULL, call_resume_worker, &resume));
    assert(!pthread_join(thread, NULL));
    assert(mica_memory_enter(&f.worker));
    assert(mica_task_release(&task, &f.worker));
    call_task_init(&f, &task, 8);
    assert(mica_task_execute(&task, &f.worker) == OK);
    call_acknowledge(&task, &f.worker, true);
    assert(mica_task_release(&task, &f.worker));
    call_fixture_release(&f);
}

static void method_call_drafts(void) {
    struct call_fixture f = {0};
    call_fixture_init(&f, mica_task_test_call_value);
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot before = {0};
    assert(mica_memory_root_push(&f.worker, &before));
    call_task_init(&f, &task, 0);
    assert(mica_task_claim(&task, &f.worker, &attempt) == OK);
    before.f_pointer = attempt.f_current.f_pointer;
    struct mica_KernelBudget budget = dispatch_budget(&attempt.f_transaction);
    struct mica_TaskDispatchSelection result = call_select(&f, &attempt,
        subscription_symbol(&f.symbols, "missing"), NULL, &budget);
    assert(result.f_status == OK && result.f_count == 0 && attempt.f_current.f_pointer == before.f_pointer);
    budget = dispatch_budget(&attempt.f_transaction);
    budget.f_bytes = 0;
    result = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(result.f_status == LIMIT && !result.f_count && !f.worker.f_control);
    assert(attempt.f_current.f_pointer == before.f_pointer);
    mica_type_Value duplicate[] = {dispatch_identity(602), f.selector};
    assert(mica_kernel_write(&attempt.f_transaction, 702, mailbox_receivers(&f.worker, duplicate, 2), true) == OK);
    struct mica_TaskDispatchVocabulary *vocabulary = mica_task_test_call_vocabulary();
    mica_type_Value parameter[] = {dispatch_identity(602), f.role,
        mica_value_symbol(vocabulary->f_unrestricted).f_value, integer(0),
        mica_value_symbol(vocabulary->f_required).f_value, integer(0)};
    assert(mica_kernel_write(&attempt.f_transaction, 703, mailbox_receivers(&f.worker, parameter, 6), true) == OK);
    budget = dispatch_budget(&attempt.f_transaction);
    result = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(result.f_status == OK && result.f_count == 2 && attempt.f_current.f_pointer == before.f_pointer);
    assert(mica_kernel_write(&attempt.f_transaction, 702, mailbox_receivers(&f.worker, duplicate, 2), false) == OK);
    call_binding(&attempt.f_transaction, 8002, true);
    budget = dispatch_budget(&attempt.f_transaction);
    result = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(result.f_status == SCHEMA && attempt.f_current.f_pointer == before.f_pointer);
    call_binding(&attempt.f_transaction, 8002, false);
    // The kernel catalogue is opaque. The invocation boundary rejects a
    // managed object with the wrong body layout before interpreting its fields.
    assert(mica_kernel_program_put(&attempt.f_transaction, 8001, integer(7), before.f_pointer) == OK);
    budget = dispatch_budget(&attempt.f_transaction);
    result = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(result.f_status == SCHEMA && attempt.f_current.f_pointer == before.f_pointer);
    call_binding(&attempt.f_transaction, 8001, false);
    budget = dispatch_budget(&attempt.f_transaction);
    result = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(result.f_status == UNKNOWN && attempt.f_current.f_pointer == before.f_pointer);
    call_binding(&attempt.f_transaction, 8002, true);
    call_body(&attempt.f_transaction, 8002, mica_task_test_call_wait, NULL);
    budget = dispatch_budget(&attempt.f_transaction);
    result = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(result.f_status == OK && result.f_count == 1 && result.f_method == dispatch_identity(601));
    struct mica_TaskImage *callee = (void *)attempt.f_current.f_pointer;
    assert(callee->f_entry == mica_task_test_call_wait && !callee->f_depth && !callee->f_caller);
    assert(mica_memory_safepoint(&f.worker, true));
    assert(mica_task_finish(&task, &f.worker, &attempt, CLOSED) == CLOSED);
    assert(mica_task_release(&task, &f.worker));
    before.f_pointer = NULL;
    // Rollback discarded the replacement binding and body together.
    call_task_init(&f, &task, 8);
    assert(mica_task_execute(&task, &f.worker) == OK);
    call_acknowledge(&task, &f.worker, true);
    assert(mica_task_release(&task, &f.worker));
    assert(mica_memory_root_pop(&f.worker, &before));
    call_fixture_release(&f);
}

static void method_call_authority(void) {
    struct call_fixture f = {0};
    call_fixture_init(&f, mica_task_test_call_value);
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 11, 11, 1, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 12, 12, 2, 0, 0, 0) == OK);
    assert(mica_kernel_declare(&tx, 13, 13, 2, 0, 0, 0) == OK);
    mica_type_Value owner = integer(7);
    assert(mica_kernel_write(&tx, 11, mailbox_receivers(&f.worker, &owner, 1), true) == OK);
    mica_type_Value right[] = {integer(8), f.selector};
    assert(mica_kernel_write(&tx, 12, mailbox_receivers(&f.worker, right, 2), true) == OK);
    for (uint64_t id = 701; id <= 704; ++id)
        assert(mica_kernel_write(&tx, 13, permission(&f.worker, 8, id), true) == OK);
    assert(mica_kernel_set_authority_policy(&tx, 11, 0, 0, 13, 0, 0, 12) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    for (unsigned alias = 0; alias < 2; ++alias) {
        mica_type_Value environment[] = {alias ? f.alias : f.selector,
            mica_value_root_get(&f.roles), mica_value_root_get(&f.prototypes)};
        mica_type_Value captured = mailbox_receivers(&f.worker, environment, 3);
        f.image.f_pointer = (void *)mica_task_new_Image(&f.worker, mica_task_test_call_invoke,
            NULL, integer(8), captured, integer(0), NULL, NULL, 0, NULL, NULL);
        assert(f.image.f_pointer);
        struct mica_TaskControl task = {0};
        call_task_init(&f, &task, 8);
        if (alias) {
            // Both selectors name exactly the same compiled method. A right for
            // one does not authorize the other or expose a partial call image.
            assert(mica_task_execute(&task, &f.worker) == PERMISSION);
            assert(!task.f_committed && task.f_state == FAILED);
        } else {
            assert(mica_task_execute(&task, &f.worker) == OK);
            call_acknowledge(&task, &f.worker, true);
        }
        assert(mica_task_release(&task, &f.worker));
    }
    call_fixture_release(&f);
}

static void method_code_lifetimes(void) {
    struct call_fixture f = {0};
    call_fixture_init(&f, mica_task_test_call_value);
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_MemoryRoot codes[2] = {{0}};
    assert(mica_task_code_init(&registry, &f.worker) == OK);
    void *libraries[] = {code_library_open(), dlopen("./code-replacement.so", RTLD_NOW | RTLD_LOCAL)};
    assert(libraries[1]);
    for (unsigned i = 0; i < 2; ++i) {
        assert(mica_memory_root_push(&f.worker, &codes[i]));
        assert(mica_task_code_register(&registry, &f.worker, libraries[i], &codes[i]) == OK);
        if (i == 0) {
            struct mica_TaskControl private_task = {0};
            struct mica_TaskAttempt private_attempt = {0};
            call_task_init(&f, &private_task, 8);
            assert(mica_task_claim(&private_task, &f.worker, &private_attempt) == OK);
            call_body(&private_attempt.f_transaction, 8001, code_entry(libraries[i]), code_reference(&codes[i]));
            struct mica_KernelBudget budget = dispatch_budget(&private_attempt.f_transaction);
            struct mica_TaskDispatchSelection unsealed = call_select(&f, &private_attempt, f.selector, NULL, &budget);
            assert(unsealed.f_status == CLOSED && !unsealed.f_count);
            assert(mica_task_finish(&private_task, &f.worker, &private_attempt, CLOSED) == CLOSED);
            assert(mica_task_release(&private_task, &f.worker));
        }
        assert(mica_task_code_seal(&registry, &f.worker, code_reference(&codes[i])) == OK);
    }
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    call_body(&tx, 8001, code_entry(libraries[0]), code_reference(&codes[0]));
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    codes[0].f_pointer = NULL;
    struct mica_TaskControl suspended = {0}, fresh = {0};
    call_task_init(&f, &suspended, 8);
    assert(mica_task_execute(&suspended, &f.worker) == OK);
    call_acknowledge(&suspended, &f.worker, false);
    assert(suspended.f_state == WAITING);
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    call_binding(&tx, 8001, false);
    call_binding(&tx, 8002, true);
    call_body(&tx, 8002, code_entry(libraries[1]), code_reference(&codes[1]));
    assert(mica_kernel_program_remove(&tx, 8001) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_memory_root_pop(&f.worker, &codes[1]));
    assert(mica_memory_root_pop(&f.worker, &codes[0]));
    // Evict the previous snapshot. The suspended image alone retains its code.
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 901, 901, 1, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &f.worker);
    assert(reap.f_status == OK && !reap.f_released && code_library_loaded());
    struct code_catalogue_worker resume = {.heap = &f.heap, .kernel = &f.kernel,
        .registry = &registry, .task = &suspended};
    pthread_t thread;
    assert(mica_memory_leave(&f.worker));
    assert(!pthread_create(&thread, NULL, code_catalogue_resume, &resume));
    assert(!pthread_join(thread, NULL));
    assert(mica_memory_enter(&f.worker));
    // A later invocation follows MethodProgram to the replacement artifact.
    call_task_init(&f, &fresh, 8);
    assert(mica_task_execute(&fresh, &f.worker) == OK);
    call_acknowledge(&fresh, &f.worker, false);
    assert(fresh.f_state == COMPLETED);
    assert(mica_task_release(&fresh, &f.worker));
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    call_binding(&tx, 8002, false);
    assert(mica_kernel_program_remove(&tx, 8002) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    assert(mica_kernel_begin(&f.kernel, &f.worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 902, 902, 1, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    reap = mica_task_code_reap(&registry, &f.worker);
    assert(reap.f_status == OK && reap.f_released == 1 && code_library_loaded());
    assert(mica_memory_leave(&f.worker));
    assert(mica_task_cancel(&suspended));
    assert(mica_memory_enter(&f.worker));
    assert(mica_task_release(&suspended, &f.worker));
    reap = mica_task_code_reap(&registry, &f.worker);
    assert(reap.f_status == OK && reap.f_released == 1 && !code_library_loaded());
    assert(mica_task_code_close(&registry, &f.worker) == OK);
    assert(mica_task_code_release(&registry, &f.worker) == OK);
    call_fixture_release(&f);
}

static void method_call_retry(void) {
    struct call_fixture f = {0};
    call_fixture_init(&f, mica_task_test_call_value);
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    call_task_init(&f, &task, 8);
    assert(mica_task_claim(&task, &f.worker, &attempt) == OK);
    struct mica_KernelBudget budget = dispatch_budget(&attempt.f_transaction);
    assert(call_select(&f, &attempt, f.selector, NULL, &budget).f_count == 1);
    assert(((struct mica_TaskImage *)attempt.f_current.f_pointer)->f_entry == mica_task_test_call_value);
    struct mica_KernelTransaction rival = {0};
    assert(mica_kernel_begin(&f.kernel, &f.worker, &rival, integer(7)) == OK);
    call_binding(&rival, 8001, false);
    call_binding(&rival, 8002, true);
    call_body(&rival, 8002, mica_task_test_call_wait, NULL);
    assert(mica_kernel_commit(&rival) == OK && mica_kernel_end(&rival));
    assert(mica_kernel_commit(&attempt.f_transaction) == CONFLICT);
    assert(mica_task_detach(&task, &f.worker) == OK);
    assert(mica_task_attempt_retry(&attempt) == OK);
    assert(mica_task_attach(&task, &f.worker, &attempt) == OK);
    assert(mica_memory_safepoint(&f.worker, true));
    budget = dispatch_budget(&attempt.f_transaction);
    assert(call_select(&f, &attempt, f.selector, NULL, &budget).f_count == 1);
    assert(((struct mica_TaskImage *)attempt.f_current.f_pointer)->f_entry == mica_task_test_call_wait);
    assert(mica_kernel_cancel(&attempt.f_transaction));
    budget = dispatch_budget(&attempt.f_transaction);
    struct mica_TaskDispatchSelection cancelled = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(cancelled.f_status == CANCELLED && !cancelled.f_count);
    assert(mica_task_finish(&task, &f.worker, &attempt, CANCELLED) == CANCELLED);
    assert(mica_task_release(&task, &f.worker));
    call_fixture_release(&f);
}

static void method_return_cleanup(void) {
    struct call_fixture f = {0};
    call_fixture_init(&f, mica_task_test_call_value);
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_MemoryRoot cleanup = {0};
    assert(mica_memory_root_push(&f.worker, &cleanup));
    call_task_init(&f, &task, 0);
    assert(mica_task_claim(&task, &f.worker, &attempt) == OK);
    cleanup.f_pointer = (void *)mica_task_new_Image(&f.worker, mica_task_test_call_cleanup,
        NULL, integer(7), integer(0), integer(0), NULL, NULL, 0, NULL, NULL);
    assert(cleanup.f_pointer);
    assert(mica_task_handler_push(&attempt, &f.worker, (void *)cleanup.f_pointer, 2) == OK);
    struct mica_KernelBudget budget = dispatch_budget(&attempt.f_transaction);
    struct mica_TaskDispatchSelection selected = call_select(&f, &attempt, f.selector, NULL, &budget);
    assert(selected.f_status == OK && selected.f_count == 1);
    assert(mica_memory_safepoint(&f.worker, true));
    assert(((struct mica_TaskImage *)attempt.f_current.f_pointer)->f_entry(&attempt) == OK);
    assert(attempt.f_boundary == 1);
    assert(mica_task_prepare(&task, &f.worker, &attempt) == OK);
    assert(mica_task_commit(&task, &f.worker, &attempt) == OK);
    assert(mica_task_finish(&task, &f.worker, &attempt, OK) == OK);
    struct mica_MemoryRoot delivery = {0};
    assert(mica_task_delivery(&task, &f.worker, &delivery) == OK);
    assert(mica_memory_safepoint(&f.worker, true));
    struct mica_TaskBoundary *boundary = (void *)delivery.f_pointer;
    assert(boundary->f_image->f_input == integer(42));
    assert(!boundary->f_image->f_handlers && !boundary->f_image->f_caller);
    struct mica_TaskEffect *effect = boundary->f_effects;
    assert(effect && effect->f_payload == integer(42));
    effect = effect->f_next;
    assert(effect && !effect->f_payload && !effect->f_next);
    assert(mica_memory_root_pop(&f.worker, &delivery));
    assert(mica_task_acknowledge(&task, &f.worker, OK) == OK);
    assert(mica_task_release(&task, &f.worker));
    assert(mica_memory_root_pop(&f.worker, &cleanup));
    call_fixture_release(&f);
}
