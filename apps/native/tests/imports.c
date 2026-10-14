// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>

static void *check_thread(void *argument) {
    (void)argument;
    assert(mica_imported_cache() == 1);
    assert(mica_cache_next() == 2);
    return NULL;
}
int main(void) {
    assert(mica_imported_next() == 1);
    assert(mica_counter_next() == 2);
    assert(mica_imported_next() == 3);
    assert(mica_global_counter == 3);
    assert(mica_imported_cache() == 1);
    pthread_t thread;
    assert(pthread_create(&thread, NULL, check_thread, NULL) == 0);
    assert(pthread_join(thread, NULL) == 0);
    assert(mica_imported_cache() == 2);
    const struct mica_Array_4_U8 *literal = mica_imported_literal();
    assert(literal == &mica_global_literal);
    assert(!memcmp(literal->elements, "A\0\xc3\xa9", 4));
    return 0;
}
