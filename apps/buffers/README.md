# Buffers and markers

`apps/shared/buffers.mica` defines markers and annotations around transactional text buffers.
Markers store a Unicode scalar position, insertion affinity, and buffer revision.
Applications explicitly rebase markers with committed deltas from `buffer_apply_result`.
Text edits and marker facts can share one transaction.

The runtime supplies `BufferStat`, `BufferLine`, and `BufferMarkers` when the library declares these relations.
See [the buffer interface](../../mdbook/src/runtime/buffers.md) for authority, revision, and persistence contracts.

Run the application scenarios:

```sh
cargo test -p mica-runtime --test buffers
```

The test harness executes 29 buffer scenarios and 10 marker scenarios in declaration order.
Each invocation commits separately. Both interpreter-only and native-enabled runs must produce the expected values.
The reversion status scenario returns a map. The other scenarios return `true` after their assertions.

These files come from omica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The Rust port corrects the annotation retirement arity and projects one column in each annotation endpoint accessor.
Those projections satisfy Rust's exact-heading row binding contract.
