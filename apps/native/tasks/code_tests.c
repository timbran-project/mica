// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void *code_library_open(void) {
    void *library = dlopen("./code-library.so", RTLD_NOW | RTLD_LOCAL);
    assert(library);
    return library;
}

static bool code_library_loaded(void) {
    void *library = dlopen("./code-library.so", RTLD_NOW | RTLD_NOLOAD);
    if (!library) return false;
    assert(dlclose(library) == 0);
    return true;
}

static struct mica_TaskCodeRef *code_reference(struct mica_MemoryRoot *root) {
    return (void *)root->f_pointer;
}

static void code_registry(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_TaskCodeRegistry registry = {0}, dependency_registry = {0};
    struct mica_MemoryRoot first = {0}, second = {0}, shared = {0}, invalid = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_task_code_init(&registry, &worker) == OK);
    assert(mica_task_code_init(&dependency_registry, &worker) == OK);
    assert(mica_task_code_init(&registry, &worker) == CLOSED);
    void *library = code_library_open();
    assert(mica_task_code_register(&registry, &worker, library, &invalid) == SCHEMA);
    assert(dlclose(library) == 0); // Failure leaves ownership with the caller.
    assert(!code_library_loaded());
    assert(mica_memory_root_push(&worker, &first));
    assert(mica_memory_root_push(&worker, &second));
    library = code_library_open();
    atomic_store(&allocation_budget, 0);
    assert(mica_task_code_register(&registry, &worker, library, &first) == OOM);
    atomic_store(&allocation_budget, -1);
    assert(!first.f_pointer && !registry.f_owners);
    assert(mica_task_code_register(&registry, &worker, library, &first) == OK);
    assert(mica_task_code_register(&dependency_registry, &worker, code_library_open(), &second) == OK);
    assert(!mica_memory_publish(&worker, &first, &shared)); // Unsealed code cannot escape.
    assert(!shared.f_registration);
    assert(mica_task_code_depend(&registry, &worker, code_reference(&first), code_reference(&second)) == OK);
    assert(mica_task_code_depend(&dependency_registry, &worker, code_reference(&second), code_reference(&first)) == OK);
    struct mica_MemoryRoot pending = {0};
    assert(mica_memory_root_push(&worker, &pending));
    pending.f_pointer = mica_memory_allocate(&worker, 2048, 0);
    assert(pending.f_pointer && mica_memory_object(pending.f_pointer)->f_state == 1);
    uint64_t collections = heap.f_collections;
    atomic_store(&allocation_budget, 0);
    struct mica_TaskCodeReap failed = mica_task_code_reap(&registry, &worker);
    atomic_store(&allocation_budget, -1);
    assert(failed.f_status == OOM && failed.f_released == 0);
    assert(heap.f_collections == collections && code_library_loaded());
    assert(mica_memory_root_pop(&worker, &pending));
    uint8_t text[512];
    memset(text, 'c', sizeof(text));
    struct mica_ValueResult string = mica_value_string(&worker, text, sizeof(text));
    assert(string.f_ok);
    assert(mica_task_code_constants(&registry, &worker, code_reference(&first), string.f_value) == OK);
    assert(mica_task_code_seal(&registry, &worker, code_reference(&first)) == OK);
    assert(!mica_memory_publish(&worker, &first, &shared)); // Every dependency must be sealed.
    assert(mica_task_code_seal(&dependency_registry, &worker, code_reference(&second)) == OK);
    assert(mica_memory_publish(&worker, &first, &shared));
    assert(mica_task_code_constants(&registry, &worker, code_reference(&first), integer(9)) == CLOSED);
    assert(mica_task_code_depend(&registry, &worker, code_reference(&first), code_reference(&second)) == CLOSED);
    assert(mica_memory_root_pop(&worker, &second));
    assert(mica_memory_root_pop(&worker, &first));
    for (unsigned round = 0; round < 3; ++round) {
        struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
        assert(reap.f_status == OK && reap.f_released == 0);
        reap = mica_task_code_reap(&dependency_registry, &worker);
        assert(reap.f_status == OK && reap.f_released == 0);
        assert(code_library_loaded());
    }
    struct mica_TaskCodeRef *reference = code_reference(&shared);
    assert(reference->f_dependencies->f_code->f_dependencies->f_code == reference);
    struct mica_HeapStringResult retained = mica_value_as_string(reference->f_constants);
    assert(retained.f_ok && retained.f_header->f_length == sizeof(text));
    assert(memcmp(retained.f_header->f_data, text, sizeof(text)) == 0);
    void *symbol = dlsym(reference->f_owner->f_library, "mica_code_test_identity");
    uint64_t (*identity)(uint64_t);
    _Static_assert(sizeof(identity) == sizeof(symbol), "loader function pointer ABI");
    memcpy(&identity, &symbol, sizeof(identity));
    assert(identity && identity(37) == 37);
    assert(mica_task_code_close(&registry, &worker) == OK);
    assert(mica_task_code_release(&registry, &worker) == CLOSED);
    assert(mica_memory_unpublish(&worker, &shared));
    struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1);
    assert(code_library_loaded()); // The dependency registry still owns its handle.
    reap = mica_task_code_reap(&dependency_registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1);
    assert(!code_library_loaded()); // Unreachable cycles retain neither library handle.
    assert(mica_task_code_release(&registry, &worker) == OK);
    assert(mica_task_code_close(&dependency_registry, &worker) == OK);
    assert(mica_task_code_release(&dependency_registry, &worker) == OK);
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct code_reaper {
    struct mica_MemoryHeap *heap;
    struct mica_TaskCodeRegistry *registry;
    struct mica_TaskCodeReap result;
};

static void *code_reap_thread(void *argument) {
    struct code_reaper *reaper = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, reaper->heap, 4096));
    assert(mica_memory_enter(&worker));
    reaper->result = mica_task_code_reap(reaper->registry, &worker);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static mica_function_type_TaskEntry code_entry(void *library) {
    void *symbol = dlsym(library, "mica_code_test_entry");
    mica_function_type_TaskEntry entry;
    _Static_assert(sizeof(entry) == sizeof(symbol), "loader entry ABI");
    memcpy(&entry, &symbol, sizeof(entry));
    assert(entry);
    return entry;
}

static struct mica_TaskCodeRegistry *code_checkpoint_registry;

static void code_active_checkpoint(struct mica_TaskAttempt *attempt) {
    struct mica_TaskImage *current = (void *)attempt->f_current.f_pointer;
    assert(current->f_code != (void *)attempt->f_executing.f_pointer);
    assert(attempt->f_executing.f_pointer);
    struct mica_TaskCodeReap reap = mica_task_code_reap(code_checkpoint_registry, attempt->f_worker);
    assert(reap.f_status == OK && reap.f_released == 0);
    assert(code_library_loaded());
}

static void code_active_entry(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskControl task = {0};
    struct mica_TaskAttempt attempt = {0};
    struct mica_TaskServices services = {0};
    struct mica_MemoryRoot codes[2] = {{0}}, target = {0}, source = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_task_code_init(&registry, &worker) == OK);
    void *libraries[2] = {code_library_open(), dlopen("./code-replacement.so", RTLD_NOW | RTLD_LOCAL)};
    assert(libraries[1]);
    for (unsigned i = 0; i < 2; ++i) {
        assert(mica_memory_root_push(&worker, &codes[i]));
        assert(mica_task_code_register(&registry, &worker, libraries[i], &codes[i]) == OK);
        assert(mica_task_code_seal(&registry, &worker, code_reference(&codes[i])) == OK);
    }
    void *symbol = dlsym(libraries[0], "mica_code_test_set_checkpoint");
    void (*set_checkpoint)(void (*)(struct mica_TaskAttempt *));
    _Static_assert(sizeof(set_checkpoint) == sizeof(symbol), "loader checkpoint ABI");
    memcpy(&set_checkpoint, &symbol, sizeof(set_checkpoint));
    assert(set_checkpoint);
    code_checkpoint_registry = &registry;
    set_checkpoint(code_active_checkpoint);
    symbol = dlsym(libraries[0], "mica_code_test_advance");
    mica_function_type_TaskEntry advance;
    memcpy(&advance, &symbol, sizeof(advance));
    assert(advance);
    mica_function_type_TaskEntry complete = code_entry(libraries[1]);
    assert(mica_memory_root_push(&worker, &target));
    assert(mica_memory_root_push(&worker, &source));
    target.f_pointer = (void *)mica_task_new_Image(&worker, complete, code_reference(&codes[1]),
        integer(7), integer(8), integer(0), NULL, NULL, 1, NULL, NULL);
    assert(target.f_pointer);
    struct mica_TaskHandler *next = mica_task_new_Handler(&worker, NULL, (void *)target.f_pointer, 1);
    assert(next);
    source.f_pointer = (void *)mica_task_new_Image(&worker, advance, code_reference(&codes[0]),
        integer(7), integer(8), integer(0), next, NULL, 1, NULL, NULL);
    assert(source.f_pointer);
    // The retry image is permanently linked code. Neither it nor the control
    // retains the intermediate module entered later in this segment.
    mailbox_task(&worker, &kernel, &task, false);
    assert(mica_task_claim(&task, &worker, &attempt) == OK);
    attempt.f_current.f_pointer = source.f_pointer;
    source.f_pointer = target.f_pointer = codes[0].f_pointer = codes[1].f_pointer = NULL;
    assert(advance(&attempt) == OK);
    assert(code_library_loaded());
    assert(complete(&attempt) == OK); // The next prologue releases the previous execution pin.
    struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1 && !code_library_loaded());
    assert(mica_task_prepare(&task, &worker, &attempt) == OK);
    assert(mica_task_commit(&task, &worker, &attempt) == OK);
    assert(mica_task_finish(&task, &worker, &attempt, OK) == OK);
    assert(mica_task_dispatch(&task, &worker, &services) == OK);
    assert(mica_task_release(&task, &worker));
    assert(mica_memory_root_pop(&worker, &source));
    assert(mica_memory_root_pop(&worker, &target));
    for (unsigned i = 2; i > 0; --i) assert(mica_memory_root_pop(&worker, &codes[i - 1]));
    reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1);
    assert(mica_task_code_close(&registry, &worker) == OK);
    assert(mica_task_code_release(&registry, &worker) == OK);
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true) && heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    code_checkpoint_registry = NULL;
}

static void code_task_lifetimes(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskControl tasks[2] = {{0}};
    struct mica_TaskServices services = {0};
    struct mica_MemoryRoot code = {0}, image = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 64));
    assert(mica_task_code_init(&registry, &worker) == OK);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &image));
    for (unsigned i = 0; i < 2; ++i) {
        void *library = dlopen(i ? "./code-replacement.so" : "./code-library.so", RTLD_NOW | RTLD_LOCAL);
        assert(library);
        assert(mica_task_code_register(&registry, &worker, library, &code) == OK);
        assert(mica_task_code_seal(&registry, &worker, code_reference(&code)) == OK);
        image.f_pointer = (void *)mica_task_new_Image(&worker, code_entry(library), code_reference(&code),
            integer(7), integer(8), integer(0), NULL, NULL, 1, NULL, NULL);
        assert(image.f_pointer);
        assert(mica_task_init(&tasks[i], &worker, &kernel, &image, 100, 3, 8, NULL) == OK);
        image.f_pointer = code.f_pointer = NULL;
    }
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_root_pop(&worker, &code));
    for (unsigned i = 0; i < 2; ++i) {
        assert(mica_task_execute(&tasks[i], &worker) == OK);
        assert(mica_task_dispatch(&tasks[i], &worker, &services) == OK);
    }
    assert(tasks[0].f_state == WAITING && tasks[1].f_state == COMPLETED);
    assert(mica_task_release(&tasks[1], &worker));
    struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1 && code_library_loaded());
    assert(!dlopen("./code-replacement.so", RTLD_NOW | RTLD_NOLOAD));
    assert(mica_task_wake(&tasks[0], &worker, tasks[0].f_generation, integer(42)) == OK);
    // Another native worker executes the retained entry after replacement and GC.
    struct concurrent_task resumed = {.heap = &heap, .task = &tasks[0]};
    pthread_t thread;
    assert(mica_memory_leave(&worker));
    assert(!pthread_create(&thread, NULL, execute_task, &resumed));
    assert(!pthread_join(thread, NULL));
    assert(resumed.status == OK);
    assert(mica_memory_enter(&worker));
    assert(mica_task_dispatch(&tasks[0], &worker, &services) == OK);
    assert(tasks[0].f_state == WAITING); // It still executes the captured version.
    assert(mica_memory_leave(&worker));
    assert(mica_task_cancel(&tasks[0]));
    assert(mica_memory_enter(&worker));
    assert(mica_task_release(&tasks[0], &worker));
    assert(mica_task_code_close(&registry, &worker) == OK);
    struct code_reaper reapers[2] = {{.heap = &heap, .registry = &registry}, {.heap = &heap, .registry = &registry}};
    pthread_t threads[2];
    assert(mica_memory_leave(&worker));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_create(&threads[i], NULL, code_reap_thread, &reapers[i]));
    for (unsigned i = 0; i < 2; ++i) assert(!pthread_join(threads[i], NULL));
    assert(reapers[0].result.f_status == OK && reapers[1].result.f_status == OK);
    assert(reapers[0].result.f_released + reapers[1].result.f_released == 1);
    assert(!code_library_loaded());
    assert(mica_memory_enter(&worker));
    assert(mica_task_code_release(&registry, &worker) == OK);
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(heap.f_retained == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void code_program_install(struct mica_MemoryWorker *worker,
                                 struct mica_KernelTransaction *tx,
                                 mica_function_type_TaskEntry entry,
                                 struct mica_TaskCodeRef *code) {
    struct mica_MemoryRoot body = {0};
    assert(mica_memory_root_push(worker, &body));
    body.f_pointer = (void *)mica_task_new_CompiledProgram(worker, entry, code);
    assert(body.f_pointer);
    assert(mica_kernel_program_put(tx, 1, integer(7), body.f_pointer) == OK);
    assert(mica_memory_root_pop(worker, &body));
}

// Selection by program ID isolates catalogue retention. The method-call
// fixtures separately exercise named-role resolution and invocation authority.
static void code_program_task(struct mica_MemoryWorker *worker,
                              struct mica_KernelTransaction *tx,
                              struct mica_TaskControl *task) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    struct mica_KernelProgramResult selected = mica_kernel_program_get(tx, 1);
    assert(selected.f_status == OK && selected.f_program);
    struct mica_TaskCompiledProgram *body = (void *)selected.f_program->f_body;
    image.f_pointer = (void *)mica_task_new_Image(worker, body->f_entry, body->f_code,
        integer(7), integer(0), integer(0), NULL, NULL, 1, NULL, NULL);
    assert(image.f_pointer);
    assert(mica_task_init(task, worker, tx->f_kernel, &image, 1000, 2, 8, NULL) == OK);
    assert(mica_memory_root_pop(worker, &image));
}

struct code_catalogue_worker {
    struct mica_MemoryHeap *heap;
    struct mica_Kernel *kernel;
    struct mica_TaskCodeRegistry *registry;
    struct mica_MemoryRoot *code;
    mica_function_type_TaskEntry entry;
    struct mica_TaskControl *task;
};
static void *code_catalogue_replace(void *argument) {
    struct code_catalogue_worker *input = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_MemoryRoot code = {0};
    struct mica_KernelTransaction tx = {0};
    assert(mica_memory_worker_init(&worker, input->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_acquire(&worker, input->code, &code));
    assert(mica_memory_safepoint(&worker, true));
    assert(mica_kernel_begin(input->kernel, &worker, &tx, integer(7)) == OK);
    code_program_install(&worker, &tx, input->entry, code_reference(&code));
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    // Evict the history entry containing the first program version. The
    // original worker's held transaction must keep that body and library alive.
    assert(mica_kernel_begin(input->kernel, &worker, &tx, integer(7)) == OK);
    assert(mica_kernel_declare(&tx, 900, 900, 1, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&tx) == OK && mica_kernel_end(&tx));
    struct mica_TaskCodeReap reap = mica_task_code_reap(input->registry, &worker);
    assert(reap.f_status == OK && !reap.f_released);
    assert(mica_memory_root_pop(&worker, &code));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}
static void *code_catalogue_resume(void *argument) {
    struct code_catalogue_worker *input = argument;
    struct mica_MemoryWorker worker = {0};
    struct mica_TaskServices services = {0};
    assert(mica_memory_worker_init(&worker, input->heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_safepoint(&worker, true));
    struct mica_TaskObservation before = mica_task_observe(input->task, &worker);
    assert(before.f_status == OK && before.f_state == WAITING);
    assert(mica_task_wake(input->task, &worker, before.f_generation, integer(1)) == OK);
    assert(mica_task_execute(input->task, &worker) == OK);
    assert(mica_task_dispatch(input->task, &worker, &services) == OK);
    struct mica_TaskObservation resumed = mica_task_observe(input->task, &worker);
    assert(resumed.f_status == OK && resumed.f_state == WAITING);
    assert(resumed.f_generation == before.f_generation + 1);
    struct mica_TaskCodeReap reap = mica_task_code_reap(input->registry, &worker);
    assert(reap.f_status == OK && !reap.f_released);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void code_catalogue_lifetimes(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskServices services = {0};
    struct mica_TaskControl suspended = {0}, replacement = {0};
    struct mica_MemoryRoot codes[2] = {{0}};
    struct mica_KernelTransaction install = {0}, held = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_kernel_init(&kernel, &worker, integer(7), 1));
    assert(mica_task_code_init(&registry, &worker) == OK);
    void *libraries[] = {code_library_open(), dlopen("./code-replacement.so", RTLD_NOW | RTLD_LOCAL)};
    assert(libraries[1]);
    for (unsigned i = 0; i < 2; ++i) {
        assert(mica_memory_root_push(&worker, &codes[i]));
        assert(mica_task_code_register(&registry, &worker, libraries[i], &codes[i]) == OK);
        assert(mica_task_code_seal(&registry, &worker, code_reference(&codes[i])) == OK);
    }
    assert(mica_kernel_begin(&kernel, &worker, &install, integer(7)) == OK);
    code_program_install(&worker, &install, code_entry(libraries[0]), code_reference(&codes[0]));
    assert(mica_kernel_commit(&install) == OK && mica_kernel_end(&install));
    codes[0].f_pointer = NULL;
    struct mica_MemoryRoot abandoned = {0};
    assert(mica_memory_root_push(&worker, &abandoned));
    void *unpublished = code_library_open();
    assert(mica_task_code_register(&registry, &worker, unpublished, &abandoned) == OK);
    assert(mica_task_code_seal(&registry, &worker, code_reference(&abandoned)) == OK);
    assert(mica_kernel_begin(&kernel, &worker, &install, integer(7)) == OK);
    code_program_install(&worker, &install, code_entry(unpublished), code_reference(&abandoned));
    assert(mica_kernel_end(&install));
    assert(mica_memory_root_pop(&worker, &abandoned));
    struct mica_TaskCodeReap aborted = mica_task_code_reap(&registry, &worker);
    assert(aborted.f_status == OK && aborted.f_released == 1 && code_library_loaded());
    assert(mica_kernel_begin(&kernel, &worker, &held, integer(7)) == OK);
    struct mica_MemoryRoot shared = {0};
    assert(mica_memory_publish(&worker, &codes[1], &shared));
    codes[1].f_pointer = NULL;
    struct code_catalogue_worker input = {.heap = &heap, .kernel = &kernel,
        .registry = &registry, .code = &shared, .entry = code_entry(libraries[1]), .task = &suspended};
    pthread_t other;
    assert(mica_memory_leave(&worker));
    assert(!pthread_create(&other, NULL, code_catalogue_replace, &input));
    assert(!pthread_join(other, NULL));
    assert(mica_memory_enter(&worker));
    assert(mica_memory_unpublish(&worker, &shared));
    struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && !reap.f_released && code_library_loaded());
    code_program_task(&worker, &held, &suspended);
    assert(mica_kernel_end(&held));
    assert(mica_memory_root_pop(&worker, &codes[1]));
    assert(mica_memory_root_pop(&worker, &codes[0]));
    assert(mica_task_execute(&suspended, &worker) == OK);
    assert(mica_task_dispatch(&suspended, &worker, &services) == OK);
    assert(mica_task_observe(&suspended, &worker).f_state == WAITING);
    assert(mica_memory_leave(&worker));
    assert(!pthread_create(&other, NULL, code_catalogue_resume, &input));
    assert(!pthread_join(other, NULL));
    assert(mica_memory_enter(&worker));
    reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && !reap.f_released && code_library_loaded());

    assert(mica_kernel_begin(&kernel, &worker, &install, integer(7)) == OK);
    code_program_task(&worker, &install, &replacement);
    assert(mica_kernel_end(&install));
    assert(mica_task_execute(&replacement, &worker) == OK);
    assert(mica_task_dispatch(&replacement, &worker, &services) == OK);
    assert(mica_task_observe(&replacement, &worker).f_state == COMPLETED);
    assert(mica_task_release(&replacement, &worker));
    assert(mica_kernel_begin(&kernel, &worker, &install, integer(7)) == OK);
    assert(mica_kernel_program_remove(&install, 1) == OK);
    assert(mica_kernel_commit(&install) == OK && mica_kernel_end(&install));
    assert(mica_kernel_begin(&kernel, &worker, &install, integer(7)) == OK);
    assert(mica_kernel_declare(&install, 901, 901, 1, 0, 0, 0) == OK);
    assert(mica_kernel_commit(&install) == OK && mica_kernel_end(&install));
    // The waiting task still retains the earlier library after catalogue removal.
    reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1 && code_library_loaded());
    assert(mica_memory_leave(&worker));
    assert(mica_task_cancel(&suspended));
    assert(mica_memory_enter(&worker));
    assert(mica_task_release(&suspended, &worker));
    reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1 && !code_library_loaded());
    assert(mica_task_code_close(&registry, &worker) == OK);
    assert(mica_task_code_release(&registry, &worker) == OK);
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

// A published closure alone retains its library, including a code/table cycle.
// Once the last closure dies, tracing must permit both the cycle and dlclose.
static void code_closure_lifetime(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_MemoryRoot code = {0}, table = {0}, closure = {0}, shared = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_task_code_init(&registry, &worker) == OK);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &table));
    assert(mica_memory_root_push(&worker, &closure));
    void *library = code_library_open();
    assert(mica_task_code_register(&registry, &worker, library, &code) == OK);
    struct mica_TaskFunctionCode **data = (void *)mica_memory_allocate(&worker, sizeof(*data), 0);
    assert(data);
    data[0] = mica_task_new_FunctionCode(&worker, code_entry(library), code_reference(&code), 0, 0, false);
    assert(data[0]);
    table.f_pointer = (void *)mica_task_new_FunctionTable(&worker, data, 1);
    assert(table.f_pointer);
    assert(mica_task_code_functions(&registry, &worker, code_reference(&code), (void *)table.f_pointer) == OK);
    assert(mica_task_code_seal(&registry, &worker, code_reference(&code)) == OK);
    struct mica_ValueResult captures = mica_value_list(&worker, NULL, 0);
    assert(captures.f_ok);
    struct mica_TaskFunctionTable *functions = (void *)table.f_pointer;
    struct mica_ValueResult function = mica_value_function(&worker, 42, (void *)functions->f_data[0], captures.f_value);
    assert(function.f_ok);
    mica_value_root_set(&closure, function.f_value);
    assert(mica_memory_publish(&worker, &closure, &shared));
    assert(mica_memory_root_pop(&worker, &closure));
    assert(mica_memory_root_pop(&worker, &table));
    assert(mica_memory_root_pop(&worker, &code));
    for (unsigned round = 0; round < 4; ++round) {
        struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
        assert(reap.f_status == OK && !reap.f_released && code_library_loaded());
        struct mica_HeapFunctionResult value = mica_value_as_function(mica_value_root_get(&shared));
        assert(value.f_ok && value.f_header->f_identity == 42);
        struct mica_TaskFunctionCode *body = (void *)value.f_header->f_target;
        assert(body->f_code->f_functions->f_data[0] == body);
    }
    assert(mica_memory_unpublish(&worker, &shared));
    struct mica_TaskCodeReap reap = mica_task_code_reap(&registry, &worker);
    assert(reap.f_status == OK && reap.f_released == 1 && !code_library_loaded());
    assert(mica_task_code_close(&registry, &worker) == OK);
    assert(mica_task_code_release(&registry, &worker) == OK);
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}
