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
| Measurement metadata | Execution tier, workers, durability, accelerator placement, whole-invocation samples, and fixed-work memory use | Implemented for repeated invocation corpus |
| Strings and collections | Unicode indexing and iteration, efficient scanning and construction, and preservation of aliased values | Implemented; general nested-list construction still needs measurement |
| Calls | Reduced allocation with exception, suspension, closure, and retry regressions | Implemented; dispatch and interpreter performance remain open |
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

Call storage now reuses cleared argument and register buffers within each VM.
Returned and unwound frames release their values before entering the buffer pool.
Checkpoint restoration clears the pool; checkpoints retain only active frames.
Focused tests cover register reset, checkpoint restoration, closure captures, exceptions, and suspension.
VM and runtime library tests pass, including existing conflict-retry and authority-refresh coverage. Relevant clippy checks pass.
The call-storage capture passes all 54 processes across six selected fixtures.
Helper calls improve from 13.66 to 13.14 milliseconds; the other changes are small.
Dispatch and interpreter overhead remain substantial. This change does not establish general call-performance parity.

The common `[@items, value]` construction now uses immutable prefix storage with spare capacity.
Ordinary lists retain exact arrays. Appends containing lists copy the prefix to prevent ownership cycles through shared tails.
This restriction includes nested list references inside maps, errors, frobs, and relations; bounded traversal falls back to copying.
Tests cover aliases, branches, encoding, concurrent access, self/cross references, exception handling, and suspension.
Workspace tests and clippy pass. The storage concurrency test passes Miri with strict provenance enabled.
The process-local value ABI is version 5. Durable encoding is unchanged. The list capture passes all 54 processes.
List construction improves from 4.51 to 0.427 milliseconds; Odin takes 0.265 milliseconds.
This result applies to scalar append. Nested-list append still copies and needs separate performance work.

Loop headers now support list scatter patterns and ignored bindings, including annotations, optional defaults, and rest bindings.
Scatter headers lower into the existing loop and binding representation with unspellable temporary names.
Row loops also accept map values. Relation rows retain exact-heading checks; missing fields raise `E_MATCH`.
The bytecode operation is named `CollectionFieldAt` to describe both cases.
Tests cover scope, type errors, missing fields, defaults, continue, closures, suspension, artifact round trips, and unique node ids.
Compiler, VM, and runtime library tests pass. Relevant clippy checks pass. Comprehensions remain pending.

List comprehensions now support mapping, filtering, sorting, keyed sorting, and loop destructuring.
They lower to ordinary loops and bindings; sorting has an explicit operation independent of lexical name lookup.
Tests cover evaluation order, ties, nested comprehensions, scope, closures, break/continue, suspension, and read-only validation.
The shared loop-pattern fixture returns its expected result, 47. Workspace tests and clippy pass.
All 22 repeated-invocation fixtures now have implementations; a full pinned release capture is the next verification step.
Keyed comprehensions currently build nested lists, so their accumulation still uses the copying append path.
The comprehension capture passes 196 of 198 processes, including all nine loop-pattern/comprehension runs.
Two Rust helper-call processes exceed the 30-second whole-process limit; the other four pass.
These failures remain in the capture. Cold dispatch-cache population needs investigation.
The small loop-pattern fixture takes 19.7 microseconds in Rust and 59.3 microseconds in Odin, including invocation overhead.
