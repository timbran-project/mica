# Tasks and Transactions

Every submitted source form or invocation runs as a task. A task is the unit of execution,
isolation, and retry. It owns a VM activation stack, a transaction, pending effects, and pending
mailbox sends.

The important rule is that Mica code can look direct while still being transactional. A verb can
assert facts and emit output in ordinary source order, but the outside world does not see those
changes until the task reaches a commit boundary.

The mental model is a private draft. Reads start from a snapshot of the world, and writes go into
the task's draft transaction. The task can read its own drafted writes before commit, but other
tasks cannot see them until the commit publishes successfully.

State changes are buffered in the task transaction:

```mica
assert AssignedTo(#inspection, #alice)
emit(#alice, "Assignment recorded.")
```

On commit, relation writes become visible, effects are published, and mailbox sends are delivered.
On abort or retry, pending writes, effects, and mailbox sends are discarded.

This matters for effects. If a task prints "Assignment recorded." and then fails, the host should
not report that the assignment happened. Mica therefore treats effects like transactional output:
they are published only after a successful commit.

Suspension is also a commit boundary. A task that calls `suspend`, `read`, `commit`, `spawn`, or
`mailbox_recv` commits its current transaction before control returns to the driver. When it
resumes, it continues with a fresh transaction and fresh authority supplied by the caller.

Call `mailbox_close(receiver)` when a task abandons a mailbox-backed operation. Closing through the
receiver revokes both mailbox capabilities, discards queued messages, and causes external producers
to observe that delivery has stopped. Cancel any change subscriptions using the mailbox before
closing it.

That means one logical task may span several transactions:

```mica
assert WorkingOn(actor(), #ticket)
commit()

let line = read(:line)
assert Observation(#ticket, line)
```

The `WorkingOn` fact is committed before the task waits for input. When input arrives, the task
resumes in a new transaction and can assert the observation.

Conflicting commits retry from the last clean boundary until the task reaches its retry limit. Local
VM state after that boundary is replayed. Pending writes, effects, and mailbox sends from the failed
attempt are discarded and rebuilt by the retry, so effectful host integrations should still treat
commit as the publication point.
