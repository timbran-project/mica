# Relational Mental Model

Mica uses relational ideas, but you do not need to start with database theory. The practical model
is small:

| Term           | Working meaning                                                   |
| -------------- | ----------------------------------------------------------------- |
| identity       | a durable key-like value, such as `#alice` or `#task42`           |
| fact           | one thing the world currently says is true                        |
| relation       | a named collection of same-shaped facts                           |
| query          | a pattern matched against facts                                   |
| query variable | a marked hole, such as `?item`, that asks Mica to return bindings |
| binding map    | one answer to a query, such as `{:work -> #inspection}`           |
| rule           | a named computed query that derives new facts from other facts    |
| transaction    | a private draft of world changes that commits atomically          |

For example:

```mica
AssignedTo(#inspection, #alice)
AssignedTo(#repair, #alice)
```

These are two facts in the `AssignedTo` relation. They have the same shape: work is assigned to a
person.

A query can ask Mica to fill one position:

```mica
AssignedTo(?work, #alice)
```

The result is a relation value:

```mica
[:work] { [#inspection], [#repair] }
```

`?work` binds a value and names the result column. Iterating that relation exposes each row as a
binding map such as `{:work -> #inspection}`. `_` is different: it is a wildcard. It matches a
value but does not bind or return it.

```mica
AssignedTo(_, #alice)
```

That asks whether any work is assigned to `#alice`, but it does not name the work.

Rules use bare variable names instead of `?` variables:

```mica
Collaborators(left, right) :-
  AssignedTo(work, left),
  AssignedTo(work, right)
```

Inside a rule, `left`, `right`, and `work` are logical variables. They are not local variables
assigned step by step. Mica searches for values that make the body true and then derives matching
`Collaborators(left, right)` facts.

Transactions are the runtime side of this model. A task runs against a private draft of the world.
`assert` and `retract` change that draft. Commit publishes the draft; abort or retry throws it away.
