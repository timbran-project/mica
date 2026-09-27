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
| Measurement metadata | Execution tier, workers, durability, accelerator placement, whole-invocation samples, and fixed-work memory use | Implemented for repeated and single-invocation workloads |
| Strings and collections | Unicode indexing and iteration, efficient scanning and construction, and preservation of aliased values | Implemented; general nested-list construction still needs measurement |
| Calls | Reduced allocation with exception, suspension, closure, and retry regressions | Implemented; dispatch and interpreter performance remain open |
| Computed scans | CPU batches and cached retrieval preparation with unchanged authority, transaction visibility, output bindings, and exact ranking | Implemented for positive rule steps and equality probes; exact retrieval measured |
| Transactional buffers | Atomic fact/text commits, conflict handling, durable recovery, marker rebasing, and client revision results | Implemented, including client results, history, compaction, and computed views; comparative measurements remain |
| Editor | Shared buffer library and programmable editor running through Rust host services | Shared library, source scenarios, confined files, browser input, and reconnect replay implemented; comparative measurements remain |
| Query and storage execution | Measured columnar/storage improvements that preserve incremental maintenance | Persistent rule collections, indexed recursive reseeding, retained visible rows, and retained derived indexes measured; initial derivation remains open |
| Query measurements | Separate initial derivation and small-update maintenance workloads | Implemented and measured; Rust has substantial initial and recursive-deletion costs |
| Mica compiler | Ported compiler with an intentional Rust assembly interface and bootstrap conformance | Assembly, frontend, bootstrap, collections, closures, exceptions, and relation operations implemented; further donor features and public installation remain |
| Ingestion | Ported ingestion applications with verified loaded facts and inference results | OWL batches, streaming input, durable resume, CycL census, and scoped sample implemented; initial ingestion comparison measured; OWL load performance remains open |

## RFC cross-check

[The comment on omica #120](https://github.com/rdaum/omica/pull/120#issuecomment-5849929673) records suggested conformance rows from the RFC review.
The RFC baseline differs from this plan's pinned donor. Draft requirements do not establish implemented behaviour.

| Suggested row | Acceptance boundary | Direction and status |
| --- | --- | --- |
| Declarations and catalogue | Matching redeclarations, metadata conflicts, constructor results, runtime calls, and filein declarations | Cross-implementation conformance; the pinned Odin branch already validates constructor metadata and returns identities |
| Units and live replacement | Atomic Add/Replace, ownership, rollback, restart, and fileout | Preserve Rust's existing Replace behaviour; distinguish proposed persistent mutable unit slots from source ownership |
| Transactions and effects | Conflict diagnostics and commit-gated effects through abort, retry, conflict, and suspension | Preserve Rust guarantees; verify Odin gaps against the pinned revision |
| Demand evaluation | Recursive tabling, completion, visibility, authority, invalidation, negation, and subscriptions | Experimental proposal; separate from implemented incremental forward derivation |

The editor port targets the Rust runtime and existing Rust hosts. It does not require Odin to implement MHP1 first.
Connecting Rust hosts to an Odin world is a separate interoperability task.
Initial derivation and small-update measurements must expose the difference between weighted maintenance and full fixpoint recomputation.
Odin #125 shares unchanged derived blocks after evaluation; it does not complete incremental maintenance.

Conformance review must cover language exceptions, default/rest parameters, selector-specific invocation, capability expiry, and source-root confinement.
The rules draft contradicts itself about unsafe-rule validation timing. Verify executable cases before treating that claim as an implemented advantage.
The demand draft also overstates current DRed support and derived-state persistence. Its proposed semantics remain separate from implementation evidence.
Buffers need explicit semantic coverage for revisions, atomic facts/text, conflicts, completion, markers, history, and recovery.

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

Computed providers now have a CPU batch hook used by positive rule steps and equality probes.
The registry validates input requirements, output arity, row association, and bound outputs.
Rule planning waits for required computed inputs. Default providers retain scalar execution semantics.
Kernel tests cover transaction overlays, repeated keys, bound outputs, malformed providers, and actual batch dispatch.
Kernel library tests and clippy pass; runtime retrieval tests pass. Retrieval preparation caching is next.

Exact embedding search now caches parsed candidate vectors, subjects, and norms within a transaction.
The cache admits at most 16 indexes, clears on every write, and does not survive transaction replacement or suspension.
Batch calls share preparation even when their reader has no persistent cache. Estimates use the requested limit instead of running a search.
Tests verify one preparation for repeated queries, exact ties, best-per-subject ranking, post-top-k filters, local writes, rollback, suspension, invalid vectors, and denied relation access.
Workspace tests pass. Seventeen focused retrieval tests pass, including the additional suspension and invalid-vector cases.
The shared retrieval workload has an independent checksum and score assertions. Pinned before/after capture remains required.
The pinned retrieval captures pass all 18 processes. Rust interpreter latency improves from 52.90 ms to 2.78 ms for 32 searches over 512 vectors.
Odin takes 6.54 ms in the after capture. Fixed-work Rust RSS remains approximately 22 MiB.
The result measures exact CPU search with preparation reuse inside each transaction, not cross-transaction caching or GPU search.

Dispatch cache insertion now updates ordered maps under short read/write locks instead of copying every prior entry.
Positional hits borrow the argument slice for lookup and retain the shared method-result slice.
Concurrent publication tests preserve independent keys and previously returned results.
Kernel, runtime, and VM library tests pass, including dispatch replacement and conflict coverage. Kernel clippy passes.
This removes a quadratic insertion algorithm; pinned cold and warm measurements must establish its effect.
Cold helper-call captures pass all 18 processes. Rust interpreter latency falls from 29.14 seconds to 27.6 ms after dispatch insertion stops copying prior entries.
The full warmed capture passes all 207 processes across 23 fixtures. Warm helper calls take 10.80 ms versus the earlier 13.14 ms; Odin takes 1.20 ms.
Other original fixtures remain within five percent of the comprehension capture. Cache contention across multiple task workers is not measured yet.

The buffer text core now uses immutable UTF-8 chunks and persistent AVL nodes with scalar, byte, and newline counts.
Splices retain unchanged chunks. Bounded search streams across chunks; line coordinates use subtree counts.
Provenance deltas distinguish edits to equal text at different positions. Normalization, disjoint transforms, and marker rebasing have explicit work limits.
Ten focused tests pass, including generated Unicode edits, fragmented-base normalization, and commuting disjoint edits. Kernel clippy passes.
This core is not yet connected to transactions or persistence. Atomic publication, recovery, runtime operations, client results, and editor integration remain required.

Buffers now participate in kernel transactions and staged snapshot publication.
Text and facts share persistence and publication, including creation, deletion, and rollback.
Reject, span, and whole-buffer conflict policies have focused concurrency coverage.
The catalogue retains deleted identities and permits name reuse after deletion.
Fjall strict and relaxed recovery retain durable text and reset volatile text with an advanced revision.
The store format is `mica-relation-kernel-state-2.0.0`; earlier formats are rejected without compatibility adapters.
The buffer journal still replays all deltas. Bounded checkpoints, runtime operations, client results, markers, and editor integration remain required.
Workspace tests and clippy pass. Recovery tests cover strict and relaxed Fjall stores, volatile reset, deletion, and staged name reuse.

Fjall buffer recovery now reads a checkpoint and fewer than 64 deltas totalling less than 1 MiB per buffer.
Checkpoint replacement and delta removal share the atomic fact/text batch. Ordinary writes append one delta and update a small counter.
Startup no longer eagerly scans commit history to compute an unused fallback version.
Focused tests verify checkpoint limits, Unicode text, reopening, and deletion. The buffer persistence probe measures complete edits, flush, and reopening.
Pinned measurements remain required to quantify checkpoint latency and recovery cost.

The first checkpoint capture rejects the 64-delta interval: strict edits take 8.07 s versus 2.32 s, and recovery takes 96 ms versus 43 ms.
All 12 processes recover correct text and revisions. Raw results remain in `benchmarks/parity/results/2026-09-26-buffer-checkpoints-64`.
A longer bounded interval is under measurement to reduce full-buffer write amplification.

The pinned Odin buffer tests require retired names to remain reserved. Kernel tombstones now reserve both identities and names.
Reads distinguish deleted buffers from unknown buffers. Recovery and staged publication retain retirement, so earlier name-based grants cannot resolve to replacement buffers.
The larger checkpoint test exposed recursive destruction of retained commit history. History nodes now release iteratively.
A regression test releases 100,000 shared commits on 64 KiB thread stacks and verifies that retained snapshots still expose their history.

The 4,096-delta interval passes all 24 processes across 4,096-edit and 32,768-edit captures.
At 4,096 edits, strict time is 2.60 s versus 2.37 s and recovery is 54 ms versus 43 ms.
At 32,768 edits, strict time is 31.7 s versus 29.6 s and recovery is 420 ms versus 325 ms.
These costs remain in the results. Bounded buffer replay does not bound Fjall journal recovery or retained commit history.
Provider startup now validates format markers on its open database instead of opening and recovering the database twice.
Format rejection and all five Fjall recovery tests pass. A pinned capture must measure the startup change.

Runtime buffer builtins now provide creation, Unicode edits, retirement, slices, search, bounded lines, viewports, and line/column navigation.
Reads and writes use the existing authority context. Policy names resolve active buffers at task boundaries, and suspension refreshes authority.
The runtime prevents pending buffer identities and names from colliding with tuple relation creation.
Seven focused tests pass, including rollback, retirement, actor grants, authority revocation during suspension, and read-only validation.
Workspace tests and clippy pass for the runtime buffer API.
Client applies/results, history, compaction, computed relations, markers, and editor integration remain pending.

The single-open capture passes all 12 processes at 4,096 edits. Recovery improves from 43–45 ms to 29–31 ms.
Strict edits still cost 2.82 s versus 2.65 s, and relaxed edits cost 81 ms versus 69 ms. RSS increases by approximately 2 MiB.
At 32,768 edits, all six relaxed processes pass: recovery improves from 328 ms to 211 ms, while edit/flush time rises from 520 ms to 604 ms.
Four strict processes time out at 120 seconds, two per revision. Those failures remain in the capture and prevent a stable strict-mode comparison.

Revision-checked applies now validate complete sequential batches before changing transaction text.
An apply seals that buffer against later mutations. Tagged results settle after publication or record conflict, resync, or abort.
Acknowledgement deltas use the client's original revision and include merged concurrent changes.
The result cache retains at most 1,024 entries and 8 MiB of delta storage, never evicts pending entries, and rejects token collisions.
Tagged task conflicts return their recorded outcome instead of automatically re-executing the submission.
The runtime exposes apply, result, and Unicode marker-rebase builtins. Result reads require buffer read authority.
Six kernel tests and eleven runtime buffer tests pass. Workspace tests and clippy pass.

Buffer history retains 32 versions for up to 256 recently changed buffers and releases entries on retirement.
Reversion stages fresh chunks with an exclusive commit check. Compaction preserves the text revision and advances an ephemeral structure generation.
All conflict policies reject writes against superseded structure. Tagged applies report resync across compaction boundaries.
History publication shares a lock with snapshot publication, so readers of a new revision can find its predecessor.
The runtime exposes compaction and reversion. Reversion requires its own invoke grant as well as buffer write authority.
Five focused kernel tests and twelve runtime buffer tests pass. Workspace tests and clippy pass.


Computed buffer providers now expose bounded statistics, lines, and revision-filtered marker windows.
They read private text and projected revisions. Projected revisions and sorted marker points are cached until local mutation.
Authority contexts compile read grants once and share them with transactions. Buffer providers check buffer and marker relation grants.
Restricted computed reads bypass shared derived and packed caches. Providers declare authority dependence, so ordinary worlds retain existing cache paths.
Six focused runtime tests pass, covering Unicode, cancelled edits, rule visibility, rollback, output bindings, marker writes, authority changes, and suspension.
Workspace tests and clippy pass for computed buffer views. The imported application scenarios remain under separate validation.

The shared marker corpus requires holes in positive rule bodies. The compiler now lowers each hole to an independent anonymous variable.
A focused test checks independent holes and incremental support after retractions. Heads, negated predicates, and guards retain their existing hole restrictions.
The 29 pinned buffer scenarios pass with both interpreter-only and native-enabled execution. Marker scenarios exposed an uncaught read-only write error; that VM path remains under repair.


Read-only tuple writes now raise catchable `E_READ_ONLY` errors, including ordinary, spliced, and wildcard operations.
Wildcard retraction checks writability before scanning, including an empty match. Other write failures retain their existing runtime error behaviour.
The shared buffer library and 39 pinned Mica scenarios now pass in interpreter-only and native-enabled runs.
The port corrects the annotation retirement arity and projects each endpoint accessor to its exact binding heading.
Workspace tests and clippy pass for this application boundary.


The editor and Mica compiler require numeric parsing. Runtime builtins now provide `parse_int`, `parse_float`, `to_int`, and `to_float`.
Conversions preserve finite binary32 and signed 56-bit integer constraints. Parsing rejects partial spellings and raises catchable type or argument errors.
Focused tests pass for integer endpoints, float rounding and underflow, nonintegral conversion, malformed input, and non-finite results.
Runtime integration tests and workspace clippy pass.

Untouched empty buffers now retain revision zero through publication, staged filein, commit-log replay, and Fjall recovery.
Their first content change commits at revision one. Computed views and tagged acknowledgements use the same revision contract.
Recovery rejects revision-zero text, tombstones, and repeated declarations. Volatile recovery advances only nonzero revisions.
The 33 kernel buffer tests, workspace library tests, shared source scenarios, and workspace clippy pass.
The editor corpus now passes its tagged revision assertion and exposes a separate uncaught JSON parse error.
Rust retains its documented no-op rule: edits that restore the original provenance do not advance the revision.
Odin can advance a revision for such writes; exact no-op revision parity is not claimed.

JSON conversion failures now raise catchable language errors. Malformed or unrepresentable values raise `E_INVARG`; non-string decoder input raises `E_TYPE`.
Eight focused JSON tests, all runtime tests, and workspace clippy pass.
The editor reaches session cleanup after this fix; application arity corrections remain in the separate editor port.

The editor fileins now load and pass 45 pinned scenarios plus a stale-keymap regression in both execution modes.
The port uses local mutable bindings, keyed comprehensions, and an indexed traversal queue.
It corrects `SearchMatch` cleanup arity and the `selected_window` call used for keymap refresh.
The harness renders the page shell and resumes explicit commits to verify final tagged results.
Workspace tests and clippy pass. Browser transport and actual filesystem requests remain unimplemented.
Host wiring exposed a creation-authority gap: ordinary actors cannot create buffers without administrative grant authority.
The next runtime boundary is a specific creation grant with transaction-local access to newly created buffers and fresh policy checks afterwards.

Buffer creation now accepts a `:make_buffer` invoke grant, including role grants, without granting administrative authority.
Only newly created buffers receive temporary read/write capabilities. Re-declaring an existing buffer confers no access.
Focused tests cover private-buffer denial, computed reads, later policy grants, and authority refresh after suspension in both execution modes.
Runtime tests and workspace clippy pass. Application ownership policy and host wiring remain separate work.

The daemon now routes four fixed editor file services through its external-request handler, with configured `--editor-root` directories.
Directory capabilities confine paths. Blocking file work runs outside the I/O executor; reads and encoded saves are limited to 8 MiB.
Saves compare opaque stamps, preserve permissions, flush temporary text, replace the destination, and flush the directory.
The host serializes its saves. External writers remain able to race the final stamp check; the documentation states that limit.
`cap-std` adds directory confinement at the host boundary, with 11 additional locked packages and no default features.
The host reuses `sha2` for content stamps and `rustix` for nonblocking file opens that reject named pipes without waiting.
Seven real-filesystem tests cover revisions, concurrent saves, Unicode/CRLF, limits, completion, symlink escapes, and named pipes.
The real driver integration test visits, edits, saves, detects an external change, and confirms replacement through Mica file commands.
Web-host and daemon tests and workspace clippy pass. Browser session transport and editor measurements remain pending.

The editor host now accepts ordered input batches and sends replies through the existing SSE session.
Sessions bind to an actor. Fixed Mica entry points run through ordinary driver endpoints with fresh task authority.
A shared workspace role grants editor operations without policy writes or code installation.
Completion tokens use a host counter independent of each browser's input sequence.

Admission validates each complete batch before queueing it. Pending input, reply caches, and editor output queues have count and byte limits.
Completed replies enter the cache before publication. Repeated sequences replay the same reply without repeating edits.
An output overflow ends the current writer so the browser reconnects and replays outstanding input.
Expired replies return HTTP 410. Permanent failures stop automatic retries and retain pending input for review.
Session and replay state remains ephemeral; restarting the daemon requires a new browser session.

All 43 web-host tests and 33 browser-client tests pass. Protocol coverage includes ordinary actors, two-session token separation, bounds, admission failures, replay, and ownership.
A real TCP/SSE test reconnects and replays a Unicode edit without duplicating text.
Workspace tests and clippy pass. Focused daemon and file-service checks also pass after the missing-stamp correction.
A browser check exercised Unicode insertion, movement, deletion, undo, file visiting, and saving a new UTF-8 file through ordinary web authority.
The file service now uses Mica's headed `none` value for absent stamps; an unheaded empty relation caused false external-change prompts.
The launcher is `scripts/editor.sh`. Browser and file checks establish functionality, not performance parity.

The rule measurements now separate initial loading and derivation from repeated small updates.
Their initial capture passes 18 processes but exposes substantial Rust costs: 27.7 ms for initial work and 761.9 ms for 16 updates.
Odin takes 2.0 ms and 15.2 ms respectively. Each update changes 32 of 8,448 closure rows.
A profile traced substantial update cost to unindexed full joins during recursive replacement-derivation searches.
Reusing maintained join indexes reduces the Rust update workload to 212.9 ms; all 18 follow-up processes pass.
Workspace tests and clippy pass, including randomized recursion, negation, deletion, and retained-snapshot coverage.
Rust remains approximately 14 times slower on updates. Initial derivation is essentially unchanged and remains a separate gap.

Maintained rule collections now reuse the existing persistent tuple store rather than copying large ordered sets in every recursive round.
The capture at `15cd6ff` passes all 18 processes and reduces the 16-update workload from 212.9 ms to 166.5 ms.
Initial derivation rises from 27.2 ms to 28.9 ms, and update RSS rises by roughly 2 MiB.
This measured tradeoff favours repeated updates. It does not close the performance gap with Odin.
Tests cover compact/radix ordering, promotion, retained branches, and recursive retractions across both storage sizes.

Recursive overdeletion now retains visible tuple storage and removes only unsupported overdeleted rows.
The capture at `84be391` passes 18 processes and reduces the 16-update workload from 166.5 ms to 118.8 ms.
Update RSS falls from approximately 28.7 MiB to 27.5 MiB. Initial derivation remains approximately 28 ms.
This is a 6.4-times update improvement from the first capture, with an approximately eight-times gap remaining against Odin.
Kernel checks cover simultaneous stored-support replacement, subsequent removal, full recomputation, and retained snapshots.

Maintained state now retains query-facing derived tables and updates their indexes when support crosses zero.
The capture at `665dfb7` passes all 18 processes and reduces the 16-update workload from 118.8 ms to 93.8 ms.
Initial derivation stays near 28 ms, and update RSS stays near 27 MiB.
This is an 8.1-times update improvement from the first capture, with an approximately 6.4-times gap remaining against Odin.
Workspace tests and clippy pass. Regressions cover secondary indexes, stored/derived overlap, unrelated commits, and retained snapshots.

The runtime now exposes `assemble(description)` for explicit Rust register-program descriptions.
It validates operands, registers, targets, nested function frames, and serializable constants before returning an ordinary Rust artifact.
Assembly neither installs code nor grants execution authority. The format uses absolute local targets and explicit constant/register operands.
Six focused tests cover malformed input, bounded nesting, Unicode control flow, closures, errors, commit continuation, and denied writes.
Runtime tests and workspace clippy pass. The compiler emitter still needs conversion to this interface.

The pinned Mica lexer and parser now run in Rust, including Unicode scalar positions and relation-valued AST output.
The frontend parses every shared benchmark fixture and reports malformed input in both execution modes.
Its port uses mutable local parser state and the public `relation_from_rows` constructor.
The corpus test exposed repeated deep comparisons of shared token lists during dispatch-cache lookup.
Value equality and ordering now check identical value words before traversing immutable payloads.
Workspace tests and clippy pass, including shared graphs, retained list views, and the frontend corpus.
The Mica emitter and self-hosted bootstrap remain pending.

The shared-list call captures pass all 18 before/after processes at `59c39a2` and `0ad9d57`.
The 400-call workload improves from 21.49 ms to 13.54 ms with essentially unchanged RSS near 21 MiB.
Odin takes 0.35 ms after the change, leaving a substantial gap. Separate but equal lists still require structural comparisons.

Positional dispatch now caches by arity when every candidate parameter has an unrestricted dispatch restriction.
The resolver supplies that proof. Selectors with any restricted candidate retain value-based keys.
This prevents unrestricted calls from retaining every changing parser and emitter state in the snapshot cache.
Regressions cover restricted overloads, arity differences, transaction-local catalogue changes, and retained snapshots.
Workspace tests and clippy pass. The full compiler bootstrap also passes after the cache change.
The fixed-work timing capture is recorded below.

The Rust assembly interface now includes kind checks and task aborts.
The Mica emitter produces separate entry and method artifacts, with catalogue dispatch for forward and recursive calls.
Its supported core includes local state, collections, calls, conditionals, loops, required parameters, and basic runtime type checks.
Tests compare execution against the Rust compiler, including alias preservation, evaluation order, Unicode, annotations, and explicit selector calls.
The bootstrap compiles the lexer, parser, and emitter, installs the emitted methods, and reproduces target artifacts byte for byte.
All six compiler tests pass in 117.7 seconds in the debug build. The test process tree peaks at 107,476 KiB.
Assembly validation tests and runtime clippy pass. These checks do not establish compiler performance parity.
Relation operations, closures, exceptions, broader parameter and pattern support, and public module installation remain pending.

The arity-cache capture at `3d5c166` passes all 18 processes across shared calls and the Mica frontend.
Shared-call latency falls from 13.54 ms to 0.67 ms, approximately 20 times faster. Odin measures 0.34 ms.
Rust RSS stays near 21 MiB. This leaves an approximately two-times timing gap on the shared-call workload.
The frontend constructs and lexes 200 Unicode declarations, then parses them into AST rows with checked counts and source positions.
Rust measures 36.1 ms and approximately 26.3 MiB peak RSS. Odin measures 37.1 ms and approximately 19.0 MiB.
The frontend result excludes emission and installation. Each implementation loads its own pinned Mica sources, with hashes in the manifest.

The emitter now supports list slices, optional/rest list destructuring, `raise`, and static-code or catch-all handlers with `finally`.
Error fields use the VM's existing option-valued message and payload semantics through the assembly interface.
Execution comparisons cover lazy defaults, omitted values, indexed-assignment results, rethrows, and finally execution on break, continue, and return.
Focused assembly and emitter checks pass. The extended compiler still bootstraps and reproduces target artifacts in both execution modes.

The emitter resolves existing relation identities from one `RelationName` read per compilation.
Emitted scans support bound arguments, output variables, repeated-variable equality, holes, and splices.
Assertions and retractions use the existing dynamic relation instructions, including transaction-local visibility.
Execution comparisons and authority regressions pass in both modes. Root compilation does not let an artifact bypass execution-time read or write grants.
Source declarations still require a compilation/installation stage that establishes their identities before assembly.

Closure conformance exposed two Rust compiler defects in direct local calls.
Optional defaults could not resolve earlier parameters. Spliced arguments bypassed the function-value arity check.
The direct binder now gives defaults access to preceding arguments and restores the caller's local bindings afterwards.
Calls with splices use the existing checked function-value path. Ordinary calls retain direct execution.
Both regressions fail before the fixes. All 238 compiler tests and compiler clippy pass afterwards.
The tests cover direct and aliased calls, missing/excess arguments, chained defaults, nested closures in defaults, and caller binding preservation.

The emitter now builds local function values with nested captures, typed parameters, dependent defaults, and rest arguments.
Closure comparisons cover aliases, captured lists, shadowed parameters, and runtime kind and arity errors.
The bootstrap target includes nested closures and an omitted default. Its artifacts match before and after the compiler compiles itself.
All eight compiler application tests pass in the full workspace run. Workspace clippy also passes with warnings denied.
Self-recursive local functions remain unsupported. Capture discovery can retain an unused outer name when an inner binding shadows it.

The pinned Bycycle OWL schema and rules now load in Rust. The application test checks taxonomy, contradictions, graph navigation, and retrieval.
Committed retractions remove unsupported derived rows while alternate taxonomy paths remain. Both execution modes pass.
The full OWL loader, batch recovery, and CycL census remain separate ingestion work.

The OWL command streams XML or gzip into batches passed to `bycycle_load_batch` through the existing invocation API.
The Mica verb commits facts, names, GUID mappings, retrieval grants, and its expected progress cursor in one transaction.
The cursor identifies exact source bytes, actor name, subject count, and completion. Resume reparses the prefix to recover namespace context.
The pinned fixture produces 13 accepted facts, two dropped repeats, and one dropped resource in both execution modes.
Tests cover literal decoding, distinct GUID punctuation, inference, pending-batch rollback, strict-store reopening, and changed-source rejection.
No ingestion throughput or memory parity claim follows from these functional checks. CycL census and comparative ingestion measurements remain open.

The CycL census validates one assertion per line and counts predicates without allocating a full AST or opening a store.
The namespaced CycL schema and seven-fact sample coexist with the OWL schema and Rust's `Arity/2` catalogue relation.
The donor's full dump router is not implemented. This port does not claim to add it or Cyc logical inference.

The OWL workspace run and workspace clippy pass. Focused CycL parser and cross-schema sample tests also pass.
The CLI regression verifies gzip census without a store, strict import, separate-process resume, completion, and actor mismatch rejection.

The first corrected ingestion capture passes all 12 processes at Rust `64e4bd4` and Odin `bfb368c`.
Strict OWL loading of 1,001 subjects and 2,001 facts takes 162.6 ms in Rust versus 21.4 ms in Odin.
Peak RSS is approximately 20.7 MiB versus 10.9 MiB. This capture excludes rules and separates setup and persisted-result verification from loading.
The 100,000-assertion CycL census takes 12.1 ms versus 94.7 ms, with approximately 4.8 MiB versus 112.8 MiB peak RSS.
Odin includes schema initialization. Its pinned schema conflicts with `Arity/2`, so the capture records a namespaced-schema adaptation.
The initial failed verification and schema runs remain in the evidence. They do not contribute accepted timing summaries.

Profiling a 10,001-subject OWL load found repeated construction of transaction-local scan indexes.
Every pending write cleared the indexes and their probe history. Interleaved reads rebuilt indexes from all pending tuples.
The overlay now updates each retained index for the changed tuple and preserves repeated-probe history across writes.
Regressions cover out-of-order insertions, assert/retract cycles, Unicode overflow keys, canonical ordering, and compact-to-radix promotion.
Both regressions fail before the change. The profiled run is diagnostic evidence, not an accepted timing sample.

Workspace tests and clippy pass with retained local scan indexes, including the compiler bootstrap and ingestion recovery tests.

The retained-index ingestion capture passes all 12 processes at Rust `f09d390`.
The strict 1,001-subject OWL median falls from 162.6 ms to 33.8 ms, approximately 4.8 times faster.
Odin measures 15.4 ms in this capture, leaving an approximately 2.2-times gap. Rust RSS increases by roughly 0.3 MiB.
CycL census timing remains approximately 12.2 ms. Larger-load scaling remains under investigation.

A follow-up 10,001-subject profile no longer shows repeated local-index reconstruction as the dominant cost.
Identity collision checks still scan `NamedIdentity` by its second column, which lacks a reverse index.
Fresh runtime catalogues now declare the `[1, 0]` index. Identity allocation retains its collision check and uses the ordinary indexed scan.
Persisted catalogues keep their stored index definitions. No schema migration or compatibility path is introduced.

All 325 runtime library tests, OWL/CycL application tests, runner tests, and workspace clippy pass after the catalogue-index change.

The larger ingestion comparison passes all 24 processes across Rust `f09d390` and `dfebf6d`, both against Odin `bfb368c`.
For 10,001 subjects and 20,001 facts, the identity-value index reduces Rust's median from 596.8 ms to 231.8 ms.
Peak RSS increases by approximately 2.2 MiB. Odin remains near 61.8 ms, leaving a roughly 3.8-times gap at this size.
This improvement applies to fresh catalogues. The separate census control passes with overlapping Rust process timing ranges.
Further OWL execution costs, rule derivation, resumed large stores, and concurrent ingestion remain separate measurement questions.

The assembly interface now exposes the existing relation-pattern, first-row cell, and collection-field instructions.
These operations retain exact relation headings, row-count checks, first-row equalities, and catchable missing-field errors.
Map iteration still permits additional keys. Assembly tests cover both successful reads and invalid descriptions; runtime clippy passes.
This closes an assembly gap required by the Mica emitter's row-pattern and comprehension work.

The emitter now compiles exact row bindings, row iteration, and filtered or sorted comprehensions.
Loops and comprehensions share binding code, including two names, typed names, and wildcards.
Comparisons preserve exact relation headings, catchable cardinality errors, lazy filters, sort-key evaluation order, and value ordering for equal keys.
Suspension retains the accumulator and captured row values. Break and continue still run intervening finally handlers.
All ten compiler application tests pass in the workspace run. The bootstrap reproduces artifacts containing rows and keyed comprehensions in both execution modes.
Workspace clippy passes. These checks establish capability coverage; source-to-artifact performance remains a separate measurement.

The emission workload builds 100 Unicode declarations and a filtered, keyed comprehension, then compiles through each pinned Mica assembler.
Rust execution checks confirm the generated program returns `[6, 4]` in both modes. The timed workload checks assembly success and source length.
The Rust benchmark task limit now matches Odin's 1,024 frames; both reports record that limit and the 100-million-instruction budget.
An initial smoke check hit Rust's 50-frame default. A fixture check also rejected `len(bytes)`; nonempty artifacts now use byte-value comparison.
Those failed checks contribute no timing evidence. Runner tests, harness contract tests, and workspace clippy pass.

The compiler capture passes all 18 processes at Rust `174e017` and Odin `bfb368c`.
Source-to-artifact compilation takes 23.4 ms in Rust versus 9.0 ms in Odin, leaving a 2.6-times gap.
Emission peak RSS is approximately 31.7 MiB versus 34.1 MiB. Rust native-enabled execution remains near its interpreter result.
The frontend control measures 36.0 ms versus 37.6 ms. Different input sizes prevent subtracting these timings to isolate emitter cost.
A separate profile finds substantial reference-count traffic and frame cleanup. Nested instruction-list growth is a candidate for bounded application-level improvement.

The emitter now accumulates instructions in 64-entry chunks and flattens them once per assembly description.
Jump patches update one chunk. A stored instruction count supplies absolute targets without traversing chunks.
All eleven compiler application tests pass, including bootstrap artifact equality, generated-program execution, exception exits, and suspension.
This bounds instruction-prefix copying within the emitter. A pinned comparison must establish its throughput effect before it is accepted as a performance improvement.

The instruction-chunk capture passes all 18 processes but shows no material gain: Rust emission changes from 23.400 ms to 23.301 ms.
Native-enabled timing and peak RSS also remain similar. The added bookkeeping is reverted; the measurement remains in the evidence.
Parser fact-row accumulation is a separate candidate because it builds a substantially larger nested list.

The parser accumulates AST facts in 64-row chunks and flattens them at its public return boundary.
Node-kind lookup traverses the chunks. Row order, AST identities, diagnostics, and the public relation/list results retain their existing representation.
All eleven compiler application tests and workspace clippy pass. The bootstrap still produces byte-identical target artifacts in both modes.
The debug test run falls from roughly 182 seconds to 52 seconds, but a pinned capture must measure the throughput change independently.

The fact-chunk capture passes all 18 processes at Rust `f3ca3e2` and Odin `bfb368c`.
Rust frontend latency falls from 36.0 ms to 19.6 ms, approximately 46%. Source-to-artifact latency falls from 23.4 ms to 19.5 ms, approximately 17%.
Odin measures 37.1 ms and 8.8 ms respectively. Rust now leads on the frontend workload but retains an approximately 2.2-times emission gap.
Process peak RSS stays similar. Native-enabled Rust execution remains close to the interpreter result.
The parser change is retained. Public installation, broader compiler capabilities, and the remaining performance workstreams are still open.

Assembly now exposes the VM's existing positional and named-role spawn instructions.
Both suspend through the task interface, preserve arguments and optional delay, and receive the continuation value in the destination register.
Assembly tests cover splices, role maps, delays, invalid operands, and live registers across continuation. Runtime clippy passes.
Receiver dispatch and spawn emission remain the next compiler application step.
