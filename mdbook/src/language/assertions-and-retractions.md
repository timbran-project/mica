# Assertions and Retractions

Assertions and retractions are how task code changes stored facts.

`assert` adds a fact to a relation:

```mica
assert AssignedTo(#inspection, #alice)
```

After this commits, the world contains the assignment fact. Other code can query it:

```mica
AssignedTo(#inspection, #alice)
AssignedTo(?work, #alice)
```

Asserting an existing fact is idempotent at the logical relation level. Mica relations are sets, so
there is no second copy of `AssignedTo(#inspection, #alice)` to count later. The operation still
participates in the current transaction and authority checks.

`retract` removes matching facts:

```mica
retract AssignedTo(#inspection, #alice)
```

This removes the stored fact if it exists. Retracting a fact that is already absent should not be
used as a control-flow signal; query first when absence is meaningful to the program.

Wildcards can be used to retract matching tuples:

```mica
retract AssignedTo(#inspection, _)
```

This removes every `AssignedTo` fact whose first position is `#inspection`, regardless of the second
value. Use wildcard retractions carefully: they are concise, but they can remove more state than a
reader expects if the relation is broad. `_` matches without binding a value. It is not the same as
a query variable such as `?item`, which names a value to return in a query result.

Retractions affect stored facts. Derived facts are consequences of rules; to make a derived fact
stop being true, change the base facts it depends on or change the rule.

`require` checks a condition and aborts the current transaction if the condition is false:

```mica
require CanMove(actor, item)
```

Unlike `assert` and `retract`, `require` does not require a relation atom. Any expression that
produces a truth value can be used.

Fact changes happen inside the current task transaction. They become durable only when the task
reaches a successful commit boundary. If the task aborts or retries, uncommitted changes are
discarded.

This means a verb can make several related changes and publish them together:

```mica
retract AssignedTo(work, previous_assignee)
assert AssignedTo(work, new_assignee)
emit(actor, "Assignment updated.")
```

Other tasks should not observe the old assignment removed without the new assignment as an
intermediate state. The transaction publishes the whole change or none of it.

Functional relation assignment uses replacement semantics:

```mica
#sensor.label = "north-line sensor"
```

This replaces the current tuple for the relation's key, rather than adding a second value.

The explicit relation form makes the replacement clearer:

```mica
retract Label(#sensor, _)
assert Label(#sensor, "north-line sensor")
```

Dot assignment is the concise form for this pattern, but only where the backing relation is declared
functional.

Prefer named relations such as `Label`, `InstalledAt`, `AssignedTo`, and `ToolResult` for concepts
the world understands. A generic ad hoc relation should be reserved for state that is truly local or
experimental.
