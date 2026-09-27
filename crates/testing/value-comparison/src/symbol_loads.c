// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <pthread.h>
#include <limits.h>

struct LoadName {
    uint8_t *bytes;
    uint64_t length, scalars;
    bool valid, ascii;
};
struct LoadOperation { unsigned kind; uint64_t name; };
struct LoadWorker {
    struct mica_SymbolTable *table;
    const struct LoadName *names;
    const struct mica_IdResult *known;
    struct LoadOperation *operations;
    struct mica_IdResult *results;
    uint64_t count, preintern, rounds, digest;
    pthread_barrier_t *ready, *start, *finish;
    bool valid, verify;
};

static void load_barrier(pthread_barrier_t *barrier) {
    int result = pthread_barrier_wait(barrier);
    if (result != 0 && result != PTHREAD_BARRIER_SERIAL_THREAD) fail("symbol load barrier failed");
}

static bool load_text_matches(struct mica_SymbolText text, const struct LoadName *name) {
    return text.f_ok && text.f_length == name->length && text.f_scalars == name->scalars
        && text.f_ascii == name->ascii && (name->length == 0 || memcmp(text.f_data, name->bytes, (size_t)name->length) == 0);
}

static void *load_worker(void *argument) {
    struct LoadWorker *worker = argument;
    for (uint64_t i = 0; i < worker->count; ++i) {
        struct LoadOperation op = worker->operations[i];
        if (op.name < worker->preintern) {
            const struct LoadName *name = &worker->names[op.name];
            if (op.kind == 0) (void)mica_value_symbol_intern(worker->table, name->bytes, name->length);
            else (void)mica_value_symbol_text(worker->table, (uint32_t)worker->known[op.name].f_number);
        }
    }
    bool valid = true;
    uint64_t digest = 0;
    load_barrier(worker->ready); load_barrier(worker->start);
    for (uint64_t round = 0; round < worker->rounds; ++round) {
        for (uint64_t i = 0; i < worker->count; ++i) {
            struct LoadOperation op = worker->operations[i];
            const struct LoadName *name = &worker->names[op.name];
            struct mica_IdResult id;
            struct mica_SymbolText text = {0};
            uint64_t observed;
            if (op.kind == 0) {
                id = mica_value_symbol_intern(worker->table, name->bytes, name->length);
                observed = id.f_ok;
                __asm__ volatile("" : "+r"(id.f_number) : : "memory");
            } else {
                id = worker->known[op.name];
                text = mica_value_symbol_text(worker->table, (uint32_t)id.f_number);
                observed = text.f_ok ? text.f_length : 0;
                if (!text.f_ok) valid = false;
                if (op.kind == 1 && text.f_ok && text.f_length != 0)
                    observed += text.f_data[0] + text.f_data[text.f_length - 1];
                if (op.kind == 2 && text.f_ok) observed += text.f_scalars + text.f_ascii;
            }
            if (round != 0 && (worker->results[i].f_ok != id.f_ok || (id.f_ok && worker->results[i].f_number != id.f_number)))
                valid = false;
            worker->results[i] = id;
            if (worker->verify) {
                if (id.f_ok != name->valid) valid = false;
                if (id.f_ok) {
                    if (op.kind == 0) text = mica_value_symbol_text(worker->table, (uint32_t)id.f_number);
                    if (!load_text_matches(text, name)) valid = false;
                }
            }
            __asm__ volatile("" : "+r"(observed) : : "memory");
            digest += observed;
        }
    }
    worker->valid = valid;
    worker->digest = digest;
    load_barrier(worker->finish);
    return NULL;
}

static int symbol_load_driver(uint64_t requested_rounds) {
    uint64_t rounds = requested_rounds == 0 ? 2 : requested_rounds;
    uint64_t count = read_number(8), preintern = read_number(8);
    if (preintern > count) fail("invalid symbol load prefix");
    struct LoadName *names = allocate_array(count, sizeof(*names));
    struct mica_IdResult *canonical = allocate_array(count, sizeof(*canonical));
    struct mica_IdResult *known = allocate_array(preintern, sizeof(*known));
    struct mica_SymbolTable table = {0};
    if (!mica_value_symbol_table_init(&table)) fail("symbol table initialization failed");
    for (uint64_t i = 0; i < count; ++i) {
        names[i].bytes = read_bytes(&names[i].length);
        names[i].valid = read_number(1) != 0;
        names[i].scalars = read_number(8);
        names[i].ascii = read_number(1) != 0;
        if (i < preintern) {
            known[i] = mica_value_symbol_intern(&table, names[i].bytes, names[i].length);
            if (!known[i].f_ok) fail("invalid pre-interned symbol");
            canonical[i] = known[i];
        }
    }
    uint64_t threads = read_number(8);
    if (threads == 0 || threads >= UINT_MAX) fail("invalid symbol worker count");
    struct LoadWorker *workers = allocate_array(threads, sizeof(*workers));
    pthread_t *handles = allocate_array(threads, sizeof(*handles));
    pthread_barrier_t ready, start, finish;
    if (pthread_barrier_init(&ready, NULL, (unsigned)threads + 1) != 0
        || pthread_barrier_init(&start, NULL, (unsigned)threads + 1) != 0
        || pthread_barrier_init(&finish, NULL, (unsigned)threads + 1) != 0) fail("symbol barrier initialization failed");
    for (uint64_t t = 0; t < threads; ++t) {
        struct LoadWorker *worker = &workers[t];
        worker->table = &table; worker->names = names; worker->known = known;
        worker->preintern = preintern; worker->rounds = rounds; worker->valid = true;
        worker->verify = requested_rounds == 0;
        worker->ready = &ready; worker->start = &start; worker->finish = &finish;
        worker->count = read_number(8);
        if (worker->count == 0) fail("symbol worker has no operations");
        worker->operations = allocate_array(worker->count, sizeof(*worker->operations));
        worker->results = allocate_array(worker->count, sizeof(*worker->results));
        for (uint64_t i = 0; i < worker->count; ++i) {
            unsigned kind = (unsigned)read_number(1);
            uint64_t name = read_number(8);
            if (kind > 2 || name >= count || (kind != 0 && name >= preintern)) fail("invalid symbol operation");
            worker->operations[i] = (struct LoadOperation){kind, name};
        }
        if (pthread_create(&handles[t], NULL, load_worker, worker) != 0) fail("symbol thread creation failed");
    }
    load_barrier(&ready);
    uint64_t begin = now(); load_barrier(&start); load_barrier(&finish);
    uint64_t elapsed = now() - begin, digest = 0, unique = 0;
    for (uint64_t t = 0; t < threads; ++t) {
        if (pthread_join(handles[t], NULL) != 0) fail("symbol thread join failed");
        struct LoadWorker *worker = &workers[t];
        if (!worker->valid) fail("symbol changed identity or returned incorrect text during concurrent load");
        digest += worker->digest;
        for (uint64_t i = 0; i < worker->count; ++i) {
            uint64_t name = worker->operations[i].name;
            struct mica_IdResult id = worker->results[i];
            if (id.f_ok != names[name].valid) fail("symbol validity mismatch");
            if (!id.f_ok) continue;
            if (id.f_number > UINT32_MAX) fail("symbol ID overflow");
            if (canonical[name].f_ok && canonical[name].f_number != id.f_number)
                fail("equal names received different IDs across workers");
            canonical[name] = id;
        }
        free(worker->operations); free(worker->results);
    }
    for (uint64_t i = 0; i < count; ++i) {
        if (canonical[i].f_ok) {
            ++unique;
            struct mica_SymbolText text = mica_value_symbol_text(&table, (uint32_t)canonical[i].f_number);
            if (!load_text_matches(text, &names[i])) fail("symbol text or metadata mismatch after concurrent load");
            struct mica_IdResult again = mica_value_symbol_intern(&table, names[i].bytes, names[i].length);
            if (!again.f_ok || again.f_number != canonical[i].f_number) fail("symbol identity changed after concurrent load");
        }
        free(names[i].bytes);
    }
    if (unique != table.f_count) fail("symbol table cardinality mismatch");
    printf("symbol_load_result %llu %llu %llu\n", (unsigned long long)elapsed, (unsigned long long)digest, (unsigned long long)unique);
    mica_value_symbol_table_release(&table);
    if (pthread_barrier_destroy(&ready) != 0 || pthread_barrier_destroy(&start) != 0
        || pthread_barrier_destroy(&finish) != 0) fail("symbol barrier destruction failed");
    free(names); free(canonical); free(known); free(workers); free(handles);
    return ferror(stdout) ? 2 : 0;
}
