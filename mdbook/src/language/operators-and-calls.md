# Operators, Indexing, and Calls

This chapter specifies how expressions group and how each call form is resolved. Parentheses can
always make the intended grouping explicit.

## Operator Precedence

The table runs from highest to lowest precedence. Operators on the same row associate to the left,
except assignment, which associates to the right.

| Form | Meaning |
| --- | --- |
| `f(...)`, `value[...]`, `value.field`, `value:selector(...)` | call, index, field, receiver call |
| `*`, `/`, `%` | multiplication, division, remainder |
| unary `-`, `!`, `not` | numeric negation and logical negation |
| `+`, `-` | addition and subtraction |
| `..` | range |
| `<`, `<=`, `>`, `>=` | ordering comparison |
| `==`, `!=` | equality comparison |
| `&&` | logical and |
| `||` | logical or |
| `=` | assignment |

Unary negation binds less tightly than multiplication. For example, `-2 * 3` means `-(2 * 3)`.
Use parentheses when that detail would surprise a reader.

`&&` and `||` short-circuit and return booleans. `!` and `not` are equivalent. See
[Values](./values.md) for truthiness, numeric comparison, and exact integer division.

## Ranges and Indexing

`start..end` constructs an inclusive range. The end may be `_` when the range is used as a list
index:

```mica
items[1]
items[1..3]
items[2.._]
```

Lists and relations accept integer indexes. A relation row is returned as a map keyed by its
heading symbols. Maps accept any Mica value as a key. Lists also accept inclusive range indexes; an
open-ended range extends through the final item. An invalid index raises `E_INDEX`; use
`index_or(collection, index, default)` for a non-raising lookup on lists, maps, or relations.

A declared functional binary relation supports field syntax:

```mica
let label = #sensor.label
#sensor.label = "temperature sensor"
```

This is relation projection and replacement, not record access. See
[Keys and Single-Valued Relations](./keyed-relations.md).

## Call Resolution

An uppercase call such as `AssignedTo(?work, actor)` is a relation query. Lowercase positional
calls such as `process(value)` are resolved in this order:

1. a lexically visible local function or function value;
2. a compiler-recognized runtime form such as `commit()`;
3. a registered runtime or host function; then
4. positional verb dispatch using selector `:process`.

The final segment of a relation name therefore begins with an ASCII uppercase letter, as in
`AssignedTo` or `workflow/AssignedTo`. Functions, built-ins, and verb selectors conventionally
begin with a lowercase letter.

Ordinary functions and built-ins take positional arguments. `@values` splices a list into a call:

```mica
let values = [2, 3]
add(@values)
```

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

See [Verbs, Roles, and Dispatch](./verbs-roles-dispatch.md) for method selection and prototype
delegation.
