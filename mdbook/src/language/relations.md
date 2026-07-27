# Relations

A relation is how Mica records that something is true in the world.

If an ordinary object system would write:

```text
lamp.location = room
lamp.name = "brass lamp"
lamp.portable = true
```

Mica usually writes facts:

```mica
LocatedIn(#lamp, #room)
Name(#lamp, "brass lamp")
Portable(#lamp)
```

This is the core shift. State is not packed into one hidden record behind `#lamp`. State is a
collection of named relationships that can be queried, derived, indexed, authorized, filed out, and
extended independently.

Each relation has a fixed number of positions. `Portable` has one position: the thing that is
portable. `LocatedIn` has two positions: the thing and the place. `Name` has two positions: the
thing and the string used as its name. In database language, those fixed-position facts are tuples
and the number of positions is the relation's arity, but the practical rule is simpler: every fact
in the same relation has the same shape.

## Relation Semantics

Relations have set semantics. A fact is either present or absent; asserting the same fact twice does
not create two logical copies. There is no meaningful tuple order inside the relation. Query results
preserve these set semantics as immutable relation values.

The positions in a relation are ordinal. `LocatedIn(#coin, #room)` means position 0 is `#coin` and
position 1 is `#room`. The positions do not have stored column names. Names come from the relation
and from how queries bind variables.

Mica values are ordinary values when stored in relations. `nothing` denotes the zero-column empty
relation; it is not SQL `NULL`, and Mica does not use SQL's three-valued logic.

Create a relation with a builtin:

```mica
make_relation(:LocatedIn, 2)
```

Named relations are durable by default: their facts survive a process restart when the runtime uses
persistent storage. A relation whose facts are useful only while the current process is running can
instead be declared volatile:

```mica
make_relation(:ActiveRequest, 1, :volatile)
make_functional_relation(:RequestPath, 2, [0], :volatile)
```

Volatile relations otherwise use the same transactions, indexes, rules, constraints, queries, and
authority checks as durable relations. Their metadata remains part of the catalogue, but their facts
are omitted from persistent commits and the relation starts empty after recovery. Volatility is a
storage-lifetime property, not an ambient visibility boundary; include an explicit owner such as a
request or endpoint identity in a tuple when its lifetime or access must be scoped.

Volatile facts do not expire automatically while the process is running. A host that installs
request or session facts must retract those facts when that lifecycle ends; volatility only defines
what recovery does after a restart.

Assert facts into it:

```mica
assert LocatedIn(#coin, #room)
```

That fact says that `#coin` is located in `#room`. Mica does not care whether `#coin` is a game
object, an agent task, a document, or an operational entity. The meaning comes from the relation and
from the rules and verbs that use it.

Query with free variables:

```mica
return LocatedIn(?thing, #room)
```

The `?thing` part is a query variable. The result is a relation value whose source form is:

```mica
[:thing] { [#coin], [#lamp] }
```

The heading names the free variables and each row contains their values. Relation values are
canonical sets, so projection removes duplicate answer rows and programs must not depend on row
order.

Relation values are iterable. Each observed row is exposed as a binding map, so existing row access
remains direct without allocating a map for every answer up front:

```mica
for row in LocatedIn(?thing, ?place)
  emit(#observer, row[:thing])
end
```

A relation call with no free variables is a predicate test:

```mica
if LocatedIn(#coin, #room)
  return "the coin is here"
end
```

You can also leave multiple positions open:

```mica
return LocatedIn(?thing, ?place)
```

That returns a relation value with `:thing` and `:place` columns.

Repeated query variables require equality:

```mica
SameRoom(?who, ?who)
```

This only matches facts where both positions contain the same value.

`_` is a wildcard, not a binding:

```mica
LocatedIn(_, #room)
```

It matches any first-position value but does not include that value in the result.

Functional relations declare key positions and support single-value projection:

```mica
make_functional_relation(:Name, 2, [0])
assert Name(#lamp, "brass lamp")
return one Name(#lamp, ?name)
```

The key-position list is zero-based. In `make_functional_relation(:Name, 2, [0])`, position 0 is the
key. For each concrete key, the relation contains at most one matching tuple. The relation as a
whole may still contain many tuples with different keys.

Functional relation metadata is a real constraint used by replacement and dot sugar. It is not just
documentation. Code that assigns through a functional relation replaces the tuple for that key
rather than adding another competing fact.

`one` projects at most one result. It is useful for relations such as `Name`, where the program
expects a single value and should fail loudly if the data is ambiguous.

If the query produces zero results, `one` returns `nothing`, the zero-column empty relation. If it
produces more than one result, `one` raises `E_AMBIGUOUS`. If the single result has exactly one free
variable, `one` returns that variable's value. If the single result has multiple free variables, the
result shape is a binding map.

See [Keys and Single-Valued Relations](./keyed-relations.md) for a fuller explanation of keys,
replacement, property-style access, and composite keys.

## Relation Value Algebra

Query results compose through four initial relational operations:

```mica
let people = Person(?person, ?name)
let active = Active(?person)

let names = project(people, :name)
let active_people = natural_join(people, active)
let either = union(Current(?person), Pending(?person))
let remaining = difference(Current(?person), Removed(?person))
```

`project` keeps the named columns and removes duplicate rows. It accepts zero columns, producing the
zero-column unit relation when the input is non-empty. `union` and `difference` require identical
headings. `natural_join` matches every shared column name; with no shared columns it produces a
Cartesian product. Join keys use canonical value identity, so an integer and float do not join
merely because language numeric equality considers them equal.

Relation values can be returned from tasks, carried across RPC or IPC value boundaries, and stored
as cells in durable named relations when all nested cells are persistable. Literal syntax uses a
symbol heading followed by rows:

```mica
[:person, :name] { [#alice, "Alice"], [#bob, "Bob"] }
```

`nothing` is exactly `[] {}`. The zero-column unit relation is `[] {[]}`. These are different
values: the former has no rows and is falsey, while the latter has one empty row and is truthy.

Dot sugar is only valid for declared functional binary relations:

```mica
return #lamp.name
#lamp.name = "golden lamp"
```

This is convenient, but it is still relation access. The assignment replaces the
`Name(#lamp, value)` fact for the key `#lamp`; it does not write a field inside a record.

The dot name maps to a declared binary relation. A read such as `#lamp.name` is equivalent to a
single-result projection such as `one Name(#lamp, ?name)`. If there is no matching tuple, the result
is `nothing`. If more than one tuple matches a non-functional backing relation, the read raises
`E_AMBIGUOUS`. There is no fallback to hidden object storage.

Mica relation calls are closer to Datalog predicates than SQL `SELECT` statements. Named relations
have no implicit row ids, no stored column names, and no SQL `NULL`. Query variables provide the
heading of a first-class answer relation; row maps are produced only when code observes individual
rows.
