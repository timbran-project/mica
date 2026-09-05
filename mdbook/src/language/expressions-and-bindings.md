# Expressions and Bindings

Mica is expression-oriented. Code is written as a sequence of expressions, and explicit `return`
exits the current function, verb, or task body.

Expression-oriented does not mean every form is pure. It means forms compose and produce values. A
relation query returns a boolean or relation value. `assert` and `retract` change the current
transaction and return `()`. `emit` records a pending effect and returns the emitted value. This
keeps the language surface uniform without pretending that all expressions are side-effect free.

## Local Names and Scope

Bindings use `let` for local names:

```mica
let name = "temperature sensor"
let count = 1 + 1
```

Assignment updates an existing mutable binding:

```mica
count = count + 1
```

`const` declares a local name that cannot be reassigned:

```mica
const limit = 10
```

Use `let` for values that are built up over time. Use `const` when a name is a local fact about the
current body.

Bindings are lexical. A name is available after its declaration in its enclosing body, and nested
bodies can read it. Branches, loop bodies, functions, and `begin ... end` introduce scopes; a name
declared inside one does not become a name in the enclosing body. Redeclaring a name in the same
scope is a compile error. A nested scope may shadow an outer name:

```mica,eval
let label = "outer"
let detail = begin
  const label = "inner"
  label
end
require label == "outer"
require detail == "inner"
return [label, detail]
```

Locals belong to an execution. A top-level `let` at the REPL does not create a persistent world
variable for the next submission. Store a fact when a value must outlive its task, and give durable
entities identity names when later source must refer to them.

An unannotated declaration without an initializer, such as `let pending`, initially contains the
zero-column empty relation `[] {}`. Prefer an explicit initializer that states the intended shape,
such as `let pending = none` for an optional result or `let pending = []` for a list. Annotated
bindings require an initializer.

Function parameters, installed verb parameters, and `const` bindings are immutable. Copy the
information into a mutable local before building an updated value. Indexed assignment through an
immutable local is also rejected; it would replace the collection stored in that binding.

Bindings, function boundaries, loop bindings, and scatter bindings may have exact
[value-kind annotations](./value-kind-annotations.md). An annotation constrains the value stored at
that boundary; it does not convert the value.

## Destructuring Lists

Scatter binding extracts positional values from a list:

```mica
let [rx, tx] = mailbox()
```

Scatter binding is useful because several builtins naturally return grouped values. `mailbox()`
returns a receive cap and a send cap. A parser may return a status and a role map. The destructuring
form keeps that shape visible at the call site.

Scatter patterns support required, optional, and rest parts:

```mica
let [head, ?middle = none, @tail] = values
```

Required names bind by zero-based position; a missing required list position raises `E_INDEX`.
Optional names use their explicit default when the source list is too short. An optional name still
consumes its position when present. A rest binding receives the remaining values as a list,
including an empty list when nothing remains. The compiler supports at most one rest binding. Put it
last to make the shape easy to read.

```mica,eval
let [first, ?second = "unspecified", @rest] = ["inspect"]
require first == "inspect"
require second == "unspecified"
require rest == []
return [first, second, rest]
```

Use braces to bind a named relation row and brackets to destructure a positional list. These are
different contracts: `let exactly {name} = rows` checks one row with the stated heading, whereas
`let [name] = values` reads a list position. A relation row is not a positional argument list.

## Splicing Values into Collections and Calls

Lists can also be built with splice syntax:

```mica
let longer = [@prefix, last]
```

This creates a new list containing every value in `prefix` followed by `last`. The spliced value
must be a list. Splicing preserves element order and does not modify the source list.

Function calls can also use argument splices, such as `f(first, @rest)`. That works for local
function calls, function-value calls, builtin calls, relation calls, task-control calls, positional
dispatch, receiver positional dispatch, `invoke`, and positional spawn.

Named-role dispatch uses map splices for dynamic role sets:

```mica
let roles = {:request -> #release_change}
:inspect(actor: #alice, @roles)
```

The splice contributes role bindings from the map. It does not splice one role's value, so
`actor: @actors` is not valid.

## Local Functions and Closures

Local functions are declared with `fn`:

```mica
fn add(left, right) => left + right

fn describe(item, ?style = :brief, @rest)
  return [item, style, rest]
end
```

Named and anonymous local functions are callable values. Both can be passed, returned, assigned, and
called through aliases. A named declaration also binds its name in the enclosing scope. They capture
local values when the function value is created:

```mica
let make_adder = fn(base) => fn(value) => base + value
let add10 = make_adder(10)
return add10(32)
```

The arrow body is a single result expression. A block body permits multiple expressions and uses
`return` to make its result explicit. A local function is useful for calculations and repeated steps
within a task. An installed verb is useful when later tasks need to discover and invoke that
behaviour through the live world's dispatch rules.

A closure captures values at creation time. Reassigning an outer binding later does not change the
captured value:

```mica,eval
let rate = 2
let charge = fn(quantity) => quantity * rate
rate = 3
require charge(10) == 20
return charge(10)
```

Saving a named function in another binding retains that callable even if its original name is
subsequently reassigned:

```mica,eval
fn increment(value) => value + 1
const saved = increment
increment = fn(value) => value + 10
require saved(3) == 4
require increment(3) == 13
```

Use an anonymous function when the surrounding code already supplies a useful name or when passing a
short calculation directly to another function. The unannotated brace form `{value} => value + 1` is
also a function expression. Use `fn(value: int) -> int => value + 1` when annotations are needed.
These local function values cannot be persisted in relation tuples; their lifetime is tied to the VM
that created them.

Required, optional, and rest parameters use the same positional vocabulary as scatter bindings.
Optional parameters need an explicit default, and rest parameters receive a list. Neither omission
nor an empty rest list is represented by a magic null value:

```mica,eval
fn describe(item, ?style = :brief, @details)
  return [item, style, details]
end
require describe("lamp") == ["lamp", :brief, []]
require describe("lamp", :full, "brass", "polished") == ["lamp", :full, ["brass", "polished"]]
return describe("lamp")
```

## Local Maps and World Relations

Maps use symbol keys heavily, but map keys are values:

```mica
let roles = {:actor -> actor, :item -> item}
roles[:container] = box
```

Role maps are ordinary map values. They become dispatch input only when passed to `invoke` or
equivalent dispatch syntax.

Dot syntax is authoring sugar for declared functional binary relations. It is not record-field
storage:

```mica
#sensor.label = "temperature sensor"
let label = #sensor.label
```

The relation backing a dot name must be declared as functional so the syntax has single-value
behaviour.

Query variables use `?name` syntax in task code:

```mica
for found in AssignedTo(?work, assignee)
  emit(actor, found[:work])
end
```

Each loop value is a binding map. Query variables do not create local variables automatically; the
local variable is the loop binding, and the named query result is read out of that map.

## Bindings Hold Independent Values

`let saved = value` initializes a separate binding with the current value. Later assignment to
either name does not change the other binding. This also applies to lists and maps: indexed
assignment replaces the collection in its target binding, while a saved value retains its previous
contents.

```mica,eval
let readings = [10, 20]
const saved = readings
readings[0] = 15
require saved == [10, 20]
require readings == [15, 20]
return [saved, readings]
```
