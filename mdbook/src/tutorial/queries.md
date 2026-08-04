# Asking Questions

Once the world contains facts, programs need to ask questions such as:

- Is Sensor 17 an instrument?
- Where is Sensor 17?
- Which instruments are at the calibration lab?
- Which people and instruments share a site?

Mica answers these questions by matching relation patterns.

## A Fully Specified Pattern Tests a Fact

A relation call with concrete values asks whether that fact is available:

```mica
if Instrument(#sensor_17)
  return "registered instrument"
else
  return "not registered"
end
```

The call is not invoking an `Instrument` function. It asks the current snapshot whether the
`Instrument(#sensor_17)` fact can be read. The answer may come from stored facts, installed rules,
or both.

## A Query Variable Marks an Unknown Position

Prefix a name with `?` when you want Mica to find its value:

```mica
LocatedAt(#sensor_17, ?site)
```

Read this as “find every `site` for which Sensor 17 is located at that site.” If the world contains:

```mica
LocatedAt(#sensor_17, #calibration_lab)
```

the query produces a relation value with one column and one row:

```mica
[:site] { [#calibration_lab] }
```

The heading comes from the query-variable name. A query with two unknown positions has two columns:

```mica
LocatedAt(?item, ?site)
```

One possible result is:

```mica
[:item, :site] {
  [#sensor_17, #calibration_lab],
  [#spectrometer_2, #north_office]
}
```

Query results are unordered sets. Do not make program behaviour depend on the order in which rows
appear.

## Observe Rows with a Loop

A relation value is iterable. Each observed row appears as a binding map:

```mica
for row in LocatedAt(?item, ?site)
  let item = row[:item]
  let site = row[:site]
  // Work with item and site.
end
```

The maps are an observation interface for rows. Mica does not eagerly allocate a map for every
answer before iteration.

## Ask for One Value Deliberately

A functional relation often has one answer for a concrete key:

```mica
let label = one Label(#sensor_17, ?text)
```

`one` has three outcomes:

- one matching value: return that value;
- no matches: return `nothing`;
- more than one match: raise `E_AMBIGUOUS`.

The ambiguity error is important. Taking an arbitrary row would hide corrupt data or a mistaken
cardinality assumption.

Functional binary relations also support property-style syntax:

```mica
let label = #sensor_17.label
```

This is relation sugar. It does not read a field from an identity record. The expression still asks
the declared `Label` relation for the value keyed by `#sensor_17`.

## Wildcards Match Without Naming

Use `_` when a position must have some value but you do not need to return it:

```mica
LocatedAt(#sensor_17, _)
```

This asks whether Sensor 17 has any recorded location. A query variable such as `?site` would add a
column to the answer; `_` does not.

## Repeated Variables Require Equal Values

When one query variable appears more than once, every occurrence must match the same value:

```mica
Pair(?value, ?value)
```

This selects only `Pair` facts whose two positions are equal.

## Queries Describe Results, Not Search Steps

The query:

```mica
LocatedAt(?instrument, #calibration_lab)
```

states the shape of the desired facts. It does not tell Mica to scan a collection from left to right
or to call a method on the lab. The relation kernel chooses how to answer from its indexes, stored
facts, rule evaluation, and snapshot state.

This declarative style becomes more powerful with rules because callers can ask the same question
without knowing whether the answer was stored directly or derived from other facts.

## Queries Run Against a Snapshot

A task begins from a consistent view of the world. Its relation reads use that snapshot plus the
task's own drafted writes. Another task cannot expose half of a concurrent update through two query
results.

Suppose one task moves Sensor 17 by retracting its former `LocatedAt` fact and asserting a new one.
A concurrent reader sees the old location or the new location after commit, never the gap between
the two operations.

## Query Results Are Values

Queries are not restricted to `if` and `for`. A result can be returned, passed to another function,
stored when all of its nested values are persistable, or combined with relational operations:

```mica
let instruments = Instrument(?item)
let local = LocatedAt(?item, #calibration_lab)
let local_instruments = natural_join(instruments, local)
return project(local_instruments, :item)
```

You do not need relation algebra for the rest of this tutorial. The important idea is that a query
produces an ordinary Mica value with a precise heading and set of rows.

## Continue

The next chapter, [Changing the World Safely](./changing-world.md), shows how facts are asserted,
retracted, and replaced within transactions.
