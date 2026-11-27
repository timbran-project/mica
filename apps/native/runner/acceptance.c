#include <assert.h>
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct process_application {
    void *library;
    struct mica_MemoryRoot code;
    char *prefix;
    char *path;
    int64_t method_id;
    int64_t program_id;
};

static mica_type_Value process_integer(int64_t number) {
    struct mica_ValueResult result = mica_value_int(number);
    assert(result.f_ok);
    return result.f_value;
}

static mica_type_Value process_symbol(struct mica_SymbolTable *symbols, const char *name) {
    struct mica_IdResult id = mica_value_symbol_intern(symbols,
        (const uint8_t *)name, strlen(name), NULL);
    assert(id.f_ok);
    struct mica_ValueResult symbol = mica_value_symbol(id.f_number);
    assert(symbol.f_ok);
    return symbol.f_value;
}

static mica_type_Value process_field(struct mica_SymbolTable *symbols,
    mica_type_Value map, const char *name) {
    struct mica_ValueResult field = mica_value_map_get(map, process_symbol(symbols, name));
    assert(field.f_ok);
    return field.f_value;
}

static int64_t process_number(mica_type_Value value) {
    struct mica_IntResult number = mica_value_as_int(value);
    assert(number.f_ok);
    return number.f_number;
}

static char *process_string(mica_type_Value value) {
    struct mica_HeapStringResult text = mica_value_as_string(value);
    assert(text.f_ok);
    char *copy = malloc(text.f_header->f_length + 1);
    assert(copy);
    memcpy(copy, text.f_header->f_data, text.f_header->f_length);
    copy[text.f_header->f_length] = 0;
    return copy;
}

static void process_write(mica_type_Value value, const char *path) {
    struct mica_HeapStringResult text = mica_value_as_string(value);
    assert(text.f_ok);
    FILE *file = fopen(path, "wb");
    assert(file);
    assert(fwrite(text.f_header->f_data, 1, text.f_header->f_length, file) == text.f_header->f_length);
    assert(fclose(file) == 0);
}

static mica_type_Value process_read(struct mica_MemoryWorker *worker, const char *path) {
    FILE *file = fopen(path, "rb");
    assert(file);
    assert(fseek(file, 0, SEEK_END) == 0);
    long length = ftell(file);
    assert(length >= 0 && fseek(file, 0, SEEK_SET) == 0);
    uint8_t *bytes = malloc(length ? (size_t)length : 1);
    assert(bytes);
    assert(fread(bytes, 1, (size_t)length, file) == (size_t)length);
    assert(fclose(file) == 0);
    struct mica_ValueResult source = mica_value_string(worker, bytes, (uint64_t)length);
    assert(source.f_ok);
    free(bytes);
    return source.f_value;
}

static void process_compile(char **arguments) {
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        execvp(arguments[0], arguments);
        perror("clang");
        _Exit(127);
    }
    int status = 0;
    pid_t waited;
    do {
        waited = waitpid(child, &status, 0);
    } while (waited < 0 && errno == EINTR);
    assert(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static void process_artifacts(struct mica_SymbolTable *symbols, mica_type_Value artifacts,
    char *library) {
    char *header = process_string(process_field(symbols, artifacts, "header_name"));
    process_write(process_field(symbols, artifacts, "header"), header);
    free(header);
    mica_type_Value sources = process_field(symbols, artifacts, "sources");
    uint64_t count = mica_value_list_length(sources).f_number;
    char **names = calloc(count ? count : 1, sizeof(*names));
    char **arguments = calloc(count + 32, sizeof(*arguments));
    assert(names && arguments);
    for (uint64_t index = 0; index < count; ++index) {
        struct mica_ValueResult unit = mica_value_list_get(sources, index);
        assert(unit.f_ok);
        struct mica_ValueResult name = mica_value_list_get(unit.f_value, 0);
        struct mica_ValueResult contents = mica_value_list_get(unit.f_value, 1);
        assert(name.f_ok && contents.f_ok);
        names[index] = process_string(name.f_value);
        process_write(contents.f_value, names[index]);
    }
    size_t position = 0;
    arguments[position++] = "clang";
    arguments[position++] = "-std=c11";
    arguments[position++] = "-shared";
    arguments[position++] = "-fPIC";
    arguments[position++] = "-pthread";
    arguments[position++] = "-Wall";
    arguments[position++] = "-Wextra";
    arguments[position++] = "-Werror";
    arguments[position++] = "-pedantic";
    arguments[position++] = "-ffp-contract=off";
    arguments[position++] = "-fno-fast-math";
    arguments[position++] = "-ffunction-sections";
    arguments[position++] = "-fno-global-isel";
    arguments[position++] = MICA_NATIVE_PROCESS_SANITIZER;
    arguments[position++] = "-fno-sanitize-recover=all";
    arguments[position++] = "-fno-omit-frame-pointer";
    arguments[position++] = "-O0";
    arguments[position++] = "-g0";
    arguments[position++] = "-I.";
    arguments[position++] = "-o";
    arguments[position++] = library;
    for (uint64_t index = 0; index < count; ++index)
        arguments[position++] = names[index];
    arguments[position++] = "-lm";
    arguments[position++] = "-ldl";
    arguments[position] = NULL;
    process_compile(arguments);
    for (uint64_t index = 0; index < count; ++index) free(names[index]);
    free(names);
    free(arguments);
}

static uint64_t process_run(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime,
    mica_function_type_TaskEntry entry, mica_type_Value arguments,
    struct mica_MemoryRoot *output, uint64_t steps, bool diagnostics) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    image.f_pointer = (void *)mica_task_new_Image(worker, entry,
        (void *)code->f_pointer, process_integer(7), arguments, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl task = {0};
    assert(mica_task_init(&task, worker, kernel, &image, steps, 3, 256, runtime) == 0);
    struct mica_TaskServices services = {0};
    for (unsigned boundary = 0; ; ++boundary) {
        assert(boundary < 16);
        uint64_t status = mica_task_execute(&task, worker);
        if (status != 0) {
            if (diagnostics) {
                fprintf(stderr, "native compiler task failed: status=%llu, remaining=%llu\n",
                    (unsigned long long)status, (unsigned long long)task.f_remaining);
                struct mica_MemoryRoot error = {0};
                if (mica_task_error(&task, worker, &error) == 0) {
                    struct mica_HeapErrorResult raised = mica_value_as_error(mica_value_root_get(&error));
                    if (raised.f_ok && raised.f_header->f_has_message) {
                        struct mica_HeapStringResult text = mica_value_as_string(raised.f_header->f_message);
                        if (text.f_ok) {
                            fwrite(text.f_header->f_data, 1, text.f_header->f_length, stderr);
                            fputc('\n', stderr);
                        }
                    }
                    assert(mica_memory_root_pop(worker, &error));
                }
            }
            assert(mica_task_release(&task, worker));
            assert(mica_memory_root_pop(worker, &image));
            return status;
        }
        assert(mica_task_dispatch(&task, worker, &services) == 0);
        struct mica_TaskObservation observed = mica_task_observe(&task, worker);
        assert(observed.f_status == 0);
        if (observed.f_state == 5) break;
        if (observed.f_state == 4) {
            assert(mica_task_wake(&task, worker, observed.f_generation, process_integer(0)) == 0);
        } else {
            assert(observed.f_state == 1);
        }
    }
    struct mica_MemoryRoot acquired = {0};
    assert(mica_memory_acquire(worker, &task.f_image, &acquired));
    mica_value_root_set(output, ((struct mica_TaskImage *)acquired.f_pointer)->f_input);
    assert(mica_memory_root_pop(worker, &acquired));
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &image));
    return 0;
}

static void *process_lookup(void *library, const char *name) {
    dlerror();
    void *address = dlsym(library, name);
    const char *error = dlerror();
    if (error) {
        fprintf(stderr, "%s: %s\n", name, error);
        abort();
    }
    return address;
}

static void *process_named(void *library, const char *prefix, const char *format) {
    size_t length = strlen(prefix) + strlen(format) + 8;
    char *name = malloc(length);
    assert(name);
    assert(snprintf(name, length, format, prefix) > 0);
    void *address = process_lookup(library, name);
    free(name);
    return address;
}

static mica_function_type_TaskEntry process_entry(struct process_application *application) {
    mica_function_type_TaskEntry entry = NULL;
    void *address = process_named(application->library, application->prefix, "mica_%s_entry_0");
    _Static_assert(sizeof(entry) == sizeof(address), "loader function pointer ABI");
    memcpy(&entry, &address, sizeof(entry));
    return entry;
}

struct process_host {
    struct mica_TaskRuntime runtime;
    atomic_uint effects;
};

static uint64_t process_emit(struct mica_MemoryWorker *worker, uint8_t *context,
    struct mica_Kernel *kernel, struct mica_KernelSnapshot *snapshot,
    mica_type_Value tenant, struct mica_TaskEffect *effect, struct mica_TaskControl *parent) {
    (void)worker;
    (void)kernel;
    (void)snapshot;
    (void)tenant;
    (void)parent;
    struct process_host *host = (void *)context;
    assert(effect->f_target == mica_value_identity(0).f_value);
    assert(effect->f_payload == process_integer(42));
    atomic_fetch_add(&host->effects, 1);
    return 0;
}

struct process_worker {
    struct mica_MemoryHeap *heap;
    struct mica_TaskScheduler *scheduler;
    struct mica_TaskServices *services;
    bool events;
    uint64_t status;
};

static void *process_work(void *opaque) {
    struct process_worker *state = opaque;
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

static int64_t process_cursor(struct mica_MemoryWorker *worker,
    struct mica_KernelTransaction *transaction, struct mica_SymbolTable *symbols) {
    mica_type_Value cells[] = {process_symbol(symbols, "native/source"), process_integer(0)}, rows[2];
    struct mica_ValueResult pattern = mica_value_list(worker, cells, 2);
    assert(pattern.f_ok);
    struct mica_KernelScanResult scanned = mica_kernel_scan(transaction, 710, pattern.f_value, 1, 0, rows, 2);
    assert(scanned.f_status == 0 && scanned.f_count <= 1);
    if (!scanned.f_count) return -1;
    return process_number(mica_value_list_get(rows[0], 1).f_value);
}

static uint64_t process_named_relation(struct mica_MemoryWorker *worker,
    struct mica_KernelTransaction *transaction, struct mica_SymbolTable *symbols,
    const char *name, mica_type_Value *rows, uint64_t capacity) {
    mica_type_Value cells[] = {process_integer(0), process_symbol(symbols, name)};
    struct mica_ValueResult pattern = mica_value_list(worker, cells, 2);
    assert(pattern.f_ok);
    struct mica_KernelScanResult scanned = mica_kernel_scan(transaction, 707, pattern.f_value, 2, 0, rows, capacity);
    assert(scanned.f_status == 0);
    return scanned.f_count;
}

static int64_t process_relation_id(struct mica_SymbolTable *symbols,
    mica_type_Value plan, const char *name) {
    return process_number(mica_value_map_get(process_field(symbols, plan, "relations"),
        process_symbol(symbols, name)).f_value);
}

static void process_fact(struct mica_MemoryWorker *worker, struct mica_KernelTransaction *transaction,
    uint64_t relation, int64_t key, int64_t value, bool present) {
    mica_type_Value cells[] = {process_integer(key), process_integer(value)}, rows[1];
    struct mica_ValueResult pattern = mica_value_list(worker, cells, 2);
    assert(pattern.f_ok);
    struct mica_KernelScanResult scanned = mica_kernel_scan(transaction, relation, pattern.f_value, 3, 0, rows, 1);
    assert(scanned.f_status == 0 && scanned.f_count == (present ? 1u : 0u));
}

static void process_publish(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_SymbolTable *symbols, struct mica_TaskDispatchContext *dispatch,
    mica_type_Value plan, struct process_application *application) {
    struct mica_KernelTransaction transaction = {0};
    assert(mica_kernel_begin(kernel, worker, &transaction, process_integer(7)) == 0);
    if (process_cursor(worker, &transaction, symbols) !=
        process_number(process_field(symbols, plan, "guard"))) {
        assert(mica_kernel_end(&transaction));
        abort();
    }
    mica_type_Value declarations = process_field(symbols, plan, "declarations");
    uint64_t count = mica_value_list_length(declarations).f_number;
    for (uint64_t index = 0; index < count; ++index) {
        mica_type_Value declaration = mica_value_list_get(declarations, index).f_value;
        uint64_t id = (uint64_t)process_number(mica_value_list_get(declaration, 0).f_value);
        uint64_t name = mica_value_as_symbol(mica_value_list_get(declaration, 1).f_value).f_number;
        uint64_t arity = (uint64_t)process_number(mica_value_list_get(declaration, 2).f_value);
        uint64_t policy = (uint64_t)process_number(mica_value_list_get(declaration, 3).f_value);
        uint64_t keys = (uint64_t)process_number(mica_value_list_get(declaration, 4).f_value);
        uint64_t indexes = (uint64_t)process_number(mica_value_list_get(declaration, 5).f_value);
        assert(mica_kernel_declare(&transaction, id, name, arity, policy, keys, indexes) == 0);
    }
    mica_type_Value writes = process_field(symbols, plan, "writes");
    count = mica_value_list_length(writes).f_number;
    for (uint64_t index = 0; index < count; ++index) {
        mica_type_Value write = mica_value_list_get(writes, index).f_value;
        uint64_t relation = (uint64_t)process_number(mica_value_list_get(write, 0).f_value);
        bool present = mica_value_as_bool(mica_value_list_get(write, 1).f_value).f_value;
        mica_type_Value row = mica_value_list_get(write, 2).f_value;
        assert(mica_kernel_write(&transaction, relation, row, present) == 0);
    }
    mica_type_Value removals = process_field(symbols, plan, "remove_programs");
    count = mica_value_list_length(removals).f_number;
    for (uint64_t index = 0; index < count; ++index) {
        uint64_t artifact = (uint64_t)process_number(mica_value_list_get(removals, index).f_value);
        assert(mica_kernel_program_remove(&transaction, artifact) == 0);
    }
    uint64_t (*install)(struct mica_KernelTransaction *, struct mica_TaskDispatchContext *,
        struct mica_TaskCodeRef *, mica_type_Value) = NULL;
    void *address = process_named(application->library, application->prefix, "mica_task_%s_install");
    _Static_assert(sizeof(install) == sizeof(address), "loader function pointer ABI");
    memcpy(&install, &address, sizeof(install));
    assert(install(&transaction, dispatch, (void *)application->code.f_pointer, process_integer(7)) == 0);
    mica_type_Value cells[] = {process_symbol(symbols, "native/source"),
        process_field(symbols, plan, "next")};
    struct mica_ValueResult cursor = mica_value_list(worker, cells, 2);
    assert(cursor.f_ok);
    assert(mica_kernel_replace(&transaction, 710, cursor.f_value) == 0);
    assert(mica_kernel_commit(&transaction) == 0 && mica_kernel_end(&transaction));
}

static void process_submit(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_SymbolTable *symbols, struct mica_TaskCodeRegistry *registry,
    struct mica_MemoryRoot *compiler_code, struct process_host *host,
    struct mica_TaskDispatchContext *dispatch, struct mica_MemoryRoot *input,
    struct mica_MemoryRoot *output, const char *unit, const char *source,
    struct process_application *application) {
    mica_type_Value arguments[] = {process_interface(worker, symbols),
        process_symbol(symbols, unit), process_read(worker, source)};
    struct mica_ValueResult list = mica_value_list(worker, arguments, 3);
    assert(list.f_ok);
    mica_value_root_set(input, list.f_value);
    assert(process_run(worker, kernel, compiler_code, &host->runtime,
        mica_source_roundtrip_method_0_0, mica_value_root_get(input), output,
        UINT64_C(2000000000), true) == 0);
    mica_type_Value plan = mica_value_root_get(output);
    application->prefix = process_string(process_field(symbols, plan, "prefix"));
    size_t path_length = strlen(application->prefix) + 8;
    application->path = malloc(path_length);
    assert(application->path);
    assert(snprintf(application->path, path_length, "./%s.so", application->prefix) > 0);
    process_artifacts(symbols, process_field(symbols, plan, "artifacts"), application->path);
    application->library = dlopen(application->path, RTLD_NOW | RTLD_LOCAL);
    if (!application->library) {
        fprintf(stderr, "%s: %s\n", application->path, dlerror());
        abort();
    }
    assert(mica_memory_root_push(worker, &application->code));
    assert(mica_task_code_register(registry, worker, application->library, &application->code) == 0);
    uint64_t (*constants)(struct mica_MemoryWorker *, struct mica_SymbolTable *,
        struct mica_TaskCodeRegistry *, struct mica_TaskCodeRef *) = NULL;
    void *address = process_named(application->library, application->prefix, "mica_task_%s_constants");
    _Static_assert(sizeof(constants) == sizeof(address), "loader function pointer ABI");
    memcpy(&constants, &address, sizeof(constants));
    assert(constants(worker, symbols, registry, (void *)application->code.f_pointer) == 0);
    mica_type_Value methods = process_field(symbols, plan, "method_ids");
    if (mica_value_list_length(methods).f_number) {
        mica_type_Value method = mica_value_list_get(methods, 0).f_value;
        application->method_id = process_number(mica_value_list_get(method, 0).f_value);
        application->program_id = process_number(mica_value_list_get(method, 1).f_value);
    }
    process_publish(worker, kernel, symbols, dispatch, plan, application);
}

static void process_rejected(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_SymbolTable *symbols, struct mica_MemoryRoot *compiler_code,
    struct process_host *host, struct mica_MemoryRoot *input, struct mica_MemoryRoot *output,
    const char *unit, const char *source) {
    mica_type_Value arguments[] = {process_interface(worker, symbols),
        process_symbol(symbols, unit), process_read(worker, source)};
    struct mica_ValueResult list = mica_value_list(worker, arguments, 3);
    assert(list.f_ok);
    mica_value_root_set(input, list.f_value);
    assert(process_run(worker, kernel, compiler_code, &host->runtime,
        mica_source_roundtrip_method_0_0, mica_value_root_get(input), output,
        UINT64_C(2000000000), false) != 0);
}

static bool process_loaded(const char *path) {
    void *library = dlopen(path, RTLD_NOW | RTLD_NOLOAD | RTLD_LOCAL);
    if (!library) return false;
    assert(dlclose(library) == 0);
    return true;
}

int main(int argc, char **argv) {
    assert(argc == 5);
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
    struct mica_MemoryRoot code = {0}, prototypes = {0}, input = {0}, output = {0}, image = {0};
    struct process_application applications[3] = {{0}};
    assert(mica_task_resources_start() && mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096) && mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == 0);
    assert(mica_kernel_init(&kernel, &worker, process_integer(7), 1));
    assert(mica_task_code_init(&registry, &worker) == 0);
    assert(mica_task_mailbox_hub_init(&hub, &worker, 16) == 0);
    assert(mica_task_scheduler_init(&scheduler, &worker, &hub, 16) == 0);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &prototypes));
    assert(mica_memory_root_push(&worker, &input));
    assert(mica_memory_root_push(&worker, &output));
    assert(mica_memory_root_push(&worker, &image));
    void *compiler_library = dlopen(NULL, RTLD_NOW);
    assert(compiler_library);
    assert(mica_task_code_register(&registry, &worker, compiler_library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry,
        (void *)code.f_pointer) == 0);
    mica_type_Value kinds[16];
    for (unsigned index = 0; index < 16; ++index) {
        struct mica_ValueResult kind = mica_value_identity(9001 + index);
        assert(kind.f_ok);
        kinds[index] = kind.f_value;
    }
    struct mica_ValueResult list = mica_value_list(&worker, kinds, 16);
    assert(list.f_ok);
    mica_value_root_set(&prototypes, list.f_value);
    struct process_host host = {.runtime = {.f_symbols = &symbols, .f_mailboxes = &hub,
        .f_spawn = &spawn, .f_dispatch = &dispatch, .f_mailbox_capacity = 64}};
    assert(mica_task_dispatch_context_init(&dispatch, &worker, &vocabulary,
        702, 703, 701, 704, mica_value_root_get(&prototypes)) == 0);
    assert(mica_task_spawn_context_init(&spawn, &worker, &scheduler,
        1000000, 3, 256, &host.runtime) == 0);
    struct mica_TaskServices services = {.f_context = (void *)&host,
        .f_emit = process_emit, .f_send = mica_task_runtime_send,
        .f_spawn = mica_task_runtime_spawn};
    struct mica_KernelTransaction install = {0};
    assert(mica_kernel_begin(&kernel, &worker, &install, process_integer(7)) == 0);
    for (uint64_t id = 701; id <= 704; ++id) {
        uint64_t arity = id == 701 ? 3 : id == 703 ? 6 : 2;
        uint64_t indexes = id == 702 ? 2 : 0;
        assert(mica_kernel_declare(&install, id, id, arity, 0, 0, indexes) == 0);
    }
    const char *catalogues[] = {"RelationName", "Arity", "FunctionalKey"};
    for (unsigned index = 0; index < 3; ++index) {
        struct mica_IdResult name = mica_value_symbol_intern(&symbols,
            (const uint8_t *)catalogues[index], strlen(catalogues[index]), NULL);
        assert(name.f_ok);
        assert(mica_kernel_declare_catalogue(&install, 707 + index, name.f_number, index + 1) == 0);
    }
    assert(mica_kernel_declare(&install, 710,
        mica_value_as_symbol(process_symbol(&symbols, "compiler/Generation")).f_number,
        2, 1, 1, 0) == 0);
    assert(mica_kernel_declare(&install, 711,
        mica_value_as_symbol(process_symbol(&symbols, "compiler/Method")).f_number,
        2, 0, 0, 0) == 0);
    assert(mica_task_source_roundtrip_install(&install, &dispatch,
        (void *)code.f_pointer, process_integer(7)) == 0);
    assert(mica_kernel_commit(&install) == 0 && mica_kernel_end(&install));
    process_submit(&worker, &kernel, &symbols, &registry, &code, &host, &dispatch,
        &input, &output, "acceptance", argv[1], &applications[0]);
    uint64_t acceptance = (uint64_t)process_relation_id(&symbols, mica_value_root_get(&output), "Acceptance");
    mica_value_root_set(&output, process_integer(0));
    mica_value_root_set(&input, process_integer(0));
    mica_function_type_TaskEntry entry = process_entry(&applications[0]);
    list = mica_value_list(&worker, NULL, 0);
    assert(list.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, entry,
        (void *)applications[0].code.f_pointer, process_integer(7), list.f_value,
        0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl parent = {0};
    assert(mica_task_init(&parent, &worker, &kernel, &image,
        1000000, 3, 256, &host.runtime) == 0);
    image.f_pointer = NULL;
    applications[0].code.f_pointer = NULL;
    assert(mica_task_execute(&parent, &worker) == 0);
    assert(mica_task_dispatch(&parent, &worker, &services) == 0);
    struct mica_TaskObservation waiting = mica_task_observe(&parent, &worker);
    for (unsigned segment = 0; waiting.f_status == 0 && waiting.f_state == 1; ++segment) {
        assert(segment < 4);
        assert(mica_task_execute(&parent, &worker) == 0);
        assert(mica_task_dispatch(&parent, &worker, &services) == 0);
        waiting = mica_task_observe(&parent, &worker);
    }
    assert(waiting.f_status == 0 && waiting.f_state == 4);
    assert(mica_kernel_begin(&kernel, &worker, &install, process_integer(7)) == 0);
    process_fact(&worker, &install, acceptance, 7, 42, true);
    process_fact(&worker, &install, acceptance, 8, 84, false);
    process_fact(&worker, &install, acceptance, 9, 99, false);
    assert(mica_kernel_end(&install));
    assert(atomic_load(&host.effects) == 0);
    process_submit(&worker, &kernel, &symbols, &registry, &code, &host, &dispatch,
        &input, &output, "probe", argv[2], &applications[1]);
    list = mica_value_list(&worker, NULL, 0);
    assert(list.f_ok);
    assert(process_run(&worker, &kernel, &applications[1].code, &host.runtime,
        process_entry(&applications[1]), list.f_value, &output, UINT64_C(1000000), true) == 0);
    assert(mica_value_root_get(&output) == process_integer(42));
    int64_t cursor = 0;
    assert(mica_kernel_begin(&kernel, &worker, &install, process_integer(7)) == 0);
    cursor = process_cursor(&worker, &install, &symbols);
    assert(mica_kernel_end(&install));
    process_rejected(&worker, &kernel, &symbols, &code, &host, &input, &output,
        "rejected", argv[4]);
    mica_type_Value relation_rows[1];
    assert(mica_kernel_begin(&kernel, &worker, &install, process_integer(7)) == 0);
    assert(process_cursor(&worker, &install, &symbols) == cursor);
    assert(process_named_relation(&worker, &install, &symbols, "NativeRejected", relation_rows, 1) == 0);
    assert(process_named_relation(&worker, &install, &symbols, "Acceptance", relation_rows, 1) == 1);
    assert(mica_value_as_identity(mica_value_list_get(relation_rows[0], 0).f_value).f_number == acceptance);
    assert(mica_kernel_end(&install));
    process_submit(&worker, &kernel, &symbols, &registry, &code, &host, &dispatch,
        &input, &output, "acceptance", argv[3], &applications[2]);
    assert(applications[2].method_id == applications[0].method_id);
    assert(applications[2].program_id != applications[0].program_id);
    list = mica_value_list(&worker, NULL, 0);
    assert(list.f_ok);
    assert(process_run(&worker, &kernel, &applications[1].code, &host.runtime,
        process_entry(&applications[1]), list.f_value, &output, UINT64_C(1000000), true) == 0);
    assert(mica_value_root_get(&output) == process_integer(43));
    assert(mica_kernel_begin(&kernel, &worker, &install, process_integer(7)) == 0);
    mica_type_Value eviction_cells[] = {process_integer(10), process_integer(100)};
    struct mica_ValueResult eviction = mica_value_list(&worker, eviction_cells, 2);
    assert(eviction.f_ok);
    assert(mica_kernel_write(&install, acceptance, eviction.f_value, true) == 0);
    assert(mica_kernel_commit(&install) == 0 && mica_kernel_end(&install));
    struct mica_TaskCodeReap reaped = mica_task_code_reap(&registry, &worker);
    assert(reaped.f_status == 0 && reaped.f_released == 0 && process_loaded(applications[0].path));
    assert(mica_task_wake(&parent, &worker, parent.f_generation, process_integer(0)) == 0);
    assert(mica_task_scheduler_submit(&scheduler, &worker, &parent, false, 0).f_status == 0);
    assert(mica_memory_leave(&worker));
    pthread_t threads[3];
    struct process_worker workers[3];
    for (unsigned index = 0; index < 3; ++index) {
        workers[index] = (struct process_worker){&heap, &scheduler, &services,
            index == 2, UINT64_MAX};
        assert(!pthread_create(&threads[index], NULL, process_work, &workers[index]));
    }
    assert(mica_memory_enter(&worker));
    struct timespec start, now;
    assert(!clock_gettime(CLOCK_MONOTONIC, &start));
    while (mica_task_scheduler_inspect(&scheduler, &worker).f_remaining) {
        assert(mica_memory_safepoint(&worker, true));
        assert(!clock_gettime(CLOCK_MONOTONIC, &now));
        assert(now.tv_sec - start.tv_sec < 30);
        assert(mica_memory_leave(&worker));
        struct timespec pause = {.tv_nsec = 1000000};
        assert(!nanosleep(&pause, NULL));
        assert(mica_memory_enter(&worker));
    }
    assert(mica_memory_leave(&worker));
    mica_task_scheduler_stop(&scheduler);
    for (unsigned index = 0; index < 3; ++index) {
        assert(!pthread_join(threads[index], NULL));
        assert(workers[index].status == 0);
    }
    assert(mica_memory_enter(&worker));
    assert(atomic_load(&host.effects) == 1);
    struct mica_TaskObservation observed = mica_task_observe(&parent, &worker);
    assert(observed.f_status == 0 && observed.f_state == 5);
    struct mica_MemoryRoot completed = {0};
    assert(mica_memory_acquire(&worker, &parent.f_image, &completed));
    mica_value_root_set(&output, ((struct mica_TaskImage *)completed.f_pointer)->f_input);
    assert(mica_memory_root_pop(&worker, &completed));
    mica_type_Value result = mica_value_root_get(&output);
    assert(mica_value_list_length(result).f_number == 2);
    assert(mica_value_list_get(result, 0).f_value == process_integer(42));
    assert(mica_value_as_int(mica_value_list_get(result, 1).f_value).f_number > 0);
    assert(mica_kernel_begin(&kernel, &worker, &install, process_integer(7)) == 0);
    process_fact(&worker, &install, acceptance, 7, 42, true);
    process_fact(&worker, &install, acceptance, 8, 84, true);
    process_fact(&worker, &install, acceptance, 9, 99, false);
    process_fact(&worker, &install, acceptance, 10, 100, true);
    assert(mica_kernel_end(&install));
    assert(mica_task_scheduler_release(&scheduler, &worker) == 0);
    assert(mica_task_release(&parent, &worker));
    reaped = mica_task_code_reap(&registry, &worker);
    assert(reaped.f_status == 0 && reaped.f_released == 1 && !process_loaded(applications[0].path));
    assert(mica_task_spawn_context_release(&spawn, &worker) == 0);
    assert(mica_task_dispatch_context_release(&dispatch, &worker) == 0);
    assert(mica_task_mailbox_hub_shutdown(&hub, &worker) == 0);
    assert(mica_task_mailbox_hub_release(&hub, &worker));
    assert(mica_kernel_release(&kernel, &worker));
    for (unsigned index = 3; index-- > 0;) {
        assert(mica_memory_root_pop(&worker, &applications[index].code));
        free(applications[index].path);
        free(applications[index].prefix);
    }
    assert(mica_memory_root_pop(&worker, &image));
    assert(mica_memory_root_pop(&worker, &output));
    assert(mica_memory_root_pop(&worker, &input));
    assert(mica_memory_root_pop(&worker, &prototypes));
    assert(mica_memory_root_pop(&worker, &code));
    assert(mica_task_code_close(&registry, &worker) == 0);
    reaped = mica_task_code_reap(&registry, &worker);
    assert(reaped.f_status == 0 && reaped.f_released == 3);
    assert(mica_task_code_release(&registry, &worker) == 0);
    assert(mica_memory_safepoint(&worker, true));
    assert(!heap.f_retained && !worker.f_roots);
    assert(mica_memory_leave(&worker) && mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
    mica_value_symbol_table_release(&symbols);
    assert(mica_task_resources_stop());
    return 0;
}
