/* Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
 * SPDX-License-Identifier: AGPL-3.0-or-later */

#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>

/* Keep platform storage opaque to generated code. Use the same allocator as
 * values so allocation failures and cleanup follow the host's allocation API. */
uint8_t *mica_foreign_allocate(uint64_t size);
void mica_foreign_release(uint8_t *data);

uint8_t *mica_foreign_mutex_create(void) {
    uint8_t *storage = mica_foreign_allocate(sizeof(pthread_mutex_t));
    if (storage == NULL) return NULL;
    if (pthread_mutex_init((pthread_mutex_t *)storage, NULL) != 0) {
        mica_foreign_release(storage);
        return NULL;
    }
    return storage;
}

void mica_foreign_mutex_lock(uint8_t *storage) {
    if (pthread_mutex_lock((pthread_mutex_t *)storage) != 0) abort();
}

void mica_foreign_mutex_unlock(uint8_t *storage) {
    if (pthread_mutex_unlock((pthread_mutex_t *)storage) != 0) abort();
}

void mica_foreign_mutex_destroy(uint8_t *storage) {
    if (pthread_mutex_destroy((pthread_mutex_t *)storage) != 0) abort();
    mica_foreign_release(storage);
}
