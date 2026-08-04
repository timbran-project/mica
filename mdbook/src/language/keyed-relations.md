# Keys and Single-Valued Relations

Mica stores facts in relations. An ordinary relation can have several answers for the same value:

```mica
make_relation(:Tag, 2)

assert Tag(#sensor, "temperature")
assert Tag(#sensor, "critical")
```

Both facts remain true. Asking for the sensor's tags can therefore return both `"temperature"` and
`"critical"`.

That is useful for tags, memberships, links, and other facts that can naturally have many answers.
It is less useful for something like a subject's current label. Code usually wants to ask for
**the** label and receive either one answer or no answer:

```mica
make_functional_relation(:Label, 2, [0])

assert Label(#sensor, "temperature sensor")
```

`make_functional_relation` declares that part of each fact is its **key**. Once a query supplies the
complete key, the relation can have at most one matching fact.

For `Label`, `[0]` selects the first position as the key:

```text
Label(subject, label)
      ^ key
```

The position numbers start at zero, so the first position is position 0. This syntax is compact, but
it is easiest to understand as saying:

> For each subject, there may be at most one label.

## The Relation Can Still Contain Many Facts

Declaring a key does not limit the whole relation to one fact. Different keys can have different
answers:

```mica
Label(#sensor, "temperature sensor")
Label(#controller, "line controller")
```

The query:

```mica
Label(#sensor, ?label)
```

can match zero or one fact because it supplies the complete key. The query:

```mica
Label(?subject, ?label)
```

can still match every label in the relation because it does not supply a key.

The guarantee is **at most one**, not exactly one. Mica does not require every subject to have a
label.

## What the Key Prevents

Relations are sets, so asserting the exact same fact twice does not create two copies. These are
different facts, however:

```mica
Label(#sensor, "temperature sensor")
Label(#sensor, "north-line sensor")
```

An ordinary relation could contain both. A `Label` relation keyed by the subject does not allow them
to coexist because both use the same key, `#sensor`.

The key is therefore more than a note for readers. It restricts which facts can exist together and
makes a single-answer query reliable.

## Why Is It Called a Functional Relation?

The name comes from the mathematical relationship between functions and relations. A function
associates each input with at most one output. It can be written as a set of input/output pairs:

```text
(#sensor, "temperature sensor")
(#controller, "line controller")
```

Those pairs are also the facts in the binary `Label` relation. When the first value is used as the
key, the relation behaves like a function from subject to label:

```text
#sensor -> "temperature sensor"
#controller -> "line controller"
```

Not every relation behaves this way. The earlier `Tag` relation associates `#sensor` with several
tags, so it is not a function from subject to tag.

For relations with more than two positions, database theory calls this guarantee a *functional
dependency*: the values in the key positions determine the values in the remaining positions. That
is what the word “functional” in `make_functional_relation` is hinting at.

The term is precise, but it can sound more abstract than the everyday feature. In practical terms,
the declaration says:

> Give Mica a complete key and there will be no more than one current answer.

## Reading One Value

The `one` operator extracts the answer from a query that is expected to produce no more than one:

```mica
return one Label(#sensor, ?label)
```

This returns `"temperature sensor"` when that is the matching label. It returns `nothing` when there
is no matching fact. If a query unexpectedly produces more than one result, it raises
`E_AMBIGUOUS` instead of silently choosing one.

Without a declared key, code could still use `one`, but it would only be assuming that nobody had
added a competing fact. The key makes the assumption an enforced part of the relation.

## Property-Style Access

A two-position functional relation keyed by its first position also supports dot syntax:

```mica
return #sensor.label
#sensor.label = "north-line sensor"
```

The read is another way to write:

```mica
one Label(#sensor, ?label)
```

The assignment replaces the `Label` fact selected by `#sensor`. After the assignment, the current
fact is:

```mica
Label(#sensor, "north-line sensor")
```

This may look like object-oriented property access, but `#sensor` does not contain a hidden `label`
field. The label remains a fact in a relation. Dot syntax is a convenience for the common case where
an object has at most one current value for a particular relation.

Many-valued facts still use ordinary relations:

```mica
Tag(#sensor, "temperature")
Tag(#sensor, "critical")
```

Mica provides property-like access where it fits without requiring all data to be stored as object
fields.

## Keys Made From More Than One Value

Sometimes one value is not enough to identify an answer. A setting could depend on both a device
and an operating profile:

```mica
make_functional_relation(:ProfileSetting, 3, [0, 1])

assert ProfileSetting(#sensor, #normal, "10s")
assert ProfileSetting(#sensor, #diagnostic, "1s")
```

`[0, 1]` makes the first two positions the key:

```text
ProfileSetting(device, profile, value)
               \____________/
                key
```

The two facts can coexist because their complete keys differ:

```text
(#sensor, #normal) -> "10s"
(#sensor, #diagnostic) -> "1s"
```

For one particular device and profile, there can be at most one setting:

```mica
ProfileSetting(#sensor, #normal, ?value)
```

This is often called a **composite key**: a key made from several values. There is no recursion and
no hidden object representing the pair. The pair is simply the information needed to select one
fact.

Sharing values also does not create an automatic reference to another relation. A `ProfileSetting`
fact and a profile fact can be joined through their shared device and profile values, but Mica does
not currently enforce foreign-key constraints between them.

Dot syntax applies only to two-position functional relations. Relations with composite keys use
ordinary relation queries.

## Declaring a Functional Relation

The current declaration form is:

```mica
make_functional_relation(:RelationName, arity, [key_positions])
```

For example:

```mica
make_functional_relation(:Label, 2, [0])
make_functional_relation(:ProfileSetting, 3, [0, 1])
```

The second argument is the number of positions in each fact. The final list identifies the
zero-based positions that form the key.

Relations are durable by default. An optional fourth argument declares a relation whose facts should
not survive a process restart:

```mica
make_functional_relation(:RequestPath, 2, [0], :volatile)
```

The declaration uses position numbers because Mica relations currently store an arity, not permanent
column names. Names such as `?subject` and `?label` belong to the query in which they appear; another
query can use different names for the same positions.
