// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <stdio.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sched.h>

static bool fail_allocation;
static _Atomic size_t allocations;
static _Atomic size_t releases;
static _Atomic size_t allocation_allowance = SIZE_MAX;

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

static void symbol_cases(void) {
    struct mica_SymbolTable table = {0};
    assert(!mica_value_symbol_text(&table, 0).f_ok);
    assert(!mica_value_symbol_intern(&table, NULL, 0).f_ok);
    fail_allocation = true;
    assert(!mica_value_symbol_table_init(&table));
    assert(table.f_mutex == NULL);
    fail_allocation = false;
    assert(mica_value_symbol_table_init(&table));
    assert(!mica_value_symbol_table_init(&table));
    assert(!mica_value_symbol_intern(&table, NULL, 1).f_ok);
    assert(!mica_value_symbol_intern(&table, (const uint8_t *)"x", UINT64_MAX).f_ok);
    const uint8_t invalid[][4] = {{0xc0, 0x80}, {0xed, 0xa0, 0x80}, {0xf4, 0x90, 0x80, 0x80}};
    const size_t widths[] = {2, 3, 4};
    for (size_t i = 0; i < 3; ++i)
        assert(!mica_value_symbol_intern(&table, invalid[i], widths[i]).f_ok);
    assert(table.f_count == 0 && table.f_arena.f_head == NULL);
    fail_allocation = true;
    assert(!mica_value_symbol_intern(&table, NULL, 0).f_ok);
    assert(table.f_count == 0 && table.f_capacity == 0);
    fail_allocation = false;
    struct mica_IdResult empty = mica_value_symbol_intern(&table, NULL, 0);
    assert(empty.f_ok && empty.f_number == 0);
    struct mica_SymbolText text = mica_value_symbol_text(&table, 0);
    assert(text.f_ok && text.f_length == 0 && text.f_scalars == 0 && text.f_ascii);
    uint8_t original[] = {0xc3, 0xa9, 0, 0xf0, 0x9f, 0x98, 0x80};
    struct mica_IdResult unicode = mica_value_symbol_intern(&table, original, sizeof(original));
    assert(unicode.f_ok && unicode.f_number == 1);
    text = mica_value_symbol_text(&table, 1);
    assert(text.f_length == 7 && text.f_scalars == 3 && !text.f_ascii);
    const uint8_t *stable = text.f_data;
    original[0] = 'x';
    assert(stable[0] == 0xc3);
    size_t before = allocations;
    uint64_t used_before = table.f_arena.f_head->f_used;
    fail_allocation = true;
    assert(mica_value_symbol_intern(&table, stable, 7).f_number == 1);
    empty = mica_value_symbol_intern(&table, NULL, 0);
    assert(empty.f_ok && empty.f_number == 0);
    fail_allocation = false;
    assert(allocations == before && table.f_count == 2);
    assert(table.f_arena.f_head->f_used == used_before);
    assert(mica_value_symbol_intern(&table, (const uint8_t *)"Name", 4).f_number == 2);
    assert(mica_value_symbol_intern(&table, (const uint8_t *)"name", 4).f_number == 3);
    // Equal hash and length are insufficient: the candidate bytes must match.
    uint64_t slot = mica_symbol_slot(&table, (const uint8_t *)"xxxx", 4, table.f_entries[2].f_hash);
    assert(table.f_buckets[slot] == 0);
    // Force a probe chain across the end of the initial bucket array.
    unsigned collisions = 0;
    for (unsigned n = 0; collisions < 12; ++n) {
        char name[32];
        int length = snprintf(name, sizeof(name), "collision-%u", n);
        if ((mica_symbol_hash((const uint8_t *)name, (uint64_t)length) & 31) != 31) continue;
        struct mica_IdResult id = mica_value_symbol_intern(&table, (const uint8_t *)name, (uint64_t)length);
        assert(id.f_ok && id.f_number == 4 + collisions);
        assert(mica_value_symbol_intern(&table, (const uint8_t *)name, (uint64_t)length).f_number == id.f_number);
        ++collisions;
    }
    for (unsigned n = 0; n < 4096; ++n) {
        char name[32];
        int length = snprintf(name, sizeof(name), "symbol-%u", n);
        struct mica_IdResult id = mica_value_symbol_intern(&table, (const uint8_t *)name, (uint64_t)length);
        assert(id.f_ok && id.f_number == n + 16);
    }
    for (unsigned n = 0; n < 4096; ++n) {
        char name[32];
        int length = snprintf(name, sizeof(name), "symbol-%u", n);
        text = mica_value_symbol_text(&table, n + 16);
        assert(text.f_ok && text.f_length == (uint64_t)length && text.f_scalars == text.f_length && text.f_ascii);
        assert(memcmp(text.f_data, name, (size_t)length) == 0);
        assert(mica_value_symbol_intern(&table, text.f_data, text.f_length).f_number == n + 16);
    }
    assert(mica_value_symbol_text(&table, 1).f_data == stable);
    assert(!mica_value_symbol_text(&table, (uint32_t)table.f_count).f_ok);
    assert(!mica_value_symbol_text(&table, UINT32_MAX).f_ok);
    uint64_t count = table.f_count;
    table.f_count = UINT64_C(1) << 32;
    assert(!mica_value_symbol_intern(&table, (const uint8_t *)"exhausted", 9).f_ok);
    assert(mica_value_symbol_intern(&table, stable, 7).f_number == 1);
    table.f_count = count;
    mica_value_symbol_table_release(&table);
    assert(table.f_count == 0 && table.f_capacity == 0 && table.f_entries == NULL && table.f_buckets == NULL);
    mica_value_symbol_table_release(&table);

    // Each allocator failure must leave published IDs usable and permit retry.
    uint8_t *large = malloc(70000);
    assert(large != NULL);
    memset(large, 'a', 70000);
    for (size_t allowance = 0; allowance < 3; ++allowance) {
        assert(mica_value_symbol_table_init(&table));
        allocation_allowance = allowance;
        struct mica_IdResult id = mica_value_symbol_intern(&table, large, 70000);
        assert(id.f_ok == (allowance == 2));
        assert(table.f_count == (id.f_ok ? 1 : 0));
        allocation_allowance = SIZE_MAX;
        id = mica_value_symbol_intern(&table, large, 70000);
        assert(id.f_ok && id.f_number == 0);
        assert(mica_value_symbol_text(&table, 0).f_length == 70000);
        for (unsigned n = 1; n < 16; ++n) {
            uint8_t name = (uint8_t)('a' + n);
            id = mica_value_symbol_intern(&table, &name, 1);
            assert(id.f_ok && id.f_number == n);
        }
        // Exhaust the current chunk so growth and the new name each allocate.
        struct mica_ValueArenaBlock *head = table.f_arena.f_head;
        head->f_used = head->f_capacity;
        large[0] = 'b';
        allocation_allowance = allowance;
        id = mica_value_symbol_intern(&table, large, 70000);
        assert(id.f_ok == (allowance == 2));
        assert(table.f_count == (id.f_ok ? 17 : 16));
        assert(table.f_capacity == (allowance == 0 ? 16 : 32));
        fail_allocation = true;
        text = mica_value_symbol_text(&table, 0);
        assert(text.f_ok && text.f_data[0] == 'a');
        id = mica_value_symbol_intern(&table, text.f_data, text.f_length);
        assert(id.f_ok && id.f_number == 0);
        fail_allocation = false;
        allocation_allowance = SIZE_MAX;
        id = mica_value_symbol_intern(&table, large, 70000);
        assert(id.f_ok && id.f_number == 16);
        large[0] = 'a';
        mica_value_symbol_table_release(&table);
    }
    free(large);
}

enum { SYMBOL_THREADS = 8, SYMBOL_SHARED = 2048 };

struct SymbolThread {
    struct mica_SymbolTable *table;
    atomic_uint *ready;
    atomic_bool *start;
    unsigned thread;
    uint32_t shared[SYMBOL_SHARED];
};

static void *symbol_thread(void *argument) {
    struct SymbolThread *task = argument;
    atomic_fetch_add(task->ready, 1);
    while (!atomic_load(task->start)) sched_yield();
    for (unsigned i = 0; i < SYMBOL_SHARED; ++i) {
        char name[80];
        unsigned shared = (i * 17 + task->thread * 31) % SYMBOL_SHARED;
        int length = snprintf(name, sizeof(name), "shared-λ-%u", shared);
        struct mica_IdResult id = mica_value_symbol_intern(task->table, (const uint8_t *)name, (uint64_t)length);
        assert(id.f_ok && id.f_number <= UINT32_MAX);
        task->shared[shared] = (uint32_t)id.f_number;
        struct mica_SymbolText view = mica_value_symbol_text(task->table, (uint32_t)id.f_number);
        assert(view.f_ok && view.f_length == (uint64_t)length);
        assert(view.f_scalars == (uint64_t)length - 1 && !view.f_ascii);
        assert(memcmp(view.f_data, name, (size_t)length) == 0);
        char unique[80];
        int unique_length = snprintf(unique, sizeof(unique), "thread-%u-name-%u", task->thread, i);
        struct mica_IdResult own = mica_value_symbol_intern(task->table, (const uint8_t *)unique, (uint64_t)unique_length);
        assert(own.f_ok);
        struct mica_SymbolText own_view = mica_value_symbol_text(task->table, (uint32_t)own.f_number);
        assert(own_view.f_ok && own_view.f_ascii && own_view.f_scalars == (uint64_t)unique_length);
        assert(own_view.f_length == (uint64_t)unique_length);
        assert(memcmp(own_view.f_data, unique, (size_t)unique_length) == 0);
        // Borrowed text remains readable after unlock while other threads grow.
        assert(memcmp(view.f_data, name, (size_t)length) == 0);
        id = mica_value_symbol_intern(task->table, view.f_data, view.f_length);
        assert(id.f_ok && id.f_number == task->shared[shared]);
        if (i % 32 == 0) {
            assert(!mica_value_symbol_text(task->table, UINT32_MAX).f_ok);
            const uint8_t invalid = 0xff;
            assert(!mica_value_symbol_intern(task->table, &invalid, 1).f_ok);
        }
    }
    return NULL;
}

static void concurrent_symbol_cases(void) {
    struct mica_SymbolTable table = {0};
    assert(mica_value_symbol_table_init(&table));
    atomic_uint ready = 0;
    atomic_bool start = false;
    struct SymbolThread *tasks = calloc(SYMBOL_THREADS, sizeof(*tasks));
    assert(tasks != NULL);
    pthread_t threads[SYMBOL_THREADS];
    for (unsigned i = 0; i < SYMBOL_THREADS; ++i) {
        tasks[i].table = &table;
        tasks[i].ready = &ready;
        tasks[i].start = &start;
        tasks[i].thread = i;
        assert(pthread_create(&threads[i], NULL, symbol_thread, &tasks[i]) == 0);
    }
    while (atomic_load(&ready) != SYMBOL_THREADS) sched_yield();
    atomic_store(&start, true);
    for (unsigned i = 0; i < SYMBOL_THREADS; ++i) assert(pthread_join(threads[i], NULL) == 0);
    assert(table.f_count == SYMBOL_SHARED * (SYMBOL_THREADS + 1));
    for (unsigned name = 0; name < SYMBOL_SHARED; ++name) {
        for (unsigned i = 1; i < SYMBOL_THREADS; ++i) assert(tasks[0].shared[name] == tasks[i].shared[name]);
    }
    free(tasks);
    mica_value_symbol_table_release(&table);
    assert(table.f_mutex == NULL);
    assert(mica_value_symbol_table_init(&table));
    struct mica_IdResult first = mica_value_symbol_intern(&table, NULL, 0);
    assert(first.f_ok && first.f_number == 0);
    mica_value_symbol_table_release(&table);
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

static uint64_t mixed_offset(uint64_t scalar) {
    const uint64_t offsets[] = {0,1,3,7};
    return (scalar / 4) * 8 + offsets[scalar % 4];
}

static void string_cases(struct mica_ValueArena *arena) {
    const uint8_t pattern[] = {'a',0xc3,0xa9,0xf0,0x9f,0x98,0x80,0};
    const uint32_t runes[] = {'a',0xe9,0x1f600,0};
    const uint64_t sizes[] = {1,15,16,17,64};
    uint8_t text[512];
    for (uint64_t i=0; i<64; ++i) memcpy(text+8*i,pattern,8);
    for (size_t n=0; n<sizeof(sizes)/sizeof(sizes[0]); ++n) {
        uint64_t repeats=sizes[n], count=repeats*4;
        struct mica_ValueResult string=mica_value_string(arena,text,repeats*8);
        assert(string.f_ok);
        const struct mica_HeapString *header=mica_value_as_string(string.f_value).f_header;
        assert(mica_value_string_length(string.f_value).f_number==count);
        assert((header->f_storage->f_index!=NULL)==(repeats*8>=128));
        for(uint64_t i=0;i<=count;++i) {
            struct mica_IdResult offset=mica_value_string_byte_offset(string.f_value,i);
            assert(offset.f_ok && offset.f_number==mixed_offset(i));
            struct mica_RuneResult rune=mica_value_string_scalar_at(string.f_value,i);
            assert(rune.f_ok==(i<count));
            if(rune.f_ok) assert(rune.f_rune==runes[i%4]);
        }
        assert(!mica_value_string_byte_offset(string.f_value,count+1).f_ok);
        assert(!mica_value_string_byte_offset(string.f_value,UINT64_MAX).f_ok);
        struct mica_ValueResult slice=mica_value_string_slice(arena,string.f_value,1,count-1);
        assert(slice.f_ok);
        const struct mica_HeapString *view=mica_value_as_string(slice.f_value).f_header;
        assert(view->f_storage==header->f_storage && view->f_data==header->f_data+1);
        for(uint64_t i=0;i<=count-2;++i) {
            struct mica_IdResult offset=mica_value_string_byte_offset(slice.f_value,i);
            assert(offset.f_ok && offset.f_number==mixed_offset(i+1)-1);
        }
        struct mica_ValueResult nested=mica_value_string_slice(arena,slice.f_value,1,count-2);
        assert(nested.f_ok && mica_value_string_scalar_at(nested.f_value,0).f_rune==0x1f600);
        struct mica_ValueResult ascii=mica_value_string_slice(arena,string.f_value,0,1);
        assert(ascii.f_ok && mica_value_as_string(ascii.f_value).f_header->f_ascii);
        struct mica_ValueResult empty=mica_value_string_slice(arena,string.f_value,count,count);
        assert(empty.f_ok && mica_value_string_length(empty.f_value).f_number==0);
        assert(!mica_value_string_scalar_at(empty.f_value,0).f_ok);
        assert(!mica_value_string_slice(arena,string.f_value,2,1).f_ok);
        assert(!mica_value_string_slice(arena,string.f_value,0,count+1).f_ok);
        assert(memcmp(header->f_data,text,repeats*8)==0);
    }
    assert(!mica_value_string_length(integer(1)).f_ok);
    assert(!mica_value_string_scalar_at(integer(1),0).f_ok);
}

static void append_cases(struct mica_ValueArena *arena) {
    uint8_t ascii[129]; memset(ascii,'a',sizeof(ascii));
    struct mica_ValueResult original=mica_value_string(arena,ascii,sizeof(ascii));
    assert(original.f_ok);
    const uint8_t emoji[]={0xf0,0x9f,0x98,0x80};
    struct mica_ValueResult first=mica_value_string_append(arena,original.f_value,emoji,4);
    assert(first.f_ok);
    const struct mica_HeapString *first_header=mica_value_as_string(first.f_value).f_header;
    assert(first_header->f_storage->f_capacity>first_header->f_length);
    struct mica_ValueResult second=mica_value_string_append(arena,first.f_value,(const uint8_t *)"b",1);
    assert(second.f_ok && mica_value_as_string(second.f_value).f_header->f_storage==first_header->f_storage);
    struct mica_ValueResult branch=mica_value_string_append(arena,first.f_value,(const uint8_t *)"c",1);
    assert(branch.f_ok && mica_value_as_string(branch.f_value).f_header->f_storage!=first_header->f_storage);
    assert(mica_value_string_scalar_at(second.f_value,130).f_rune=='b');
    assert(mica_value_string_scalar_at(branch.f_value,130).f_rune=='c');
    assert(mica_value_string_length(first.f_value).f_number==130);
    assert(!mica_value_string_scalar_at(first.f_value,130).f_ok);
    assert(mica_value_string_scalar_at(first.f_value,129).f_rune==0x1f600);
    assert(mica_value_string_length(original.f_value).f_number==129);
    assert(mica_value_string_append(arena,first.f_value,NULL,0).f_value==first.f_value);
    assert(!mica_value_string_append(arena,first.f_value,NULL,1).f_ok);
    const uint8_t invalid[]={0xed,0xa0,0x80};
    uint64_t used=first_header->f_storage->f_used;
    assert(!mica_value_string_append(arena,first.f_value,invalid,sizeof(invalid)).f_ok);
    assert(first_header->f_storage->f_used==used);
    assert(!mica_value_string_append(arena,first.f_value,emoji,UINT64_MAX).f_ok);
    // Append overlapping bytes from the same backing storage.
    const struct mica_HeapString *second_header=mica_value_as_string(second.f_value).f_header;
    struct mica_ValueResult overlap=mica_value_string_append(arena,second.f_value,second_header->f_data+127,7);
    assert(overlap.f_ok && mica_value_string_length(overlap.f_value).f_number==135);
    assert(mica_value_string_scalar_at(overlap.f_value,133).f_rune==0x1f600);
    struct mica_ValueResult slice=mica_value_string_slice(arena,overlap.f_value,128,135);
    assert(slice.f_ok);
    struct mica_ValueResult slice_append=mica_value_string_append(arena,slice.f_value,emoji,4);
    assert(slice_append.f_ok && mica_value_string_length(slice_append.f_value).f_number==8);
    assert(mica_value_string_scalar_at(slice_append.f_value,7).f_rune==0x1f600);
    assert(mica_value_string_length(slice.f_value).f_number==7);
    mica_type_Value versions[100]; versions[0]=original.f_value;
    for(unsigned i=1;i<100;++i) {
        struct mica_ValueResult next=mica_value_string_append(arena,versions[i-1],emoji,4);
        assert(next.f_ok); versions[i]=next.f_value;
    }
    for(unsigned i=0;i<100;++i) {
        assert(mica_value_string_length(versions[i]).f_number==129+i);
        assert(mica_value_string_byte_offset(versions[i],129+i).f_number==129+4*i);
        for(unsigned j=0;j<i;++j) assert(mica_value_string_scalar_at(versions[i],129+j).f_rune==0x1f600);
    }
    struct mica_ValueResult joined=mica_value_string_concat(arena,first.f_value,first.f_value);
    assert(joined.f_ok && mica_value_string_length(joined.f_value).f_number==260);
    assert(!mica_value_string_concat(arena,first.f_value,integer(1)).f_ok);
    assert(!mica_value_string_concat(arena,integer(1),first.f_value).f_ok);
    // Header allocation can fail even when backing storage has spare capacity.
    struct mica_ValueArena failed={0}; fail_allocation=true;
    assert(!mica_value_string_append(&failed,versions[99],emoji,4).f_ok);
    fail_allocation=false;
    assert(failed.f_head==NULL && mica_value_string_length(versions[99]).f_number==228);
}

static void search_cases(struct mica_ValueArena *arena) {
    const uint8_t bytes[]={'a',0xc3,0xa9,0xf0,0x9f,0x98,0x80,0};
    uint8_t text[1024];
    for(unsigned i=0;i<128;i++) memcpy(text+i*8,bytes,8);
    struct mica_ValueResult hay=mica_value_string(arena,text,sizeof(text));
    assert(hay.f_ok);
    // Compare every small substring against a scalar-by-scalar search oracle.
    for(uint64_t begin=0;begin<8;begin++) for(uint64_t width=0;width<=8;width++) {
        struct mica_ValueResult needle=mica_value_string_slice(arena,hay.f_value,begin,begin+width);
        assert(needle.f_ok);
        for(uint64_t start=0;start<=514;start++) {
            uint64_t expected=start;
            for(;expected+width<=512;expected++) {
                uint64_t j=0;
                for(;j<width;j++) if(mica_value_string_scalar_at(hay.f_value,expected+j).f_rune!=
                    mica_value_string_scalar_at(needle.f_value,j).f_rune) break;
                if(j==width) break;
            }
            struct mica_IdResult found=mica_value_string_find(hay.f_value,needle.f_value,start);
            assert(found.f_ok==(expected+width<=512));
            if(found.f_ok) assert(found.f_number==expected);
        }
    }
    struct mica_ValueResult missing=mica_value_string(arena,(const uint8_t *)"aaaaab",6);
    assert(missing.f_ok && !mica_value_string_find(hay.f_value,missing.f_value,0).f_ok);
    struct mica_ValueResult ascii=mica_value_string(arena,(const uint8_t *)"aaaaaaaab",9);
    assert(ascii.f_ok && mica_value_string_find(ascii.f_value,missing.f_value,0).f_number==3);
    assert(!mica_value_string_find(hay.f_value,missing.f_value,UINT64_MAX).f_ok);
    assert(!mica_value_string_find(integer(1),missing.f_value,0).f_ok);
    assert(!mica_value_string_find(hay.f_value,integer(1),0).f_ok);
}

static void utf8_cases(struct mica_ValueArena *arena) {
    const struct { uint32_t scalar; uint8_t bytes[4]; uint64_t width; } valid[] = {
        {0, {0}, 1}, {0x7f, {0x7f}, 1}, {0x80, {0xc2, 0x80}, 2},
        {0x7ff, {0xdf, 0xbf}, 2}, {0x800, {0xe0, 0xa0, 0x80}, 3},
        {0xd7ff, {0xed, 0x9f, 0xbf}, 3}, {0xe000, {0xee, 0x80, 0x80}, 3},
        {0xffff, {0xef, 0xbf, 0xbf}, 3}, {0x10000, {0xf0, 0x90, 0x80, 0x80}, 4},
        {0x10ffff, {0xf4, 0x8f, 0xbf, 0xbf}, 4}, {0xfffd, {0xef, 0xbf, 0xbd}, 3}
    };
    for (size_t i = 0; i < sizeof(valid) / sizeof(valid[0]); ++i) {
        struct mica_Utf8Decode decoded = mica_utf8_decode(valid[i].bytes, valid[i].width);
        assert(decoded.f_ok && decoded.f_rune == valid[i].scalar && decoded.f_width == valid[i].width);
        struct mica_Utf8Encode encoded = mica_utf8_encode(valid[i].scalar);
        assert(encoded.f_ok && encoded.f_width == valid[i].width);
        assert(memcmp(encoded.f_bytes.elements, valid[i].bytes, sizeof(valid[i].bytes)) == 0);
        struct mica_Utf8Scan scan = mica_utf8_scan(valid[i].bytes, valid[i].width);
        assert(scan.f_ok && scan.f_scalars == 1 && scan.f_ascii == (valid[i].scalar < 128));
        for (uint64_t n = 0; n < valid[i].width; ++n) {
            assert(!mica_utf8_decode(valid[i].bytes, n).f_ok);
        }
    }
    const struct { uint8_t bytes[4]; uint64_t width; } invalid[] = {
        {{0x80},1}, {{0xbf},1}, {{0xc0,0x80},2}, {{0xc1,0xbf},2},
        {{0xe0,0x9f,0xbf},3}, {{0xed,0xa0,0x80},3}, {{0xed,0xbf,0xbf},3},
        {{0xf0,0x8f,0xbf,0xbf},4}, {{0xf4,0x90,0x80,0x80},4}, {{0xf5,0x80,0x80,0x80},4},
        {{0xff},1}, {{0xc2,0x7f},2}, {{0xe1,0x80,0xc0},3}, {{0xf1,0x80,0x80,0xff},4}
    };
    struct mica_ValueArenaBlock *head = arena->f_head;
    uint64_t used = head->f_used;
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        struct mica_Utf8Decode decoded = mica_utf8_decode(invalid[i].bytes, invalid[i].width);
        assert(!decoded.f_ok && decoded.f_width == 0 && decoded.f_rune == 0);
        assert(!mica_utf8_scan(invalid[i].bytes, invalid[i].width).f_ok);
        assert(!mica_value_string(arena, invalid[i].bytes, invalid[i].width).f_ok);
        assert(arena->f_head == head && head->f_used == used);
    }
    uint8_t aligned_test[34]; memset(aligned_test,'a',sizeof(aligned_test));
    for(uint64_t length=0;length<=32;length++) {
        struct mica_Utf8Scan scan=mica_utf8_scan(aligned_test+1,length);
        assert(scan.f_ok && scan.f_ascii && scan.f_scalars==length);
        for(uint64_t bad=0;bad<length;bad++) {
            aligned_test[bad+1]=0xff;
            assert(!mica_utf8_scan(aligned_test+1,length).f_ok);
            aligned_test[bad+1]='a';
        }
    }
    const uint8_t bad_tail[] = {'a',0xff};
    assert(mica_utf8_decode(bad_tail,2).f_ok && !mica_utf8_scan(bad_tail,2).f_ok);
    assert(!mica_utf8_decode(NULL,1).f_ok && !mica_utf8_decode(NULL,0).f_ok);
    assert(!mica_utf8_scan(NULL,1).f_ok);
    assert(mica_utf8_scan(NULL,0).f_ok && mica_utf8_scan(NULL,0).f_ascii);
    for (uint32_t scalar = 0; scalar <= 0x10ffff; ++scalar) {
        bool valid_scalar = scalar < 0xd800 || scalar >= 0xe000;
        struct mica_RuneResult rune = mica_unicode_scalar(scalar);
        struct mica_Utf8Encode encoded = mica_utf8_encode(scalar);
        assert(rune.f_ok == valid_scalar && encoded.f_ok == valid_scalar);
        if (!valid_scalar) continue;
        struct mica_Utf8Decode decoded = mica_utf8_decode(encoded.f_bytes.elements, encoded.f_width);
        assert(decoded.f_ok && decoded.f_rune == scalar && decoded.f_width == encoded.f_width);
    }
    assert(!mica_utf8_encode(UINT32_MAX).f_ok && !mica_unicode_scalar(0x110000).f_ok);
}

static void collection_cases(void) {
    struct mica_ValueArena arena={0};
    mica_type_Value input[]={integer(1),integer(2),integer(3)};
    struct mica_ValueResult list=mica_value_list(&arena,input,3);
    assert(list.f_ok && mica_value_list_length(list.f_value).f_number==3);
    input[0]=integer(99);
    assert(mica_value_list_get(list.f_value,0).f_value==integer(1));
    assert(!mica_value_list_get(list.f_value,3).f_ok);
    assert(!mica_value_list_get(list.f_value,UINT64_MAX).f_ok);
    assert(!mica_value_list_length(integer(0)).f_ok);
    assert(!mica_value_map_length(list.f_value).f_ok);
    assert(!mica_value_list_set(&arena,list.f_value,3,integer(9)).f_ok);
    assert(!mica_value_list_slice(&arena,list.f_value,2,1).f_ok);
    assert(!mica_value_list_slice(&arena,list.f_value,0,UINT64_MAX).f_ok);
    struct mica_ValueResult empty=mica_value_list_slice(&arena,list.f_value,3,3);
    assert(empty.f_ok && mica_value_list_length(empty.f_value).f_number==0);
    struct mica_ValueResult slice=mica_value_list_slice(&arena,list.f_value,1,3);
    assert(slice.f_ok && mica_value_list_get(slice.f_value,0).f_value==integer(2));
    struct mica_ValueResult versions[129];
    versions[0]=list;
    for(unsigned i=0;i<128;i++) {
        // Appending an earlier view as a child must not change that view or
        // introduce a logical cycle, even when the backing contains that child.
        versions[i+1]=mica_value_list_append(&arena,versions[i].f_value,versions[i].f_value);
        assert(versions[i+1].f_ok);
    }
    for(unsigned i=0;i<129;i++) {
        assert(mica_value_list_length(versions[i].f_value).f_number==3+i);
        assert(mica_value_list_get(versions[i].f_value,0).f_value==integer(1));
        if(i) assert(mica_value_list_get(versions[i].f_value,2+i).f_value==versions[i-1].f_value);
    }
    struct mica_ValueResult branch=mica_value_list_append(&arena,versions[10].f_value,integer(42));
    assert(branch.f_ok && mica_value_list_get(branch.f_value,13).f_value==integer(42));
    assert(mica_value_list_get(versions[11].f_value,13).f_value==versions[10].f_value);
    struct mica_ValueResult changed=mica_value_list_set(&arena,slice.f_value,0,branch.f_value);
    assert(changed.f_ok && mica_value_list_get(changed.f_value,0).f_value==branch.f_value);
    assert(mica_value_list_get(slice.f_value,0).f_value==integer(2));
    // A tail slice shares capacity; a non-tail slice must allocate another backing.
    struct mica_ValueResult tail=mica_value_list_slice(&arena,versions[128].f_value,2,131);
    struct mica_ValueResult tail_append=mica_value_list_append(&arena,tail.f_value,integer(55));
    assert(tail_append.f_ok && mica_value_list_get(tail_append.f_value,129).f_value==integer(55));
    assert(mica_value_list_length(versions[128].f_value).f_number==131);
    struct mica_ValueMapEntry entries[]={{integer(2),list.f_value},{integer(4),slice.f_value}};
    struct mica_ValueResult map=mica_value_map(&arena,entries,2);
    assert(map.f_ok);
    for(int key=1;key<=5;key++) {
        struct mica_ValueResult updated=mica_value_map_set(&arena,map.f_value,integer(key),changed.f_value);
        assert(updated.f_ok);
        assert(mica_value_map_length(updated.f_value).f_number==(key==2 || key==4 ? 2 : 3));
        assert(mica_value_map_get(updated.f_value,integer(key)).f_value==changed.f_value);
        assert(mica_value_map_get(map.f_value,integer(2)).f_value==list.f_value);
        assert(mica_value_map_length(map.f_value).f_number==2);
    }
    struct mica_ValueResult nested=mica_value_map_set(&arena,map.f_value,slice.f_value,branch.f_value);
    assert(nested.f_ok && mica_value_map_get(nested.f_value,slice.f_value).f_value==branch.f_value);
    // Fresh destination arenas force the allocator path. Failure must not alter
    // either the source values or the reusable tail's published used count.
    struct mica_ValueArena failed={0};
    struct mica_HeapListResult current=mica_value_as_list(tail_append.f_value);
    uint64_t used=current.f_header->f_storage->f_used;
    fail_allocation=true;
    assert(!mica_value_list(&failed,input,3).f_ok);
    assert(!mica_value_list_append(&failed,tail_append.f_value,integer(0)).f_ok);
    assert(!mica_value_list_append(&failed,list.f_value,integer(0)).f_ok);
    assert(!mica_value_list_set(&failed,list.f_value,0,integer(0)).f_ok);
    assert(!mica_value_list_slice(&failed,list.f_value,0,1).f_ok);
    assert(!mica_value_map_set(&failed,map.f_value,integer(3),integer(0)).f_ok);
    fail_allocation=false;
    assert(failed.f_head==NULL && current.f_header->f_storage->f_used==used);
    assert(mica_value_list_get(list.f_value,0).f_value==integer(1));
    assert(!mica_value_list(&failed,input,UINT64_MAX).f_ok);
    assert(!mica_value_list(&failed,NULL,1).f_ok);
    // A shorter-lived destination can borrow children from this arena. Releasing
    // the destination leaves all source views intact.
    struct mica_ValueResult borrowed=mica_value_list_set(&failed,list.f_value,0,slice.f_value);
    assert(borrowed.f_ok && mica_value_list_get(borrowed.f_value,0).f_value==slice.f_value);
    mica_value_arena_release(&failed);
    assert(mica_value_list_get(slice.f_value,0).f_value==integer(2));
    mica_value_arena_release(&arena);
}

static void relation_cases(void) {
    struct mica_ValueArena arena={0};
    struct mica_ValueTuple empty_rows[3]={{0},{0},{0}};
    struct mica_ValueResult empty=mica_value_relation(&arena,NULL,0,NULL,0);
    struct mica_ValueResult unit=mica_value_relation(&arena,NULL,0,empty_rows,3);
    assert(empty.f_ok && empty.f_value==0 && unit.f_ok && unit.f_value!=0);
    assert(!mica_value_is_unit(empty.f_value) && mica_value_is_unit(unit.f_value));
    assert(!mica_value_is_unit(integer(0)));
    assert(mica_value_relation_length(unit.f_value).f_number==1);
    assert(mica_value_relation_arity(unit.f_value).f_number==0);
    assert(mica_value_relation_row(unit.f_value,0).f_ok);
    assert(!mica_value_relation_row(unit.f_value,1).f_ok);
    assert(!mica_value_relation_row(empty.f_value,0).f_ok);
    assert(mica_value_compare(empty.f_value,unit.f_value).f_number==-1);
    assert(mica_value_compare(unit.f_value,empty.f_value).f_number==1);
    assert(!mica_value_relation_arity(integer(0)).f_ok);
    assert(!mica_value_relation_column_at(unit.f_value,0).f_ok);
    assert(!mica_value_relation_column(unit.f_value,0).f_ok);
    uint32_t heading[]={9,3,6};
    mica_type_Value cells[][3]={{integer(2),integer(1),unit.f_value},{integer(0),integer(4),empty.f_value},{integer(2),integer(1),unit.f_value}};
    struct mica_ValueTuple rows[3];
    for(unsigned i=0;i<3;i++) rows[i]=(struct mica_ValueTuple){cells[i],3};
    struct mica_ValueResult relation=mica_value_relation(&arena,heading,3,rows,3);
    assert(relation.f_ok);
    struct mica_RelationResult view=mica_value_relation_view(relation.f_value);
    assert(view.f_ok && view.f_relation.f_arity==3 && view.f_relation.f_length==2);
    assert(view.f_relation.f_heading[0]==3 && view.f_relation.f_heading[1]==6 && view.f_relation.f_heading[2]==9);
    assert(mica_value_relation_column(relation.f_value,6).f_number==1);
    assert(!mica_value_relation_column(relation.f_value,5).f_ok);
    assert(mica_value_relation_column_at(relation.f_value,2).f_number==9);
    assert(!mica_value_relation_column_at(relation.f_value,UINT64_MAX).f_ok);
    struct mica_TupleResult first=mica_value_relation_row(relation.f_value,0);
    assert(first.f_ok && first.f_tuple.f_arity==3);
    assert(mica_value_tuple_get(first.f_tuple,0).f_value==integer(1));
    assert(mica_value_tuple_get(first.f_tuple,1).f_value==unit.f_value);
    assert(mica_value_tuple_get(first.f_tuple,2).f_value==integer(2));
    assert(!mica_value_tuple_get(first.f_tuple,3).f_ok);
    assert(heading[0]==9 && cells[0][0]==integer(2));
    // The relation owns its heading, tuple descriptors, and cell array.
    heading[0]=99; cells[0][0]=integer(99); rows[0].f_arity=0;
    assert(mica_value_relation_column_at(relation.f_value,2).f_number==9);
    assert(mica_value_tuple_get(first.f_tuple,2).f_value==integer(2));
    struct mica_ValueResult rebuilt=mica_value_relation(&arena,view.f_relation.f_heading,3,view.f_relation.f_rows,2);
    assert(rebuilt.f_ok && mica_value_compare(relation.f_value,rebuilt.f_value).f_number==0);
    // Empty relations with headings are not the zero-column empty sentinel.
    struct mica_ValueResult headed=mica_value_relation(&arena,heading,3,NULL,0);
    assert(headed.f_ok && headed.f_value!=0 && mica_value_relation_length(headed.f_value).f_number==0);
    assert(!mica_value_is_unit(headed.f_value));
    uint32_t duplicates[]={3,3};
    assert(!mica_value_relation(&arena,duplicates,2,NULL,0).f_ok);
    assert(!mica_value_relation(&arena,NULL,1,NULL,0).f_ok);
    assert(!mica_value_relation(&arena,heading,3,rows,3).f_ok);
    assert(!mica_value_relation(&arena,NULL,0,NULL,1).f_ok);
    assert(!mica_value_relation(&arena,heading,65536,NULL,0).f_ok);
    assert(!mica_value_relation(&arena,heading,3,rows,UINT64_MAX).f_ok);
    struct mica_ValueTuple null_cells={NULL,1};
    assert(!mica_value_relation(&arena,heading,1,&null_cells,1).f_ok);
    // Tuple construction copies its source words, but borrows nested values.
    struct mica_TupleResult copied=mica_value_tuple(&arena,first.f_tuple.f_data,3);
    assert(copied.f_ok && mica_value_tuple_compare(copied.f_tuple,first.f_tuple).f_number==0);
    assert(mica_value_tuple(&arena,NULL,0).f_ok);
    assert(!mica_value_tuple(&arena,NULL,1).f_ok);
    assert(!mica_value_tuple(&arena,first.f_tuple.f_data,UINT64_MAX).f_ok);
    assert(mica_value_tuple_compare((struct mica_ValueTuple){NULL,0},first.f_tuple).f_number==-1);
    // Exercise the accepted width boundary as well as the rejected 65,536 case.
    uint32_t *wide_heading=calloc(UINT16_MAX,sizeof(*wide_heading));
    assert(wide_heading);
    for(uint32_t i=0;i<UINT16_MAX;i++) wide_heading[i]=UINT16_MAX-i;
    struct mica_ValueResult wide=mica_value_relation(&arena,wide_heading,UINT16_MAX,NULL,0);
    assert(wide.f_ok && mica_value_relation_arity(wide.f_value).f_number==UINT16_MAX);
    assert(mica_value_relation_column_at(wide.f_value,0).f_number==1);
    assert(mica_value_relation_column_at(wide.f_value,UINT16_MAX-1).f_number==UINT16_MAX);
    free(wide_heading);
    mica_type_Value *wide_cells=calloc(65536,sizeof(*wide_cells));
    assert(wide_cells);
    struct mica_TupleResult wide_tuple=mica_value_tuple(&arena,wide_cells,65536);
    assert(wide_tuple.f_ok && wide_tuple.f_tuple.f_arity==65536);
    assert(mica_value_tuple_get(wide_tuple.f_tuple,65535).f_value==0);
    free(wide_cells);
    // Force failures at each allocation of a large reverse-ordered relation.
    // No failed attempt may modify the input or publish a partial relation.
    const uint64_t count=8192;
    mica_type_Value *large_cells=calloc(count*3,sizeof(*large_cells));
    struct mica_ValueTuple *large_rows=calloc(count,sizeof(*large_rows));
    assert(large_cells && large_rows);
    uint32_t large_heading[]={3,2,1};
    for(uint64_t i=0;i<count;i++) {
        large_cells[i*3]=integer((int64_t)i);
        large_cells[i*3+1]=integer(0);
        large_cells[i*3+2]=integer((int64_t)(count-i));
        large_rows[i]=(struct mica_ValueTuple){large_cells+i*3,3};
    }
    bool succeeded=false;
    for(size_t allowance=0;allowance<12;allowance++) {
        struct mica_ValueArena destination={0};
        allocation_allowance=allowance;
        struct mica_ValueResult attempt=mica_value_relation(&destination,large_heading,3,large_rows,count);
        allocation_allowance=SIZE_MAX;
        assert(large_heading[0]==3 && large_cells[2]==integer((int64_t)count));
        if(attempt.f_ok) {
            assert(mica_value_relation_length(attempt.f_value).f_number==count);
            struct mica_TupleResult row=mica_value_relation_row(attempt.f_value,0);
            assert(mica_value_tuple_get(row.f_tuple,0).f_value==integer(1));
            succeeded=true;
        }
        mica_value_arena_release(&destination);
        if(succeeded) break;
    }
    assert(succeeded);
    free(large_rows); free(large_cells);
    struct mica_ValueArena destination={0};
    fail_allocation=true;
    assert(!mica_value_tuple(&destination,first.f_tuple.f_data,3).f_ok);
    assert(!mica_value_relation(&destination,NULL,0,empty_rows,1).f_ok);
    fail_allocation=false;
    struct mica_ValueResult borrowed=mica_value_relation(&destination,view.f_relation.f_heading,3,view.f_relation.f_rows,2);
    assert(borrowed.f_ok && mica_value_compare(borrowed.f_value,relation.f_value).f_number==0);
    mica_value_arena_release(&destination);
    assert(mica_value_tuple_get(first.f_tuple,1).f_value==unit.f_value);
    mica_value_arena_release(&arena);
}

static void traversal_cases(void) {
    struct mica_ValueArena source={0}, destination={0};
    mica_type_Value roots[20]={0};
    roots[1]=mica_value_bool(true);
    roots[2]=integer(-1234567);
    roots[3]=floating(-0.25f);
    roots[4]=mica_value_identity(123).f_value;
    roots[5]=mica_value_symbol(42).f_value;
    roots[6]=mica_value_error_code(7).f_value;
    // Large strings exercise copying of bytes and scalar indexes across chunks.
    uint8_t *text=malloc(70000); assert(text);
    for(size_t i=0;i<70000;i+=2) { text[i]=0xc3; text[i+1]=0xa9; }
    roots[7]=mica_value_string(&source,text,70000).f_value;
    roots[7]=mica_value_string_slice(&source,roots[7],10,35000).f_value;
    roots[8]=mica_value_bytes(&source,text,70000).f_value;
    free(text);
    roots[9]=mica_value_list(&source,roots,9).f_value;
    roots[9]=mica_value_list_append(&source,roots[9],roots[9]).f_value;
    struct mica_ValueMapEntry entries[]={{roots[7],roots[9]},{integer(1),roots[8]}};
    roots[10]=mica_value_map(&source,entries,2).f_value;
    roots[11]=mica_value_range(&source,roots[9],true,roots[10]).f_value;
    roots[12]=mica_value_error(&source,7,true,roots[7],true,roots[11]).f_value;
    roots[13]=mica_value_capability(9).f_value;
    roots[14]=mica_value_frob(&source,123,roots[12]).f_value;
    roots[15]=mica_value_function(9).f_value;
    uint32_t heading[]={3,1};
    mica_type_Value row_cells[]={roots[12],roots[10]};
    struct mica_ValueTuple row={row_cells,2};
    roots[16]=mica_value_relation(&source,heading,2,&row,1).f_value;
    struct mica_ValueTuple empty={0};
    roots[17]=mica_value_relation(&source,NULL,0,&empty,1).f_value;
    // Optional storage can contain a value that is semantically absent. Copies
    // must ignore it and clear the destination field, including error messages.
    roots[18]=mica_value_range(&source,integer(0),false,roots[9]).f_value;
    roots[19]=mica_value_error(&source,7,false,roots[7],false,roots[9]).f_value;
    mica_type_Value absent_range=mica_value_range(&source,integer(0),false,integer(123)).f_value;
    mica_type_Value absent_error=mica_value_error(&source,7,false,0,false,integer(123)).f_value;
    assert(mica_value_hash(roots[18]).f_number==mica_value_hash(absent_range).f_number);
    assert(mica_value_hash(roots[19]).f_number==mica_value_hash(absent_error).f_number);
    struct mica_ValueResult aggregate=mica_value_list(&source,roots,20);
    assert(aggregate.f_ok);
    struct mica_IdResult expected=mica_value_hash(aggregate.f_value);
    assert(expected.f_ok);
    struct mica_ValueTuple source_tuple={roots,20};
    struct mica_IdResult expected_tuple=mica_value_tuple_hash(source_tuple);
    assert(expected_tuple.f_ok);
    struct mica_TupleResult copied_tuple=mica_value_tuple_copy(&destination,source_tuple);
    assert(copied_tuple.f_ok);
    struct mica_ValueResult copied=mica_value_copy(&destination,aggregate.f_value);
    assert(copied.f_ok && copied.f_value!=aggregate.f_value);
    assert(mica_value_compare(copied.f_value,aggregate.f_value).f_number==0);
    // Same-arena copies must also retain earlier values when allocation grows.
    struct mica_ValueResult same_arena=mica_value_copy(&source,roots[16]);
    assert(same_arena.f_ok && mica_value_compare(same_arena.f_value,roots[16]).f_number==0);
    // Every allocator failure must return an unpublished failure. Releasing a
    // partially populated destination must reclaim every recursive allocation.
    bool succeeded=false;
    for(size_t allowance=0;allowance<128;allowance++) {
        struct mica_ValueArena failed={0};
        allocation_allowance=allowance;
        struct mica_ValueResult attempt=mica_value_copy(&failed,roots[16]);
        allocation_allowance=SIZE_MAX;
        assert(mica_value_hash(aggregate.f_value).f_number==expected.f_number);
        if(attempt.f_ok) {
            assert(mica_value_hash(attempt.f_value).f_number==mica_value_hash(roots[16]).f_number);
            succeeded=true;
        } else assert(attempt.f_value==0);
        mica_value_arena_release(&failed);
        if(succeeded) break;
    }
    assert(succeeded);
    mica_value_arena_release(&source);
    assert(mica_value_hash(copied.f_value).f_number==expected.f_number);
    assert(mica_value_tuple_hash(copied_tuple.f_tuple).f_number==expected_tuple.f_number);
    struct mica_ValueResult string=mica_value_list_get(copied.f_value,7);
    assert(string.f_ok && mica_value_string_length(string.f_value).f_number==34990);
    assert(mica_value_string_scalar_at(string.f_value,34989).f_rune==0xe9);
    struct mica_ValueResult range=mica_value_list_get(copied.f_value,18);
    assert(mica_value_as_range(range.f_value).f_header->f_end==0);
    struct mica_ValueResult error=mica_value_list_get(copied.f_value,19);
    assert(mica_value_as_error(error.f_value).f_header->f_message==0);
    assert(mica_value_as_error(error.f_value).f_header->f_value==0);
    // The copied list has its own append storage and accepts another view.
    struct mica_ValueResult appended=mica_value_list_append(&destination,copied.f_value,integer(99));
    assert(appended.f_ok && mica_value_list_length(copied.f_value).f_number==20);
    assert(mica_value_list_get(appended.f_value,20).f_value==integer(99));
    assert(!mica_value_hash(UINT64_C(255)<<56).f_ok);
    assert(!mica_value_copy(&destination,UINT64_C(255)<<56).f_ok);
    mica_value_arena_release(&destination);
    // Deep nesting traverses child values rather than pointer bits.
    struct mica_ValueArena deep_source={0},deep_destination={0};
    mica_type_Value deep=integer(17);
    for(unsigned i=0;i<256;i++) deep=mica_value_frob(&deep_source,1,deep).f_value;
    uint64_t deep_hash=mica_value_hash(deep).f_number;
    copied=mica_value_copy(&deep_destination,deep); assert(copied.f_ok);
    mica_value_arena_release(&deep_source);
    assert(mica_value_hash(copied.f_value).f_number==deep_hash);
    mica_value_arena_release(&deep_destination);
}

static void codec_cases(void) {
    struct mica_ValueArena source={0},encoded={0},decoded={0};
    struct mica_ValueCodecOptions ids={true,false},names={0},caps={true,true};
    uint8_t *text=malloc(20000); assert(text);
    for(size_t i=0;i<20000;i+=2) { text[i]=0xc3; text[i+1]=0xa9; }
    mica_type_Value string=mica_value_string(&source,text,20000).f_value;
    free(text);
    mica_type_Value error=mica_value_error(&source,7,true,string,true,integer(-3)).f_value;
    mica_type_Value roots[]={string,error,0,mica_value_range(&source,error,false,0).f_value};
    mica_type_Value list=mica_value_list(&source,roots,4).f_value;
    uint32_t heading[]={9,3};
    mica_type_Value cells[]={list,error};
    struct mica_ValueTuple row={cells,2};
    mica_type_Value relation=mica_value_relation(&source,heading,2,&row,1).f_value;
    uint64_t hash=mica_value_hash(relation).f_number;
    assert(mica_value_is_persistable(relation));
    struct mica_ValueResult wire=mica_value_encode(&encoded,NULL,relation,ids);
    assert(wire.f_ok);
    const struct mica_HeapBytes *bytes=mica_value_as_bytes(wire.f_value).f_header;
    struct mica_ValueDecodeResult prefix=mica_value_decode(&decoded,NULL,bytes->f_data,bytes->f_length,ids);
    assert(prefix.f_ok && prefix.f_consumed==bytes->f_length);
    assert(compare(prefix.f_value,relation)==0);
    // A stream decoder consumes exactly one record; the exact decoder rejects suffixes.
    uint8_t stream[16]={0};
    struct mica_ValueDecodeResult first=mica_value_decode(&decoded,NULL,stream,sizeof(stream),ids);
    assert(first.f_ok && first.f_value==0 && first.f_consumed==8);
    assert(!mica_value_decode_exact(&decoded,NULL,stream,sizeof(stream),ids).f_ok);
    assert(!mica_value_decode_exact(&decoded,NULL,NULL,1,ids).f_ok);
    assert(!mica_value_decode_exact(&decoded,NULL,NULL,0,ids).f_ok);
    // Each foreign allocation failure can leave arena scratch, but publishes no value.
    for(unsigned operation=0;operation<2;operation++) {
        bool succeeded=false;
        for(size_t allowance=0;allowance<128;allowance++) {
            struct mica_ValueArena attempt_arena={0};
            allocation_allowance=allowance;
            struct mica_ValueResult attempt=operation==0
                ? mica_value_encode(&attempt_arena,NULL,relation,ids)
                : mica_value_decode_exact(&attempt_arena,NULL,bytes->f_data,bytes->f_length,ids);
            allocation_allowance=SIZE_MAX;
            if(attempt.f_ok) {
                if(operation==0) {
                    const struct mica_HeapBytes *actual=mica_value_as_bytes(attempt.f_value).f_header;
                    assert(actual->f_length==bytes->f_length);
                    assert(memcmp(actual->f_data,bytes->f_data,(size_t)bytes->f_length)==0);
                } else assert(compare(attempt.f_value,relation)==0);
                succeeded=true;
            } else assert(attempt.f_value==0);
            assert(mica_value_hash(relation).f_number==hash);
            mica_value_arena_release(&attempt_arena);
            if(succeeded) break;
        }
        assert(succeeded);
    }
    mica_value_arena_release(&source);
    mica_value_arena_release(&encoded);
    assert(mica_value_hash(prefix.f_value).f_number==hash);
    mica_value_arena_release(&decoded);

    // Source and destination symbol tables have unrelated IDs. Encoded names
    // and decoded heap storage must outlive the source bytes and symbol table.
    struct mica_SymbolTable source_symbols={0},target_symbols={0};
    assert(mica_value_symbol_table_init(&source_symbols));
    assert(mica_value_symbol_table_init(&target_symbols));
    const uint8_t spelling[]={0xc3,0xa9,0,'x'};
    struct mica_IdResult id=mica_value_symbol_intern(&source_symbols,spelling,sizeof(spelling));
    assert(id.f_ok);
    assert(mica_value_symbol_intern(&target_symbols,(const uint8_t *)"other",5).f_ok);
    mica_type_Value symbol=mica_value_symbol((uint32_t)id.f_number).f_value;
    assert(mica_value_is_persistable(symbol));
    assert(!mica_value_encode(&encoded,NULL,symbol,names).f_ok);
    assert(!mica_value_encode(&encoded,&source_symbols,mica_value_symbol(999).f_value,names).f_ok);
    wire=mica_value_encode(&encoded,&source_symbols,symbol,names); assert(wire.f_ok);
    mica_value_symbol_table_release(&source_symbols);
    bytes=mica_value_as_bytes(wire.f_value).f_header;
    assert(!mica_value_decode_exact(&decoded,NULL,bytes->f_data,bytes->f_length,names).f_ok);
    assert(!mica_value_decode_exact(&decoded,&target_symbols,bytes->f_data,bytes->f_length,ids).f_ok);
    struct mica_ValueResult named=mica_value_decode_exact(&decoded,&target_symbols,bytes->f_data,bytes->f_length,names);
    assert(named.f_ok);
    uint32_t target_id=(uint32_t)mica_value_as_symbol(named.f_value).f_number;
    assert(target_id!=id.f_number);
    mica_value_arena_release(&encoded);
    struct mica_SymbolText name=mica_value_symbol_text(&target_symbols,target_id);
    assert(name.f_ok && name.f_length==sizeof(spelling));
    assert(memcmp(name.f_data,spelling,sizeof(spelling))==0);
    mica_value_symbol_table_release(&target_symbols);
    mica_value_arena_release(&decoded);

    // Restrictions recurse, but absent optional fields do not count as children.
    mica_type_Value cap=mica_value_capability(1).f_value;
    mica_type_Value function=mica_value_function(0).f_value;
    assert(!mica_value_is_persistable(cap) && !mica_value_is_persistable(function));
    mica_type_Value absent=mica_value_range(&source,integer(1),false,cap).f_value;
    assert(mica_value_is_persistable(absent));
    assert(mica_value_encode(&encoded,NULL,absent,ids).f_ok);
    mica_type_Value nested=mica_value_list(&source,&cap,1).f_value;
    assert(!mica_value_is_persistable(nested));
    assert(!mica_value_encode(&encoded,NULL,nested,ids).f_ok);
    wire=mica_value_encode(&encoded,NULL,nested,caps); assert(wire.f_ok);
    bytes=mica_value_as_bytes(wire.f_value).f_header;
    assert(!mica_value_decode_exact(&decoded,NULL,bytes->f_data,bytes->f_length,ids).f_ok);
    assert(mica_value_decode_exact(&decoded,NULL,bytes->f_data,bytes->f_length,caps).f_ok);
    assert(!mica_value_encode(&encoded,NULL,function,caps).f_ok);
    // Reject lengths before accessing an invalid span or allocating count-sized storage.
    uint8_t invalid[16]={0};
    memset(invalid,255,8); invalid[6]=7;
    assert(!mica_value_decode_exact(&decoded,NULL,invalid,8,ids).f_ok);
    memset(invalid,0,16); invalid[7]=255; invalid[6]=16; invalid[8]=2;
    assert(!mica_value_decode_exact(&decoded,NULL,invalid,16,ids).f_ok);
    mica_value_arena_release(&source);
    mica_value_arena_release(&encoded);
    mica_value_arena_release(&decoded);
}

int main(void) {
    codec_cases();
    traversal_cases();
    relation_cases();
    collection_cases();
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
    assert(mica_value_checked_rem(floating(0x1p120f), floating(0x1p-149f)).f_value == floating(0));
    assert(mica_value_checked_rem(floating(-4), floating(2)).f_value == floating(0));
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
    assert(extracted.f_header->f_scalars == 3 && !extracted.f_header->f_ascii);
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
    utf8_cases(&arena);
    string_cases(&arena);
    append_cases(&arena);
    search_cases(&arena);
    comparison_cases(&arena);
    map_cases(&arena);
    symbol_cases();
    concurrent_symbol_cases();
    mica_value_arena_release(&arena);
    assert(arena.f_head == NULL);
    mica_value_arena_release(&arena);
    assert(allocations == releases);
    return 0;
}
