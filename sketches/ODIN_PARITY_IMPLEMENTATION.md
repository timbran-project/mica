# Odin capability and performance parity

This plan records the implementation requested on 2026-09-26.
Rust starts at `350713b`. The initial Odin reference is `bfb368c`.
Pending changes in either checkout are outside the reference baseline.

## Acceptance criteria

Each workstream requires implementation, focused regressions, and relevant integration tests.
Performance claims require measured comparisons with identical work and verified results.
Passing an early workstream does not complete this plan.

| Workstream | Required result | Status |
| --- | --- | --- |
| Shared baselines | Pinned Rust and Odin revisions, shared source corpus, independent expected results, and reproducible captures | In progress |
| Measurement metadata | Execution tier, workers, durability, accelerator placement, whole-invocation samples, and fixed-work memory use | Pending |
| Strings and collections | Unicode indexing and iteration, efficient scanning and construction, and preservation of aliased values | Pending |
| Calls | Reduced allocation with exception, suspension, closure, and retry regressions | Pending |
| Computed scans | CPU batches and cached retrieval preparation with unchanged authority, transaction visibility, output bindings, and exact ranking | Pending |
| Transactional buffers | Atomic fact/text commits, conflict handling, durable recovery, marker rebasing, and client revision results | Pending |
| Editor | Shared buffer library and programmable editor running through Rust host services | Pending |
| Query and storage execution | Measured columnar/storage improvements that preserve incremental maintenance | Pending |
| Query measurements | Separate initial derivation and small-update maintenance workloads | Pending |
| Mica compiler | Ported compiler with an intentional Rust assembly interface and bootstrap conformance | Pending |
| Ingestion | Ported ingestion applications with verified loaded facts and inference results | Pending |

## Commit boundaries

1. Shared source runner, conformance corpus, and measurement protocol.
2. String semantics and representation, followed by collection construction and call storage changes.
3. Batched computed scans, followed by cached exact retrieval.
4. Buffer storage and transactions, persistence, runtime operations, then editor integration.
5. Independently measured rule execution and storage improvements.
6. Rust assembly interface, Mica compiler, and ingestion applications.

Each boundary can contain multiple conventional commits when separate changes need separate verification.
Commits include only files for this objective. No push is authorized.

## Verification

- Run `cargo fmt --all` and relevant Rust tests for each implementation change.
- Run relevant clippy checks with warnings denied.
- Run workspace tests and clippy at integration boundaries.
- Compare results before comparing timings. Unsupported features remain explicit gaps.
- Measure fixed invocation counts in fresh processes for memory comparisons.
- Record compiler identities, source hashes, build commands, machine details, and raw samples.
- Preserve incremental maintenance, live replacement semantics, authority checks, and committed effect delivery.

## Current evidence

The initial survey was read-only. The first pinned release baseline is now recorded.
The implementation adds `mica bench`, a pinned corpus, and a capture launcher under `benchmarks/parity`.
The runner tests cover suspended work, result changes after warmup, invalid counts, and failed setup.
Runtime and driver library tests pass. Relevant clippy checks pass with warnings denied.

Initial conformance probes expose additional language gaps: `len`, loop destructuring, and comprehensions.
The manifest records these gaps. The helper-call fixture exceeds a 40-second debug probe limit.
These probes establish conformance status only. They are not release performance measurements.

The initial release capture contains 198 processes: 162 passed and 36 failed.
All Odin fixtures passed. Six Rust fixtures fail across both tiers and all three repetitions.
The failures require `len`, `string_append`, loop destructuring, or comprehensions.
The release helper-call fixture passes; its earlier debug timeout was not a conformance failure.
Raw samples, metadata, and failures are in `benchmarks/parity/results/2026-09-26-baseline`.

String and basic collection support now includes `len`, scalar indexing and iteration, `string_append`, `string_span`, and `string_find_any`.
String storage retains immutable prefixes, caches scalar counts, and lazily samples Unicode offsets.
The string header fits within the existing heap enum footprint. The process-local value ABI is version 4; durable encoding is unchanged.
Tests cover Unicode boundaries, aliases, concurrent append and index publication, closures, exceptions, suspension, and codec round trips.
Workspace tests and clippy pass. Three storage tests pass under Miri with strict provenance enabled.
The pinned string capture passes 192 of 198 processes. Only loop-pattern and comprehension fixtures still fail.
String construction improves from 304 to 264 microseconds; short-string slicing is essentially unchanged.
The newly runnable list-building fixture takes 4.51 milliseconds versus Odin's 0.27 milliseconds.
Loop patterns, comprehensions, list construction, and call allocation remain open.
