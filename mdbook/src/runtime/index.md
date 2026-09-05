# Runtime Overview

The Mica runtime executes compiled tasks against a live relation store. The runtime is responsible
for making the language feel direct while preserving the transactional rules that keep a shared
world coherent.

The core runtime concepts are:

- a relation kernel that stores facts, relation metadata, and rules;
- tasks that run bytecode over a transaction;
- a task manager that owns task state and suspended continuations;
- a driver that resumes tasks after timers, input, child-spawn completion, or mailbox readiness;
- hosts that translate protocol traffic into task submissions, input, and effects;
- retrieval helpers that use ordinary relations plus computed search relations to record embeddings,
  retrieved context, and answer artefacts.

The runtime is transactional by default. Code can feel direct and live while still committing state
changes, effects, and mailbox sends at explicit boundaries.

A typical flow looks like this:

1. A host or REPL submits source or a verb invocation.
2. The compiler produces bytecode for a task.
3. The task runs against a transaction and authority context.
4. If the task commits, relation writes become visible and effects are routed.
5. If the task suspends, the driver records why and resumes it later.

The runtime does not require all state to be durable. Endpoint state, capabilities, and mailboxes
are runtime concerns. Durable state stores the world's facts, rules, definitions, and policy.

[Tasks and Transactions](./tasks-and-transactions.md) and [Task Control](./task-control.md) specify
execution boundaries. [Subscriptions](./subscriptions.md) covers settled change delivery, while
[Catalogue and Introspection](./catalogue-and-introspection.md) describes the live schema and
runtime observation surfaces.

## Hosting a Live Runtime

A Rust host builds a `DriverOwner` with `DriverResources` and obtains the interfaces appropriate to
each part of the host:

| Interface             | Responsibility                                                                                                 |
| --------------------- | -------------------------------------------------------------------------------------------------------------- |
| `DriverOwner`         | Own the driver, its event pump, and orderly shutdown.                                                          |
| `DriverAdministrator` | Evaluate administrative source and check, install, or export filein units.                                     |
| `DriverClient`        | Open endpoint sessions and access host resource and naming operations.                                         |
| `EndpointSession`     | Submit work with the endpoint's principal and actor, deliver input, and own temporary facts and subscriptions. |

Opening an endpoint establishes its execution context. Supplying an `actor` role to an individual
verb call changes a dispatch argument; it does not replace that context's authority. Source
evaluation through a session and invocation through a session both use endpoint policy. Keep the
administrator interface with the host component responsible for changing installed definitions.

An evaluation or invocation returns an `InvocationHandle`. Its initial report describes the first
execution segment, which may already have completed or may have suspended for a timer, input, or
another operation. `wait()` observes the eventual `InvocationOutcome`: completed value, aborted
error, cancellation reason, or driver failure. Several host tasks may await the same handle, and
later waits receive the retained outcome. Dropping a wait future removes that waiter; explicit
`cancel()` requests cancellation of the suspended invocation.

The driver also produces events for committed effects, subscription readiness, and background tasks.
Take its event pump once and keep it running while work is outstanding. The event queue is bounded:
when it fills, producers wait for the pump to drain it. This applies while submitting work and
during shutdown as well as while awaiting completion. `drive_until` and `drive_invocation` poll an
operation together with the pump, delivering events as progress is made.

Use `spawn_router` with a `DriverEventRouter` when event handlers need to await work, including
calls back into Mica. Each registered handler runs in its own task and receives events in order
through a bounded queue. A handler that exhausts that queue is disconnected and logged, so hosts
should size their queues and finish handlers according to the traffic they accept. Keep each
registration alive for as long as its handler is needed.

Invocation handles own their completion path. To transfer a suspended invocation's later events to
the pump, consume the handle with `detach()`. To publish its eventual terminal result through the
router while retaining handle-based completion internally, use `watch_invocation`. Both operations
make the host's chosen completion consumer explicit.

## Endpoint Resources and Shutdown

An endpoint can own named scopes of volatile facts. Replacing one scope atomically changes its fact
set; applying a scope diff requires each retracted fact to belong to that scope. Shared facts remain
present while another scope or endpoint owns them. Facts that were already present before the driver
first claimed them are retained when that ownership ends. This lets independent host components
publish overlapping observations without deleting one another's data during cleanup.

Close an endpoint when its protocol session ends. Closing stops further session submissions, waits
for its active host operations to settle, cancels its suspended tasks, cancels its subscriptions,
and removes facts asserted for its scopes. Use `close_with_pump` when the caller owns the pump, or
`close` while a separate pump task continues draining events. These operations return a report
containing the cancelled task ids and relation changes.

Dropping the final session reference or calling `close_in_background` schedules cleanup on the
active Compio runtime. Scheduling cleanup and completing it are separate events. Await explicit
closure when the next host operation depends on the resources having been released. Cancellation of
a language task discards its continuation, so host resource cleanup belongs to this lifecycle; it
does not depend on the task executing a `finally` block.

For orderly shutdown, finish the host's producers, close its sessions, and call
`DriverOwner::shutdown` with its pump. Shutdown stops admission, cancels asynchronous work and
suspended tasks, closes remaining endpoint resources and subscription mailboxes, flushes
persistence, and joins dispatcher workers. Continue to drive the Compio runtime until shutdown
returns. This gives the host a completion point for both resource cleanup and persistence errors.

## Bytecode and Native Execution

Compilation produces a register program. Instructions load values, calculate results, branch, query
relations, and request operations from the host. The VM owns the active frames and registers; the
task layer owns transaction boundaries, retries, limits, and delivery of committed effects. This
separation lets an embedding supply its own host while using the same language execution rules.

Builds with the `cranelift` feature can compile suitable loops to native machine code. The VM
recognizes a loop from its bytecode and decides whether to compile it from the current iteration
range and remaining instruction budget. Compiled code is cached on the in-memory program and can be
reused by later executions of that program.

Native execution retains ordinary value semantics. It checks candidate numeric kinds before unboxing
values and checks collection elements when it reads them. Integer overflow, invalid indexing, or a
value that does not match a specialization causes a return to the interpreter. The native attempt
writes scratch state; a failed attempt is discarded before the interpreter resumes, so partially
calculated values cannot leak into the task.

Completed native work is charged in bytecode instructions. A native loop can also stop at an
instruction-budget boundary and report the exact bytecode position to resume. Task limits and
transaction behaviour therefore apply across both execution paths. Authors write the same loop:

```mica,eval
let total = 0
for value in 1..2000
  total = total + value
end
require total == 2001000
```

Native code is a process-local cache derived from bytecode. Stored programs preserve instructions
and persistable constants; loading them reconstructs the program and its type facts. Native
compilation can then happen in the receiving process as the program executes.
