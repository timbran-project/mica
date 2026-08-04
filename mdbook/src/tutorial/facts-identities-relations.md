# Identities, Facts, and Relations

The equipment service needs to talk about Alice, Sensor 17, the calibration lab, and a work order.
These things can change without becoming different things. A sensor can move, receive a new label,
or become assigned to another project while remaining Sensor 17.

Mica represents that continuity with identities:

```mica
#alice
#sensor_17
#calibration_lab
#work_order_42
```

An identity is a durable value, not a record containing all of an object's state. Facts describe the
identity.

## Create Identities Before Referring to Them

A named identity is installed with `make_identity`:

```mica
make_identity(:alice)
make_identity(:sensor_17)
make_identity(:calibration_lab)
```

The argument is a symbol such as `:alice`. Once installed, source refers to its identity value as
`#alice`.

This split prevents an accidental spelling from silently creating a new durable object. The compiler
resolves `#alice` against the current world before a task runs.

## A Fact Is One Accepted Statement

Here are three facts about the sensor:

```mica
Instrument(#sensor_17)
Label(#sensor_17, "Temperature sensor 17")
LocatedAt(#sensor_17, #calibration_lab)
```

Read them as sentences:

- Sensor 17 is an instrument.
- Sensor 17 has the label “Temperature sensor 17.”
- Sensor 17 is located at the calibration lab.

A fact is present or absent. Relations have set semantics, so asserting the same fact twice does not
create two logical copies.

## A Relation Names a Kind of Fact

`Instrument`, `Label`, and `LocatedAt` are relations. Each relation has a fixed number of positions,
called its _arity_:

```mica
make_relation(:Instrument, 1)
make_relation(:LocatedAt, 2)
make_functional_relation(:Label, 2, [0])
```

`Instrument` has one position. Every fact in it names one instrument. `LocatedAt` has two positions:
the located thing and its location.

The positions have meaning because of the relation's design and use; they are not stored column
names. The declaration fixes the shape, and documentation and conventions explain what each position
means.

`Label` is a _functional relation_. The list `[0]` declares position zero as its key. Each identity
may have at most one label, although the relation can hold labels for many identities:

```mica
Label(#sensor_17, "Temperature sensor 17")
Label(#calibration_lab, "Calibration lab")
```

Keys are constraints, not hints. Mica uses them to reject ambiguity and to support replacement and
property-style syntax.

## Assert Facts to Record Them

The `assert` form adds facts to the task's draft transaction:

```mica
assert Instrument(#sensor_17)
assert Label(#sensor_17, "Temperature sensor 17")
assert LocatedAt(#sensor_17, #calibration_lab)
```

These changes become visible to other tasks when the current task commits.

## An Object Is Its Fact Neighbourhood

In a record-oriented language, code might expect `sensor.label`, `sensor.location`, and
`sensor.owner` to live in one structure. Mica does not keep a hidden record behind `#sensor_17`.
Instead, the useful view of the object is its _fact neighbourhood_: all relevant stored and derived
facts that mention the identity.

For example:

```mica
Instrument(#sensor_17)
Label(#sensor_17, "Temperature sensor 17")
LocatedAt(#sensor_17, #calibration_lab)
RequiresCalibration(#sensor_17)
AssignedTo(#sensor_17, #air_quality_project)
```

Different parts of a system can introduce new relations without enlarging one privileged record.
Maintenance can add `RequiresCalibration`; project planning can add `AssignedTo`; policy can add
facts about who may change either relation.

The identity provides continuity while the relations provide meaning.

## Relations Are Not Necessarily Database Tables

A table is a useful first analogy: a binary relation contains same-shaped pairs much as a table
contains rows. The analogy becomes incomplete in three important ways:

1. A relation can combine stored and rule-derived facts behind one query surface.
2. Query results are first-class relation values, not only rows returned by an external database.
3. Relation reads and writes participate directly in task transactions and authority checks.

It is therefore safer to think “named set of facts” first and borrow the table analogy only when it
helps.

## Prefer Specific Relations

A generic property relation can store almost anything:

```mica
Property(#sensor_17, :location, #calibration_lab)
Property(#sensor_17, :label, "Temperature sensor 17")
```

That convenience discards useful structure. `LocatedAt` and `Label` can have different keys,
authority policy, rules, indexes, and meaning. A program can ask for locations without filtering a
mixed property bag.

Use a specific relation when a relationship has domain meaning. Mica is designed to let a world grow
new kinds of facts without forcing them through one universal slot mechanism.

## Durable and Volatile Facts

Named relations are durable by default when the runtime uses persistent storage. Some facts are
meaningful only during the current process, such as an active HTTP request:

```mica
make_relation(:ActiveRequest, 1, :volatile)
```

Volatile facts still use transactions, queries, rules, and authority. Their facts are simply omitted
from persistent commits and begin empty after recovery. Volatility is about storage lifetime, not
access control or automatic expiry.

## Continue

Facts become most useful when the program can ask questions about them. Continue to
[Asking Questions](./queries.md).
