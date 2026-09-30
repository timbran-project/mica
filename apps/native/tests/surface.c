// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>

static void *thread_cache(void *unused) {
    (void)unused;
    assert(mica_cache_next() == 1);
    assert(mica_cache_next() == 2);
    return NULL;
}

static void arithmetic(int64_t a, int64_t b) {
    int64_t expected;
    bool ok = !__builtin_add_overflow(a, b, &expected);
    struct mica_Checked actual = mica_checked_add(a, b);
    assert(actual.f_ok == ok && actual.f_number == (ok ? expected : 0));
    ok = !__builtin_sub_overflow(a, b, &expected);
    actual = mica_checked_sub(a, b);
    assert(actual.f_ok == ok && actual.f_number == (ok ? expected : 0));
    ok = !__builtin_mul_overflow(a, b, &expected);
    actual = mica_checked_mul(a, b);
    assert(actual.f_ok == ok && actual.f_number == (ok ? expected : 0));
    ok = b != 0 && !(a == INT64_MIN && b == -1);
    actual = mica_checked_div(a, b);
    assert(actual.f_ok == ok && actual.f_number == (ok ? a / b : 0));
    actual = mica_checked_rem(a, b);
    expected = b == 0 || b == -1 ? 0 : a % b;
    assert(actual.f_ok == (b != 0) && actual.f_number == expected);
}

int main(int argc, char **argv) {
    uint8_t offset_bytes[8] = {0};
    assert(mica_signed_offset(offset_bytes + 4, -4) == offset_bytes);
    assert(mica_signed_offset(offset_bytes, 8) == offset_bytes + 8);
    assert(mica_signed_offset(offset_bytes + 8, -1) == offset_bytes + 7);
    assert(mica_pointer_distance(offset_bytes, offset_bytes + 8) == -8);
    assert(mica_pointer_distance(offset_bytes + 8, offset_bytes) == 8);
    if (argc == 2) {
        switch (argv[1][0]) {
        case 'n': (void)mica_adopt_bytes(NULL, NULL); break;
        case 's': (void)mica_left_shift(1, 64); break;
        case 'd': (void)mica_u64_div(1, 0); break;
        case 'a': (void)mica_array_roundtrip(256, 1); break;
        case 'p': (void)mica_immediate(UINT64_MAX, 1); break;
        case 'h': (void)mica_immediate(1, 7); break;
        case 'u': (void)mica_string_length(mica_immediate(1, 3)); break;
        case 'e': (void)mica_value_payload(UINT64_C(7) << 56); break;
        case 'm': (void)mica_string_length((UINT64_C(7) << 56) | 1); break;
        default: return 2;
        }
        return 3;
    }
    const int64_t edges[] = {INT64_MIN, INT64_MIN + 1, -(INT64_C(1) << 55),
        -3037000500, -3037000499, -2, -1, 0, 1, 2, 3037000499, 3037000500,
        (INT64_C(1) << 55) - 1, INT64_MAX - 1, INT64_MAX};
    for (size_t i = 0; i < sizeof(edges) / sizeof(edges[0]); ++i) {
        for (size_t j = 0; j < sizeof(edges) / sizeof(edges[0]); ++j) {
            arithmetic(edges[i], edges[j]);
        }
        struct mica_Checked neg = mica_checked_neg(edges[i]);
        assert(neg.f_ok == (edges[i] != INT64_MIN));
        assert(neg.f_number == (neg.f_ok ? -edges[i] : 0));
    }
    uint64_t random = 1;
    for (unsigned i = 0; i < 10000; ++i) {
        random = random * UINT64_C(6364136223846793005) + 1;
        int64_t a = mica_bits_signed(random);
        random = random * UINT64_C(6364136223846793005) + 1;
        arithmetic(a, mica_bits_signed(random));
    }
    assert(mica_u8_mul(255, 255) == 1);
    assert(mica_u16_mul(65535, 65535) == 1);
    assert(mica_u32_mul(UINT32_MAX, UINT32_MAX) == 1);
    assert(mica_u32_sub(0, 1) == UINT32_MAX);
    assert(mica_u64_div(UINT64_MAX, 3) == UINT64_MAX / 3);
    assert(mica_u64_rem(UINT64_MAX, 7) == UINT64_MAX % 7);
    for (unsigned shift = 0; shift < 64; ++shift) {
        assert(mica_left_shift(1, shift) == (UINT64_C(1) << shift));
        assert(mica_right_shift(UINT64_MAX, shift) == (UINT64_MAX >> shift));
    }
    assert(mica_float_bits(mica_negative_zero()) == UINT32_C(0x80000000));
    assert(mica_float_bits(mica_subnormal()) == 1);
    const uint32_t bits[] = {0, 1, 0x80000000, 0x7f800000, 0xff800000, 0x7fc00001, 0x3f800000, 0x7f7fffff};
    for (unsigned i = 0; i < sizeof(bits) / sizeof(bits[0]); ++i) {
        float f = mica_bits_float(bits[i]);
        assert(mica_float_bits(f) == bits[i]);
        assert(mica_float_finite(f) == ((bits[i] & 0x7f800000) != 0x7f800000));
    }
    assert(mica_float_add(1.25f, 2.5f) == 3.75f);
    assert(mica_float_sub(1.25f, 2.5f) == -1.25f);
    assert(mica_float_mul(1.25f, 2.5f) == 3.125f);
    assert(mica_float_div(1.0f, 4.0f) == 0.25f);
    assert(mica_float_rem(-7.5f, 2.0f) == -1.5f);
    assert(mica_float_rem(0x1p120f, 0x1p-149f) == 0.0f);
    assert(mica_float_rem(0x1p-148f, 0x1.8p-148f) == 0x1p-148f);
    assert(mica_float_bits(mica_float_rem(-4.0f, 2.0f)) == UINT32_C(0x80000000));
    assert(mica_float_neg(-2.5f) == 2.5f);
    assert(mica_float_trunc(-2.5f) == -2.0f);
    assert(mica_float_less(-2.5f, 1.0f));
    assert(mica_float_equal(0.0f, -0.0f));
    assert(!mica_float_equal(NAN, NAN));
    assert(mica_int_float(INT64_C(16777217)) == 16777216.0f);
    assert(mica_float_int(-0x1p63f).f_number == INT64_MIN);
    assert(mica_float_int(-0x1p63f).f_ok);
    assert(!mica_float_int(0x1p63f).f_ok);
    assert(!mica_float_int(INFINITY).f_ok);
    assert(!mica_float_int(NAN).f_ok);
    assert(mica_float_int(-2.5f).f_number == -2);
    assert(mica_float_int(nextafterf(0x1p63f, 0.0f)).f_ok);
    assert(mica_max_u8() == UINT8_MAX && mica_max_u16() == UINT16_MAX && mica_max_u32() == UINT32_MAX);
    assert(mica_narrow(UINT64_MAX) == UINT8_MAX && mica_extend(UINT32_MAX) == INT64_C(4294967295));
    assert(mica_signed_bits(INT64_MIN) == UINT64_C(0x8000000000000000));
    assert(mica_symbol_roundtrip(UINT32_MAX) == UINT32_MAX);
    for (unsigned i = 0; i < 256; ++i) {
        assert(mica_array_roundtrip(i, i * 37) == i * 37);
        if (i == 7 || i == 8 || i == 9 || i == 10 || i == 11 || i == 12 || i == 14 || i == 16) continue;
        mica_type_Value v = mica_immediate(UINT64_C(0x00ffffffffffffff), (uint8_t)i);
        assert(mica_value_tag(v) == i && mica_value_payload(v) == UINT64_C(0x00ffffffffffffff));
    }
    struct mica_ImmediatePair pair = mica_immediate_pair(42, 2);
    assert(pair.f_value == mica_immediate(42, 2));
    assert(pair.f_padding.elements[0] == 0 && pair.f_padding.elements[1] == 0);
    mica_type_Value immediate_slot = 0;
    assert(mica_immediate_store(&immediate_slot, 42, 2) == pair.f_value);
    assert(immediate_slot == pair.f_value);
    uint8_t *allocation = malloc(16);
    assert(allocation != NULL);
    uint8_t *owner_slot = NULL;
    uint8_t *adopted = mica_adopt_bytes(&owner_slot, allocation);
    assert(adopted == allocation && owner_slot == allocation);
    adopted[0] = 42;
    assert(owner_slot[0] == 42);
    free(owner_slot);
    uint64_t words[] = {7, 11, 13};
    assert(mica_typed_offset(words, 2) == 13);
    uint8_t *storage = malloc(sizeof(struct mica_StringHeader) + 16);
    assert(storage != NULL);
    memcpy(storage + sizeof(struct mica_StringHeader), "abc", 4);
    mica_type_Value string = mica_string_header(storage, 3);
    assert(mica_value_tag(string) == 7);
    assert(mica_string_length(string) == 3);
    assert(memcmp(mica_string_bytes(string), "abc", 4) == 0);
    mica_type_Value *slot = (mica_type_Value *)(storage + sizeof(struct mica_StringHeader) + 8);
    assert(mica_slot_share(slot, string) == string && *slot == string);
    free(storage);
    assert(mica_header_alignment() == _Alignof(struct mica_StringHeader));
    assert(mica_pointer_size() == sizeof(const uint16_t *));
    assert(mica_pointer_alignment() == _Alignof(const uint16_t *));
    assert(mica_counter_next() == 1 && mica_counter_next() == 2);
    assert(mica_global_counter == 2);
    assert(mica_cache_next() == 1);
    pthread_t thread;
    assert(pthread_create(&thread, NULL, thread_cache, NULL) == 0);
    void *status = &thread;
    assert(pthread_join(thread, &status) == 0 && status == NULL);
    assert(mica_cache_next() == 2);
    const uint8_t expected_bytes[] = {65, 0, 195, 169};
    assert(memcmp(mica_literal()->elements, expected_bytes, sizeof(expected_bytes)) == 0);
    return 0;
}
