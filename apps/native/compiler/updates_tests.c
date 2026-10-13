// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

static mica_type_Value update_integer(int64_t number) {
    struct mica_ValueResult value = mica_value_int(number);
    assert(value.f_ok);
    return value.f_value;
}
static mica_type_Value update_row(struct mica_MemoryWorker *worker, int64_t a, int64_t b, int64_t c, unsigned width) {
    mica_type_Value cells[] = {update_integer(a), update_integer(b), update_integer(c)};
    struct mica_ValueResult value = mica_value_list(worker, cells, width);
    assert(value.f_ok);
    return value.f_value;
}
static bool update_present(struct mica_KernelTransaction *tx, uint64_t id, mica_type_Value row) {
    return mica_kernel_find(mica_kernel_index(mica_kernel_view(tx, id), 0), row, 0, false) != NULL;
}
static void source_body_checks(struct mica_MemoryWorker *worker, struct mica_Kernel *kernel) {
    struct mica_KernelTransaction tx = {0}, rival = {0};
    struct mica_MemoryRoot pattern = {0};
    assert(mica_memory_root_push(worker, &pattern));
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    assert(mica_kernel_replace(&tx, 705, update_row(worker, 1, 10, 0, 2)) == 0);
    assert(mica_kernel_replace(&tx, 705, update_row(worker, 1, 11, 0, 2)) == 0);
    assert(!update_present(&tx, 705, update_row(worker, 1, 10, 0, 2)));
    assert(update_present(&tx, 705, update_row(worker, 1, 11, 0, 2)));
    assert(mica_kernel_write(&tx, 705, update_row(worker, 1, 12, 0, 2), true) == 4);
    assert(mica_kernel_replace(&tx, 705, update_row(worker, 1, 12, 0, 3)) == 2);
    mica_type_Value cells[] = {update_integer(1), mica_value_capability(9).f_value};
    struct mica_ValueResult forbidden = mica_value_list(worker, cells, 2);
    assert(forbidden.f_ok && mica_kernel_replace(&tx, 705, forbidden.f_value) == 3);
    assert(update_present(&tx, 705, update_row(worker, 1, 11, 0, 2)));
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, 0) == 0);
    assert(mica_kernel_replace(&tx, 705, update_row(worker, 1, 12, 0, 2)) == 10);
    assert(update_present(&tx, 705, update_row(worker, 1, 11, 0, 2)));
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == 0);
    struct mica_TaskAttempt limited = {.f_remaining = 1};
    assert(mica_source_roundtrip_relation_replace(worker, &limited, &tx, 705,
        update_row(worker, 1, 12, 0, 2)) == 10);
    assert(update_present(&tx, 705, update_row(worker, 1, 11, 0, 2)));
    limited.f_remaining = 0;
    assert(mica_source_roundtrip_relation_write(worker, &limited, &tx, 706,
        update_row(worker, 90, 91, 92, 3), true) == 10);
    assert(!update_present(&tx, 706, update_row(worker, 90, 91, 92, 3)));
    assert(mica_kernel_begin(kernel, worker, &rival, update_integer(7)) == 0);
    assert(mica_kernel_replace(&rival, 705, update_row(worker, 1, 99, 0, 2)) == 0);
    assert(mica_kernel_commit(&tx) == 0);
    assert(mica_kernel_commit(&rival) == 5);
    assert(mica_kernel_end(&rival) && mica_kernel_end(&tx));

    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    assert(mica_kernel_declare(&tx, 708, 708, 3, 1, 6, 0) == 0);
    assert(mica_kernel_replace(&tx, 708, update_row(worker, 1, 2, 3, 3)) == 0);
    assert(mica_kernel_replace(&tx, 708, update_row(worker, 9, 2, 3, 3)) == 0);
    assert(!update_present(&tx, 708, update_row(worker, 1, 2, 3, 3)));
    assert(update_present(&tx, 708, update_row(worker, 9, 2, 3, 3)));
    for (int64_t i = 0; i < 200; ++i)
        assert(mica_kernel_write(&tx, 706, update_row(worker, i, 0, 0, 3), true) == 0);
    assert(mica_kernel_commit(&tx) == 0);
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    assert(mica_kernel_begin(kernel, worker, &rival, update_integer(7)) == 0);
    mica_value_root_set(&pattern, update_row(worker, -1, 0, 0, 3));
    struct mica_TaskAttempt attempt = {.f_remaining = 1000000};
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 706, mica_value_root_get(&pattern), 1) == 0);
    assert(!mica_kernel_work(&tx)->f_changes);
    mica_value_root_set(&pattern, update_row(worker, 0, 0, 0, 3));
    struct mica_KernelBudget budget = {.f_steps = UINT64_MAX, .f_rows = UINT64_MAX, .f_rounds = UINT64_MAX, .f_bytes = UINT64_MAX};
    assert(mica_kernel_read(&tx, 706, &budget).f_status == 0);
    attempt.f_remaining = 1000000;
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, 1, UINT64_MAX, UINT64_MAX) == 0);
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 706, mica_value_root_get(&pattern), 0) == 10);
    for (int64_t i = 0; i < 200; ++i)
        assert(update_present(&tx, 706, update_row(worker, i, 0, 0, 3)));
    assert(attempt.f_remaining < 1000000);
    assert(mica_kernel_set_limits(&tx, UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX) == 0);
    attempt.f_remaining = 0;
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 706, mica_value_root_get(&pattern), 0) == 10);
    assert(update_present(&tx, 706, update_row(worker, 0, 0, 0, 3)));
    attempt.f_remaining = 1000000;
    assert(mica_memory_safepoint(worker, true));
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 706, mica_value_root_get(&pattern), 0) == 0);
    assert(!update_present(&tx, 706, update_row(worker, 0, 0, 0, 3)));
    assert(update_present(&rival, 706, update_row(worker, 0, 0, 0, 3)));
    // Aborting the draft restores every retracted row.
    assert(mica_kernel_end(&rival) && mica_kernel_end(&tx));
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    assert(mica_kernel_cancel(&tx));
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 706, mica_value_root_get(&pattern), 0) == 11);
    assert(mica_kernel_end(&tx));

    // Retraction removes stored support, never rows that remain derivable.
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    assert(mica_kernel_declare(&tx, 709, 709, 3, 0, 0, 0) == 0);
    struct mica_KernelRuleTerm *terms = NULL;
    for (unsigned i = 3; i > 0; --i) {
        terms = mica_kernel_rule_variable(worker, i - 1, terms);
        assert(terms);
    }
    struct mica_KernelRuleAtom *atom = mica_kernel_rule_atom(worker, 706, false, terms, NULL);
    assert(atom);
    struct mica_ValueResult source = mica_value_string(worker, (const uint8_t *)"", 0);
    assert(source.f_ok);
    struct mica_KernelRuleDef *rule = mica_kernel_rule_definition(worker, 709, terms, atom, NULL,
        update_integer(7), source.f_value);
    assert(rule && mica_kernel_rule_add(&tx, 100, rule, true) == 0);
    assert(mica_kernel_write(&tx, 709, update_row(worker, 999, 0, 0, 3), true) == 0);
    assert(mica_kernel_commit(&tx) == 0);
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    attempt.f_remaining = 1000000;
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 709, mica_value_root_get(&pattern), 0) == 0);
    assert(!update_present(&tx, 709, update_row(worker, 999, 0, 0, 3)));
    // The operation records just the stored row, not the 200 derived rows.
    struct mica_KernelNode *writes = mica_kernel_change(mica_kernel_work(&tx), 709)->f_writes;
    assert(writes && !writes->f_left && !writes->f_right && !writes->f_present);
    struct mica_KernelRelationResult visible = mica_kernel_read(&tx, 709, &budget);
    assert(visible.f_status == 0);
    assert(mica_kernel_find(mica_kernel_index(visible.f_relation, 0),
        update_row(worker, 0, 0, 0, 3), 0, false));
    assert(mica_kernel_end(&tx));

    // Replacement and exact retraction need only write rights. A wildcard
    // retraction observes matching rows and must also have read rights.
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(7)) == 0);
    assert(mica_kernel_declare(&tx, 802, 802, 2, 0, 0, 0) == 0);
    assert(mica_kernel_declare(&tx, 803, 803, 1, 0, 0, 0) == 0);
    assert(mica_kernel_write(&tx, 803, update_row(worker, 7, 0, 0, 1), true) == 0);
    for (uint64_t id = 705; id <= 706; ++id) {
        mica_type_Value grant[] = {update_integer(99), mica_value_identity(id).f_value};
        struct mica_ValueResult row = mica_value_list(worker, grant, 2);
        assert(row.f_ok && mica_kernel_write(&tx, 802, row.f_value, true) == 0);
    }
    assert(mica_kernel_set_authority_policy(&tx, 803, 0, 0, 0, 802, 0, 0, 0) == 0);
    assert(mica_kernel_commit(&tx) == 0);
    assert(mica_kernel_end(&tx));
    assert(mica_kernel_begin(kernel, worker, &tx, update_integer(99)) == 0);
    assert(mica_kernel_replace(&tx, 705, update_row(worker, 1, 21, 0, 2)) == 0);
    assert(mica_kernel_write(&tx, 706, update_row(worker, 0, 0, 0, 3), false) == 0);
    assert(mica_source_roundtrip_relation_retract(worker, &attempt, &tx, 706, mica_value_root_get(&pattern), 0) == 12);
    assert(update_present(&tx, 706, update_row(worker, 1, 0, 0, 3)));
    assert(mica_kernel_end(&tx));
    assert(mica_memory_root_pop(worker, &pattern));
}
