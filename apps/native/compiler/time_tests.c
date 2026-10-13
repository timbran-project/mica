// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdio.h>

static mica_type_Value source_integer(int64_t number);
static mica_type_Value source_cell(mica_type_Value list, uint64_t index);

static void source_duration_checks(void) {
    const int64_t integers[] = {-1, 0, 1, 18446744073, 18446744074, INT64_C(36028797018963967)};
    for (unsigned i = 0; i < sizeof(integers) / sizeof(integers[0]); ++i) {
        struct mica_IdResult duration = mica_source_roundtrip_duration(source_integer(integers[i]));
        bool valid = integers[i] >= 0 && integers[i] <= 18446744073;
        assert(duration.f_ok == valid);
        if (valid) assert(duration.f_number == (uint64_t)integers[i] * UINT64_C(1000000000));
    }
    // Compare the integer decoder with an independent double-precision oracle,
    // including signs, subnormals, half-millisecond ties, infinities and NaNs.
    uint32_t bits = 0;
    const uint32_t edges[] = {0, 0x80000000, 1, 0x3a03126e, 0x3a03126f,
        0x3a031270, 0x3ac49ba5, 0x3ac49ba6, 0x3f000000, 0x7f800000, 0xff800000,
        0x7fc00000, 0x5089705f, 0x50897060, 0x50897061};
    for (unsigned i = 0; i < 10000; ++i) {
        bits = bits * 1664525u + 1013904223u;
        uint32_t sample = i < sizeof(edges) / sizeof(edges[0]) ? edges[i] : bits;
        float seconds;
        memcpy(&seconds, &sample, sizeof(seconds));
        struct mica_ValueResult encoded = mica_value_float(seconds);
        if (!isfinite(seconds)) {
            assert(!encoded.f_ok);
            continue;
        }
        assert(encoded.f_ok);
        struct mica_IdResult duration = mica_source_roundtrip_duration(encoded.f_value);
        double millis = round((double)seconds * 1000.0);
        bool valid = isfinite(seconds) && seconds >= 0 && millis <= 18446744073709.0;
        assert(duration.f_ok == valid);
        if (valid) assert(duration.f_number == (uint64_t)millis * UINT64_C(1000000));
    }
    assert(!mica_source_roundtrip_duration(mica_value_bool(false)).f_ok);
}

static bool source_time_fact(struct mica_Kernel *kernel, struct mica_MemoryWorker *worker, int64_t key) {
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    mica_type_Value cells[] = {source_integer(key), source_integer(1)}, matches[1];
    struct mica_ValueResult row = mica_value_list(worker, cells, 2);
    assert(row.f_ok);
    struct mica_KernelScanResult result = mica_kernel_scan(&tx, 705, row.f_value, 3, 0, matches, 1);
    assert(result.f_status == 0);
    assert(mica_kernel_end(&tx));
    return result.f_count != 0;
}

static void source_time_execute(struct mica_TaskControl *task, struct mica_MemoryWorker *worker, unsigned stage) {
    uint64_t status = mica_task_execute(task, worker);
    if (status) fprintf(stderr, "timed source stage %u: status %llu\n", stage, (unsigned long long)status);
    assert(status == 0);
}

static void source_time_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel,
        struct mica_MemoryRoot *code, struct mica_TaskRuntime *runtime, struct mica_TaskServices *services) {
    source_duration_checks();
    struct mica_MemoryRoot image = {0}, completed = {0};
    assert(mica_memory_root_push(worker, &image));
    struct mica_ValueResult empty = mica_value_list(worker, NULL, 0);
    assert(empty.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_2_0,
        (void *)code->f_pointer, source_integer(7), empty.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(image.f_pointer);
    struct mica_TaskControl task = {0};
    assert(mica_task_init(&task, worker, kernel, &image, 100000, 3, 16, runtime) == 0);
    for (unsigned segment = 0; segment < 3; ++segment) {
        assert(!source_time_fact(kernel, worker, 10 + segment));
        source_time_execute(&task, worker, segment);
        assert(source_time_fact(kernel, worker, 10 + segment));
        struct mica_MemoryRoot prepared = {0};
        assert(mica_memory_acquire(worker, &task.f_prepared, &prepared));
        struct mica_TaskBoundary *boundary = (void *)prepared.f_pointer;
        assert(boundary->f_kind == (segment == 0 ? 3 : 2));
        if (segment) {
            struct mica_TaskWaitRequest *request = boundary->f_request;
            assert(request && !request->f_mailbox);
            assert(request->f_timeout == (segment == 1 ? UINT64_C(2000000) : UINT64_MAX));
            assert(mica_value_is_unit(request->f_reply));
        }
        assert(mica_task_dispatch(&task, worker, services) == 0);
        assert(mica_memory_root_pop(worker, &prepared));
        assert(mica_memory_safepoint(worker, true));
        if (segment) {
            assert(task.f_state == 4);
            uint64_t generation = task.f_generation;
            assert(mica_task_wake(&task, worker, generation, source_integer(40 + segment)) == 0);
            assert(mica_task_wake(&task, worker, generation, source_integer(99)) != 0);
        } else {
            assert(task.f_state == 1);
        }
    }
    source_time_execute(&task, worker, 3);
    assert(mica_task_dispatch(&task, worker, services) == 0 && task.f_state == 5);
    assert(mica_memory_acquire(worker, &task.f_image, &completed));
    mica_type_Value value = ((struct mica_TaskImage *)completed.f_pointer)->f_input;
    assert(mica_value_is_unit(source_cell(value, 0)));
    assert(source_cell(value, 1) == source_integer(41) && source_cell(value, 2) == source_integer(42));
    assert(source_cell(source_cell(value, 3), 1) == source_integer(42));
    assert(mica_value_string_length(source_cell(source_cell(value, 3), 0)).f_number == 19);
    assert(mica_memory_root_pop(worker, &completed));
    assert(mica_task_release(&task, worker));

    empty = mica_value_list(worker, NULL, 0);
    assert(empty.f_ok);
    image.f_pointer = (void *)mica_task_new_Image(worker, mica_source_roundtrip_method_3_0,
        (void *)code->f_pointer, source_integer(7), empty.f_value, 0, NULL, NULL, 0, NULL, NULL);
    assert(mica_task_init(&task, worker, kernel, &image, 100000, 3, 16, runtime) == 0);
    source_time_execute(&task, worker, 4);
    assert(mica_task_dispatch(&task, worker, services) == 0 && task.f_state == 5);
    assert(mica_memory_acquire(worker, &task.f_image, &completed));
    value = ((struct mica_TaskImage *)completed.f_pointer)->f_input;
    assert(mica_value_list_length(value).f_number == 6);
    for (unsigned i = 0; i < 6; ++i) assert(source_cell(value, i) == mica_value_bool(true));
    assert(mica_memory_root_pop(worker, &completed));
    assert(mica_task_release(&task, worker));
    assert(mica_memory_root_pop(worker, &image));
}
