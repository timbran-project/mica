# Changing the World Safely

An equipment service changes constantly. Instruments move, assignments begin and end, inspections
complete, and labels are corrected. Mica expresses those changes directly, but it does not publish
each line independently. Every task works on a private draft and commits a coherent transition.

## Assert a New Fact

`assert` adds a fact to the current draft:

```mica
assert LocatedAt(#sensor_17, #calibration_lab)
```

The task can read its own assertion immediately. Other tasks see it only after commit.

## Retract a Fact That Is No Longer True

`retract` removes matching facts from the draft:

```mica
retract LocatedAt(#sensor_17, #calibration_lab)
```

A wildcard can remove the current value without first naming it:

```mica
retract LocatedAt(#sensor_17, _)
```

Use wildcard retraction with care on non-functional relations. It means “remove every matching
fact,” not “pick one.”

## Replace a Functional Value

If location is declared as a functional binary relation keyed by its first position:

```mica
make_functional_relation(:LocatedAt, 2, [0])
```

property assignment replaces the value for that key:

```mica
#sensor_17.locatedAt = #north_office
```

The assignment is relation replacement sugar. Mica retracts the existing `LocatedAt` tuple for
`#sensor_17` and asserts the new tuple as part of the same transaction.

Use a functional relation only when the domain really promises one value per key. An instrument may
have one current physical location, while it may participate in many projects; `LocatedAt` and
`AssignedTo` should therefore have different cardinality.

## A Task Publishes One Transition

Consider transferring an instrument and recording why:

```mica
retract LocatedAt(#sensor_17, _)
assert LocatedAt(#sensor_17, #north_office)
assert TransferReason(#sensor_17, "field deployment")
```

These statements run in source order inside one task, but the committed world changes atomically.
Readers do not observe the new location without the transfer reason if both writes belong to the
same successful commit.

If the task raises an error before commit, neither change becomes visible.

## Effects Follow the Commit

A task can prepare output for a host:

```mica
assert LocatedAt(#sensor_17, #north_office)
emit(#operations_feed, "Sensor 17 transferred")
```

Mica buffers the effect with the transaction. The host receives it only after a successful commit.
An aborted task should not announce a transfer that did not happen.

The same principle applies to mailbox sends. State changes and the messages caused by them cross the
commit boundary together.

## Concurrent Tasks May Retry

Tasks read snapshots and commit optimistic transactions. If concurrent work creates a conflict, the
runtime can retry a retry-safe task against a fresh snapshot. Code should therefore avoid performing
untracked external side effects directly in the middle of a transaction.

Use Mica's committed effects and transactional mailbox sends for outcomes that must agree with world
state. Host integrations should make their commit boundary explicit.

## Suspension Starts a New Transaction

Operations such as waiting for input, receiving from a mailbox, spawning work, or explicitly
committing can suspend a task. Suspension commits the current transaction before the driver takes
control. When the task resumes, it runs with a fresh transaction and explicitly supplied fresh
authority.

This means one source-level task may contain more than one committed transition. Code on the far
side of a suspension must be prepared for the world to have changed while it was waiting.

## Transactions Preserve Invariants You State

Atomicity prevents partial publication, but it does not invent domain rules. If every instrument
must have exactly one location, declare `LocatedAt` functional and ensure creation supplies a
location. If only calibrated instruments may be assigned to field work, check that condition in the
verb that performs the assignment and protect the underlying relation with authority policy.

Transactions give those invariants a reliable boundary: validation and writes can succeed or fail
together.

## Persistent Does Not Mean Append-Only

Durable facts can be retracted and replaced. Persistence means committed state survives the process;
it does not mean old facts remain logically true forever.

If history matters, model it explicitly. Instead of overwriting the only location fact, a system
could record transfer identities with timestamps and derive the current location from the latest
accepted transfer. Mica does not impose one history model on every relation.

## Continue

So far, every fact has been stored directly. [Deriving New Knowledge](./rules.md) shows how Mica can
keep conclusions synchronized with their causes.
