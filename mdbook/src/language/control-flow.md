# Control Flow

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

Loops include `while`:

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

`for value in values` iterates over list-like values. When iterating a map, Mica uses the ordinary
`key, value` order:

```mica
for key, value in properties
  render_property(key, value)
end
```

Relation queries are also iterable because they return relation values. A structural row pattern
binds the projected cells directly:

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
