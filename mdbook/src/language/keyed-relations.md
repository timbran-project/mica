# Keys and Single-Valued Relations

Mica stores facts in relations. An ordinary relation can have several answers for the same value:

```mica
make_relation(:Tag, 2)

assert Tag(#lamp, "metal")
assert Tag(#lamp, "valuable")
```

Both facts remain true. Asking for the lamp's tags can therefore return both `"metal"` and
`"valuable"`.

That is useful for tags, memberships, links, and other facts that can naturally have many answers.
It is less useful for something like an object's current name. Code usually wants to ask for **the**
name of an object and receive either one answer or no answer:

```mica
make_functional_relation(:Name, 2, [0])

assert Name(#lamp, "brass lamp")
```

`make_functional_relation` declares that part of each fact is its **key**. Once a query supplies the
complete key, the relation can have at most one matching fact.

For `Name`, `[0]` selects the first position as the key:

```text
Name(object, name)
     ^ key
```

The position numbers start at zero, so the first position is position 0. This syntax is compact, but
it is easiest to understand as saying:

> For each object, there may be at most one name.

## The Relation Can Still Contain Many Facts

Declaring a key does not limit the whole relation to one fact. Different keys can have different
answers:

```mica
Name(#lamp, "brass lamp")
Name(#coin, "gold coin")
```

The query:

```mica
Name(#lamp, ?name)
```

can match zero or one fact because it supplies the complete key. The query:

```mica
Name(?object, ?name)
```

can still match every name in the relation because it does not supply a key.

The guarantee is **at most one**, not exactly one. Mica does not require every object to have a
name.

## What the Key Prevents

Relations are sets, so asserting the exact same fact twice does not create two copies. These are
different facts, however:

```mica
Name(#lamp, "brass lamp")
Name(#lamp, "golden lamp")
```

An ordinary relation could contain both. A `Name` relation keyed by the object does not allow them
to coexist because both use the same key, `#lamp`.

The key is therefore more than a note for readers. It restricts which facts can exist together and
makes a single-answer query reliable.

## Why Is It Called a Functional Relation?

The name comes from the mathematical relationship between functions and relations. A function
associates each input with at most one output. It can be written as a set of input/output pairs:

```text
(#lamp, "brass lamp")
(#coin, "gold coin")
```

Those pairs are also the facts in the binary `Name` relation. When the first value is used as the
key, the relation behaves like a function from object to name:

```text
#lamp -> "brass lamp"
#coin -> "gold coin"
```

Not every relation behaves this way. The earlier `Tag` relation associates `#lamp` with several
tags, so it is not a function from object to tag.

For relations with more than two positions, database theory calls this guarantee a *functional
dependency*: the values in the key positions determine the values in the remaining positions. That
is what the word “functional” in `make_functional_relation` is hinting at.

The term is precise, but it can sound more abstract than the everyday feature. In practical terms,
the declaration says:

> Give Mica a complete key and there will be no more than one current answer.

## Reading One Value

The `one` operator extracts the answer from a query that is expected to produce no more than one:

```mica
return one Name(#lamp, ?name)
```

This returns `"brass lamp"` when that is the matching name. It returns `nothing` when there is no
matching fact. If a query unexpectedly produces more than one result, it raises `E_AMBIGUOUS`
instead of silently choosing one.

Without a declared key, code could still use `one`, but it would only be assuming that nobody had
added a competing fact. The key makes the assumption an enforced part of the relation.

## Property-Style Access

A two-position functional relation keyed by its first position also supports dot syntax:

```mica
return #lamp.name
#lamp.name = "golden lamp"
```

The read is another way to write:

```mica
one Name(#lamp, ?name)
```

The assignment replaces the `Name` fact selected by `#lamp`. After the assignment, the current fact
is:

```mica
Name(#lamp, "golden lamp")
```

This may look like object-oriented property access, but `#lamp` does not contain a hidden `name`
field. The name remains a fact in a relation. Dot syntax is a convenience for the common case where
an object has at most one current value for a particular relation.

Many-valued facts still use ordinary relations:

```mica
Tag(#lamp, "metal")
Tag(#lamp, "valuable")
```

Mica provides property-like access where it fits without requiring all data to be stored as object
fields.

## Keys Made From More Than One Value

Sometimes one value is not enough to identify an answer. A label could depend on both a stair and
the destination reached from that stair:

```mica
make_functional_relation(:SideLabel, 3, [0, 1])

assert SideLabel(#stair, #tavern, "down")
assert SideLabel(#stair, #cellar, "up")
```

`[0, 1]` makes the first two positions the key:

```text
SideLabel(stair, destination, label)
          \________________/
                key
```

The two facts can coexist because their complete keys differ:

```text
(#stair, #tavern) -> "down"
(#stair, #cellar) -> "up"
```

For one particular stair and destination, there can be at most one label:

```mica
SideLabel(#stair, #tavern, ?label)
```

This is often called a **composite key**: a key made from several values. There is no recursion and
no hidden object representing the pair. The pair is simply the information needed to select one
fact.

Sharing values also does not create an automatic reference to another relation. A `SideLabel` fact
and a passage fact can be joined through their shared stair and destination values, but Mica does
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
make_functional_relation(:Name, 2, [0])
make_functional_relation(:SideLabel, 3, [0, 1])
```

The second argument is the number of positions in each fact. The final list identifies the
zero-based positions that form the key.

Relations are durable by default. An optional fourth argument declares a relation whose facts should
not survive a process restart:

```mica
make_functional_relation(:RequestPath, 2, [0], :volatile)
```

The declaration uses position numbers because Mica relations currently store an arity, not permanent
column names. Names such as `?object` and `?name` belong to the query in which they appear; another
query can use different names for the same positions.
