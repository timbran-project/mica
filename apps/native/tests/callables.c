// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
static uint64_t external_double(uint64_t value) { return value * 2; }
int main(int argc, char **argv) {
  (void)argv;
  if (argc > 1) { mica_dispatch(NULL, 1); return 1; }
  assert(mica_chain(41) == 42);
  assert(mica_aggregates(99) == 100);
  assert(mica_dispatch(external_double, 21) == 42);
  assert(mica_dispatch(mica_a_factory(), UINT64_MAX) == 0);
  assert(mica_is_null(NULL) && !mica_is_null(mica_a_factory()));
  uint64_t slot = 0;
  assert(mica_memory_dispatch(&slot, 77) == 77 && slot == 77);
  assert(mica_borrow_dispatch(&slot) == &slot);
  struct mica_CallablePacket packet = {mica_a_factory(), 13, mica_packet_identity};
  struct mica_CallablePacket result = packet.f_transform(packet);
  assert(result.f_callback(result.f_value) == 14);
  return 0;
}
