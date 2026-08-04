# Designing Your First Mica System

Mica offers unusual flexibility, but a first design does not need to be abstract. Begin with the
statements your system must remember, the questions it must answer, and the checked actions it must
perform.

This chapter turns the equipment example into a repeatable design process.

## 1. Name Durable Things

List the entities whose continuity matters even when their properties change:

- people;
- instruments;
- sites;
- projects;
- inspections;
- work orders.

Use identities for these things. Do not create a new identity merely because an existing object's
label, state, or location changed.

Not every value needs identity. A temperature, label string, status symbol, or timestamp can remain
an ordinary value when it has no independent fact neighbourhood.

## 2. Write Facts as Sentences

Before writing declarations, state the domain facts in plain language:

- Sensor 17 is an instrument.
- Sensor 17 is located at the calibration lab.
- Alice works at the calibration lab.
- Work order 42 concerns Sensor 17.
- Inspection 9 found that Sensor 17 requires calibration.

Turn each stable kind of statement into a specific relation:

```mica
Instrument(#sensor_17)
LocatedAt(#sensor_17, #calibration_lab)
WorksAt(#alice, #calibration_lab)
Concerns(#work_order_42, #sensor_17)
InspectionFinding(#inspection_9, #sensor_17, :requires_calibration)
```

Choose position order deliberately and document it. Relation positions are ordinal, so a consistent
convention prevents mistakes.

## 3. Decide Cardinality and Lifetime

For each relation, ask:

- Can there be many facts for the same leading values?
- Which positions, if any, form a key?
- Is the fact durable or meaningful only during this process?

An instrument may have one current location but many inspection findings:

```mica
make_functional_relation(:LocatedAt, 2, [0])
make_relation(:InspectionFinding, 3)
```

An active HTTP request may be volatile; an inspection record should be durable.

Do not declare a functional relation merely to make `one` or dot syntax convenient. The key should
express a real invariant.

## 4. Separate Causes from Conclusions

Mark facts that arrive from observations or decisions:

- where the instrument is;
- what an inspection found;
- which project received an assignment;
- who approved a repair.

Store those facts. Then identify conclusions that should follow mechanically:

- whether an instrument is ready for use;
- who can collect it;
- which work orders are blocked;
- whether an inspection is overdue.

Define those conclusions with rules. This keeps derived state synchronized with its causes and makes
the reasoning inspectable.

## 5. Define Questions Before Interfaces

Write the relation patterns that each workflow needs:

```mica
ReadyForUse(?instrument)
CanCollect(#alice, ?instrument)
Concerns(?work_order, #sensor_17)
LocatedAt(?instrument, #calibration_lab)
```

These questions reveal missing relations and awkward position choices early. They also become the
shared interface used by a CLI, web page, background task, or agent. The domain query need not be
redesigned for each host.

## 6. Make Actions Checked Transitions

List the changes users and processes may request:

- transfer an instrument;
- record an inspection;
- assign an instrument;
- approve a repair;
- close a work order.

Install a verb for each domain action that benefits from dispatch, invoke authority, inspection, or
replacement. The verb should check domain preconditions and perform related writes in one
transaction.

Keep calculation helpers as ordinary functions. Not every internal operation is a world-level verb.

## 7. Design Authority Alongside Data

Identify actors and host principals, then ask what each must read, write, invoke, grant, or emit.
Group related relations and verbs into policy surfaces where that makes review easier.

Do not defer authority until after every interface is written. A relation layout that exposes mixed
tenant data under one broad read grant may need a different application boundary.

Keep live credentials and capabilities out of durable facts.

## 8. Put Bootstrap Source in Owned Units

Organize fileins around cohesive ownership:

- core identities and relation declarations;
- rules;
- domain verbs;
- authority policy;
- host-specific routes or views;
- initial or example data.

Named filein units make replacement and fileout understandable. Avoid one enormous bootstrap file
whose definitions cannot evolve independently, but also avoid fragmenting every relation into a
separate unit.

## 9. Test Meaning, Not Only Parsing

A useful test loads the real fileins and exercises the world through actual queries and
invocations. For the equipment system, verify at least:

- initial facts and query shapes;
- derived facts before and after changing their causes;
- functional replacement;
- successful and rejected verb transitions;
- authority differences between actors;
- fileout and reload where persistence matters.

Executable examples are especially important in a live language. A code block that merely parses
can still describe the wrong committed behaviour.

## 10. Keep Proposed Ideas Separate from Current Behaviour

Mica is evolving. A design note may describe a valuable direction without matching the current
runtime. Tutorial and reference pages should state what a reader can run now. Clearly label planned
syntax, future storage behaviour, and unimplemented host features.

When code and prose disagree, inspect the implementation and tests before “fixing” either side.

## A Compact Mental Model

When approaching a Mica system, remember this sequence:

```text
identities provide continuity
facts describe the current world
queries ask what is true
rules derive what follows
verbs perform checked transitions
tasks publish transactions
authority limits each task
fileins evolve the continuing world
```

This model is useful for collaborative applications, simulations, knowledge systems, operational
services, and agent workspaces. None of it depends on a game-world metaphor.

The next part of the guide develops complete runnable systems. The
[Language Overview](../language/index.md) and [Runtime Overview](../runtime/index.md) provide exact
reference material for the concepts introduced here.
