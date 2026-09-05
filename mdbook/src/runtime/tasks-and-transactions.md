# Tasks and Transactions

A task is one running computation: a submitted source body or a verb invocation, including the
functions and verbs it calls. It owns the VM stack, local values, a relation transaction, and
buffered output. Calling another verb stays in the same task. `spawn` starts a separate task.

A task and a transaction have different lifetimes. A task may wait for input or another computation
and then continue. Each wait commits the work done so far; continuation starts another transaction.
This lets a long conversation or workflow proceed without keeping a database transaction open
throughout the wait.

## Reading and Drafting Changes

Reads start from a snapshot of the world. Assertions, retractions, and functional replacements
modify the task's private draft. Later queries in the same transaction see those changes, including
their consequences for derived relations. Other tasks see them after a successful commit.

For example, this whole body completes in one transaction:

```mica,eval
make_relation(:Reading, 2)
assert Reading(:sensor_a, 20)
require Reading(:sensor_a, 20)

retract Reading(:sensor_a, 20)
assert Reading(:sensor_a, 21)
let exactly {temperature} = Reading(:sensor_a, ?temperature)
require temperature == 21
```

The same rule applies inside a verb. A helper that updates a relation and a helper that reads it
share their caller's transaction. The source does not need to pass a transaction object between
helpers.

Query results are immutable values. Saving `let rows = Reading(?sensor, ?temperature)` saves those
answers; changing `Reading` later does not change `rows`. Run the query again to observe the updated
relation. This distinction also matters when a local value survives a suspension.

## Commit and Publication

Normal task completion commits automatically. These forms also end the current transaction:

| Form | What happens after the commit |
| --- | --- |
| `commit()` | the driver schedules continuation |
| `suspend(seconds)` | continuation waits for the timer |
| `suspend()` | continuation waits for an explicit host resumption |
| `read(metadata)` | continuation waits for endpoint input |
| `mailbox_recv(receivers, timeout)` | continuation receives queued messages or a timeout |
| `spawn :work(...)` | the driver submits a child and resumes the parent with its task id |
| `external_request(kind, payload)` | the host performs the request and supplies its result |

A boundary publishes relation writes before exposing the task's buffered effects and mailbox sends.
Subscription registrations and cancellations requested by the task are also applied at the
boundary. A conflict causes a retry before the host receives the suspension or spawn request.

```mica
assert AssignedTo(#inspection, #alice)
emit(#alice, "Assignment recorded.")
```

The message is buffered with the assignment. If the task raises an unhandled error before its next
commit, both are discarded. If it commits successfully, the host can deliver the message knowing
that the assignment was published. `emit` queues a value for the host; calling it does not itself
flush the transaction.

Publishing an effect and completing its external delivery are separate events. A host may still
need to write to a socket or invoke a service after receiving committed output. When an external
operation needs an acknowledgement, use the host request or mailbox protocol for that operation
and record the acknowledged result in a subsequent transaction.

## Publication and Persistence

Publication makes a committed snapshot visible to other tasks in the current process. Persistence
determines when the host can recover those facts after a restart. The in-memory provider keeps the
world for the life of that process. The Fjall provider stores durable relation metadata, rules, and
facts, then reconstructs the in-memory world when opened again.

With Fjall, the host chooses a durability mode. The runner exposes it through `--durability`:

```sh
cargo run --bin mica -- --storage fjall --store world-db --durability strict eval 'return ()'
```

| Mode | When a durable commit returns |
| --- | --- |
| `relaxed` | after the ordered background writer accepts the commit into its queue |
| `strict` | after the writer applies the commit and syncs the journal |

The default is `relaxed`. In that mode, another task can observe published facts while their write
is still queued. Strict mode puts the journal sync before publication and release of buffered
effects. An embedding host can call `flush_persistence()` to wait for earlier queued writes and
sync the journal in either mode. A task's `commit()` ends its transaction; it uses the configured
provider mode rather than changing that mode.

Relation durability is a separate choice. A `:volatile` relation retains its definition across a
Fjall restart but starts with no stored rows. Use it for process-lifetime facts such as open
endpoints. Durable relations recover their stored rows. Derived answers are recomputed from the
recovered facts and active rules; execution caches and live capabilities are not recovered as
durable authority.

## Continuing After a Boundary

Local bindings and the call stack survive suspension. The resumed task receives a fresh relation
transaction. The driver also rebuilds the actor's authority from current policy, or the principal's
policy when there is no actor. A permission change committed during a wait therefore takes effect
when the task continues.

```mica
assert WorkingOn(#alice, #ticket)
let line = read(:line)

let exactly {status} = TicketStatus(#ticket, ?status)
if status != :open
  raise E_CLOSED, "The ticket is no longer open."
end
assert Observation(#ticket, line)
```

`read` commits `WorkingOn` before waiting. Once input arrives, the task checks `TicketStatus` in its
new transaction. Checking it before the wait would describe the earlier snapshot; it would not
reserve the ticket for the entire conversation.

An explicit `commit()` is useful when the next stage should begin against a newly published world
even though no input is needed. It also divides failure handling: an error in a later transaction
does not undo facts or output from earlier committed transactions. Place boundaries where that
partial progress has a clear meaning in the application.

## Conflicts and Replay

At commit, the kernel checks the task's writes against changes published since its snapshot.
Competing functional replacements for the same key can conflict. Changes to independent keys can
commit independently. An earlier predicate query alone does not reserve the facts it inspected;
model competing updates through a shared functional key when they must exclude one another.

For example, a relation `TicketStatus(ticket, status)` keyed by `ticket` makes a ticket's state one
contended value. Two tasks that both try to replace its initial status cannot silently publish
incompatible replacements based on that same initial state. The task that retries reads the updated
status and can decide whether its transition still applies.

On a conflict, the runtime restores the VM checkpoint at the last successful boundary, opens a new
transaction, and re-executes that segment. It discards the failed attempt's writes, effects, mailbox
sends, and pending subscription operations. Locals changed during the failed attempt are restored
with the checkpoint. A value delivered when the task resumed is included in that checkpoint, so a
retry processes the same input rather than requesting it again.

Keep externally visible work behind the runtime's publication boundary. Host builtins should use
buffered effects or suspend for external requests so replay does not perform an external action
while a transaction is still speculative.

## Errors and Execution Budgets

An unhandled language error aborts the current transaction. A handled error allows the task to
continue with the same draft: `try`, `catch`, `recover`, and `finally` do not create nested
transactions or savepoints. Validate input before changing facts when the recovery path should
leave those facts alone, or explicitly restore the intended state in that path.

The host configures execution budgets. The defaults allow 1,000,000 VM instructions between host
responses, a call depth of 50, and up to 10 conflict retries for the task. Retry counts carry across
suspensions. Exceeding a runtime budget terminates execution through the host error path; it is not
an application error that a `catch` clause can extend indefinitely.

Task ids, suspended continuations, and mailbox capabilities identify live runtime resources.
Durable progress belongs in relations. After a restart, a host can use those facts to decide which
work to submit again.
