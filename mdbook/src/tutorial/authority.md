# Authority Is Part of the Model

A shared system must answer two different questions:

1. Who or what does this task concern?
2. What is this running task allowed to do?

Mica keeps those questions separate. Mentioning `#alice`, passing Alice in an `actor` role, or
storing a fact about Alice does not grant Alice's permissions. Authority comes from the context
attached to the task.

## Policy Is Durable; Capabilities Are Ephemeral

The world can store policy facts:

```mica
HasRole(#alice, #technician)
RoleCanRead(#technician, :equipment)
RoleCanWrite(#technician, :movement)
RelationInSurface(:equipment, :Label)
RelationInSurface(:equipment, :LocatedAt)
RelationInSurface(:movement, :LocatedAt)
```

Rules derive effective permissions such as `CanRead`, `CanWrite`, `CanInvoke`, and `CanEffect`. When
a task or session begins, the runtime builds an `AuthorityContext` from the current policy snapshot.

Operations then check that context cheaply. The hot path does not rerun the complete policy query
for every tuple read or fact written.

The shape is:

```text
durable role and policy facts
    -> derived effective Can* relations
    -> AuthorityContext for this task or session
    -> cheap operation checks
```

Capability values inside the runtime are different. A mailbox send capability, for example, is a
live token authorizing one runtime operation. Such tokens are ephemeral and cannot be persisted as
ordinary facts.

## Identity Does Not Confer Authority

This invocation contains Alice's identity:

```mica
:transfer(
  actor: #alice,
  instrument: #sensor_17,
  destination: #north_office
)
```

It does not prove that the caller is Alice or that the task may invoke `:transfer`. The host or
parent task establishes the principal and supplies authority when submitting the task.

This prevents code from escalating merely by constructing a role map with a more privileged
identity.

## Relation Grants Cover Whole Relations

Read and write authority applies to a named relation, not automatically to selected rows. Granting
read access to `AssignedTo` allows an unbound query to observe every readable tuple in that
relation. The runtime does not infer row ownership from a position named `actor`, `owner`, or
`user`.

If a system needs per-tenant isolation, make the tenant explicit and bind it in the interface:

```mica
AssignedToTenant(#acme, #sensor_17, #air_quality_project)
```

Then expose verbs or derived relations that constrain access to the authorized tenant. Do not assume
a broad relation grant becomes row-level security automatically.

## Authority Is Refreshed at Boundaries

Policy can change while the world is running. New tasks receive authority derived from the current
snapshot. A suspended task resumes with explicitly supplied fresh authority rather than relying on
an indefinitely checkpointed context.

This balances two needs:

- policy changes should take effect at understandable boundaries;
- relation reads and writes should not perform expensive policy derivation on every operation.

## Authority and Domain Validation Are Complementary

Suppose Alice may write `LocatedAt`. That permission means the task may attempt a location change.
It does not mean every destination is valid or every instrument is transferable.

The transfer verb can still enforce domain rules:

```mica
ReadyForTransfer(instrument, destination) || return :invalid_transfer
```

Conversely, satisfying the domain predicate does not grant write authority. Both checks matter.

## Effects Need Explicit Authority

External output crosses the Mica world boundary. Console messages, HTTP responses, tool requests,
and other host effects require effect authority. A task that may read or write facts does not
automatically gain permission to make an external request.

External credentials should enter as session or task authority. Durable policy may say that an agent
can use a search tool; the live API credential itself should not become a durable fact that can be
filed out.

## Bootstrap Conservatively

The initial root context can create the policy relations and grants needed for normal actors. After
bootstrap, run user and host work with narrowly derived authority rather than root authority.

Review policy as data:

- Which roles exist?
- Which surfaces group relations and verbs?
- Which actors hold each role?
- Can an actor grant more authority?
- Which effects cross into hosts?

Because these answers are relations, they can be queried, derived, inspected, tested, and exported
with the rest of the world.

## Continue

The final tutorial chapter, [Designing Your First Mica System](./designing-a-system.md), turns the
individual concepts into a practical modelling process.
