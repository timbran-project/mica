# Value-Kind Annotations

Mica code is dynamically typed unless an author adds an exact value-kind annotation at a supported
boundary. An annotation names one runtime kind and keeps that boundary invariant without changing
the value:

```mica
let total: int = 0

fn add(left: int, right: int) -> int
  return left + right
end
```

Annotations are optional. Leaving them out preserves ordinary dynamic Mica behaviour.

## Exact Kinds

The supported source names are:

| Source name  | Runtime value kind           |
| ------------ | ---------------------------- |
| `bool`       | Boolean                      |
| `int`        | 56-bit signed integer        |
| `float`      | finite binary32 float        |
| `identity`   | identity                     |
| `string`     | string                       |
| `bytes`      | byte string                  |
| `symbol`     | symbol                       |
| `error_code` | error code                   |
| `error`      | raised or caught error value |
| `capability` | ephemeral capability         |
| `frob`       | frob                         |
| `function`   | function value               |
| `list`       | list                         |
| `map`        | map                          |
| `range`      | range                        |
| `relation`   | immutable relation value     |

These names are contextual rather than reserved words. A local may still be named `int` or
`relation` when the name is not in an annotation position.

Each value-kind annotation is exact. `int` does not accept `float`, even when the values compare as
numerically equal. Structural relation types and aliases refine the `relation` kind with headings,
cell types, alternatives, and cardinality; see
[Structural Relation Types](./structural-relation-types.md).

## Supported Boundaries

Ordinary `let` and `const` bindings may be annotated. An annotated ordinary binding requires an
initializer, and every later assignment to a mutable annotated binding must preserve its kind:

```mica
let count: int = 0
const label: string = "ready"
count = count + 1
```

The invariant also applies to assignments made by a closure that captures the binding.

Required and optional `fn` parameters may be annotated. An optional parameter must have an explicit
default. A rest parameter is always constructed as a list, so `list` is its only valid annotation:

```mica
fn describe(item: identity, ?style: symbol = :brief, @details: list) -> string
  return to_literal([item, style, details])
end

let render = fn(value: string) -> string => value
```

One- and two-name `for` bindings may be annotated:

```mica
for index: int, row: map in rows
  emit(actor, [index, row])
end
```

Scatter bindings support annotations on required, optional, and rest names:

```mica
let [head: int, ?middle: int = 0, @tail: list] = values
```

Installed `verb` parameters and results may be annotated:

```mica
verb echo(value @ #string: string) -> string
  return value
end
```

Brace lambdas, catch bindings, query variables, explicit `method` fileout envelopes, and relation
rule variables do not currently accept annotations.

## Checks, Proofs, and Errors

Annotations never convert values. The compiler handles an annotated value in one of three ways:

- when it can prove an exact match, it emits no runtime kind check;
- when it can prove a mismatch, compilation fails at the annotated boundary;
- when the value is dynamic, it emits one check at that boundary.

A failed dynamic check raises the catchable error code `E_TYPE`. The error identifies the binding or
parameter, names the expected and actual kinds, and retains the unchanged offending value in
`err.value`:

```mica
try
  let count: int = from_literal("\"not an integer\"")
catch E_TYPE as err
  return [err.message, err.value]
end
```

Checks occur when a value enters an annotated binding or parameter and before an assignment changes
an annotated binding. Reading an already checked binding does not check it again.

Function and verb result annotations are different: they are proof-only. Every reachable normal exit
must have the declared kind, and the compiler emits no check or conversion at `return`. A dynamic
result must first cross an annotated local boundary:

```mica
fn decode_count(source: string) -> int
  let count: int = from_literal(source)
  return count
end
```

Bare returns and fallthrough results produce `()`, whose structural type is `unit` and whose outer
kind is `relation`. A scalar result annotation therefore rejects those paths.

## Outer Kinds

An exact outer kind does not imply a structural or behavioural contract:

- `identity` says only that the value is an identity. It says nothing about the identity's facts,
  prototypes, or relation membership.
- `frob` says only that the value is a frob. It does not constrain its delegate or payload.
- `capability` says only that the value is a live capability. It grants no authority, does not make
  the capability persistable, and does not change serialization rules.
- `function` says only that the value is a function value. It does not describe parameter or result
  kinds and does not prove that an arbitrary function call has a particular result.
- `error` says only that the value is a structured error value. It does not restrict the error code,
  message, or payload.
- `relation` accepts every relation value. Use a structural relation type when heading, row shape,
  discriminator, cell types, or cardinality are part of the contract.

## Dispatch Is Independent

Dispatch restrictions and value-kind annotations serve different phases. In this header:

```mica
verb inspect(value @ #string: string) -> string
  return value
end
```

`@ #string` helps select the applicable verb through the dispatch system. After selection,
`: string` constrains the parameter value. An annotation does not affect applicability, specificity,
fallback selection, or persisted dispatch restriction facts. Two methods cannot be overloaded by
differing only in value-kind annotations.

## Performance

Declared and inferred exact kinds feed the same compiler facts. Adding an annotation where the
compiler already proves the kind does not request a different representation and emits no check.

Dynamic boundaries have real cost. An assignment from a dynamic call checks each time it executes,
and an annotated binding over dynamically typed collection cells checks every yielded element:

```mica
for value: int in values
  total = total + value
end
```

Use annotations to state an invariant that matters to the program. Do not mechanically annotate
locals whose exact kinds are already obvious.
