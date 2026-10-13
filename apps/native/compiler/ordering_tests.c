// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel) {
    struct mica_MemoryRoot input = {0}, output = {0}, heading = {0};
    assert(mica_memory_root_push(worker, &input) && mica_memory_root_push(worker, &output));
    assert(mica_memory_root_push(worker, &heading));
    mica_type_Value columns[] = {mica_value_symbol(1).f_value, mica_value_symbol(2).f_value};
    struct mica_ValueResult names = mica_value_list(worker, columns, 2);
    assert(names.f_ok);
    mica_value_root_set(&heading, names.f_value);
    struct mica_ValueResult list = mica_value_list(worker, NULL, 0);
    assert(list.f_ok);
    mica_value_root_set(&input, list.f_value);
    for (int64_t i = 512; i > 0; --i) {
        struct mica_ValueResult text = mica_value_string(worker, (const uint8_t *)"retained", 8);
        assert(text.f_ok);
        mica_type_Value cells[] = {source_integer(i), text.f_value};
        struct mica_ValueResult row = mica_value_list(worker, cells, 2);
        assert(row.f_ok);
        list = mica_value_list_append(worker, mica_value_root_get(&input), row.f_value);
        assert(list.f_ok);
        mica_value_root_set(&input, list.f_value);
    }
    struct mica_KernelTransaction tx = {0};
    assert(mica_kernel_begin(kernel, worker, &tx, source_integer(7)) == 0);
    struct mica_TaskAttempt attempt = {.f_remaining = 20};
    mica_value_root_set(&output, source_integer(99));
    assert(mica_source_roundtrip_sort(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 10);
    assert(mica_value_root_get(&output) == source_integer(99));
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_sort(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 0);
    assert(attempt.f_remaining < 1000000);
    assert(mica_memory_safepoint(worker, true));
    for (uint64_t i = 0; i < 512; ++i) {
        struct mica_ValueResult row = mica_value_list_get(mica_value_root_get(&output), i);
        assert(row.f_ok && mica_value_list_get(row.f_value, 0).f_value == source_integer((int64_t)i + 1));
        assert(mica_value_string_length(mica_value_list_get(row.f_value, 1).f_value).f_number == 8);
        row = mica_value_list_get(mica_value_root_get(&input), i);
        assert(row.f_ok && mica_value_list_get(row.f_value, 0).f_value == source_integer(512 - (int64_t)i));
    }
    // The allowance reaches canonical construction but cannot finish copying
    // and sorting all rows. A failed result carries no partially built value.
    attempt.f_remaining = 2000;
    struct mica_source_roundtrip_RelationResult relation = mica_source_roundtrip_relation_from_rows(
        worker, &attempt, &tx, mica_value_root_get(&heading), mica_value_root_get(&input));
    assert(relation.f_status == 10 && !relation.f_value && !relation.f_shape);
    attempt.f_remaining = 1000000;
    relation = mica_source_roundtrip_relation_from_rows(worker, &attempt, &tx,
        mica_value_root_get(&heading), mica_value_root_get(&input));
    assert(relation.f_status == 0 && !relation.f_shape);
    mica_value_root_set(&output, relation.f_value);
    assert(mica_value_relation_length(mica_value_root_get(&output)).f_number == 512);
    assert(mica_memory_safepoint(worker, true));
    assert(mica_kernel_cancel(&tx));
    assert(mica_source_roundtrip_sort(worker, &attempt, &tx, mica_value_root_get(&input), &output) == 11);
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &heading));
    assert(mica_memory_root_pop(worker, &output) && mica_memory_root_pop(worker, &input));
}
