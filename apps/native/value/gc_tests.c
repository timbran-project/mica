// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>
#include <string.h>

static mica_type_Value checked(struct mica_ValueResult result) {
    assert(result.f_ok);
    return result.f_value;
}

static mica_type_Value integer(int64_t value) { return checked(mica_value_int(value)); }

static void collect(struct mica_MemoryWorker *worker) {
    assert(mica_memory_safepoint(worker, true));
    memset(worker->f_nursery, 0xa5, worker->f_capacity);
}

static void value_graph(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    assert(mica_value_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 65536));
    assert(mica_memory_enter(&worker));
    mica_type_Value immediate[] = {0, mica_value_bool(true), integer(-17),
        checked(mica_value_float(1.5f)), checked(mica_value_identity(32)),
        checked(mica_value_symbol(19)), checked(mica_value_error_code(3)),
        checked(mica_value_capability(71)), checked(mica_value_function(22))};
    struct mica_MemoryRoot root = {0};
    mica_memory_root_push(&worker, &root);
    for (size_t i = 0; i < sizeof(immediate) / sizeof(*immediate); ++i) {
        mica_value_root_set(&root, immediate[i]);
        collect(&worker);
        assert(mica_value_root_get(&root) == immediate[i]);
    }
    uint8_t utf8[512];
    for (unsigned i = 0; i < sizeof(utf8); i += 2) { utf8[i] = 0xc3; utf8[i+1] = 0xa9; }
    mica_type_Value text = checked(mica_value_string(&worker, utf8, sizeof(utf8)));
    mica_type_Value slice = checked(mica_value_string_slice(&worker, text, 19, 211));
    mica_type_Value appended = checked(mica_value_string_append(&worker, slice, utf8, 4));
    mica_type_Value list_values[] = {text, slice, appended, integer(7)};
    mica_type_Value list = checked(mica_value_list(&worker, list_values, 4));
    mica_type_Value list_view = checked(mica_value_list_slice(&worker, list, 1, 3));
    mica_type_Value list_append = checked(mica_value_list_append(&worker, list_view, text));
    struct mica_ValueMapEntry entries[] = {
        {integer(3), list}, {integer(1), list_view}, {integer(3), list_append}, {integer(2), text}};
    mica_type_Value map = checked(mica_value_map(&worker, entries, 4));
    struct mica_ValueTuple tuples[] = {{.f_data = list_values, .f_arity = 2},
        {.f_data = list_values + 2, .f_arity = 2}};
    uint32_t heading[] = {9, 3};
    mica_type_Value relation = checked(mica_value_relation(&worker, heading, 2, tuples, 2));
    mica_type_Value error = checked(mica_value_error(&worker, 17, true, slice, true, map));
    mica_type_Value frob = checked(mica_value_frob(&worker, 42, relation));
    mica_type_Value range = checked(mica_value_range(&worker, integer(-1), true, integer(9)));
    mica_type_Value values[] = {text, slice, appended, list, list_view, list_append, map,
        relation, error, frob, range, checked(mica_value_copy(&worker, relation)),
        checked(mica_value_bytes(&worker, utf8, 512)), checked(mica_value_bytes(&worker, NULL, 0)),
        checked(mica_value_string(&worker, NULL, 0))};
    mica_type_Value graph = checked(mica_value_list(&worker, values, sizeof(values) / sizeof(*values)));
    struct mica_IdResult hash = mica_value_hash(graph);
    assert(hash.f_ok);
    struct mica_ValueCodecOptions options = {.f_symbol_ids = true, .f_allow_capabilities = true};
    mica_type_Value wire = checked(mica_value_encode(&worker, NULL, graph, options));
    const struct mica_HeapBytes *encoded = mica_value_as_bytes(wire).f_header;
    uint64_t length = encoded->f_length;
    uint8_t *expected = malloc(length);
    assert(expected);
    memcpy(expected, encoded->f_data, length);
    struct mica_MemoryRoot wire_root = {0};
    mica_memory_root_push(&worker, &wire_root);
    mica_value_root_set(&root, graph);
    mica_value_root_set(&wire_root, wire);
    collect(&worker);
    graph = mica_value_root_get(&root);
    wire = mica_value_root_get(&wire_root);
    assert(mica_value_hash(graph).f_number == hash.f_number);
    encoded = mica_value_as_bytes(wire).f_header;
    assert(encoded->f_length == length && memcmp(encoded->f_data, expected, length) == 0);
    for (unsigned round = 0; round < 3; ++round) {
        mica_type_Value actual = checked(mica_value_encode(&worker, NULL, graph, options));
        encoded = mica_value_as_bytes(actual).f_header;
        assert(encoded->f_length == length && memcmp(encoded->f_data, expected, length) == 0);
        collect(&worker);
        assert(mica_value_root_get(&root) == graph);
    }
    free(expected);
    const struct mica_HeapList *outer = mica_value_as_list(graph).f_header;
    text = outer->f_data[0]; list = outer->f_data[3];
    const struct mica_HeapString *before = mica_value_as_string(text).f_header;
    mica_type_Value grown = checked(mica_value_string_append(&worker, text, utf8, 2));
    assert(mica_value_as_string(grown).f_header->f_storage != before->f_storage);
    mica_type_Value extended = checked(mica_value_list_append(&worker, list, text));
    assert(mica_value_as_list(extended).f_header->f_storage != mica_value_as_list(list).f_header->f_storage);
    assert(mica_memory_root_pop(&worker, &wire_root));
    assert(mica_memory_root_pop(&worker, &root));
    collect(&worker);
    assert(heap.f_retained == 0 && heap.f_allocated == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void tuple_and_buffer(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    assert(mica_value_heap_init(&heap));
    assert(mica_memory_worker_init(&worker, &heap, 1024));
    assert(mica_memory_enter(&worker));
    mica_type_Value items[] = {checked(mica_value_string(&worker, (const uint8_t *)"tuple", 5)), integer(3)};
    struct mica_TupleResult tuple = mica_value_tuple(&worker, items, 2);
    assert(tuple.f_ok);
    struct mica_MemoryRoot root = {.f_pointer = (void *)tuple.f_tuple.f_data};
    mica_memory_root_push(&worker, &root);
    struct mica_ValueBuffer *buffer = mica_value_buffer(&worker);
    assert(buffer && mica_buffer_append(&worker, buffer, (const uint8_t *)"abc", 3));
    struct mica_MemoryRoot buffer_root = {.f_pointer = (void *)buffer};
    mica_memory_root_push(&worker, &buffer_root);
    collect(&worker);
    tuple.f_tuple.f_data = (void *)root.f_pointer;
    assert(mica_value_string_length(mica_value_tuple_get(tuple.f_tuple, 0).f_value).f_number == 5);
    buffer = (void *)buffer_root.f_pointer;
    uint8_t suffix[400]; memset(suffix, 'x', sizeof(suffix));
    assert(mica_buffer_append(&worker, buffer, suffix, sizeof(suffix)));
    collect(&worker);
    buffer = (void *)buffer_root.f_pointer;
    assert(buffer->f_length == 403 && memcmp(buffer->f_data, "abc", 3) == 0);
    assert(buffer->f_data[402] == 'x');
    assert(mica_memory_root_pop(&worker, &buffer_root));
    assert(mica_memory_root_pop(&worker, &root));
    collect(&worker);
    assert(heap.f_allocated == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

static void overflow_append(void) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker worker = {0};
    assert(mica_value_heap_init(&heap));
    // No object fits: exercise private mature storage without a nursery fast path.
    assert(mica_memory_worker_init(&worker, &heap, 64));
    assert(mica_memory_enter(&worker));
    uint8_t utf8[512];
    for (unsigned i = 0; i < sizeof(utf8); i += 2) { utf8[i] = 0xc3; utf8[i + 1] = 0xa9; }
    mica_type_Value text = checked(mica_value_string(&worker, utf8, sizeof(utf8)));
    text = checked(mica_value_string_append(&worker, text, utf8, 2));
    mica_type_Value first_text = text;
    const struct mica_StringStorage *text_storage = mica_value_as_string(text).f_header->f_storage;
    mica_type_Value items[128];
    for (unsigned i = 0; i < 128; ++i) items[i] = integer(i);
    mica_type_Value list = checked(mica_value_list(&worker, items, 128));
    list = checked(mica_value_list_append(&worker, list, integer(128)));
    mica_type_Value first_list = list;
    const struct mica_ListStorage *list_storage = mica_value_as_list(list).f_header->f_storage;
    for (unsigned i = 0; i < 64; ++i) {
        text = checked(mica_value_string_append(&worker, text, utf8, 2));
        list = checked(mica_value_list_append(&worker, list, integer(129 + i)));
        assert(mica_value_as_string(text).f_header->f_storage == text_storage);
        assert(mica_value_as_list(list).f_header->f_storage == list_storage);
    }
    assert(mica_value_string_length(first_text).f_number == 257);
    assert(mica_value_list_length(first_list).f_number == 129);
    assert(mica_value_string_length(text).f_number == 321);
    assert(mica_value_list_get(list, 192).f_value == integer(192));
    mica_type_Value values[] = {first_text, text, first_list, list};
    mica_type_Value graph = checked(mica_value_list(&worker, values, 4));
    uint64_t hash = mica_value_hash(graph).f_number;
    struct mica_MemoryRoot root = {0};
    assert(mica_memory_root_push(&worker, &root));
    mica_value_root_set(&root, graph);
    collect(&worker);
    graph = mica_value_root_get(&root);
    assert(mica_value_hash(graph).f_number == hash);
    text = checked(mica_value_list_get(graph, 1));
    list = checked(mica_value_list_get(graph, 3));
    mica_type_Value grown_text = checked(mica_value_string_append(&worker, text, utf8, 2));
    mica_type_Value grown_list = checked(mica_value_list_append(&worker, list, integer(193)));
    // Collection freezes both spill allocations and promoted nursery allocations.
    assert(mica_value_as_string(grown_text).f_header->f_storage != mica_value_as_string(text).f_header->f_storage);
    assert(mica_value_as_list(grown_list).f_header->f_storage != mica_value_as_list(list).f_header->f_storage);
    assert(mica_memory_root_pop(&worker, &root));
    collect(&worker);
    assert(heap.f_retained == 0 && heap.f_allocated == 0);
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    assert(mica_memory_heap_release(&heap));
}

struct publication_test {
    struct mica_MemoryHeap *heap;
    struct mica_MemoryRoot *shared;
    uint64_t hash;
    unsigned rounds;
};

static void *consume_publication(void *argument) {
    struct publication_test *test = argument;
    struct mica_MemoryWorker worker = {0};
    assert(mica_memory_worker_init(&worker, test->heap, 2048));
    assert(mica_memory_enter(&worker));
    struct mica_MemoryRoot input = {0}, output = {0};
    assert(mica_memory_acquire(&worker, test->shared, &input));
    assert(mica_memory_root_push(&worker, &output));
    for (unsigned i = 0; i < test->rounds; ++i) {
        mica_type_Value text = checked(mica_value_list_get(mica_value_root_get(&input), 1));
        mica_type_Value grown = checked(mica_value_string_append(&worker, text, (const uint8_t *)"x", 1));
        assert(mica_value_as_string(grown).f_header->f_storage != mica_value_as_string(text).f_header->f_storage);
        mica_type_Value value = checked(mica_value_list_append(&worker, mica_value_root_get(&input), integer(i)));
        assert(mica_value_as_list(value).f_header->f_storage != mica_value_as_list(mica_value_root_get(&input)).f_header->f_storage);
        mica_value_root_set(&output, value);
        assert(mica_memory_safepoint(&worker, i % 19 == 0));
        assert(mica_value_hash(mica_value_root_get(&input)).f_number == test->hash);
        value = mica_value_root_get(&output);
        assert(mica_value_list_get(value, 3).f_value == integer(i));
    }
    assert(mica_memory_root_pop(&worker, &output));
    assert(mica_memory_root_pop(&worker, &input));
    assert(mica_memory_leave(&worker));
    assert(mica_memory_worker_release(&worker));
    return NULL;
}

static void published_values(uint64_t nursery_capacity) {
    struct mica_MemoryHeap heap = {0};
    struct mica_MemoryWorker producer = {0};
    assert(mica_value_heap_init(&heap));
    assert(mica_memory_worker_init(&producer, &heap, nursery_capacity));
    assert(mica_memory_enter(&producer));
    mica_type_Value values[] = {integer(7), checked(mica_value_string(&producer, (const uint8_t *)"shared", 6))};
    // Leave spare capacity so publication, rather than a full backing, prevents reuse.
    values[1] = checked(mica_value_string_append(&producer, values[1], (const uint8_t *)"!", 1));
    const struct mica_StringStorage *text_storage = mica_value_as_string(values[1]).f_header->f_storage;
    assert(text_storage->f_used < text_storage->f_capacity);
    mica_type_Value value = checked(mica_value_list(&producer, values, 2));
    value = checked(mica_value_list_append(&producer, value, integer(8)));
    const struct mica_ListStorage *list_storage = mica_value_as_list(value).f_header->f_storage;
    assert(list_storage->f_used < list_storage->f_capacity);
    uint64_t hash = mica_value_hash(value).f_number;
    struct mica_MemoryRoot source = {0}, shared = {0};
    assert(mica_memory_root_push(&producer, &source));
    assert(!mica_memory_root_push(&producer, &source));
    mica_value_root_set(&source, value);
    assert(mica_memory_publish(&producer, &source, &shared));
    assert(!mica_memory_publish(&producer, &source, &shared));
    assert(mica_memory_root_pop(&producer, &source));
    assert(mica_memory_leave(&producer));
    assert(mica_memory_worker_release(&producer));
    assert(!mica_memory_heap_release(&heap));
    pthread_t threads[4];
    struct publication_test tests[4];
    for (unsigned i = 0; i < 4; ++i) {
        tests[i] = (struct publication_test){.heap = &heap, .shared = &shared, .hash = hash, .rounds = 50 + i * 70};
        assert(!pthread_create(&threads[i], NULL, consume_publication, &tests[i]));
    }
    for (unsigned i = 0; i < 4; ++i) assert(!pthread_join(threads[i], NULL));
    struct mica_MemoryWorker cleanup = {0};
    assert(mica_memory_worker_init(&cleanup, &heap, 1024));
    assert(mica_memory_enter(&cleanup));
    struct mica_MemoryRoot retained = {0};
    assert(mica_memory_acquire(&cleanup, &shared, &retained));
    assert(mica_memory_unpublish(&cleanup, &shared));
    assert(!mica_memory_unpublish(&cleanup, &shared));
    collect(&cleanup);
    assert(mica_value_hash(mica_value_root_get(&retained)).f_number == hash);
    assert(mica_memory_root_pop(&cleanup, &retained));
    collect(&cleanup);
    assert(heap.f_allocated == 0 && heap.f_retained == 0);
    assert(mica_memory_leave(&cleanup));
    assert(mica_memory_worker_release(&cleanup));
    assert(mica_memory_heap_release(&heap));
}

int main(void) {
    value_graph();
    tuple_and_buffer();
    overflow_append();
    published_values(4096);
    published_values(64);
    return 0;
}
