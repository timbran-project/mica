// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

// Exercise positional domains independently of source parameter lowering.
// Different role names must not affect positional overload ordering.
static void source_dispatch_arguments(struct mica_MemoryWorker *worker,
        struct mica_TaskDispatchVocabulary *vocabulary,
        struct mica_TaskDispatchContext *dispatch, struct mica_Kernel *kernel) {
    struct mica_MemoryRoot parameters = {0}, supplied = {0}, output = {0};
    struct mica_MemoryRoot fixed = {0}, varied = {0}, view = {0}, cache = {0};
    struct mica_MemoryRoot *roots[] = {&parameters, &supplied, &output, &fixed, &varied, &view, &cache};
    for (unsigned i = 0; i < 7; ++i) assert(mica_memory_root_push(worker, roots[i]));
    mica_type_Value any = mica_value_symbol(vocabulary->f_unrestricted).f_value;
    mica_type_Value method = mica_value_identity(9999).f_value;
    for (unsigned rest = 0; rest < 2; ++rest) {
        mica_type_Value rows[3];
        for (unsigned i = 0; i < 3; ++i) {
            mica_type_Value cells[] = {method, mica_value_symbol(100 + i).f_value, any,
                mica_value_int(i).f_value, mica_value_symbol(i == 0 ? vocabulary->f_required :
                i == 1 ? vocabulary->f_optional : vocabulary->f_rest).f_value, mica_value_int(77).f_value};
            struct mica_ValueResult row = mica_value_list(worker, cells, 6);
            assert(row.f_ok);
            rows[i] = row.f_value;
        }
        struct mica_ValueResult list = mica_value_list(worker, rows, rest ? 3 : 2);
        assert(list.f_ok);
        mica_value_root_set(&parameters, list.f_value);
        for (unsigned count = 0; count <= 4; ++count) {
            mica_type_Value values[] = {mica_value_int(10).f_value, mica_value_int(20).f_value,
                mica_value_int(30).f_value, mica_value_int(40).f_value};
            list = mica_value_list(worker, values, count);
            assert(list.f_ok);
            mica_value_root_set(&supplied, list.f_value);
            struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
                .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
            struct mica_TaskDispatchSignatureResult signature = mica_task_dispatch_signature(worker,
                vocabulary, method, mica_value_root_get(&parameters), &budget);
            assert(signature.f_status == 0);
            varied.f_pointer = (void *)signature.f_signature;
            assert(mica_memory_safepoint(worker, true));
            struct mica_TaskDispatchBinding bound = mica_task_dispatch_bind(worker, vocabulary,
                (void *)varied.f_pointer, mica_value_root_get(&supplied), &budget);
            assert(bound.f_status == 0 && bound.f_applicable == (count >= 1 && (rest || count <= 2)));
            if (!bound.f_applicable) continue;
            mica_value_root_set(&output, bound.f_arguments);
            assert(mica_memory_safepoint(worker, true));
            mica_type_Value args = mica_value_root_get(&output);
            assert(mica_value_list_length(args).f_number == (rest ? 3 : 2));
            assert(mica_value_list_get(args, 0).f_value == values[0]);
            assert(mica_value_list_get(args, 1).f_value == mica_value_int(count > 1 ? 20 : 77).f_value);
            if (rest) {
                mica_type_Value tail = mica_value_list_get(args, 2).f_value;
                assert(mica_value_list_length(tail).f_number == (count > 2 ? count - 2 : 0));
                for (unsigned i = 2; i < count; ++i)
                    assert(mica_value_list_get(tail, i - 2).f_value == values[i]);
            }
        }
    }
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, mica_value_int(7).f_value) == 0);
    struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX,
        .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
    struct mica_KernelRelationResult relation = mica_kernel_read(&tx, 701, &budget);
    assert(relation.f_status == 0);
    view.f_pointer = (void *)relation.f_relation;
    for (unsigned mode = 0; mode < 3; ++mode) {
        for (unsigned side = 0; side < 2; ++side) {
            mica_type_Value cells[] = {method, mica_value_symbol(100 + side).f_value, any,
                mica_value_int(0).f_value, mica_value_symbol(side == 0 || mode == 0 ? vocabulary->f_required :
                mode == 1 ? vocabulary->f_optional : vocabulary->f_rest).f_value, mica_value_int(0).f_value};
            struct mica_ValueResult row = mica_value_list(worker, cells, 6);
            assert(row.f_ok);
            struct mica_ValueResult rows = mica_value_list(worker, &row.f_value, 1);
            assert(rows.f_ok);
            mica_value_root_set(&parameters, rows.f_value);
            struct mica_TaskDispatchSignatureResult signature = mica_task_dispatch_signature(worker,
                vocabulary, method, mica_value_root_get(&parameters), &budget);
            assert(signature.f_status == 0);
            (side ? &varied : &fixed)->f_pointer = (void *)signature.f_signature;
        }
        assert(mica_memory_safepoint(worker, true));
        for (unsigned reverse = 0; reverse < 2; ++reverse) {
            struct mica_TaskDispatchMatch result = mica_task_dispatch_positional_more_specific(worker,
                vocabulary, (void *)view.f_pointer, mica_value_root_get(&dispatch->f_prototypes), &cache,
                (void *)(reverse ? varied.f_pointer : fixed.f_pointer),
                (void *)(reverse ? fixed.f_pointer : varied.f_pointer), &budget);
            assert(result.f_status == 0 && result.f_matched == (mode != 0 && !reverse));
            assert(mica_memory_safepoint(worker, true));
        }
    }
    assert(mica_kernel_end(&tx));
    for (unsigned i = 7; i-- > 0;) assert(mica_memory_root_pop(worker, roots[i]));
}

static void source_call_failures(struct mica_MemoryWorker *worker,
        struct mica_SymbolTable *symbols, struct mica_Kernel *kernel,
        struct mica_TaskRuntime *runtime, struct mica_MemoryRoot *code) {
    struct mica_MemoryRoot image = {0};
    assert(mica_memory_root_push(worker, &image));
    struct mica_IdResult name = mica_value_symbol_intern(symbols, (const uint8_t *)"native_expression_2", 19);
    assert(name.f_ok);
    mica_type_Value selector = mica_value_symbol(name.f_number).f_value;
    mica_type_Value original = mica_value_identity(1002).f_value;
    for (unsigned mode = 0; mode < 3; ++mode) {
        struct mica_KernelTransaction tx = {0};
        assert(mica_kernel_begin(kernel, worker, &tx, mica_value_int(7).f_value) == 0);
        mica_type_Value fields[] = {original, selector};
        struct mica_ValueResult row = mica_value_list(worker, fields, 2);
        assert(row.f_ok);
        if (mode == 1) assert(mica_kernel_write(&tx, 702, row.f_value, false) == 0);
        if (mode == 2) {
            // Both arithmetic methods have the same required named signature.
            assert(mica_kernel_write(&tx, 702, row.f_value, true) == 0);
            fields[0] = mica_value_identity(1003).f_value;
            row = mica_value_list(worker, fields, 2);
            assert(row.f_ok && mica_kernel_write(&tx, 702, row.f_value, true) == 0);
        }
        assert(mica_kernel_commit(&tx) == 0 && mica_kernel_end(&tx));
        mica_type_Value values[] = {mica_value_int(1).f_value, mica_value_int(2).f_value};
        struct mica_ValueResult arguments = mica_value_list(worker, values, 2);
        assert(arguments.f_ok);
        image.f_pointer = (void *)mica_task_new_Image(worker, source_named_entry,
            (void *)code->f_pointer, mica_value_int(mode == 0 ? 9 : 7).f_value,
            arguments.f_value, 0, NULL, NULL, 0, NULL, NULL);
        assert(image.f_pointer);
        struct mica_TaskControl task = {0};
        assert(mica_task_init(&task, worker, kernel, &image, 100000, 0, 16, runtime) == 0);
        uint64_t expected = mode == 0 ? 12 : mode == 1 ? 15 : 16;
        assert(mica_task_execute(&task, worker) == expected);
        assert(mica_task_release(&task, worker));
    }
    assert(mica_memory_root_pop(worker, &image));
}

// source_cases and their structural result checks come from Rust execution of
// the same source. Every case runs as a native task, including commit or abort.
static void source_conformance(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    struct mica_SymbolTable symbols = {0};
    struct mica_Kernel kernel = {0};
    struct mica_TaskCodeRegistry registry = {0};
    struct mica_TaskDispatchVocabulary vocabulary = {0};
    struct mica_TaskDispatchContext dispatch = {0};
    struct mica_TaskRuntime runtime = {.f_symbols = &symbols, .f_dispatch = &dispatch};
    struct mica_MemoryRoot code = {0}, image = {0}, result = {0};
    assert(mica_task_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 4096));
    assert(mica_memory_enter(&worker));
    assert(mica_value_symbol_table_init(&symbols));
    mica_type_Value tenant = mica_value_int(7).f_value;
    assert(mica_kernel_init(&kernel, &worker, tenant, 1));
    assert(mica_task_code_init(&registry, &worker) == 0);
    assert(mica_memory_root_push(&worker, &code));
    assert(mica_memory_root_push(&worker, &image));
    assert(mica_memory_root_push(&worker, &result));
    void *library = dlopen(NULL, RTLD_NOW);
    assert(library);
    assert(mica_task_code_register(&registry, &worker, library, &code) == 0);
    assert(mica_task_source_roundtrip_constants(&worker, &symbols, &registry, (void *)code.f_pointer) == 0);
    assert(mica_task_dispatch_vocabulary_init(&symbols, &vocabulary) == 0);
    mica_type_Value kinds[16];
    for (unsigned i = 0; i < 16; ++i) kinds[i] = mica_value_identity(9001 + i).f_value;
    struct mica_ValueResult prototypes = mica_value_list(&worker, kinds, 16);
    assert(prototypes.f_ok);
    assert(mica_task_dispatch_context_init(&dispatch, &worker, &vocabulary,
        702, 703, 701, 704, prototypes.f_value) == 0);
    struct mica_KernelTransaction setup = {0};
    assert(mica_kernel_begin(&kernel, &worker, &setup, tenant) == 0);
    for (uint64_t id = 701; id <= 705; ++id) {
        uint64_t arity = id == 701 ? 3 : id == 703 ? 6 : 2;
        assert(mica_kernel_declare(&setup, id, id, arity, 0, 0, 0) == 0);
    }
    assert(mica_task_source_roundtrip_install(&setup, &dispatch, (void *)code.f_pointer, tenant) == 0);
    assert(mica_kernel_commit(&setup) == 0 && mica_kernel_end(&setup));
    for (size_t i = 0; i < sizeof(source_cases) / sizeof(source_cases[0]); ++i) {
        mica_type_Value arguments = source_cases[i].arguments(&worker, &symbols);
        image.f_pointer = (void *)mica_task_new_Image(&worker, source_cases[i].entry,
            (void *)code.f_pointer, tenant, arguments, 0, NULL, NULL, 0, NULL, NULL);
        assert(image.f_pointer);
        struct mica_TaskControl task = {0};
        assert(mica_task_init(&task, &worker, &kernel, &image, 100000, 3, 16, &runtime) == 0);
        assert(mica_memory_safepoint(&worker, true));
        uint64_t status = mica_task_execute(&task, &worker);
        if (status != source_cases[i].status) fprintf(stderr, "source case %zu: status %llu, expected %llu\n", i, (unsigned long long)status, (unsigned long long)source_cases[i].status);
        assert(status == source_cases[i].status);
        struct mica_MemoryRoot acquired = {0};
        if (status) {
            assert(mica_memory_acquire(&worker, &task.f_error, &acquired));
            mica_value_root_set(&result, mica_value_root_get(&acquired));
        } else {
            struct mica_TaskServices services = {0};
            assert(mica_task_dispatch(&task, &worker, &services) == 0);
            assert(mica_task_observe(&task, &worker).f_state == 5);
            assert(mica_memory_acquire(&worker, &task.f_image, &acquired));
            mica_value_root_set(&result, ((struct mica_TaskImage *)acquired.f_pointer)->f_input);
        }
        assert(mica_memory_root_pop(&worker, &acquired));
        assert(mica_task_release(&task, &worker));
        assert(mica_memory_safepoint(&worker, true));
        source_cases[i].check(mica_value_root_get(&result), &symbols, &worker);
    }
    // An infinite source loop must still honour the task's instruction budget.
    struct mica_ValueResult empty = mica_value_list(&worker, NULL, 0);
    assert(empty.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, source_spin_entry,
        (void *)code.f_pointer, tenant, empty.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl limited = {0};
    assert(mica_task_init(&limited, &worker, &kernel, &image, 100, 0, 16, &runtime) == 0);
    assert(mica_task_execute(&limited, &worker) == 10);
    assert(mica_task_release(&limited, &worker));
    // A recursive source call consumes the logical depth limit, regardless of
    // native tail transfers between generated entry functions.
    mica_type_Value argument = mica_value_int(100).f_value;
    struct mica_ValueResult args = mica_value_list(&worker, &argument, 1);
    assert(args.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, source_recursive_entry,
        (void *)code.f_pointer, tenant, args.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    limited = (struct mica_TaskControl){0};
    assert(mica_task_init(&limited, &worker, &kernel, &image, 100000, 0, 4, &runtime) == 0);
    assert(mica_task_execute(&limited, &worker) == 10);
    assert(mica_task_release(&limited, &worker));
    // Copying a splice is bounded even when it occupies a single instruction.
    mica_type_Value many[512];
    for (unsigned i = 0; i < 512; ++i) many[i] = mica_value_int(i).f_value;
    struct mica_ValueResult sequence = mica_value_list(&worker, many, 512);
    assert(sequence.f_ok);
    mica_type_Value supplied[] = {sequence.f_value, sequence.f_value};
    args = mica_value_list(&worker, supplied, 2);
    assert(args.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(&worker, source_splice_entry,
        (void *)code.f_pointer, tenant, args.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    limited = (struct mica_TaskControl){0};
    assert(mica_task_init(&limited, &worker, &kernel, &image, 100, 0, 4, &runtime) == 0);
    assert(mica_task_execute(&limited, &worker) == 10);
    assert(mica_task_release(&limited, &worker));
    source_dispatch_arguments(&worker, &vocabulary, &dispatch, &kernel);
    source_call_failures(&worker, &symbols, &kernel, &runtime, &code);
    assert(mica_task_dispatch_context_release(&dispatch, &worker) == 0);
    assert(mica_kernel_release(&kernel, &worker));
    assert(mica_memory_root_pop(&worker, &result));
    assert(mica_memory_root_pop(&worker, &image));
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
