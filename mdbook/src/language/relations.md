# Relations

A relation is how Mica records that something is true in the world.

If an ordinary object system would write:

```text
sensor.location = lab
sensor.label = "temperature sensor"
sensor.calibrated = true
```

Mica usually writes facts:

```mica
InstalledAt(#sensor, #lab)
Label(#sensor, "temperature sensor")
Calibrated(#sensor)
```

This is the core shift. State is not packed into one hidden record behind `#sensor`. State is a
collection of named relationships that can be queried, derived, indexed, authorized, filed out, and
extended independently.

Each relation has a fixed number of positions. `Calibrated` has one position: the instrument whose
calibration is current. `InstalledAt` has two positions: the instrument and the site. `Label` has
two positions: the subject and its label string. In database language, those fixed-position facts
are tuples and the number of positions is the relation's arity, but the practical rule is simpler:
every fact in the same relation has the same shape.

## Relation Semantics

Relations have set semantics. A fact is either present or absent; asserting the same fact twice does
not create two logical copies. There is no meaningful tuple order inside the relation. Query results
preserve these set semantics as immutable relation values.

The positions in a relation are ordinal. `InstalledAt(#sensor, #lab)` means position 0 is `#sensor`
and position 1 is `#lab`. The positions do not have stored column names. Names come from the
relation and from how queries bind variables.

Mica values are ordinary values when stored in relations. `nothing` denotes the zero-column empty
relation; it is not SQL `NULL`, and Mica does not use SQL's three-valued logic.

Create a relation with a builtin:

```mica
make_relation(:InstalledAt, 2)
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
assert InstalledAt(#sensor, #lab)
```

That fact says that `#sensor` is installed at `#lab`. The meaning comes from the relation and from
the rules and verbs that use it.

Query with free variables:

```mica
return InstalledAt(?instrument, #lab)
```

The `?instrument` part is a query variable. The result is a relation value whose source form is:

```mica
[:instrument] { [#sensor], [#controller] }
```

The heading names the free variables and each row contains their values. Relation values are
canonical sets, so projection removes duplicate answer rows and programs must not depend on row
order.

Relation values are iterable. Each observed row is exposed as a binding map, so existing row access
remains direct without allocating a map for every answer up front:

```mica
for row in InstalledAt(?instrument, ?site)
  emit(#observer, row[:instrument])
end
```

A relation call with no free variables is a predicate test:

```mica
if InstalledAt(#sensor, #lab)
  return "installed"
end
```

You can also leave multiple positions open:

```mica
return InstalledAt(?instrument, ?site)
```

That returns a relation value with `:instrument` and `:site` columns.

Repeated query variables require equality:

```mica
LinkedTo(?subject, ?subject)
```

This only matches facts where both positions contain the same value.

`_` is a wildcard, not a binding:

```mica
InstalledAt(_, #lab)
```

It matches any first-position value but does not include that value in the result.

Functional relations declare key positions and support single-value projection:

```mica
make_functional_relation(:Label, 2, [0])
assert Label(#sensor, "temperature sensor")
return one Label(#sensor, ?label)
```

The key-position list is zero-based. In `make_functional_relation(:Label, 2, [0])`, position 0 is
the key. For each concrete key, the relation contains at most one matching tuple. The relation as a
whole may still contain many tuples with different keys.

Functional relation metadata is a real constraint used by replacement and dot sugar. It is not just
documentation. Code that assigns through a functional relation replaces the tuple for that key
rather than adding another competing fact.

`one` projects at most one result. It is useful for relations such as `Label`, where the program
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
return #sensor.label
#sensor.label = "temperature sensor A"
```

This is convenient, but it is still relation access. The assignment replaces the
`Label(#sensor, value)` fact for the key `#sensor`; it does not write a field inside a record.

The dot name maps to a declared functional binary relation. A read such as `#sensor.label` is
equivalent to a single-result projection such as `one Label(#sensor, ?label)`. If there is no
matching tuple, the result is `nothing`. There is no fallback to hidden object storage.

Mica relation calls are closer to Datalog predicates than SQL `SELECT` statements. Named relations
have no implicit row ids, no stored column names, and no SQL `NULL`. Query variables provide the
heading of a first-class answer relation; row maps are produced only when code observes individual
rows.
