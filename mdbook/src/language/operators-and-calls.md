# Operators, Indexing, and Calls

This chapter specifies how expressions group and how each call form is resolved. Parentheses can
always make the intended grouping explicit.

## Operator Precedence

The table describes the current parser, from highest to lowest binding power. Binary operators on
the same row associate to the left, except assignment, which associates to the right.

| Form                                                         | Meaning                               |
| ------------------------------------------------------------ | ------------------------------------- |
| `f(...)`, `value[...]`, `value.field`, `value:selector(...)` | call, index, field, receiver call     |
| unary `-`, `!`, `not`                                        | numeric negation and logical negation |
| `*`, `/`, `%`                                                | multiplication, division, remainder   |
| `+`, `-`                                                     | addition and subtraction              |
| `..`                                                         | range                                 |
| `<`, `<=`, `>`, `>=`                                         | ordering comparison                   |
| `==`, `!=`                                                   | equality comparison                   |
| `&&`                                                         | logical and                           |
| `\|\|`                                                      | logical or                            |
| `=`                                                          | assignment                            |

Calls, indexing, and field access bind before unary operators, which bind before arithmetic.
For example, `-values[0] + 3` adds three to the negated first element, and `scale * measure(item)`
multiplies by the function's result. Parentheses can override that grouping:

```mica,eval
fn twice(value) => value * 2
require (-2 + 3) == 1
require -(2 + 3) == -5
require (3 * twice(4)) == 24
return true
```

Comparisons are ordinary binary operations, so express a bounded test as
`low <= value && value <= high`, rather than chaining it as `low <= value <= high`.

`&&` and `||` short-circuit and return booleans. `!` and `not` are equivalent. See
[Values](./values.md) for truthiness, numeric comparison, and exact integer division.

Short-circuiting controls evaluation, not just the result. In `condition && action()`, the call
runs only when `condition` is truthy. Neither logical operator selects a non-boolean operand as a
fallback value: `none || "fallback"` is `true`. Use `if`, an option match, or `index_or` to select
an actual value.

## Ranges and Indexing

`start..end` constructs an inclusive range. Integer ranges can be iterated directly; they do not
first expand into a list. An open-ended range uses `_` as its endpoint and is useful for list slices:

```mica
items[1]
items[1..3]
items[2.._]
```

Lists and relations accept **zero-based** integer indexes. A relation row is returned as a map keyed by its heading
symbols. Maps accept any Mica value as a key. Lists also accept inclusive range indexes; an
open-ended range extends through the final item. An invalid index raises `E_INDEX`; use
`index_or(collection, index, default)` for a non-raising lookup on lists, maps, or relations.

```mica,eval
let items = ["first", "second", "third", "fourth"]
require items[0] == "first"
require items[1..2] == ["second", "third"]
require items[2.._] == ["third", "fourth"]
require items[4.._] == []
require index_or(items, 4, "missing") == "missing"
return items
```

Slice endpoints are bounds, not requests to clamp or wrap. A negative index, a reversed explicit
slice, or a slice extending past the list raises `E_INDEX`. The open-ended slice at the list's
length is valid and returns an empty list. Strings and bytes do not support this index syntax.

An indexed assignment requires a mutable local list or map as its immediate target. For nested
data, extract the inner collection, construct its replacement, then assign that replacement into
the outer collection. For example:

```mica,eval
let settings = {:display -> {:colour -> "amber"}}
let display = settings[:display]
display[:colour] = "green"
settings[:display] = display
require settings[:display][:colour] == "green"
return settings
```

A declared functional binary relation supports field syntax:

```mica
let label = #sensor.label
#sensor.label = "temperature sensor"
```

This is relation projection and replacement, not record access. See
[Keys and Single-Valued Relations](./keyed-relations.md).

## Call Resolution

An uppercase call such as `AssignedTo(?work, actor)` is a relation query. Lowercase positional calls
such as `process(value)` are resolved in this order:

1. a lexically visible local function or function value;
2. a compiler-recognized runtime form such as `commit()`;
3. a registered runtime or host function; then
4. positional verb dispatch using selector `:process`.

There is one deliberate exception for installed verb parameters: a role called `actor` does not
hide the runtime call `actor()`. The bare name `actor` still refers to the supplied role value;
`actor()` reads the runtime context. The same exception applies to other role names that coincide
with registered runtime functions. Ordinary local bindings still take lexical precedence.

The final segment of a relation name therefore begins with an ASCII uppercase letter, as in
`AssignedTo` or `workflow/AssignedTo`. Functions, built-ins, and verb selectors conventionally begin
with a lowercase letter.

Ordinary functions and built-ins take positional arguments. `@values` splices a list into a call:

```mica
let values = [2, 3]
add(@values)
```

Function arguments are evaluated from left to right. Each argument keeps the value it had when
evaluated, even if a later argument changes the binding from which it came. Binary operands follow
the same rule:

```mica,eval
fn pair(left, right) => [left, right]

let count = 1
require pair(count, count = 2) == [1, 2]
count = 1
require count + (count = 2) == 3

let transform = fn(value) => value + 1
transform = fn(value) => value + 2
require transform(3) == 5
```

Assigning another function to a mutable binding changes subsequent calls through that binding.
The function value for a call is selected before its arguments are evaluated.

Named-role dispatch starts with a symbol and names every role explicitly:

```mica
:approve(actor: #reviewer, request: #change_request)
```

Receiver syntax adds a role named `receiver`:

```mica
#change_request:approve(actor: #reviewer)
```

It is equivalent to `:approve(receiver: #change_request, actor: #reviewer)`. The receiver is not
privileged during dispatch. A role map can be spliced into named-role dispatch:

```mica
let roles = {:actor -> #reviewer, :request -> #change_request}
:approve(@roles)
```

`invoke(selector, roles)` performs the same named-role dispatch dynamically. Its selector is a
symbol and its roles argument is a map.

Receiver syntax requires the selected verb to have a role named `receiver`. It does not rename a
role such as `request` or `item`. For a verb declared as `approve(actor, request)`, use
`:approve(actor: person, request: change)`; for `approve(actor, receiver)`, the equivalent receiver
call is `change:approve(actor: person)`.

Positional dispatch obtains argument-to-role mapping from the installed parameter order. Named
roles are usually clearer for calls involving several identities with different meanings. The
selector can also be computed with `:(selector)(actor: person, request: change)`, or with
`change:(selector)(actor: person)` for a receiver call.

See [Verbs, Roles, and Dispatch](./verbs-roles-dispatch.md) for method selection and prototype
delegation.
