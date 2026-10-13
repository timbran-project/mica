# Native tasks

This module generates task execution, transaction retry, boundary delivery, timed suspension, mailbox waits, and native worker scheduling.
It composes the generated kernel and heap.
The generated process uses allocation, atomics, and pthread primitives from the platform layer.
Rust runs the generator and test harness only.

`native_tasks/program()` produces the runtime IR.
`native_tasks/test_program()` adds generated entries and a rendezvous for concurrency fixtures.
The fixtures call those entries through the same task protocol.

## Ownership

`TaskControl` occupies stable native storage.
Its mutex protects state, wake generations, cancellation, and the active transaction pointer.
Its registered heap roots retain the current image, prepared boundary, and early reply.
The owner retains the control and kernel until all workers and event producers relinquish them.

`TaskAttempt` belongs to one worker.
It holds the transaction, retry image, current image, staged effects, and authority.
Closing an attempt removes its roots in reverse order.
Images and effect chains are immutable after construction.
Each image can retain its caller continuation; suspension and retry preserve that managed chain.
Publication copies their reachable graphs into the shared heap.

A caller must enter managed execution before operations that accept a worker.
Task-lock waiters leave managed execution before waiting, then enter again after acquiring the lock.
`task_cancel` is an event-thread operation outside managed execution.
Release requires exclusive ownership after worker and event activity stops.

## Execution and publication

`task_execute` claims a runnable control and opens a fresh attempt.
The image supplies a generated entry with the `TaskEntry` ABI.
The entry advances the current image, stages effects, and requests completion, a wait, or continuation.

The source compiler emits one shared native body per Mica program, with separate entry wrappers for its resumptions.
Each wrapper selects the restoration of its captured environment and delivered input before entering the shared control-flow graph.
The body roots source registers for the duration of that entry and unlinks them before the wrapper makes a tail transfer.
All entry points allocate the program's shared slot layout.
Shared blocks execute directly, without creating extra task images or transaction boundaries.

Register roots occupy one zeroed native array. A loop registers its slots, keeping initialization code independent of the register count.
Values reload through these slots after collection. Success and failure both restore the enclosing root chain.

Registers with disjoint live intervals share slots. Registers needed within the same instruction keep separate slots, including operands, destinations, and exception captures.
Resumptions restore only registers live at their entry, so dead values cannot overwrite reused slots.

`native_source/compile(module, prefix, source, relations, identities, method_ids)` accepts explicit identity bindings.
The `identities` map binds bootstrap identity values to native-world numeric IDs.
Prototype restrictions, identity constants, and identities nested in supported literal containers use those bindings.
An unbound identity or an invalid target ID fails compilation. Bootstrap identity numbers are never copied implicitly.
Installation preserves each parameter's prototype restriction alongside its position, mode, and default.
The native dispatcher applies the current transaction's delegation graph and primitive-prototype configuration.

Preparation orders the effects and publishes an immutable `TaskBoundary` before the kernel commit.
The kernel memory control bounds preparation and checks cancellation during graph allocation and copying.
A conflict discards that boundary and restores the retry image.
Retry retains the segment authority and delivered input.
A scheduled resumption opens a fresh transaction and obtains authority from current policy.

The kernel publication outcome determines whether replay is legal.
After publication, errors cannot authorize replay.
`task_delivery` permits one claimant to acquire the boundary and its ordered effects.
`task_acknowledge` records the delivery outcome and advances the task state.
`task_dispatch` invokes the generated service entries in effect order, then acknowledges the boundary.
Services receive `(worker, context, kernel, snapshot, tenant, effect)`.
The traced effect contains the target, payload, and any typed subscription request.
The snapshot is the exact committed view, including the view selected by a read-only commit.
Preparation registers an empty shared root; successful commit fills it with the frozen candidate without allocating.
This root retains the view after the attempt ends and commit history advances.
Acknowledgement or failed preparation releases it.
Subscription delivery must still refresh authority before exposing historical rows to a tenant.
Each effect gets one delivery attempt.
If a service fails, dispatch preserves the first error and attempts the remaining committed effects.
An unsuccessful delivery leaves a failed, committed task.
This protocol does not provide durable external delivery across process failure.

A wait boundary carries a generation token.
`task_wake` accepts one reply for that token, including during delivery before acknowledgement.
An early reply remains rooted until acknowledgement completes.
Only then can the continuation become runnable.
Duplicate and stale replies return `kernel_closed`.
Generation exhaustion returns `kernel_limit` before publication.

Cancellation competes with publication through the kernel transaction signal.
Before publication, cancellation prevents the draft and its effects from becoming visible.
After publication, cancellation can stop continuation but cannot erase committed effects.

`task_mailbox_request` validates receivers and stages a receive request in the attempt.
The prepared boundary retains that request and its receiver list through collection.
Conflict retry discards the request before executing the segment again.
The timeout is a nanosecond duration; `UINT64_MAX` means no deadline.
Registration must start that duration after publication.
The scheduler registers the request after delivery and resumes the task when a reply or timeout wins.

`task_suspend_request` stages a timer without a mailbox.
Its timeout uses the same nanosecond duration and no-deadline sentinel.
A zero duration still commits and schedules a continuation; an indefinite wait requires an explicit wake.
Timers resume with unit, `()`. Mailbox timeouts return an empty list.
`TaskWaitRequest` retains the timeout result through publication and collection.
Cancellation and external replies compete with expiry through the same generation check.

`task_observe` reads task state under its mutex.
`task_fail_wait` records a registration failure only for the matching waiting generation.
It cannot overwrite an accepted wake, cancellation, or a later generation.

## Handler continuations

`TaskHandler` retains a captured image and the outer handler chain.
Catch frames transfer raised errors to generated handler entries.
Cleanup frames run when an error or normal transfer leaves their protected region.
`TaskUnwind` retains the pending target, value, and remaining handler chain while cleanup executes.
These records are immutable and participate in collection and publication with the task image.

`task_handler_push` installs a frame in the current image.
`task_raise` starts error propagation, and `task_leave` exits toward a captured target.
Both operations return a status after selecting the next image.
Generated regions then tail-call that image's entry.
Catch matching and source control flow belong to those generated entries.

`task_cleanup_done` resumes the pending transfer.
Suspension preserves that transfer separately from the delivered input.
A caught error inside cleanup preserves the pending transfer.
An escaping error or return replaces it.
Conflict retry restores the original frames from the segment checkpoint.
Cancellation and runtime failures discard the continuation without executing cleanup.

An unhandled language error aborts the current draft and returns `task_language_error`.
`task_error` acquires a root for the retained error value.
If error publication fails, the task reports allocation failure and remains uncommitted.
The caller releases the acquired root before its worker exits.

## Ready queue

The bounded FIFO contains scalar indexes into an admitted array of task controls.
The owner keeps the array stable until every consumer exits.
`task_drain` processes segments from the queue, dispatches their effects, and records task outcomes.
Each submitted index represents one scheduling request.
The owner submits continuations after acknowledgement or wakeup makes them runnable.

An empty blocking queue leaves its consumer outside managed execution.
Collection can therefore proceed while all consumers wait.
Closing the queue rejects submissions and wakes consumers.
Consumers drain existing entries before they exit.
Release requires a closed, empty queue after all consumers exit.

## Scheduler

`TaskScheduler` starts empty with a bounded pool of slots.
`task_scheduler_submit` admits initialized controls while native workers are running or idle.
Each admission gets a non-reused public ID; the ready queue uses private slot indexes.
Slot reuse cannot redirect a stale wakeup or cancellation to another task.
Admission can delay the first segment until a monotonic deadline.
The scheduler tracks delayed, queued, running, waiting, and finished slots.
`task_scheduler_worker` executes segments, delivers committed effects, and registers waits.
It keeps the slot running through delivery, so an early reply cannot start concurrent execution.

`task_scheduler_events` scans registered waits and enqueues accepted replies or expired deadlines.
It consumes existing notifications before scanning; notifications that arrive during the scan remain pending.
A nonblocking pipe carries readiness signals, and a monotonic clock supplies deadline measurements.
Both native workers and the event loop leave managed execution before blocking.
The current event loop scans the admission table once per notification batch.

After admission, route wakeups and cancellation through the scheduler entry points.
These operations notify the event loop and preserve the one-entry-per-slot invariant.
`task_scheduler_stop` runs outside managed execution, cancels admitted tasks, and wakes parked threads.
An empty scheduler stays open until explicit shutdown.
Release follows worker and event-loop exit.
The scheduler releases owned controls automatically when they finish.
Owned controls must use storage from `memory_platform_allocate`.
Successful owned admission transfers that storage; callers must use the public ID instead of retaining the control pointer.
For caller-owned controls, `task_scheduler_withdraw` retires the finished slot before the caller releases the control.
Direct control release fails while the control belongs to a scheduler.
A failed admission leaves ownership with the caller.
Stopping before workers start also permits release of reserved queue entries and owned controls.

## Mailboxes

`TaskMailboxHub` owns bounded queues and their shared heap roots.
Its mutex protects lookup, message links, receive selection, and closure.
Queue linking takes constant time after lookup; publication can copy the payload graph.
Receive prepares groups in receiver order, preserves FIFO order, and skips duplicate receivers.
It removes messages only after `task_wake` accepts the complete reply.
Preparation failures leave queued messages intact.
If cancellation or a stale generation rejects the wake, queued messages also remain intact.

One base runtime owns the capability namespace.
Call `task_resources_start` before opening attempts or resource registries.
Startup and shutdown require exclusive ownership. Shutdown rejects active attempts, schedulers, and hubs.
Mailbox identities survive namespace restart and are never reused.
Send and receive capabilities have distinct kinds; closing a queue invalidates both.
Hubs reject capabilities issued for another hub.

`task_mailbox_service` connects committed send effects to a hub.
Direct `task_mailbox_send` reports delivery status to external producers.
Creation and closure manage ephemeral resources outside relation transactions.
Source builtins expose creation, closure, and transactional sends. Receive instructions suspend through the mailbox service.

`task_mailbox_stream_open` registers a tagged stream with a positive message budget.
Its ID comes from `task_resource_reserve` and must never be reused.
Each empty stream reserves one mailbox slot, so ordinary sends cannot consume the space needed for a recovery marker.
Registration returns `kernel_limit` when the mailbox has no unreserved space.

`task_mailbox_stream_send` receives a payload, an overflow marker, and a terminal flag.
A budget or mailbox-capacity overflow replaces that stream's queued messages with the overflow marker.
A terminal delivery replaces them with its supplied payload, for example a revocation marker.
Both replacements stop further sends on the stream. Unrelated messages preserve FIFO order.
The result reports `status` and whether replacement occurred.
Allocation failure leaves the queue and stream state unchanged.

Receiving messages restores the empty-stream reservations.
`task_mailbox_stream_close` releases the reservation and stops future delivery; already queued messages remain valid.
Mailbox closure or hub release frees all remaining stream controls.
These operations supply bounded transport for subscriptions.

`task_subscription_request` stages a typed registration and returns a capability from the process-wide namespace.
It checks the sender capability, subject, binding width, column mask, cursor mode, and current read authority.
Catalogue event requests require root authority.
Subscriptions to catalogue projections require catalogue authority at registration and delivery, as direct reads do.
Stored-fact subscriptions reject computed projections. Relation schema and policy dependencies participate in commit validation.
The request retains its pattern and full-width cursor through collection and publication.
`task_subscription_cancel` stages cancellation in the same effect sequence.
Retry discards both operations and never reuses an abandoned registration identity.
Staging neither opens a stream nor consumes mailbox capacity.

Source code uses `subscribe_changes(sender, subject, relation, bindings, initial[, cursor[, queue_budget]])` and `cancel_subscription(capability)`.
Subjects are `:facts`, `:relation`, or root-only `:catalogue` events.
The relation is `some(identity)` or `some(name)`; name lookup requires catalogue authority and observes the transaction's draft catalogue.
Bindings are a list of options, with `none` for each unbound column.
Catalogue events require `none` for the relation and an empty binding list.

Initial mode is `:changes` or `:snapshot`; snapshot mode rejects a resume cursor.
The optional cursor is `none` or `some(nonnegative_integer)`. The positive queue budget defaults to 64 messages.
Invalid argument shapes raise catchable `E_INVARG`; resource, cancellation, and authority failures preserve their runtime status.
Both calls stage effects for commit. Cancellation returns unit.

`TaskSubscriptionHub` owns a bounded registry for one kernel and mailbox hub.
Initialize its vocabulary from the runtime symbol table before entering managed execution, then pass it to hub initialization.
The hub copies scalar symbol IDs; the symbol table must outlive the subscription messages.
`task_subscription_service` activates committed requests and rejects delivery from another kernel.
Each registration owns its request, an independent history cursor, and the exact initial snapshot when requested.
Changes-only registrations retain no initial snapshot. A supplied cursor remains a full-width integer.
Admission reserves a mailbox stream only after all registry allocation and root registration succeeds.
An admission failure leaves no stream and is reported as a committed delivery failure.

Committed cancellation looks up the subscription capability in that registry and releases its stream and roots.
The capability can be delegated; cancellation does not require impersonating the registering tenant.
An aborted cancellation leaves the registration active. Already queued messages remain valid.
Closing the hub rejects admission and retires its entries, including entries whose mailboxes have already closed.
Release requires exclusive ownership after delivery workers exit. The kernel and mailbox hub must remain alive until release.
Authority refresh uses the current world before selecting a retained snapshot or history batch.

Message builders produce row batches, catalogue batches, and recovery markers with the documented symbol keys.
They build each map with one constructor call and retain borrowed payloads through their caller's roots.
Cursor conversion rejects values above Mica's maximum integer; it never truncates a native counter.

`task_subscription_rows` selects stored or complete relation changes between retained snapshots.
A null starting snapshot requests all matching rows. Selection skips shared row subtrees and applies bound-column filters.
Its operation budget covers traversal, result rows, allocation, and cancellation.
The caller must refresh read authority from the current world before exposing historical rows.

Delivery uses `task_subscription_claim` to capture the request, initial view, and cursor under the registry lock.
The caller roots those references before scanning outside the lock. One claim can be active for each registration.
`task_subscription_accept` checks the claim generation before enqueueing and advances the cursor only after queue acceptance.
A scan failure calls `task_subscription_unclaim`; delayed cleanup cannot release a later claim.
Cancellation can retire an entry during selection. A subsequent acceptance then delivers nothing.
Overflow or terminal delivery retires the entry after its marker reaches the mailbox.
`task_subscription_pump` performs one claimed batch, including authority refresh and queue acceptance.
It advances over unrelated commits without queueing an empty change message.
An initial batch uses the registration's retained snapshot, even if history has since expired.
A later history gap produces a resynchronization marker and retires the registration.
Payload freezing occurs before taking the registry and mailbox locks.

`task_subscription_catalogue_entries` compares immutable catalogue indexes.
It selects relation creation and removal, and rule installation, replacement, deactivation, and removal.
The current kernel has no relation-removal operation; that selection path supports retained snapshot differences.

`task_subscription_events` drives delivery on a native worker.
It attaches the world's notification endpoint, then copies registration IDs under the registry lock.
Each pass processes one batch per registration outside that lock.
Commits, admission, competing-claim completion, and shutdown signal a nonblocking pipe.
The loop drains signals before scanning and leaves managed execution before waiting.
The current dispatcher scans the bounded registration table per notification batch.

The caller supplies per-batch step, row, round, and byte limits with zero status and no cancellation cell.
Closing the hub cancels in-flight selection through its own atomic cell and wakes an idle dispatcher.
A budget or allocation failure returns its status, preserves the unaccepted cursor, and detaches the dispatcher.
The owner can restart it after addressing that failure. It must join the dispatcher before releasing the hub.
Only one dispatcher owns a world's endpoint; competing attachment cannot replace it.
The kernel invokes only the nonblocking signal while holding its publication mutex.
Detachment excludes in-flight callbacks before the event context is freed.

`task_mailbox_changed` watches the hub revision outside managed execution.
It checks the revision under the hub lock before waiting, so intervening sends cannot lose their notification.
One event dispatcher can watch a hub containing many suspended receivers.
The hub retains no task pointers.
Shutdown wakes watchers; release follows their exit and removes every mailbox root.

## Code ownership

`TaskCodeRegistry` owns loaded library handles. A managed `TaskCodeRef` retains each handle, its constants, and its direct module dependencies.
The registry does not root these references. Unreachable dependency cycles can therefore be collected.
Initialization and release require exclusive ownership of a zero-initialized registry.
The registry retains an empty heap root until release, which prevents premature heap destruction.

`task_code_register` transfers a library handle only on success. Its output must be a registered root of the calling worker.
On failure, the caller still owns the handle. `task_code_depend` and `task_code_constants` construct the private dependency graph.
`task_code_seal` closes that graph to mutation. Publication rejects unsealed references, including unsealed dependencies.

`TaskCompiledProgram` pairs a native entry with its code reference.
The kernel's program catalogue retains these managed graphs in transaction snapshots.
Compiled-body changes publish with facts; rollback discards staged bodies, and older snapshots retain replaced bodies.
`task_invoke` connects named-role selection and selector authority to the transaction's `MethodProgram` relation and compiled-body catalogue.

`TaskImage.code` retains the selected code version through suspension and retry.
`native_tasks/entry_context` pins that version in `TaskAttempt.executing` before the entry can reach a safepoint.
This separate root protects the executing module when a helper advances the current image to another module.
A required tail transfer removes the caller frame before the next entry replaces the pin.
Permanently linked runtime entries use a null code reference. Loaded entries require a sealed reference.

`task_code_reap` first requests a collection. A failed collection releases no handles.
Successful tracing marks the native owners of reachable references. The reaper detaches unmarked owners under the heap mutex.
It leaves managed execution before calling `dlclose`, without a runtime lock held.
A failed close restores ownership for a later attempt. Concurrent reapers cannot detach the same owner.
`task_code_close` rejects further registration and mutation. Release requires no remaining owners or in-flight retirements.

## Dispatch helpers

`task_dispatch_reaches` traverses a `Delegates` relation acquired with `kernel_read` in the calling transaction.
It follows all ranks, handles cycles, and charges scans and scratch allocation to the supplied budget.
The caller can retain that relation view across later draft changes and collection.

Initialize `TaskDispatchVocabulary` with the runtime symbol table before entering managed execution.
`task_dispatch_signature` validates six-column `Param` rows, orders them by position, and builds a role index.
It rejects duplicate roles or positions, mixed method IDs, invalid modes, and invalid required/optional/rest ordering.
`task_dispatch_bind_named` takes that immutable signature and a map of named arguments.
It supplies optional defaults and reports missing required roles as inapplicable.
Rest arguments accept a supplied list or default to an empty list.
Additional call roles are allowed.
`task_dispatch_matches` checks exact values, unrestricted parameters, primitive prototypes, and frob delegation.
The native world supplies primitive identities in value-tag order; the matcher does not assume another runtime's identity numbers.
`task_dispatch_implies` compares restrictions through the same graph, including equivalent restrictions in delegation cycles.

The caller supplies a private cache root for one selection operation.
Cache entries retain complete reachable sets and their immutable relation views, including negative results.
A draft rewrite gets a separate entry. A failed traversal cannot install a partial result.
Discard the cache when selection ends.

`task_dispatch_resolve` reads `MethodSelector`, `Param`, and `Delegates` through the calling transaction.
It binds candidate arguments, checks restrictions, and removes candidates dominated by a more specific signature.
Named rest outranks unrestricted optional: both accept omission, but a supplied rest argument must be a list.
An additional unrestricted optional role creates no priority. Equivalent restrictions in delegation cycles create no priority.
The result reports zero matches, one selected method with bound arguments, or multiple ambiguous matches.
Failures return a separate status and clear the result payload.
Each call observes staged catalogue and delegation changes; its private cache ends with the selection.
`task_invoke_authorize` checks the current segment's selector-specific invocation rights.
Effective invoke policy contains `(tenant, selector)` rows. A compiled selector index keeps checks independent of policy relation scans.
Relation rights and selector rights use separate indexes, even when their numeric identifiers match.
The check records a policy dependency for conflict validation.

`task_invoke_mint` creates an ephemeral capability for one selector and the segment's authority epoch.
`task_invoke_check` rejects a different selector, an unknown token, or a different epoch.
Conflict retries retain the epoch and authority. Scheduled resumptions obtain fresh authority, a fresh epoch, and an empty token index.
The resource namespace never reuses token or epoch identifiers, including after restart.
Images can carry capability values, but cannot retain authority caches or token indexes.
`task_invoke` resolves `MethodProgram(method, artifact)` and looks up the artifact in the compiled-body catalogue.
It rejects malformed or duplicate bindings, invalid body layouts, and unsealed code.
A successful selection installs a callee image with declaration-ordered arguments as its environment.
The selected code reference and caller continuation remain reachable through that image.
Errors, missing methods, and ambiguous selection leave the current image unchanged.
The result retains the selection API's distinct status and match count; callers must check both before transferring.

A supplied continuation makes a normal call and increases logical depth.
A null continuation makes a tail call and preserves the existing return target.
`task_return` passes the result to the caller through the existing unwind machinery, running cleanup handlers along the way.
Returning from the outermost method reaches a generated completion entry.
Generated entries use required tail transfers after successful invocation and return helpers.
General source lowering to these operations remains separate work.

## Child admission

`task_spawn_request` captures a selector, role map, and delay in nanoseconds in the current attempt.
The generated entry first saves its continuation, then requests the spawn boundary and returns.
Rollback and conflict retry discard the request with the other staged effects.

After publication, `task_spawn_service` creates the child image and calls `task_scheduler_launch`.
The scheduler owns an admitted child control and releases it after completion or cancellation.
The service delivers the integer child ID to the parent before acknowledging the committed boundary.
A failed reply cannot undo admission or replay the committed effect.
Scheduler exhaustion and shutdown reject admission without retaining a child control.

The child image holds the captured invocation, without a selected method or authority cache.
When scheduled, the child opens a fresh transaction, obtains current authority, and resolves its selector.
Method replacement and policy revocation during a delay therefore affect the child's invocation.
The child inherits the parent's tenant; named arguments cannot change that authority.
Selection consumes the child's logical execution budget.
Missing and ambiguous selections produce distinct native outcomes, `task_no_method` and `task_ambiguous_dispatch`.
Language error construction remains a source-lowering responsibility.

`TaskSpawnContext` holds catalogue IDs, vocabulary, child limits, and a published prototype root.
Initialize and release it exclusively; keep it and the scheduler alive until delivery workers stop.
Services borrow the parent control during delivery. They never store that pointer in a managed image or effect.
Lowering must capture ambient `principal`, `actor`, and `endpoint` roles before staging, where the language contract requires them.
The native request API receives the completed role map.

## Validation

Run the bootstrap-tier comparison and sanitizer fixtures:

```sh
CC=clang cargo test --release -p mica-runtime --test native_codegen \
  native_tasks_execute_on_both_mica_tiers -- --nocapture
```

The test requires identical generated C from both bootstrap tiers.
`MICA_NATIVE_THREAD_SANITIZER=1` selects ThreadSanitizer instead of AddressSanitizer and UndefinedBehaviorSanitizer.

Run the C, libgccjit object, and libgccjit shared-library fixtures:

```sh
cargo run --release -p mica-value-comparison --bin native-backend -- \
  --check-fixtures --fixture tasks --sanitize --output /tmp/mica-native-tasks
```

Fixtures cover conflict replay, effect order, allocation failure, worker migration, authority renewal, wake races, publication races, and root retirement.
Tests use small nurseries and forced collection at legal points.
One fixture holds the publication lock while a commit worker parks, then forces collection before releasing the lock.
The test functions contain assertions for world state, task state, and effect delivery.
Mailbox fixtures cover allocation failure, capability lifetime, competing receives, cancellation, watcher shutdown, and collection after producer exit.
Handler fixtures cover nested cleanup, suspension, worker replacement, caught and escaping errors, conflict replay, cancellation, and limits.
Timer fixtures cover zero and finite durations, explicit wake, cancellation, deadline overflow, collection, and resumption on scheduler workers.
Expiry races against cancellation and external wake check single enqueue, stale-reply rejection, and wait-root retirement.
Code fixtures load separate generated libraries and cover dependency cycles, constant retention, failed collection, suspension, worker replacement, and competing reapers.
An active-entry fixture advances its image, then forces collection while the previous library still has a native frame.
Service fixtures overwrite commit history before delivery and collect inside each service while checking the retained snapshot version.
Catalogue fixtures replace loaded code on another worker, retain an older snapshot, resume a suspended entry on another worker, and reclaim aborted installations.
Method-call fixtures cover staged bindings, rollback, conflict retry, selector rights, normal and tail returns, cleanup, and replacement during suspension.
Spawn fixtures cover rollback, conflict retry, delayed resolution, policy revocation, admission and reply failures, cancellation, and concurrent parent/child execution.

Native `lower(text)` uses locale-independent Unicode 15.1 lowercasing, including final sigma and the two-scalar mapping for `İ`.
`string_equal_fold(left, right)` compares the resulting lowercase strings, as specified by the language guide.
It does not normalize text or apply full case folding: `Straße` and `STRASSE` remain unequal.
Wrong argument counts or types raise catchable `E_INVARG` errors.

Compact lookup tables contain the Unicode mappings and case-context properties, with source hashes and the Unicode licence notice.
The string walk and sigma lookahead charge work and check cancellation and collection.
Output remains rooted during construction and is published only after the operation succeeds.

Native `edit_distance(left, right)` computes Levenshtein distance over Unicode scalars, with unit insertion, deletion, and substitution costs.
Its rooted scratch arrays scale with the shorter input. Inner-loop checks charge work and permit cancellation and collection.

The generated string-literal codec uses double quotes and preserves readable Unicode.
It emits `\"`, `\\`, `\0`, `\t`, `\n`, and `\r` as short escapes.
Other controls and invisible characters use lowercase `\u{hex}` escapes, without leading zeroes.
The classification uses Unicode 15.1 categories Cc, Cf, Zl, Zp, and Zs (except ordinary space), plus Default_Ignorable_Code_Point.
Visible combining marks remain unchanged. Escaping a format character preserves its value, including within an emoji sequence.

`literal_quote` writes a string token. `literal_read_string` accepts a byte offset and returns the cursor after its closing quote.
The reader accepts one to six Unicode hex digits, including uppercase digits and leading zeroes.
Unknown escapes and malformed Unicode escapes remain literal, matching the existing reader; `\xNN` is not decoded.
Both helpers bound work and allocation, poll for collection, and return distinct syntax, type, limit, and cancellation outcomes.
Native `from_literal(text)` uses a dedicated value reader, independent of the full compiler.
It reads strings, numbers, booleans, symbols, bytes, lists, maps, relations, ranges, errors, options, results, and raw identities with optional frob payloads.
Rejected text returns `err(error(E_PARSE, "invalid literal", text))`; a non-string argument raises `E_TYPE`.
Resource limits and cancellation remain runtime outcomes. Nested helpers share one budget, and nesting depth is bounded at 64.
Native `to_literal(value)` writes these value kinds using one shared output buffer across recursive calls.
Strings and quoted symbol names follow the Unicode quoting policy above; bytes use padded URL-safe Base64.
Capabilities, functions, and containers holding them raise `E_INVARG`.
The writer shares the caller's budget and bounds nesting at 64. Limit and cancellation outcomes remain distinct.
Identity literals use raw numeric IDs; named identity lookup remains unfinished.

The internal numeric literal writer emits decimal integers and shortest scientific binary32 text.
It uses six stack limbs for exact rounding intervals and shares the caller's allocation and work budgets.

Native `is_builtin(symbol)` reports the source backend's supported builtin inventory, including its variadic calls.
Compiler forms and Rust-only builtins return false. Invalid arguments raise catchable `E_TYPE` or `E_INVARG` errors.

Relation-value builtins include `project`, `union`, `difference`, and `natural_join`, with spliced arguments and headings wider than query masks.
Union and difference merge ordered rows. Natural joins index shared columns and retain each shared column once.
Join keys use canonical value equality: integer `1` and float `1.0` remain distinct.
These operations charge work and allocation budgets, poll for collection, and preserve the output on cancellation or failure.
The algebra fixture compares results with Rust-hosted Mica and checks resource failures under ASan and UBSan.

The source-backend lexer check compiles the `lex` verb directly from `apps/compiler/lex.mica`.
It compares complete tokens and diagnostics with Rust-hosted Mica and repeats each input with forced collection.
The fixture supplies catalogue identities; it does not execute the filein identity declaration or bootstrap the parser and emitter.

```sh
CC=clang cargo test --release -p mica-runtime --test native_codegen \
  native_source_executes_the_compiler_lexer -- --nocapture
```

The frontend dependency check covers the lexer, parser, AST readers, emitter, and their nested functions.
It validates the emitted instruction forms and resolves builtin, method, and catalogue dependencies.
It excludes `emit_source`, which serializes Rust bytecode; native compilation consumes `emit_program` descriptions.
This gate checks dependencies. The frontend execution gate below checks the complete source-to-register-description path.

```sh
cargo test --release -p mica-runtime --test native_codegen \
  native_source_supports_compiler_frontend_dependencies
```

The C generator dependency check covers the core generator modules and their scalar and value examples.
It checks every method and nested function for supported instructions, builtins, and resolved method calls.
It excludes no methods. These sources contain no catalogue queries.
The gate establishes dependency closure. The execution check below exercises the C emitter itself.

```sh
cargo test --release -p mica-runtime --test native_codegen \
  native_source_supports_c_generator_dependencies
```

The C emitter check compiles the example builders, `native/emit_c`, and their transitive method dependencies into the generated runtime.
It follows calls inside nested functions and preserves the original verb sources.
The runtime receives an example selector, then builds the native IR and emits C entirely in Mica.
Empty, scalar, value, and structured-builder examples must produce byte-identical C to Rust-hosted Mica, with ordinary and forced collection.

The structured example uses captured builder callbacks, a range loop, and conditional `continue`.
The resulting C must compile. Independent assertions check the value example and the structured loop's results.
Both bootstrap tiers must also produce identical code for the generator.
The sanitizer harness uses separate function sections so the linker can extend calls across large AArch64 modules.

```sh
CC=clang cargo test --release -p mica-runtime --test native_codegen \
  native_source_executes_the_c_emitter -- --ignored --nocapture
```

This gate executes authoring builders and C emission on the generated runtime.
Native lowering of Mica register descriptions and a complete self-hosting rebuild remain separate acceptance gates.
Generation and sanitizer compilation take several minutes, so ordinary test runs exclude this check.

The full parser check compiles the lexer and every verb in `apps/compiler/parse.mica`.
It compares complete `parse` and `parse_rows` results with Rust-hosted Mica, including diagnostics for malformed inputs.
Each input also runs with forced collection. Both bootstrap tiers must produce identical C output.
The large fixture uses `-O0` with ASan and UBSan; smaller source tests exercise optimized C.
Generation and compilation take several minutes, so ordinary test runs exclude this integration test.

```sh
CC=clang cargo test --release -p mica-runtime --test native_codegen \
  native_source_executes_the_compiler_parser -- --ignored --nocapture
```

The complete frontend check compiles the existing lexer, parser, AST helpers, and emitter together.
The generated runtime receives source text and returns register descriptions or diagnostics from the failing stage.
It compares complete results with the same compiler sources on Rust-hosted Mica.
The corpus includes nested functions, patterns, loops, handlers, type annotations, and syntax and semantic errors.
Unknown escapes and malformed Unicode escapes retain their literal contents under the agreed quoting contract.

Each case also runs with forced collection, and both bootstrap tiers must produce identical C.
The large fixture uses `-O0` with ASan and UBSan. Smaller source tests exercise optimized C.
Ordinary test runs exclude this integration test because generation and C compilation take several minutes.

```sh
CC=clang cargo test --release -p mica-runtime --test native_codegen \
  native_source_executes_the_compiler_frontend -- --ignored --nocapture
```

This gate ends at `emit_program` register descriptions.
Native lowering of those descriptions, live installation, and the complete self-hosting rebuild remain separate acceptance gates.
The fixture supplies catalogue identities and excludes `emit_source`, which calls the Rust bytecode assembler.

## Remaining integration

The broader integration gate is in `sketches/NATIVE_TASKS_AND_MEMORY_DESIGN.md`, section 16.1.
`TaskServices` defines entry slots for emission, mailbox sends, subscriptions, and child requests.
The service context has stable storage and owns roots for its managed state.
The generated spawn service admits children and delivers their IDs to waiting parents.
General source lowering, language-level method installation, transactional buffers, and live compiler qualification remain unfinished.
Method facts and compiled bodies can already be staged in one kernel transaction; `task_invoke` resolves and executes that view.
Source lowering for catch patterns, guards, and structured exits still needs integration with the handler primitives.

Entries explicitly charge logical work through `task_poll`.
The current budget persists across retries and waits.
Language-specific budget renewal and task-wide resource accounting still need integration.
These limitations prevent a claim that the complete language task contract is implemented.
