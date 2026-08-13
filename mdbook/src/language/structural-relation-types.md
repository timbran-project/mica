# Structural Relation Types

Mica relation types describe a value's heading, cell types, allowed row alternatives, and
cardinality. They refine the ordinary `relation` value kind without introducing a separate runtime
container.

```mica
relation<{:person -> identity, :name -> string}>
relation<{:value -> string}> where rows in 0..1
relation<{:case -> :ok, :value -> string}
       | {:case -> :error, :value -> error}> where rows in 1
```

The heading is exact: a value with extra or missing columns does not satisfy the type. Leaving out
the cardinality clause means any number of rows. The supported cardinalities are `0`, `0..1`, `1`,
`1..*`, and `0..*`.

Type aliases can name structural types and accept parameters:

```mica
type PersonName = relation<{:person -> identity, :name -> string}>
type maybe_pair<T> = relation<{:left -> T, :right -> T}> where rows in 0..1
```

Aliases installed by filein are live-world declarations. Later source can use them, fileout
preserves them, and replacing their source unit updates the compiler context.

## Unit, Option, Result, and Empty Relations

Four superficially similar values have deliberately different meanings:

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

Bare `return`, fallthrough, and side-effect-only builtins produce `()`. Unit is truthy. `[] {}` is
an ordinary falsey relation; it is not absence, failure, JSON null, or an omitted argument.

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

Query expressions always produce relation values. Binding syntax states how many rows are allowed
and extracts named cells:

```mica
let exactly {label} = Label(#sensor, ?label)

if let {location} = LocatedAt(#sensor, ?location)
  emit(#observer, location)
end

for {item, location} in LocatedAt(?item, ?location)
  emit(#observer, [item, location])
end
```

`let exactly` requires exactly one row. `if let` accepts zero or one row and takes its `else` branch
for zero. Either raises `E_CARDINALITY` for excess rows. `for` accepts any cardinality and binds
once per row.

Functional dot reads are strict: a missing or ambiguous value raises `E_CARDINALITY`. Use an
optional query binding when absence is expected. List and map indexing is likewise strict and raises
`E_INDEX`; use `index_or(collection, index, fallback)` when a fallback is part of the API.

## JSON Boundaries

JSON null is represented explicitly as `{:json -> :null}` by the JSON conversion builtins. It does
not implicitly become `none`, `()`, or `[] {}`. Options and other relations also have no implicit
JSON representation; project them to the desired wire shape explicitly.
