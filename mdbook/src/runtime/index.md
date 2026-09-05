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

## Bytecode and Native Execution

Compilation produces a register program. Instructions load values, calculate results, branch,
query relations, and request operations from the host. The VM owns the active frames and registers;
the task layer owns transaction boundaries, retries, limits, and delivery of committed effects.
This separation lets an embedding supply its own host while using the same language execution
rules.

Builds with the `cranelift` feature can compile suitable loops to native machine code. The VM
recognizes a loop from its bytecode and decides whether to compile it from the current iteration
range and remaining instruction budget. Compiled code is cached on the in-memory program and can
be reused by later executions of that program.

Native execution retains ordinary value semantics. It checks candidate numeric kinds before
unboxing values and checks collection elements when it reads them. Integer overflow, invalid
indexing, or a value that does not match a specialization causes a return to the interpreter. The
native attempt writes scratch state; a failed attempt is discarded before the interpreter resumes,
so partially calculated values cannot leak into the task.

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
