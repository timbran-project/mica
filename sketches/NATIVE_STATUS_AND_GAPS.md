# Native Mica: current state and remaining gaps

Assessment date: 14 October 2026. Validation date: 26 November 2026.
Reviewed commit: `4c2da75`, with the local changes described below.

The validation results below come from focused and ignored integration runs on this worktree. They do not represent a complete workspace test run.

## Where we are

We have substantial Mica-written generators for a native runtime, plus native execution of the existing Mica-written compiler and generator in integration fixtures.
A constrained native executable now compiles and executes one Mica source file without Rust runtime services after process start.
This one-shot fixture is not a general self-hosted system.
General catalogue ownership, source replacement, persistence, and successive self-hosting remain unfinished.
Language coverage, resource accounting, and performance qualification also remain incomplete.

The endpoint is a Mica-written system that compiles and executes Mica, manages its live world, and supports transactional tasks across native threads.
Rust can produce the initial bootstrap and provide comparison results. It must not supply the resulting system's runtime services.
Using a platform C compiler, libgccjit, or operating-system primitives does not contradict that endpoint.

Capability coverage and measured performance are separate obligations. Passing a compiler rebuild does not establish either full language coverage or acceptable application performance.
After completing the capabilities and measured improvements, review the remaining performance gaps with the maintainer.

## What exists

These entries describe implementation, not a claim that every associated test currently passes.

| Area | Current implementation | Remaining boundary |
| --- | --- | --- |
| Native generation | Typed IR, semantic builders, ownership and relocation analysis, structured C emission, module interfaces, and a generated libgccjit backend. | Generation cost remains substantial. Backend reproduction alone does not establish whole-system self-hosting. |
| Values and memory | Scalars, identities, strings, Unicode helpers, symbols, collections, persistent maps, relation values, codecs, worker nurseries, publication copying, and shared-heap collection. | Collection pauses, retained snapshots, module retirement, and per-task resource limits need integrated workload evidence. |
| Relation kernel | Indexed snapshots, transactional declarations and writes, conflict detection, candidate preparation outside the publication lock, and atomic publication. | Relation removal, transactional buffers, and durable world storage remain separate work. |
| Queries and rules | Composable query plans, recursive stratified rules, transactional rule definitions, draft visibility, affected-component maintenance, and deletion recomputation. | Some operators materialize intermediate results. Maintenance still traverses metadata and compares changed stored rows. |
| Authority and dispatch | Snapshot-derived authority, selector-specific invocation checks, epoch expiry, prototype dispatch, catalogue checks, and target-specific emission rights. | Complete tenant workflows need integration coverage across compilation, installation, execution, and host services. |
| Tasks | Retry, commit boundaries, continuations, suspension, native worker scheduling, mailboxes, child tasks, subscriptions, and staged effects. | Complete language-level accounting and production host integration remain unfinished. External delivery is not crash-durable. |
| Source compilation | Existing lexer, parser, and register-description emitter feed native analysis and lowering. Closures, calls, control flow, handlers, relation operations, and task boundaries have implementations. | Supported constructs and builtins are not the complete language/runtime surface. General catalogue setup and source management still depend on harness orchestration. |
| Code installation | Generated installers stage method facts and compiled bodies together. Code references retain constants, dependencies, and versions across snapshots and suspended tasks. | General source submission, replacement, and retirement workflows are still missing. The standalone fixture loads one application with fixed bindings. |

Implementation entry points:

- [Native generation](../apps/native/README.md), [memory](../apps/native/memory/), and [values](../apps/native/value/).
- [Kernel](../apps/native/kernel/), [queries](../apps/native/query/), and [rules](../apps/native/rules/).
- [Tasks and code ownership](../apps/native/tasks/README.md).
- [Existing compiler](../apps/compiler/), [native compiler entry](../apps/native/compiler/program.mica), and [generated installer](../apps/native/compiler/install.mica).
- [Standalone native process](../apps/native/runner/acceptance.c) and its [acceptance source](../apps/native/runner/acceptance.mica).

The register descriptions are compiler input to native lowering. The generated process does not interpret them as bytecode.
The compiler already exists in Mica. It does not need to be rewritten from Rust.

Several older status paragraphs in the native README and design sketches describe implemented components as missing.
For example, task execution, dispatch, and rule maintenance now exist. Those historical inventories must not determine the next work item.

## What the evidence establishes

The [native integration suite](../crates/runtime/tests/native_codegen.rs) contains focused fixtures for values, the kernel, tasks, source features, separate modules, and compiler execution.
Many compare generated artifacts across both Rust bootstrap execution tiers and execute generated C with sanitizers.
Test presence alone is not evidence that the current worktree passes the suite.

The compilation-pipeline fixture runs `native_source/compile` on the generated runtime, starting from source text.
It compares generated IR and C, then compiles and executes the resulting applications against a separate runtime.
Its small source corpus demonstrates composition of the pipeline. It does not establish complete application compatibility.
Rust still constructs the fixture inputs and orchestrates compilation, linking, and execution.

A separate runtime-generation diagnostic completed before `4c2da75`:

- The native process executed the Mica generator and produced runtime C matching the bootstrap output.
- Interface metadata differed in effect ordering. Commit `4c2da75` fixed that ordering, with focused regression coverage.
- That diagnostic took 247.46 seconds and consumed about 2.50 billion logical steps, with 934 collections.
- It used sanitizers and a 1 GiB managed-heap limit. Peak process RSS was approximately 1.19 GiB. Heap capacity does not bound total RSS.

These are measurements from one diagnostic, not normal startup costs or a Rust-versus-native performance result.
They exclude the preceding host generation and C build costs.
The complete native runtime rebuild gate passed in 724.40 seconds before the standalone runner was added.
The run reused 49 generated bootstrap artifacts, rebuilt eight empty objects, and linked 258 translation units.
It compared ordinary and forced-collection outputs with the Rust oracle, then linked and executed sample applications with sanitizers.
The standalone-runner additions did not alter the runtime generator, and the gate was not rerun after those additions.

The standalone acceptance test passed in 178.41 seconds with a populated compiler cache.
The native executable read the source, ran the Mica compiler, built and loaded the application, and executed transactional tasks.
The fixture checked suspension and resumption, method dispatch, one committed effect, spawned rollback, final facts, and clean teardown.

There is still no demonstrated successive rebuild of the complete compiler and runtime with Rust unavailable after the initial bootstrap.

Existing [comparison tools](../crates/testing/value-comparison/) cover values, symbols, transactions, queries, and rule workloads.
They provide useful infrastructure. They do not establish current whole-application parity, multicore fairness, or bounded collection latency.
Use Rust as an oracle only where the agreed semantics match.

## Remaining challenges

### 1. A usable native source-to-execution path

The public [runner](../crates/runner/src/main.rs) uses Rust `SourceRunner`.
The [native backend tool](../crates/testing/value-comparison/src/bin/native-backend.rs) also uses Rust to bootstrap and orchestrate generated artifacts.
Neither is the final native application runner.

The constrained [acceptance fixture](../apps/native/runner/acceptance.c) now provides the first native source-to-execution path.
Its C host owns process initialization, file access, Clang invocation, dynamic loading, host observation, assertions, and shutdown.
Generated Mica code owns compilation, scheduling, dispatch, transactions, continuations, spawning, effects, rollback, and code ownership.
Rust supplies the initial compiler artifact and fixed ABI. It provides no runtime service after the native executable starts.

The fixture accepts one source file with fixed tenant, relation, identity, and method bindings.
It does not provide general catalogue allocation or live source replacement.
A production native entry point still needs general world initialization and source management.
This constrained result completes the integration milestone, not the full project endpoint.

### 2. Live compilation and catalogue ownership

The compiler currently accepts explicit relation bindings, identity mappings, and method IDs from its caller.
A running world must supply these through coherent APIs instead of test-specific setup.
That includes identity allocation, name resolution, declaration handling, source units, replacement, and ownership of installed definitions.

Code publication primitives already exist. Build the source-level workflow around them rather than replacing them.
Failed compilation or installation must leave the world unchanged.
Replacement must preserve code used by older snapshots and suspended tasks, then retire it when those references disappear.
Compiler work must obey tenant authority and resource limits without holding the publication lock.

### 3. Language and service coverage

Concrete known gaps include:

- Destructuring catch patterns, as recorded in the [task integration limitations](../apps/native/tasks/README.md#remaining-integration).
- Native identity creation and named identity resolution for literal input.
- Language-level method installation and complete filein/unit replacement behaviour.
- Transactional buffers and relation removal.
- General host/service integration and durable world storage.

The [native builtin registry](../apps/native/compiler/builtins.mica) is explicit and incomplete.
The availability fixture deliberately rejects `make_identity`, `assemble`, and `to_xml`.
This does not mean each Rust builtin should be copied: `assemble` targets Rust bytecode, so its native-facing purpose needs separate treatment.
Computed catalogue relations already exist. That does not establish support for every external computed-relation provider.
Value serialization helpers and mutable scratch buffers are not durable storage or transactional user buffers.

Make the remaining coverage inventory against an agreed application and language corpus.
Record unsupported behaviour and deliberate semantic differences separately. Do not infer complete parity from compiler dependency closure.
Do not reopen settled authority, quoting, closure-identity, or rule-maintenance decisions as incidental implementation choices.

### 4. Resource control and multicore behaviour

Scheduling and synchronization exist. Their presence does not establish predictable behaviour under competing tenant workloads.
Task-wide accounting and language-specific budget renewal still need integration.
Row-count polling bounds do not bound elapsed time when a comparison, copy, compilation, or collection processes a large value.

Measure responsiveness with concurrent compilation, queries, updates, suspended tasks, and cancellation.
Distinguish publication-lock time from preparation time, collection pauses, and scheduler delay.
Track memory retained by snapshots, continuations, subscriptions, and loaded code separately from eventual GC reclamation.
Resolve strict/relaxed durability ordering and external-delivery guarantees before claiming persistent operation.

### 5. Performance and practical self-hosting

Recent changes address real costs: persistent map updates, list-prefix sharing, indexed ownership propagation, relocation analysis, and repeated collection.
They do not establish that compiler generation is now cheap or that native applications outperform Rust.

Measure bootstrap generation, native compiler execution, C/JIT compilation, and application execution separately.
Reuse unchanged generated modules during development. Keep generated artifacts and measurement output outside tracked source.
Retain the existing transaction, query, and rule comparisons, then add measurements for the agreed application corpus.

Complete self-hosting needs a reproducible build procedure that uses the native compiler and generator after the initial bootstrap.
Demonstrate the next generation building and running the same system, including module interfaces and installation metadata.
Treat this as a milestone test, not a prerequisite for every local edit.

## Validation in this checkout

The checkout contains uncommitted implementation and test changes.
The focused native checks and the two ignored integration milestones described above pass.

- `crates/runtime/tests/native_codegen.rs` contains the modular runtime-bootstrap harness, caching, and parallel compilation.
  Its cache regression also rebuilds an interrupted empty object and executes the linked result.
- The compiler and subscription C fixtures request collection through explicit pressure.
  Their focused suites passed before the complete runtime rebuild gate passed.
- `apps/native/kernel/tests.c` contains the repaired heap-admission and preparation-cancellation fixtures.
- `apps/native/runner/` contains the standalone host and acceptance source. The cached standalone acceptance test passes.
- `sketches/NATIVE_TASKS_AND_MEMORY_DESIGN.md` contains a separate large draft. This assessment leaves it untouched.

No build or test process remains running. The complete workspace suite has not run.

## Recommended order of work

1. Extend the constrained standalone runner with general source submission and catalogue ownership.
   Preserve native ownership of language and transaction semantics.
2. Expand the agreed corpus and complete missing capabilities in coherent, reviewable commits.
   Keep unresolved semantic choices explicit and separate from implementation defects.
3. Demonstrate a reproducible self-hosting rebuild, then review application performance and remaining gaps with the maintainer.

This order is a recommendation. It does not reduce the endpoint to a successful demo or postpone performance concerns until completion.

For further development, run at most one expensive integration job at a time.
State its purpose and expected cost before starting it, and terminate its process group when work stops.
Run focused checks during implementation and reserve complete rebuilds for relevant milestones.
A failed or stalled check calls for diagnosis, not another overlapping run or an unexplained increase in its budget.
Validation should answer a specific engineering question and then end.
