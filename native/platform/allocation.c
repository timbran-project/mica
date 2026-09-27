/* Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
 * SPDX-License-Identifier: AGPL-3.0-or-later */

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>

_Static_assert(sizeof(size_t) == 8, "64-bit size_t required");

uint8_t *mica_foreign_allocate(uint64_t size) {
    return malloc((size_t)size);
}

void mica_foreign_release(uint8_t *data) {
    free(data);
}
