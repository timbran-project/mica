# Structural Relation Types

Mica relation types describe a value's heading, cell types, allowed row alternatives, and
cardinality. They refine the ordinary `relation` value kind without introducing a separate runtime
container.

```mica
type Person = relation<{:person -> identity, :name -> string}>
type OptionalText = relation<{:value -> string}> where rows in 0..1
type TextResult = relation<{:case -> :ok, :value -> string}
                        | {:case -> :error, :value -> error}> where rows in 1
```

The heading is exact: a value with extra or missing columns does not satisfy the type. Leaving out
the cardinality clause means any number of rows. Use `rows in n` for an exact count, `rows in m..n`
for inclusive bounds, and `rows in m..*` for a minimum without an upper bound. Common contracts are
`0` for an empty relation, `0..1` for an optional row, `1` for a required row, and `1..*` for a
nonempty set.

Cardinality counts distinct rows after the relation removes duplicates. It does not count how many
rows were written in the source. Literal spelling does not distinguish equal values:

```mica,eval
let reading: relation<{:value -> float}> where rows in 1 = [:value] { [1.0], [1.00] }
require reading == [:value] { [1.0] }

let units: relation<{:value -> unit}> where rows in 1 = [:value] { [()], [[] { [] }] }
require units == [:value] { [()] }
```

This also applies inside lists, maps, and nested relation cells. Floats use their binary32 value, so
two decimal spellings that round to the same float contribute one row. Integer and float cells
retain distinct kinds when relations compare rows.

Type aliases can name structural types and accept parameters:

```mica
type PersonName = relation<{:person -> identity, :name -> string}>
type maybe_pair<T> = relation<{:left -> T, :right -> T}> where rows in 0..1
```

Aliases installed by filein are live-world declarations. Later source can use them, fileout
preserves them, and replacing their source unit updates the compiler context.

## Unit, Option, Result, and Empty Relations

These superficially similar values have different meanings:

| Meaning                   | Source form                   | Structural shape                  |
| ------------------------- | ----------------------------- | --------------------------------- |
| completed with no payload | `()`                          | zero columns, exactly one row     |
| expected absence          | `none`                        | `option<T>` with zero rows        |
| successful optional value | `some(value)`                 | `option<T>` with one `:value` row |
| recoverable outcome       | `ok(value)` or `err(problem)` | one discriminated `result<T>` row |
| an actual empty relation  | `[] {}`                       | zero columns, zero rows           |

The standard aliases are equivalent to:

```mica
type unit = relation<{}> where rows in 1
type option<T> = relation<{:value -> T}> where rows in 0..1
type result<T> = relation<
  {:case -> :ok, :value -> T}
  | {:case -> :error, :value -> error}
> where rows in 1
```

Bare `return`, an empty body, and side-effect-only builtins produce `()`. A nonempty body returns
its last expression when execution reaches its end. Unit is truthy. `[] {}` is an ordinary falsey
relation; it is not absence, failure, JSON null, or an omitted argument.

Options nest without collapsing:

```mica
fn describe(value: option<option<string>>) -> string
  return match value
  case none
    "not supplied"
  case some(inner)
    match inner
    case none
      "supplied without text"
    case some(text)
      text
    end
  end
end
```

`none`, `some(())`, `some(none)`, and `some([] {})` are distinct relation values.

## Matching Variants

`match` inspects structural alternatives and binds their cells:

```mica
fn parsed_label(source: string) -> string
  return match from_literal(source)
  case ok(value)
    to_literal(value)
  case err(problem)
    match problem.message
    case some(message)
      message
    case none
      "invalid literal"
    end
  end
end
```

A match over a closed structural type must be exhaustive. Guards may refine a case. A wildcard case
is required when the source type is dynamic and the compiler cannot prove the complete alternative
set.

An `err` value is a returned, recoverable outcome. It does not catch or replace a raised runtime
error. Use `try`/`catch` for raised failures. A caught error's `message` and `value` fields are
options because either may be absent; its `code` is always an error code.

## Query Cardinality

Queries with named variables produce relation values. Binding syntax states how many rows are
allowed and extracts named cells:

```mica
let exactly {label} = Label(#sensor, ?label)

if let {location} = LocatedAt(#sensor, ?location)
  emit(#observer, location)
end

for {item, location} in LocatedAt(?item, ?location)
  emit(#observer, [item, location])
end
```

`let exactly` requires exactly one row. A row-form `if let` accepts zero or one row and takes its
`else` branch for zero. Both require the heading stated by the pattern and raise `E_CARDINALITY` for
excess rows or a mismatched heading. `for` accepts any cardinality and binds once per row.

The row-form conditional is useful for an optional property: absence is expected, while two values
would violate the caller's assumption. An ordinary `match` row case instead tests for exactly one
row with that heading and proceeds to the next case on any mismatch. Variant conditionals such as
`if let some(value) = result` also use pattern matching, with the unmatched value taking `else`.

Queries with no named variables are boolean predicate tests. Use `if Label(#sensor, "ready")` to
test a specific fact, and `Label(#sensor, ?label)` to obtain a relation of possible labels.

Functional dot reads are strict: a missing or ambiguous value raises `E_CARDINALITY`. Use an
optional query binding when absence is expected. List and map indexing is likewise strict and raises
`E_INDEX`; use `index_or(collection, index, fallback)` when a fallback is part of the API.

## JSON Boundaries

JSON null is represented explicitly as `{:json -> :null}` by the JSON conversion builtins. It does
not implicitly become `none`, `()`, or `[] {}`. Options and other relations also have no implicit
JSON representation; project them to the desired wire shape explicitly.
