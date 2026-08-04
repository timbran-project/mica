# A Program That Keeps Its State

Most programs begin with source code and reconstruct their working state each time they start. They
may load rows from a database into records, recreate service objects, register request handlers, and
rebuild caches. Source code is primary; the running process is temporary.

Mica still has source files and a process, but its centre of gravity is different. The live world is
the primary environment. Source can install identities, relation definitions, rules, and verbs into
that world. With a persistent store, those definitions and the durable facts they govern survive a
process restart.

This is a *persistent programming model*: the programmer changes a continuing environment instead
of treating every process start as the birth of a new application.

## Two Kinds of Source Activity

Mica source does two related jobs.

Some source changes what the world knows or can do:

```mica
make_identity(:sensor_17)
make_relation(:Instrument, 1)
assert Instrument(#sensor_17)
```

This creates a named identity, creates a unary relation, and records a fact. Once committed to a
persistent store, later tasks can refer to `#sensor_17` and query `Instrument`.

Other source runs an action against the current world:

```mica
return Instrument(#sensor_17)
```

This task asks whether the fact is present and returns a boolean-like relational result. A verb
invocation, an HTTP request, or work resumed from a mailbox is also a task running against the
current world.

The boundary is not “schema code versus application code.” Both definitions and actions are Mica
source. The distinction is whether the source installs something for future tasks or performs work
now.

## Fileins Bootstrap and Evolve a World

A *filein* loads source into a world. It can create definitions before later statements depend on
them, so one file may contain this sequence:

```mica
make_identity(:alice)
make_identity(:sensor_17)
make_relation(:ResponsibleFor, 2)
assert ResponsibleFor(#alice, #sensor_17)
```

Run a filein against an in-memory world while experimenting:

```sh
cargo run --bin mica -- filein path/to/example.mica
```

Use a named persistent store when the world must survive the command:

```sh
cargo run --bin mica -- \
  --storage fjall --store equipment-db \
  filein --unit equipment --replace path/to/example.mica
```

The `--unit equipment --replace` form gives the imported source a durable unit name. Loading a
replacement updates definitions owned by that unit rather than accumulating another unrelated copy.
The [Filein and Fileout](../runtime/filein-fileout.md) reference gives the exact ownership and
replacement rules.

## The Store Is Not a Serialized Process

Persistence does not mean Mica freezes a VM process and resumes its memory image later. Durable
state consists of world information such as named relations, facts, identities, rules, verbs, and
policy. Runtime-only mechanisms remain runtime-only:

- an open network connection is not a durable fact;
- a live capability token is not stored as policy;
- a mailbox and its queued wakeups are not ordinary world data;
- a host's request-local state can be held in volatile relations that start empty after recovery.

This separation keeps durable meaning inspectable while allowing the process to rebuild ephemeral
machinery safely.

## Definitions Can Change While the World Lives

Suppose the equipment service begins with direct responsibility facts:

```mica
ResponsibleFor(#alice, #sensor_17)
```

Later, the organization decides responsibility should follow project assignment. A maintainer can
install a rule that derives responsibility from `AssignedTo` and `WorksOn`. Existing facts do not
need to be copied into new Rust records, and callers can continue asking the same relational
question.

That flexibility has a cost: definitions are live state and must be maintained deliberately.
Filein units, fileout, tests, review, and backups are part of programming in Mica, not afterthoughts
for an operations team.

## Changes Still Happen in Discrete Steps

A persistent world does not mean every expression becomes visible immediately. Each submitted
action runs as a task with a transaction. The task reads a consistent snapshot, prepares changes in
private, and commits them as one transition. Other tasks observe the world before or after that
transition, not halfway through it.

Persistence answers “what continues to exist?” Transactions answer “when does a change become
visible?” The tutorial treats them separately because both matter.

## A Useful Comparison

| Conventional service | Mica world |
| --- | --- |
| rows persist; application objects are rebuilt | identities, facts, and installed definitions persist |
| handlers are registered when the process starts | verbs can be installed in the world |
| authorization rules often live in middleware | policy can be expressed as durable relations |
| background code updates cached conclusions | rules derive conclusions from their causes |
| a deployment replaces the running program | fileins can evolve a continuing world |

This is not a claim that every external integration belongs in durable state. Hosts still own
network protocols and operating-system resources. Mica's model is about keeping domain meaning and
live behaviour together, while making the boundary to ephemeral host machinery explicit.

## Continue

The next chapter, [Identities, Facts, and Relations](./facts-identities-relations.md), explains the
basic pieces that make up the durable world.
