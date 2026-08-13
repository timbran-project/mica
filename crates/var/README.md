# mica-var

`mica-var` defines Mica's compact value representation. It is the bottom layer shared by the
relation kernel, runtime, compiler, and runner.

The central type is `Value`, a one-word tagged value. Immediate values such as identities, symbols,
booleans, small integers, reduced-precision floats, and error codes stay inline. The zero-column
empty relation, spelled `[] {}` in source, also has an immediate representation. Larger immutable
data such as strings, bytes, lists, maps, and finite relations live on the heap and are shared with
`Arc`.

## What's Here

- `src/value.rs`: `Value`, `ValueKind`, `Identity`, `ErrorValue`, encoding, constructors, accessors,
  display, and ordering.
- `src/codec.rs`: owned value encoding and decoding for storage and transport records.
- `src/heap.rs`: immutable heap-backed strings, bytes, lists, maps, and relations.
- `src/symbol.rs`: interned symbol representation and symbol metadata.
- `src/traits.rs`: common conversion and helper traits.
- `src/visit.rs`: borrowed `ValueRef` views and depth-first value traversal.
- `src/tests.rs`: unit coverage for core value behaviour.
- `tests/properties.rs`: property tests for value ordering, equality, and collection behaviour.
- `benches/var_benches.rs`: microbenchmarks for the value layer.
- `fuzz/`: cargo-fuzz target for value operations.

## Role In Mica

Relations store tuples of `Value`. The runtime moves `Value` through registers. The compiler emits
literal `Value`s into bytecode. Keeping this type compact is important because relation scans,
joins, dispatch matching, and VM execution all move values heavily.

## Licence

Mica is licensed under the GNU Affero General Public License v3.0. See the repository root
`LICENSE`.
