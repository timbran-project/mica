/* Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 * Independent assertions against the generated public functions. */

#include <assert.h>

static bool fail_allocation;
static unsigned live_allocations;
static unsigned allocation_attempts;

uint8_t *mica_foreign_allocate(uint64_t size) {
    ++allocation_attempts;
    if (fail_allocation) return NULL;
    uint8_t *result = native_test_allocate(size);
    if (result != NULL) ++live_allocations;
    return result;
}

void mica_foreign_release(uint8_t *data) {
    if (data != NULL) {
        assert(live_allocations > 0);
        --live_allocations;
    }
    native_test_release(data);
}

static void check_integers(void) {
    const int64_t minimum = -INT64_C(36028797018963968);
    const int64_t maximum = INT64_C(36028797018963967);
    const int64_t cases[] = {
        INT64_MIN, minimum - 1, minimum, minimum + 1, -1, 0, 1,
        maximum - 1, maximum, maximum + 1, INT64_MAX
    };
    for (size_t a = 0; a < sizeof(cases) / sizeof(cases[0]); ++a) {
        for (size_t b = 0; b < sizeof(cases) / sizeof(cases[0]); ++b) {
            struct mica_IntResult result = mica_checked_add(cases[a], cases[b]);
            bool valid = cases[a] >= minimum && cases[a] <= maximum &&
                         cases[b] >= minimum && cases[b] <= maximum;
            int64_t sum = 0;
            if (valid) {
                /* Two signed 56-bit values sum without overflowing int64_t. */
                sum = cases[a] + cases[b];
                valid = sum >= minimum && sum <= maximum;
            }
            assert(result.f_ok == valid);
            assert(result.f_number == (valid ? sum : 0));
        }
    }
    for (uint64_t tag = 0; tag < 256; ++tag) {
        assert(mica_tag((tag << 56) | UINT64_C(0x123456789abcde)) == tag);
    }
}

static void expect_error(struct mica_Arena *arena, const uint8_t *source,
                         uint64_t extent, int64_t length, uint64_t status) {
    const uint64_t used = arena->f_used;
    const struct mica_StringResult result = mica_string_copy(arena, source, extent, length);
    assert(result.f_status == status);
    assert(result.f_text.f_data == NULL && result.f_text.f_length == 0);
    assert(arena->f_used == used);
}

static void check_strings(void) {
    struct mica_ArenaResult created = mica_arena_create(32);
    assert(created.f_ok && created.f_arena.f_capacity == 32);
    assert(created.f_arena.f_used == 0 && live_allocations == 1);
    struct mica_Arena *arena = &created.f_arena;
    struct mica_StringResult empty = mica_string_copy(arena, NULL, 0, 0);
    assert(empty.f_status == 0 && empty.f_text.f_data == NULL);
    assert(empty.f_text.f_length == 0 && arena->f_used == 0);

    /* UTF-8: e-acute, crab, followed by an embedded zero byte. Length is bytes. */
    uint8_t input[] = {0xc3, 0xa9, 0xf0, 0x9f, 0xa6, 0x80, 0};
    const uint8_t expected[] = {0xc3, 0xa9, 0xf0, 0x9f, 0xa6, 0x80, 0};
    struct mica_StringResult first = mica_string_copy(arena, input, sizeof(input), sizeof(input));
    assert(first.f_status == 0 && first.f_text.f_length == sizeof(input));
    assert(first.f_text.f_data != input && arena->f_used == sizeof(input));
    memset(input, 0xff, sizeof(input));
    assert(memcmp(first.f_text.f_data, expected, sizeof(expected)) == 0);
    struct mica_StringResult second = mica_string_copy(arena, first.f_text.f_data, sizeof(expected), sizeof(expected));
    assert(second.f_status == 0 && second.f_text.f_data != first.f_text.f_data);
    assert(memcmp(second.f_text.f_data, expected, sizeof(expected)) == 0);
    assert(memcmp(first.f_text.f_data, expected, sizeof(expected)) == 0);

    expect_error(arena, expected, sizeof(expected), -1, 1);
    expect_error(arena, expected, sizeof(expected), INT64_MAX, 1);
    expect_error(arena, expected, 1, 2, 1);
    expect_error(arena, NULL, 1, 1, 1);
    uint8_t large[32] = {0};
    expect_error(arena, large, sizeof(large), sizeof(large), 2);
    assert(live_allocations == 1);
    mica_arena_release(arena);
    assert(arena->f_data == NULL && arena->f_capacity == 0 && arena->f_used == 0);
    assert(live_allocations == 0);
    /* Old views cease to be valid at release and are never dereferenced here. */
    mica_arena_release(arena);
    expect_error(arena, expected, sizeof(expected), 1, 2);

    created = mica_arena_create(4);
    struct mica_StringResult exact = mica_string_copy(&created.f_arena, large, 4, 4);
    assert(exact.f_status == 0 && created.f_arena.f_used == 4);
    expect_error(&created.f_arena, large, 1, 1, 2);
    mica_arena_release(&created.f_arena);

    /* The copy primitive handles overlapping readable input. */
    created = mica_arena_create(8);
    memcpy(created.f_arena.f_data, "abcde", 5);
    created.f_arena.f_used = 1;
    struct mica_StringResult overlap = mica_string_copy(&created.f_arena, created.f_arena.f_data, 5, 5);
    assert(overlap.f_status == 0 && memcmp(overlap.f_text.f_data, "abcde", 5) == 0);
    created.f_arena.f_used = 9;
    expect_error(&created.f_arena, large, 1, 1, 2);
    mica_arena_release(&created.f_arena);

    unsigned attempts = allocation_attempts;
    created = mica_arena_create(0);
    assert(created.f_ok && created.f_arena.f_data == NULL);
    assert(allocation_attempts == attempts);
    expect_error(&created.f_arena, large, 1, 1, 2);
    mica_arena_release(&created.f_arena);
    created = mica_arena_create(UINT64_MAX);
    assert(!created.f_ok && created.f_arena.f_data == NULL);
    assert(allocation_attempts == attempts);

    fail_allocation = true;
    created = mica_arena_create(8);
    assert(!created.f_ok && created.f_arena.f_data == NULL);
    assert(created.f_arena.f_capacity == 0 && created.f_arena.f_used == 0);
    assert(allocation_attempts == attempts + 1 && live_allocations == 0);
    mica_arena_release(&created.f_arena);
    fail_allocation = false;

    for (unsigned i = 0; i < 100; ++i) {
        created = mica_arena_create(8);
        assert(created.f_ok);
        assert(mica_string_copy(&created.f_arena, expected, sizeof(expected), sizeof(expected)).f_status == 0);
        mica_arena_release(&created.f_arena);
    }
    assert(live_allocations == 0);
}

int main(void) {
    check_integers();
    check_strings();
    return 0;
}
