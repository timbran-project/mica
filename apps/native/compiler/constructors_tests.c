// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
    struct mica_MemoryRoot *code, struct mica_SymbolTable *symbols) {
    struct mica_MemoryRoot message = {0}, arguments = {0}, image = {0};
    assert(mica_memory_root_push(worker, &message));
    assert(mica_memory_root_push(worker, &arguments));
    assert(mica_memory_root_push(worker, &image));
    struct mica_IdResult other = mica_value_symbol_intern(symbols, (const uint8_t *)"other", 5);
    assert(other.f_ok);
    uint32_t heading = (uint32_t)other.f_number;
    struct mica_TaskRuntime runtime = {.f_symbols = symbols};
    // Both construction and raise reject empty relations other than none.
    // Dynamic error calls also reject zero or four arguments.
    for (unsigned kind = 0; kind < 4; ++kind) {
        struct mica_ValueResult value;
        if (kind < 2) {
            value = mica_value_relation(worker, kind ? &heading : NULL, kind, NULL, 0);
        } else {
            mica_type_Value cells[4] = {source_integer(1), source_integer(2), source_integer(3), source_integer(4)};
            value = mica_value_list(worker, cells, kind == 2 ? 0 : 4);
        }
        assert(value.f_ok);
        mica_value_root_set(&message, value.f_value);
        for (unsigned operation = 1; operation <= 2; ++operation) {
            mica_type_Value cells[] = {0, 0, source_integer(kind < 2 ? operation : 3), mica_value_root_get(&message)};
            value = mica_value_list(worker, cells, 4);
            assert(value.f_ok);
            mica_value_root_set(&arguments, value.f_value);
            image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_2_0,
                (void *)code->f_pointer, source_integer(7), mica_value_root_get(&arguments), 0, NULL, NULL, 0, NULL, NULL);
            assert(image.f_pointer);
            struct mica_TaskControl task = {0};
            assert(mica_task_init(&task, worker, kernel, &image, 100000, 3, 16, &runtime) == 0);
            assert(mica_memory_safepoint(worker, true));
            assert(mica_task_execute(&task, worker) == 7);
            assert(mica_task_release(&task, worker));
        }
    }
    assert(mica_memory_root_pop(worker, &image));
    assert(mica_memory_root_pop(worker, &arguments));
    assert(mica_memory_root_pop(worker, &message));
}
