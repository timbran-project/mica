# Deriving New Knowledge

Some facts are observations or decisions that a program should store directly:

```mica
WorksAt(#alice, #calibration_lab)
LocatedAt(#sensor_17, #calibration_lab)
RequiresCalibration(#sensor_17)
```

Other facts are conclusions. If Alice works at the site where Sensor 17 is located, Alice can collect
the sensor. Storing that conclusion separately would create synchronization work: every location or
staffing change would have to find and update all affected `CanCollect` facts.

A rule records the reason instead:

```mica
CanCollect(person, instrument) :-
  WorksAt(person, site),
  LocatedAt(instrument, site)
```

Read this from right to left:

> If a person works at a site, and an instrument is located at the same site, then that person can
> collect that instrument.

No prior Datalog knowledge is required. The rule is a maintained definition of a relation.

## The Head Names the Conclusion

The first line is the rule *head*:

```mica
CanCollect(person, instrument) :-
```

It describes the fact to derive. The lines after `:-` form the *body*: the conditions that must all
be satisfied.

`person`, `instrument`, and `site` are logical variables. Mica searches for values that make the
body facts true, then emits the corresponding head facts.

They are not mutable local variables and the body is not an imperative sequence of assignments.
The shared `site` name expresses the join: both body facts must use the same site value.

## Callers Use Derived Relations Normally

After the rule is installed, task code queries it like any other relation:

```mica
return CanCollect(#alice, ?instrument)
```

The caller does not need a different API for derived facts. If `CanCollect` also has directly
asserted facts, reads see the union of stored and derived results.

This uniformity lets a system change how knowledge is produced without forcing every caller to
change its query.

## Changes Propagate Through the Definition

If Sensor 17 moves away from Alice's site, the body no longer matches and the derived
`CanCollect(#alice, #sensor_17)` fact disappears. If it moves back, the conclusion reappears.

The program does not issue a compensating retraction for `CanCollect`. That fact was never an
independent stored decision; it follows from current causes.

Mica can maintain suitable rule results incrementally as commits add and retract base facts. The
ordinary rule syntax states meaning; authors do not write a separate event handler for every
possible change.

## Multiple Rules Mean Multiple Reasons

A relation can have more than one rule. Suppose a person can collect an instrument either because
they work at its site or because the instrument is assigned to their project:

```mica
CanCollect(person, instrument) :-
  WorksAt(person, site),
  LocatedAt(instrument, site)

CanCollect(person, instrument) :-
  WorksOn(person, project),
  AssignedTo(instrument, project)
```

The derived relation contains facts supported by either rule. Set semantics remove duplicate
answers when both reasons apply.

## Negation Means “Not Derivable Here”

Rules can exclude a known condition:

```mica
ReadyForUse(instrument) :-
  Instrument(instrument),
  not RequiresCalibration(instrument)
```

`not` means that `RequiresCalibration(instrument)` cannot be derived from the current snapshot. It
does not prove an eternal negative fact, and it is not SQL's three-valued `NOT`.

The positive `Instrument(instrument)` condition limits the instruments under consideration. A rule
cannot safely invent an unlimited universe of values from a negative condition alone.

Mica requires stratified negation: negative dependencies must have a well-defined evaluation order.
Mutually negative definitions such as “A when not B” and “B when not A” are rejected.

## Recursion Describes Paths and Containment

Rules can refer to their own derived relation. Consider components installed inside assemblies:

```mica
Contains(assembly, component) :-
  InstalledIn(component, assembly)

Contains(assembly, component) :-
  InstalledIn(inner, assembly),
  Contains(inner, component)
```

The first rule handles direct installation. The second says an assembly also contains everything
inside a directly installed subassembly. Callers can query `Contains` without writing a traversal
loop.

Positive recursion is evaluated until another pass adds no new facts. In formal terms, Mica
computes a finite least fixpoint. Practically, it repeatedly follows the stated reasons until the
answer stops growing.

## Store Causes; Derive Consequences

A useful design test is:

- Store a fact when it represents an observation, input, choice, or event the world must remember.
- Derive a fact when it should always follow from other current facts.

For example, store that an inspection occurred and what it found. Derive whether an instrument is
currently eligible for use when that answer follows mechanically from inspection and service facts.

This distinction avoids stale caches while keeping important historical decisions durable.

## Rules Are Installed World Definitions

A rule is not merely a local function in the file that mentions it. Installing the source changes
the live world's catalogue. The definition can be inspected, filed out, replaced through filein
unit ownership, and used by later tasks.

Because rules are live definitions, changes deserve the same review and migration discipline as
schema and application code. Replacing a rule can change many visible conclusions at once.

## Continue

Rules define what follows from the world's facts. [Installing Behaviour](./behaviour.md) explains how
verbs define actions that people and processes can invoke.
