# Syntax Quick Reference

This chapter is a compact map of the current surface syntax. Details live in the topical reference
chapters.

## Comments

```mica
// line comment
```

## Qualified Names

Names may use `/`-separated namespaces:

```mica
#workflow/reviewer
:workflow/approve
workflow/AssignedTo(#change_request, #workflow/reviewer)
```

The final segment determines whether a call is relation-shaped: `workflow/AssignedTo(...)` is a
relation query because `AssignedTo` begins with an uppercase letter.

## Bindings

```mica
let value = expression
const fixed = expression
value = value + 1
let [first, ?middle = none, @rest] = values

fn add(left, right) => left + right

fn describe(item)
  return item
end
```

Bindings and `fn` parameters and results may name one exact runtime value kind:

```mica
let count: int = 0
let [head: string, @tail: list] = values

fn add(left: int, right: int) -> int => left + right
```

Annotations check kinds without converting values. See
[Value-Kind Annotations](./value-kind-annotations.md) for the supported boundaries and kinds.

## Collections

```mica
[1, 2, 3]
[@prefix, last]
{:name -> "sensor", :calibrated -> true}
items[2]
items[2.._]
```

`[@prefix, last]` splices the list in `prefix` into a new list before `last`. `2.._` is an
open-ended range used by index operations.

Operator precedence and call resolution are specified in
[Operators, Indexing, and Calls](./operators-and-calls.md).

## DOM Markup

```mica
dom <button type="submit" class={class}>
  Save
</button>

dom <ul>{@items}</ul>
```

`dom <...>` builds DOM node values and lowers to `dom_element(...)` and `dom_text(...)`. Use
`{expr}` for dynamic attributes or children, and `{@expr}` to splice a list of child nodes. Bare
`<tag>` is not expression syntax; the `dom` prefix is required.

## Relations

```mica
make_relation(:AssignedTo, 2)
make_functional_relation(:Label, 2, [0])

assert AssignedTo(#inspection, #technician)
retract AssignedTo(#inspection, _)

AssignedTo(#inspection, #technician)
AssignedTo(?work, #technician)
let exactly {label} = Label(#sensor, ?label)
if let {location} = LocatedAt(#sensor, ?location)
  return some(location)
end

[:work, :owner] { [#inspection, #alice], [#repair, #bob] }
[] {}
()
```

`[0]` is a zero-based key-position list for a functional relation. `?thing` and `?name` are query
variables that bind returned values. `_` is a wildcard that matches without binding.

A relation literal has a unique symbol heading followed by rows of exactly that arity. `[] {}` is
the zero-column empty relation; `()` is the zero-column unit relation.

`assert` and `retract` require relation atoms. `require` accepts any boolean condition:

```mica
require CanMove(actor, item)
```

## Rules

```mica
ReadyForReview(reviewer, change) :-
  AssignedReviewer(change, reviewer),
  Completed(change),
  not ReviewRecorded(change)
```

Rule variables are conventionally bare names such as `reviewer` and `change`. The current compiler
also accepts `?name` in rule atoms, but bare names are the preferred rule style.

## Control

```mica
if condition
  expression
elseif other_condition
  expression
else
  expression
end

while condition
  expression
end

break
continue

begin
  expression
end

for value in values
  expression
end

for key, value in map
  expression
end
```

## Errors

```mica
raise E_PERMISSION, "denied", item

try
  expression
catch E_PERMISSION as err
  err.message
catch
  "fallback"
finally
  cleanup()
end

recover risky()
catch E_FAIL => none
catch => "fallback"
end
```

## Verbs and Dispatch

```mica
verb approve(actor @ #reviewer, request @ #change_request)
  return true
end

verb echo(value @ #string: string) -> string
  return value
end

:approve(actor: #alice, request: #release_change)
#release_change:approve(actor: #alice)
```

`actor @ #reviewer` is a role restriction. `#release_change:approve(actor: #alice)` is receiver-call
sugar for a named-role dispatch with `receiver: #release_change`; it is not classic method-table
lookup.

## Task Control

```mica
commit()
suspend(1)
read(:line)
let child = spawn :tick(actor: actor()) after 5

let [rx, tx] = mailbox()
mailbox_send(tx, "ready")
let ready = mailbox_recv([rx], 0)
mailbox_close(rx)
```

## Filein Definitions

```mica
make_identity(:sensor)
make_relation(:Object, 1)
assert Object(#sensor)

verb inspect(actor, subject)
  return subject
end
```
