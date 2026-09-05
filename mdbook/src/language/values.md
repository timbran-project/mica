# Values

Mica values are the things code can compute with, pass to verbs, put in lists and maps, and store in
relation tuples. Some values are ordinary data, some are durable references into the world, and some
are ephemeral runtime authority.

Current value families include:

- booleans: `true`, `false`;
- integers;
- floats;
- strings;
- symbols such as `:approve`;
- error codes such as `E_FAIL`;
- identity values such as `#alice`;
- lists such as `[1, 2, 3]`;
- maps such as `{:name -> "sensor"}`;
- immutable relation values, including structural options and results;
- frobs such as `#event<{:actor -> #alice}>`;
- bytes;
- ranges such as `1..5`;
- structured errors;
- local function values;
- ephemeral capability values.

The value layer is intentionally small and regular. Named relations can store any persistable value,
and verbs can accept ordinary values, identities, frobs, or relation values through the same
role-binding mechanism. The language should not force authors to turn every structured value into a
durable object just so it can be passed around.

Bytes and relation values have source literals and can cross storage and host value boundaries.
A relation value is persistable when every cell it contains is persistable. Capability and local
function values are ephemeral; neither can be serialized as durable world data.

## Choosing a Value Shape

Use a list when position matters, a map when local keys matter, and a relation value when named
columns and a set of rows matter. Use an identity when other facts need to refer to the same entity
over time. These choices can be combined: a relation tuple can contain a list, and an option can
contain an identity. Choose the outer shape to express what the caller should do with the value.

There is no implicit conversion between these shapes. In particular, a list containing one value
is not `some(value)`, and a map containing `:value` is not a one-row relation. Their indexing,
matching, and persistence behaviour follow their actual kinds.

Primitive values behave like values in most dynamic languages:

```mica
42
true
"temperature sensor"
:approve
E_PERMISSION
[1, 2, 3]
{:name -> "sensor"}
[:work, :owner] { [#inspection, #alice], [#repair, #bob] }
```

`[] {}` is the zero-column empty relation. It is falsey because it has no rows, but it is not an
absence or null sentinel. Unit is written `()` and is equivalent to `[] {[]}`: a zero-column
relation containing one empty row. It is truthy. An empty relation with a heading, such as
`[:thing] {}`, is also falsey but remains distinct from `[] {}` because its heading is part of the
value. Expected absence uses `none` and `some(value)`; see
[Structural Relation Types](./structural-relation-types.md).

Relation literals have a symbol heading followed by a set of rows:

```mica
[:thing, :owner] {
  [#inspection, #alice],
  [#repair, #bob],
}
```

Each row must match the heading arity. Heading names must be unique. Relations have set semantics,
so duplicate rows are removed. Heading columns and rows are canonicalized together: changing the
written column order does not change the value if each cell still has the same column name.
Iteration and integer indexing expose the canonical row order, which is not insertion order.
Do not interpret the first row as the most recent or most important result.

```mica,eval
let first = [:name, :count] { ["lamps", 2], ["lamps", 2] }
let second = [:count, :name] { [2, "lamps"] }
require first == second
return first
```

This returns one row. Equality includes the heading, so the same cells under different column names
would describe a different relation value.

## Truthiness

Conditions accept values of any kind. The complete falsey set is `false`, an empty list, and an
empty relation of any heading. Every other value is truthy. In particular, `0`, `0.0`, `""`,
`b""`, and `{}` are truthy. The option `none` is falsey because it is an empty relation;
`some(false)` is truthy because it has a row.

```mica,eval
require !false
require ![]
require !none
require some(false)
require 0
require ""
require {}
require ()
return true
```

Test the property you mean. To distinguish an empty string from a nonempty string, compare it with
`""`; `if text` does not make that distinction. To inspect an option's payload, use `match` or
`if let` rather than treating the option itself as the payload's boolean value.

At the JSON boundary, JSON `null` maps to the explicit tagged map `{:json -> :null}` and that tag
maps back to `null`. Relation values, including options, have no implicit JSON representation and
must be projected into lists or maps explicitly.

Indexing is strict. Reading an absent list position, relation row, or map key raises `E_INDEX`;
invalid index types and out-of-range indexed assignments raise the same error. Optional bindings
handle absent arguments explicitly and do not rely on a missing index producing a sentinel value.
Use `index_or(collection, index, default)` when absence is expected and should produce a default.

## Strings, Bytes, and Names

Strings use double quotes and contain Unicode text. Supported escapes are `\"`, `\\`, `\n`, `\r`,
`\t`, and `\0` (the null character). `\u{...}` accepts one to six hexadecimal digits naming a Unicode
scalar value, such as `\u{e9}` for `é` or `\u{1f980}` for `🦀`. Unicode characters may also appear
directly in source. Other backslash sequences, including malformed Unicode escapes, are preserved
literally. To include the text of a valid escape, escape its backslash: `"\\u{e9}"` contains six
characters, starting with a backslash.

```mica,eval
let label = "Montréal"
let message = "First line\nSecond line"
let quoted = "She said \"ready\"."
require label != "Montreal"
require label == "Montr\u{e9}al"
return [label, message, quoted]
```

`to_literal` writes string contents using these escapes, including control characters that would
otherwise be invisible in source. `from_literal` decodes that text into `ok(value)` or returns
`err(problem)` for an invalid literal. This boundary preserves the value's contents:

```mica,eval
let text = "header\0body\u{1}\nMontréal"
require from_literal(to_literal(text)) == ok(text)
```

Byte literals contain **URL-safe, padded base64**, not text to encode as bytes. For example,
`b"aGk="` contains the two bytes for `hi`, and `b""` is an empty byte string. The base64 alphabet
uses `-` and `_` where standard base64 uses `+` and `/`. Invalid encoding is a compile error.
Use bytes for opaque binary content and strings for text whose character encoding is already known.

Symbols are interned names used for selectors, relation names, policy surfaces, message tags, and
other program-facing labels:

```mica
:approve
:tool_call
:inspection
```

Use a quoted symbol when the name contains spaces, Unicode, punctuation, or a reserved word.
The contents follow the same escaping rules as a string, while the value remains a symbol:

```mica,eval
require :"inspection complete" == to_symbol("inspection complete")
require :"Montréal" != "Montréal"
require :"approve" == :approve
require from_literal(to_literal(:"line\nbreak")) == ok(:"line\nbreak")
```

Quoted symbols also name relation columns, row bindings, literal types, and dispatch selectors.
Quoting changes how a name is written, without changing which name it denotes:

```mica,eval
let rows: relation<{:"display name" -> string}> = [:"display name"] {["Ada"]}
let exactly {:"display name" -> label} = rows
let status: :"ready to review" = :"ready to review"
require label == "Ada"
return [label, status]
```

Error codes are also values. By convention, error-code literals begin with `E_`:

```mica
E_PERMISSION
E_NOT_FOUND
```

Errors can be raised and recovered by the error-handling surface, but the code itself is still a
value. Mica does not require a closed universe of built-in error names.

## Collections Are Values

Lists are ordered sequences:

```mica
["inspect", "repair", "calibrate"]
```

Maps are associative values:

```mica
{:actor -> #alice, :request -> #release_change}
```

Maps remain useful even in a relation-first language. Relations are for world state and queryable
facts. Maps are for local structured values: role maps, configuration, decoded messages, frob payloads,
and temporary results. A map can be stored in a relation tuple, but doing so usually means the
relation cannot query inside that map without additional derived facts or host support.

Map keys are unique under canonical value equality. If a map literal repeats a key, the last value
wins. Maps are stored in canonical key order, not insertion order. Use an explicit list of keys if
your presentation requires a particular order.

Lists, maps, and relation values are immutable. An indexed assignment builds a replacement collection
and stores it in the named local binding. Another binding holding the previous value retains it:

```mica,eval
let readings = [10, 20]
const saved = readings
readings[0] = 15
require saved == [10, 20]
require readings == [15, 20]

let labels = {:status -> "queued", :status -> "ready"}
labels[:colour] = "amber"
require labels[:status] == "ready"
return [saved, readings, labels]
```

List replacement requires an existing zero-based position. Map replacement can insert a new key.
Relation values cannot be changed with indexed assignment: build another relation value instead.
To change a named relation in the world, use `assert`, `retract`, or declared functional dot syntax.

## Identities and Delegated Values

Identity values are different. `#alice` is not the contents of Alice, and it is not a pointer to a
hidden Alice structure. It is a stable key-like value that can appear in relations:

```mica
Actor(#alice)
Name(#alice, "Alice")
AssignedTo(#inspection, #alice)
```

An identity is also not the primary key of one privileged object table. It can appear in many
relations, sometimes in key-like positions and sometimes as an ordinary referenced value.

This is why identity and equality are separate concerns. Two values can be equal because they are
the same integer or string. Two identities are equal when they are the same identity value. Whether
two identities describe equivalent domain entities is a modelled relationship, not something baked
into the value representation.

For example:

```mica
EquivalentAgent(#planner, #backup_planner)
SamePerson(#alice_account, #alice_profile)
```

Those are domain claims. They do not merge identity values at the runtime level.

`to_literal` preserves identity values as identity source. It uses a named `#identity` when a
source-compatible identity name is available and a numeric `#12345` form otherwise. A relation's
identity is still an identity; its naming symbol is a separate value. Numeric identity text refers
to the same identity number when decoded, while a named identity is resolved in the receiving
world. Choose an explicit application naming protocol when transferring references between
independently created worlds.

Frobs are delegated values. They carry a delegate identity plus a payload and can participate in
dispatch without becoming durable world objects.

Frob delegation is value-level interpretation. It is separate from prototype delegation between
durable identities, which is used by role matching and dispatch.

## Persistence Is Recursive

Capability values are runtime authority tokens. They may appear while a task is running, but they
are not persistable source literals and should not be treated as durable policy. Local functions
also belong to a running VM. Install a verb when behaviour needs to remain available in the world;
storing a local closure in a fact is not a way to install behaviour.

Containers do not hide ephemeral values from persistence checks. A list containing a capability,
a map with a function as a key, a frob containing either, or a relation with an ephemeral cell is
also non-persistable. Errors and ranges follow the same rule for their payloads and endpoints.
Ordinary strings, numbers, symbols, identities, and bytes are durable values.

## Numeric Values

Mica has two numeric value families: integers and floats.

- Mica `Int` is a 56-bit signed integer (`-2^55 ..= 2^55 - 1`).
- Mica `Float` is a finite IEEE-754 binary32 (single-precision) value. Infinity and NaN are not
  valid Mica values; operations that would produce them return `E_ARITH` or `E_DIV` instead.
  Negative zero canonicalizes to positive zero.

Mica `Float` has less integer precision than Mica `Int`. Binary32 can represent every integer up to
`2^24` exactly, but above that some integers round to the nearest representable float. A 56-bit Mica
integer always carries more precision than a binary32 float.

Both integer endpoints have decimal source literals. The negative endpoint is one unit farther
from zero than the positive endpoint, so its sign is part of validating the literal:

```mica,eval
let minimum = -36028797018963968
let maximum = 36028797018963967
require minimum + maximum == -1
require from_literal(to_literal(minimum)) == ok(minimum)
```

### Numeric Equality And Key Identity

Mica has two distinct comparison concepts:

1. **Language numeric comparison** is used by `==`, `!=`, `<`, `<=`, `>`, `>=` in expressions and by
   relation rule guards. When both operands are numeric, integers and floats compare by numeric
   value rather than by value kind. Therefore `1 == 1.0` is true.

2. **Canonical value order** is the total, type-sensitive order used by maps, relation tuples,
   indexes, hashing, and persistence. An integer and a float remain distinct stored values even when
   numerically equal, and may be distinct keys.

```mica,eval
require 1 == 1.0                         // numeric equality
let labels = {1 -> "int", 1.0 -> "float"} // distinct map keys
require labels[1] == "int"
require labels[1.0] == "float"
require [1] != [1.0]                     // structural equality
return labels
```

Numeric equality applies when the two operands themselves are numbers. It does not recursively
coerce cells inside lists, maps, frobs, or relation values. This is why the list comparison above is
false even though its individual numeric elements compare equal.

Arithmetic on two integers stays integer where the operation permits it. Mixing an integer and a
float converts the arithmetic operands to binary32, which can lose precision. Mixed *comparison*
does not round the integer to binary32 first: a large integer and a nearby rounded float can compare
unequal. Integer overflow and non-finite arithmetic results raise `E_ARITH`; division or remainder
by zero raises `E_DIV`. The arithmetic operators do not concatenate strings or collections.

### Division Result Kinds

Division follows a result-kind rule:

- `4 / 2` produces `Int(2)` because integer division is exact.
- `4 / 2.0` produces `Float(2.0)` because a float operand produces a float result.
- `5 / 2` produces `Float(2.5)` because integer division is not exact.

The numeric values `4 / 2` and `4 / 2.0` are numerically equal (`2 == 2.0` is true), but the
resulting values have different kinds and occupy different map or relation keys.

### Structural Sorting

Structural sorting uses canonical value order, so it can distinguish and order numeric values that
language equality considers numerically equal. A sorted list may place `1` before `1.0` even though
`1 == 1.0`.
