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
- ephemeral capability values.

The value layer is intentionally small and regular. Named relations can store any persistable value,
and verbs can accept ordinary values, identities, frobs, or relation values through the same
role-binding mechanism. The language should not force authors to turn every structured value into a
durable object just so it can be passed around.

Bytes and relation values have source literals and can cross task, storage, RPC, and IPC value
boundaries. A relation value is persistable when every cell it contains is persistable. Capability
values are deliberately neither serializable nor persistable.

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
so duplicate rows are removed and row order is not observable.

At the JSON boundary, JSON `null` maps to the explicit tagged map `{:json -> :null}` and that tag
maps back to `null`. Relation values, including options, have no implicit JSON representation and
must be projected into lists or maps explicitly.

Indexing is strict. Reading an absent list position, relation row, or map key raises `E_INDEX`;
invalid index types and out-of-range indexed assignments raise the same error. Optional bindings
handle absent arguments explicitly and do not rely on a missing index producing a sentinel value.
Use `index_or(collection, index, default)` when absence is expected and should produce a default.

Symbols are interned names used for selectors, relation names, policy surfaces, message tags, and
other program-facing labels:

```mica
:approve
:tool_call
:inspection
```

Error codes are also values. By convention, error-code literals begin with `E_`:

```mica
E_PERMISSION
E_NOT_FOUND
```

Errors can be raised and recovered by the error-handling surface, but the code itself is still a
value. Mica does not require a closed universe of built-in error names.

Lists are ordered sequences:

```mica
["inspect", "repair", "calibrate"]
```

Maps are associative values:

```mica
{:actor -> #alice, :request -> #release_change}
```

Maps remain useful even in a relation-first language. Relations are for world state and queryable
facts. Maps are for local structured values: role maps, options, decoded messages, frob payloads,
and temporary results. A map can be stored in a relation tuple, but doing so usually means the
relation cannot query inside that map without additional derived facts or host support.

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
EquivalentAgent(#planner_v1, #planner_v2)
SamePerson(#alice_account, #alice_profile)
```

Those are domain claims. They do not merge identity values at the runtime level.

Frobs are delegated values. They carry a delegate identity plus a payload and can participate in
dispatch without becoming durable world objects.

Frob delegation is value-level interpretation. It is separate from prototype delegation between
durable identities, which is used by role matching and dispatch.

Capability values are runtime authority tokens. They may appear while a task is running, but they
are not persistable source literals and should not be treated as durable policy.

## Numeric Values

Mica has two numeric value families: integers and floats.

- Mica `Int` is a 56-bit signed integer (`-2^55 ..= 2^55 - 1`).
- Mica `Float` is a finite IEEE-754 binary32 (single-precision) value. Infinity and NaN are not
  valid Mica values; operations that would produce them return `E_ARITH` or `E_DIV` instead.
  Negative zero canonicalizes to positive zero.

Mica `Float` has less integer precision than Mica `Int`. Binary32 can represent every integer up to
`2^24` exactly, but above that some integers round to the nearest representable float. A 56-bit Mica
integer always carries more precision than a binary32 float.

### Numeric Equality And Key Identity

Mica has two distinct comparison concepts:

1. **Language numeric comparison** is used by `==`, `!=`, `<`, `<=`, `>`, `>=` in expressions and by
   relation rule guards. When both operands are numeric, integers and floats compare by numeric
   value rather than by value kind. Therefore `1 == 1.0` is true.

2. **Canonical value order** is the total, type-sensitive order used by maps, relation tuples,
   indexes, hashing, and persistence. An integer and a float remain distinct stored values even when
   numerically equal, and may be distinct keys.

```mica
1 == 1.0          -- true: language numeric equality
{:1 -> "int", 1.0 -> "float"}  -- two entries: canonical key identity
```

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
