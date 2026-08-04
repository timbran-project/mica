# Expressions and Bindings

Mica is expression-oriented. Code is written as a sequence of expressions, and explicit `return`
exits the current function, verb, or task body.

Expression-oriented does not mean every form is pure. It means forms compose and produce values. A
relation query returns a boolean or relation value. `assert` and `retract` change the current
transaction and return `nothing`, the zero-column empty relation. `emit` records a pending effect
and returns the emitted value. This keeps the language surface uniform without pretending that all
expressions are side-effect free.

Bindings use `let` for local names:

```mica
let name = "temperature sensor"
let count = 1 + 1
```

Assignment updates an existing mutable binding:

```mica
count = count + 1
```

`const` declares a local name that should not be reassigned:

```mica
const limit = 10
```

Use `let` for values that are built up over time. Use `const` when a name is a local fact about the
current body.

Bindings, function boundaries, loop bindings, and scatter bindings may have exact
[value-kind annotations](./value-kind-annotations.md). An annotation constrains the value stored at
that boundary; it does not convert the value.

Scatter binding can destructure list-like values:

```mica
let [rx, tx] = mailbox()
```

Scatter binding is useful because several builtins naturally return grouped values. `mailbox()`
returns a receive cap and a send cap. A parser may return a status and a role map. The destructuring
form keeps that shape visible at the call site.

Scatter patterns support required, optional, and rest parts:

```mica
let [head, ?middle = nothing, @tail] = values
```

Required names bind by position. Optional names use their default when the source list is too short.
A rest binding receives the remaining values as a list. The compiler supports at most one rest
binding.

Lists can also be built with splice syntax:

```mica
let longer = [@prefix, last]
```

This creates a new list containing every value in `prefix` followed by `last`. It is intended for
common list-building code without hiding allocation behind stringy helper conventions.

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

Local functions are declared with `fn`:

```mica
fn add(left, right) => left + right

fn describe(item, ?style = :brief, @rest)
  return [item, style, rest]
end
```

Named local functions can be called directly in the same task body. Anonymous functions are values,
so they can be passed, returned, assigned, and called through aliases. They capture local values
when the function value is created:

```mica
let make_adder = fn(base) => fn(value) => base + value
let add10 = make_adder(10)
return add10(32)
```

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
