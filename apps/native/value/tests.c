// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>

static bool fail_allocation;
static size_t allocations;
static size_t releases;
static size_t allocation_allowance = SIZE_MAX;

uint8_t *mica_foreign_allocate(uint64_t size) {
    if (fail_allocation || allocation_allowance == 0) return NULL;
    --allocation_allowance;
    uint8_t *pointer = native_test_allocate(size);
    if (pointer != NULL) ++allocations;
    return pointer;
}

void mica_foreign_release(uint8_t *pointer) {
    if (pointer != NULL) ++releases;
    native_test_release(pointer);
}

static mica_type_Value integer(int64_t number) {
    struct mica_ValueResult result = mica_value_int(number);
    assert(result.f_ok);
    return result.f_value;
}

static mica_type_Value floating(float number) {
    struct mica_ValueResult result = mica_value_float(number);
    assert(result.f_ok);
    return result.f_value;
}

static int64_t compare(mica_type_Value left, mica_type_Value right) {
    struct mica_IntResult result = mica_value_compare(left, right);
    assert(result.f_ok);
    return result.f_number;
}

static void map_cases(struct mica_ValueArena *arena) {
    const size_t sizes[] = {0, 1, 2, 3, 7, 32, 257, 4096};
    for (size_t n = 0; n < sizeof(sizes) / sizeof(sizes[0]); ++n) {
        size_t length = sizes[n];
        struct mica_ValueMapEntry *input = calloc(length + 1, sizeof(*input));
        assert(input != NULL);
        int64_t expected[31];
        for (unsigned i = 0; i < 31; ++i) expected[i] = -1;
        for (size_t i = 0; i < length; ++i) {
            unsigned key = (unsigned)((length - i) % 31);
            input[i] = (struct mica_ValueMapEntry){integer(key), integer((int64_t)i)};
            expected[key] = (int64_t)i;
        }
        struct mica_ValueResult map = mica_value_map(arena, input, length);
        assert(map.f_ok);
        const struct mica_HeapMap *header = mica_value_as_map(map.f_value).f_header;
        size_t count = 0;
        for (unsigned key = 0; key < 31; ++key) {
            struct mica_ValueResult found = mica_value_map_get(map.f_value, integer(key));
            assert(found.f_ok == (expected[key] >= 0));
            if (found.f_ok) {
                assert(found.f_value == integer(expected[key]));
                assert(header->f_data[count].f_key == integer(key));
                ++count;
            }
        }
        assert(header->f_length == count);
        assert(!mica_value_map_get(map.f_value, integer(-1)).f_ok);
        for (size_t i = 0; i < length; ++i) {
            assert(input[i].f_key == integer((int64_t)((length - i) % 31)));
            assert(input[i].f_value == integer((int64_t)i));
        }
        free(input);
    }
    assert(!mica_value_map(arena, NULL, 1).f_ok);
    assert(!mica_value_map_get(integer(0), integer(0)).f_ok);
    struct mica_ValueArena failed = {0};
    fail_allocation = true;
    assert(!mica_value_map(&failed, NULL, 0).f_ok);
    fail_allocation = false;
    assert(failed.f_head == NULL);
    struct mica_ValueMapEntry *large = calloc(4096, sizeof(*large));
    assert(large != NULL);
    allocation_allowance = 1;
    assert(!mica_value_map(&failed, large, 4096).f_ok);
    allocation_allowance = SIZE_MAX;
    // The first buffer remains arena-owned when scratch allocation fails.
    assert(failed.f_head != NULL);
    mica_value_arena_release(&failed);
    free(large);
    struct mica_ValueMapEntry dummy = {integer(0), integer(0)};
    assert(!mica_value_map(arena, &dummy, UINT64_MAX).f_ok);


    struct mica_ValueResult a = mica_value_string(arena, (const uint8_t *)"same", 4);
    struct mica_ValueResult b = mica_value_string(arena, (const uint8_t *)"same", 4);
    assert(a.f_ok && b.f_ok && a.f_value != b.f_value);
    struct mica_ValueMapEntry pairs[] = {
        {floating(1), integer(30)}, {a.f_value, integer(1)},
        {integer(1), integer(20)}, {b.f_value, integer(2)}
    };
    struct mica_ValueResult map = mica_value_map(arena, pairs, 4);
    assert(map.f_ok && mica_value_as_map(map.f_value).f_header->f_length == 3);
    assert(mica_value_map_get(map.f_value, a.f_value).f_value == integer(2));
    assert(mica_value_map_get(map.f_value, integer(1)).f_value == integer(20));
    assert(mica_value_map_get(map.f_value, floating(1)).f_value == integer(30));
    struct mica_ValueMapEntry reordered[] = {pairs[2], pairs[0], pairs[3]};
    struct mica_ValueResult equivalent = mica_value_map(arena, reordered, 3);
    assert(equivalent.f_ok && compare(map.f_value, equivalent.f_value) == 0);
    struct mica_ValueMapEntry nested[] = {{map.f_value, integer(99)}};
    struct mica_ValueResult outer = mica_value_map(arena, nested, 1);
    assert(outer.f_ok);
    assert(mica_value_map_get(outer.f_value, equivalent.f_value).f_value == integer(99));
}

static void comparison_cases(struct mica_ValueArena *arena) {
    assert(compare(integer(-1), integer(0)) == -1);
    assert(compare(integer(1), floating(1)) == -1);
    struct mica_BoolResult canonical = mica_value_equal(integer(1), floating(1));
    struct mica_BoolResult language = mica_value_language_equal(integer(1), floating(1));
    assert(canonical.f_ok && !canonical.f_value && language.f_ok && language.f_value);
    assert(compare(floating(-2), floating(-1)) == -1);
    assert(compare(0, 0) == 0);
    assert(compare(0, integer(0)) == 1);
    const int64_t integers[] = {-(INT64_C(1) << 55), -16777217, -2, -1, 0, 1, 2,
        16777217, (INT64_C(1) << 55) - 1};
    const float floats[] = {-FLT_MAX, -0x1p55f, -16777216.0f, -1.5f, -1.0f,
        -0x1p-149f, 0.0f, 0x1p-149f, 1.0f, 1.5f, 16777216.0f, 0x1p55f, FLT_MAX};
    _Static_assert(LDBL_MANT_DIG >= 56, "comparison oracle requires exact Mica integers");
    for (unsigned i = 0; i < sizeof(integers) / sizeof(integers[0]); ++i) {
        for (unsigned j = 0; j < sizeof(floats) / sizeof(floats[0]); ++j) {
            long double a = integers[i], b = floats[j];
            int64_t expected = (a > b) - (a < b);
            struct mica_IntResult forward = mica_value_language_compare(integer(integers[i]), floating(floats[j]));
            struct mica_IntResult reverse = mica_value_language_compare(floating(floats[j]), integer(integers[i]));
            assert(forward.f_ok && forward.f_number == expected);
            assert(reverse.f_ok && reverse.f_number == -expected);
        }
    }
    assert(!mica_value_compare_int_float(0, NAN).f_ok);
    struct mica_ValueResult a = mica_value_string(arena, (const uint8_t *)"a\0b", 3);
    struct mica_ValueResult b = mica_value_string(arena, (const uint8_t *)"a\0c", 3);
    struct mica_ValueResult prefix = mica_value_string(arena, (const uint8_t *)"a\0", 2);
    assert(a.f_ok && b.f_ok && prefix.f_ok);
    assert(compare(a.f_value, b.f_value) == -1 && compare(b.f_value, a.f_value) == 1);
    assert(compare(prefix.f_value, a.f_value) == -1);
    mica_type_Value values[] = {integer(1), a.f_value};
    struct mica_ValueResult list = mica_value_list(arena, values, 2);
    struct mica_ValueResult equal = mica_value_list(arena, values, 2);
    assert(list.f_ok && equal.f_ok && compare(list.f_value, equal.f_value) == 0);
    values[1] = b.f_value;
    struct mica_ValueResult greater = mica_value_list(arena, values, 2);
    assert(greater.f_ok && compare(list.f_value, greater.f_value) == -1);
    struct mica_ValueResult open_a = mica_value_range(arena, integer(0), false, a.f_value);
    struct mica_ValueResult open_b = mica_value_range(arena, integer(0), false, b.f_value);
    struct mica_ValueResult closed = mica_value_range(arena, integer(0), true, integer(0));
    assert(open_a.f_ok && open_b.f_ok && closed.f_ok);
    assert(compare(open_a.f_value, open_b.f_value) == 0);
    assert(compare(open_a.f_value, closed.f_value) == -1);
    struct mica_ValueResult error_a = mica_value_error(arena, 7, false, a.f_value, false, list.f_value);
    struct mica_ValueResult error_b = mica_value_error(arena, 7, false, b.f_value, false, greater.f_value);
    assert(error_a.f_ok && error_b.f_ok && compare(error_a.f_value, error_b.f_value) == 0);
    struct mica_ValueResult frob_a = mica_value_frob(arena, 7, list.f_value);
    struct mica_ValueResult frob_b = mica_value_frob(arena, 7, greater.f_value);
    assert(frob_a.f_ok && frob_b.f_ok && compare(frob_a.f_value, frob_b.f_value) == -1);
}

int main(void) {
    const int64_t minimum = -(INT64_C(1) << 55);
    const int64_t maximum = (INT64_C(1) << 55) - 1;
    assert(mica_value_empty_relation() == 0);
    assert(mica_value_kind(0) == 16 && mica_value_tag(0) == 0);
    const int64_t integers[] = {minimum, minimum + 1, -17, -1, 0, 1, 17, maximum - 1, maximum};
    for (unsigned i = 0; i < sizeof(integers) / sizeof(integers[0]); ++i) {
        mica_type_Value value = integer(integers[i]);
        assert(mica_value_tag(value) == 2);
        assert((value & UINT64_C(0x00ffffffffffffff)) == ((uint64_t)integers[i] & UINT64_C(0x00ffffffffffffff)));
        struct mica_IntResult extracted = mica_value_as_int(value);
        assert(extracted.f_ok && extracted.f_number == integers[i]);
        assert(!mica_value_as_float(value).f_ok);
    }
    const int64_t invalid[] = {minimum - 1, maximum + 1, INT64_MIN, INT64_MAX};
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        struct mica_ValueResult result = mica_value_int(invalid[i]);
        assert(!result.f_ok && result.f_value == 0);
    }
    assert(mica_value_bool(false) == (UINT64_C(1) << 56));
    assert(mica_value_bool(true) == ((UINT64_C(1) << 56) | 1));
    assert(mica_value_as_bool(mica_value_bool(true)).f_value);
    assert(!mica_value_as_bool(integer(1)).f_ok);
    assert(floating(-0.0f) == floating(0.0f));
    assert(floating(0.0f) == (UINT64_C(3) << 56));
    assert(!mica_value_float(INFINITY).f_ok && !mica_value_float(-INFINITY).f_ok && !mica_value_float(NAN).f_ok);
    assert(!mica_value_float_from_bits(0x7fc00001).f_ok);
    assert(mica_value_float_from_bits(0x80000000).f_value == floating(0.0f));
    assert(mica_value_as_float(floating(-1.25f)).f_number == -1.25f);
    assert(mica_value_float_from_bits(1).f_ok);
    assert(mica_value_identity(UINT64_C(0x00ffffffffffffff)).f_ok);
    assert(!mica_value_identity(UINT64_C(0x0100000000000000)).f_ok);
    assert(mica_value_identity(0).f_ok && mica_value_function(0).f_ok);
    assert(!mica_value_capability(0).f_ok && mica_value_capability(1).f_ok);
    assert(mica_value_as_symbol(mica_value_symbol(UINT32_MAX).f_value).f_number == UINT32_MAX);
    assert(mica_value_as_error_code(mica_value_error_code(77).f_value).f_number == 77);
    assert(!mica_value_checked_add(integer(maximum), integer(1)).f_ok);
    assert(!mica_value_checked_sub(integer(minimum), integer(1)).f_ok);
    assert(!mica_value_checked_mul(integer(maximum), integer(maximum)).f_ok);
    assert(mica_value_checked_add(integer(maximum), integer(minimum)).f_value == integer(-1));
    assert(mica_value_checked_mul(integer(-7), integer(6)).f_value == integer(-42));
    assert(!mica_value_checked_div(integer(7), integer(2)).f_ok);
    assert(mica_value_checked_div(integer(-8), integer(2)).f_value == integer(-4));
    assert(!mica_value_checked_div(integer(1), integer(0)).f_ok);
    assert(!mica_value_checked_div(integer(minimum), integer(-1)).f_ok);
    assert(mica_value_checked_rem(integer(-7), integer(2)).f_value == integer(-1));
    assert(!mica_value_checked_rem(integer(1), integer(0)).f_ok);
    assert(!mica_value_checked_neg(integer(minimum)).f_ok);
    assert(mica_value_checked_neg(integer(-7)).f_value == integer(7));
    assert(!mica_value_checked_add(integer(1), floating(1)).f_ok);
    assert(!mica_value_checked_add(mica_value_bool(true), mica_value_bool(false)).f_ok);
    assert(mica_value_checked_add(floating(1.25f), floating(2.5f)).f_value == floating(3.75f));
    assert(!mica_value_checked_mul(floating(FLT_MAX), floating(2.0f)).f_ok);
    assert(!mica_value_checked_div(floating(1), floating(-0.0f)).f_ok);
    assert(mica_value_checked_rem(floating(-7.5f), floating(2)).f_value == floating(-1.5f));
    assert(mica_value_to_float(integer(16777217)).f_value == floating(16777216.0f));
    assert(mica_value_to_int(floating(-0x1p55f)).f_value == integer(minimum));
    assert(!mica_value_to_int(floating(0x1p55f)).f_ok);
    assert(!mica_value_to_int(floating(1.5f)).f_ok);
    assert(mica_value_to_int(floating(-17)).f_value == integer(-17));
    assert(!mica_value_to_float(mica_value_bool(true)).f_ok);
    struct mica_ValueArena arena = {0};
    uint8_t *first = mica_value_arena_allocate(&arena, 3);
    assert(first != NULL && (uintptr_t)first % 8 == 0);
    memcpy(first, "abc", 3);
    for (unsigned i = 0; i < 1000; ++i) {
        uint8_t *allocation = mica_value_arena_allocate(&arena, 1000);
        assert(allocation != NULL && (uintptr_t)allocation % 8 == 0);
        memset(allocation, (int)(i & 255), 1000);
    }
    assert(memcmp(first, "abc", 3) == 0);
    struct mica_ValueArenaBlock *head_before_failure = arena.f_head;
    uint64_t used_before_failure = arena.f_head->f_used;
    fail_allocation = true;
    assert(mica_value_arena_allocate(&arena, UINT64_C(1) << 20) == NULL);
    assert(arena.f_head == head_before_failure && arena.f_head->f_used == used_before_failure);
    struct mica_ValueArena empty_arena = {0};
    assert(!mica_value_string(&empty_arena, (const uint8_t *)"abc", 3).f_ok);
    assert(empty_arena.f_head == NULL);
    fail_allocation = false;
    assert(mica_value_arena_allocate(&arena, UINT64_MAX) == NULL);
    assert(mica_value_arena_allocate(&arena, 0) == NULL);
    struct mica_ValueResult composed = mica_test_composed(&arena);
    assert(composed.f_ok && mica_value_as_range(composed.f_value).f_header->f_start == integer(3));
    uint8_t original[] = {0xc3, 0xa9, 0, 0x78};
    struct mica_ValueResult text = mica_value_string(&arena, original, sizeof(original));
    assert(text.f_ok && mica_value_kind(text.f_value) == 7);
    original[0] = 0;
    struct mica_HeapStringResult extracted = mica_value_as_string(text.f_value);
    assert(extracted.f_ok && extracted.f_header->f_length == 4);
    assert(extracted.f_header->f_data[0] == 0xc3 && extracted.f_header->f_data[2] == 0);
    assert(!mica_value_as_bytes(text.f_value).f_ok);
    assert(!mica_value_string(&arena, NULL, 1).f_ok);
    assert(!mica_value_string(&arena, original, UINT64_MAX).f_ok);
    assert(mica_value_string(&arena, NULL, 0).f_ok);
    assert(mica_value_bytes(&arena, NULL, 0).f_ok);
    mica_type_Value items[] = {integer(3), text.f_value, mica_value_bool(true)};
    struct mica_ValueResult list = mica_value_list(&arena, items, 3);
    assert(list.f_ok);
    items[0] = integer(4);
    const struct mica_HeapList *list_header = mica_value_as_list(list.f_value).f_header;
    assert(list_header->f_length == 3 && list_header->f_data[0] == integer(3));
    assert(list_header->f_data[1] == text.f_value);
    struct mica_ValueResult range = mica_value_range(&arena, integer(-3), true, integer(7));
    assert(range.f_ok);
    const struct mica_HeapRange *range_header = mica_value_as_range(range.f_value).f_header;
    assert(range_header->f_start == integer(-3) && range_header->f_has_end && range_header->f_end == integer(7));
    struct mica_ValueResult frob = mica_value_frob(&arena, 42, list.f_value);
    assert(frob.f_ok && mica_value_as_frob(frob.f_value).f_header->f_value == list.f_value);
    assert(!mica_value_frob(&arena, UINT64_MAX, list.f_value).f_ok);
    struct mica_ValueResult error = mica_value_error(&arena, 17, true, text.f_value, true, list.f_value);
    assert(error.f_ok && mica_value_as_error(error.f_value).f_header->f_code == 17);
    assert(!mica_value_error(&arena, 17, true, integer(3), false, 0).f_ok);
    comparison_cases(&arena);
    map_cases(&arena);
    mica_value_arena_release(&arena);
    assert(arena.f_head == NULL);
    mica_value_arena_release(&arena);
    assert(allocations == releases);
    return 0;
}
