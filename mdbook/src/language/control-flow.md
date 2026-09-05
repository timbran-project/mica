# Control Flow

Control flow selects which expressions run inside a task. `if`, `match`, and `begin` also produce
values, so they can appear on the right-hand side of a binding or be returned by a function.

## Branches and Early Exits

Conditionals are expressions:

```mica
if Calibrated(instrument)
  return true
elseif RequiresCalibration(instrument)
  return false
else
  return false
end
```

The value of an `if` expression is the value produced by the branch that runs. When a branch uses
`return`, it exits the current body immediately instead of becoming the branch value.

Each condition is tested only if the preceding conditions were falsey. An unselected branch is not
evaluated. If there is no `else` and no condition succeeds, the result is `()`. An empty selected
branch also produces unit.

```mica,eval
let attempts = 2
let label = if attempts == 0
  "not started"
elseif attempts < 3
  "in progress"
else
  "needs attention"
end
require label == "in progress"
```

Conditions use [truthiness](./values.md#truthiness), not an implicit numeric or string conversion.
For example, zero and an empty string are both truthy. Write `attempts > 0` or `text != ""` when
those are the tests the program needs.

This makes guard-oriented code natural:

```mica
if Calibrated(instrument) == false
  return false
end
```

or, when the condition is short:

```mica
Calibrated(instrument) || return false
```

## Loops

`while` reevaluates its condition before each iteration:

```mica
let i = 0
while i < 10
  i = i + 1
end
```

Use `break` to leave the nearest loop and `continue` to skip to the next loop iteration:

```mica
while true
  let line = read(:line)
  line == "quit" && break
  line == "" && continue
  emit(actor, line)
end
```

and `for`:

```mica
for value in values
  emit(actor, value)
end

for key, value in properties
  render_property(key, value)
end
```

`for` evaluates its iterable expression once. The number and shape of the bindings determine what
each iteration receives:

| Iterable | One binding | Two bindings |
| --- | --- | --- |
| list | element | zero-based index, element |
| map | value | key, value |
| relation value | row map | zero-based row index, row map |
| closed integer range | integer | zero-based offset, integer |

Maps and relation values use canonical order. Lists and ranges have their natural sequence order.
Use a list when iteration order carries application meaning.

```mica,eval
let numbered = []
for index, label in ["inspect", "repair"]
  numbered = [@numbered, [index, label]]
end
require numbered == [[0, "inspect"], [1, "repair"]]
```

Loop bindings are local to the loop. Bind a mutable accumulator before the loop when the result
must be used afterward. The value of the `for` or `while` expression itself is `()`; it does not
collect the values of its body automatically.

Queries with named variables are iterable because they return relation values. A structural row
pattern binds the projected cells directly:

```mica
for {work} in AssignedTo(?work, actor)
  if let {label} = Label(work, ?label)
    emit(actor, label)
  end
end
```

Loops are mainly for imperative work inside a task: rendering output, building lists, validating
input, or coordinating effects. Do not use loops to encode stable derived knowledge when a rule
would express the relationship directly. For example, recursive dependencies belong in a `Requires`
rule, not in every verb that needs to walk a dependency graph.

Guard-style early returns are idiomatic when they keep control flow direct:

```mica
Calibrated(instrument) || return false
```

Use this style for preconditions that stop the body. Prefer a full `if` when there is meaningful
alternative work to perform.

## Blocks and Ranges

`begin ... end` groups a sequence of expressions into one expression:

```mica
let value = begin
  let adjusted = raw + 1
  adjusted * 2
end
```

Ranges are expressions:

```mica
items[2..5]
items[2.._]
```

An underscore endpoint means an open-ended range. Range indexing applies to lists; integer indexing
also applies to lists and relation rows, while maps use value keys.

A closed integer range includes both endpoints. Ascending ranges iterate in steps of one; a range
whose end is below its start has no iterations:

```mica,eval
let total = 0
for number in 2..4
  total = total + number
end
require total == 9

for number in 4..2
  raise E_TEST, "A descending range has no iterations."
end
```

Use an open endpoint for a list slice extending to the list's end. Use a closed range for a counted
loop, and `while` when termination depends on changing state or input.
