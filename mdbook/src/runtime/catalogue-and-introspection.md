# Catalogue and Introspection

Mica exposes its schema, rules, methods, source ownership, and endpoint state as relations. These
surfaces let tools inspect a live system without reading private runtime structures.

## Relation and Rule Catalogue

The core catalogue relations are:

| Relation                                     | Meaning                             |
| -------------------------------------------- | ----------------------------------- |
| `Relation(relation)`                         | registered relation identity        |
| `RelationName(relation, name)`               | name symbol                         |
| `Arity(relation, arity)`                     | tuple width                         |
| `RelationDurability(relation, durability)`   | `:durable` or `:volatile`           |
| `ConflictPolicy(relation, policy)`           | set or functional conflict policy   |
| `FunctionalKey(relation, ordinal, position)` | ordered functional key positions    |
| `Index(relation, index)`                     | declared index identity             |
| `IndexPosition(index, ordinal, position)`    | indexed tuple positions             |
| `IndexStorageKind(index, kind)`              | index storage representation        |
| `ArgumentName(relation, position, name)`     | argument name metadata when present |
| `Rule(rule)`                                 | installed rule identity             |
| `RuleHead(rule, relation)`                   | rule head relation                  |
| `RuleSource(rule, source)`                   | filed-in rule source                |
| `ActiveRule(rule, active)`                   | rule activation state               |

For example, a task with read authority for both catalogue relations can list relation names and
arities:

```mica
return natural_join(
  RelationName(?relation, ?name),
  Arity(?relation, ?arity)
)
```

Rule helpers provide direct access where a procedural form is more convenient:

```mica
let active = rules(:Requires)
let source = describe_rule(active[0])
```

`disable_rule` changes catalogue state and requires administrative authority.

### Following Identities Through the Catalogue

A relation name is a symbol used to find a relation identity. The other catalogue relations refer
to that identity. Resolve it once, then use the same value to inspect its arity, key, and indexes:

```mica,eval
make_functional_relation(:TeamLabel, 3, [0, 1])
let exactly {relation} = RelationName(?relation, :TeamLabel)
require Arity(relation, 3)
require ConflictPolicy(relation, :functional)
require FunctionalKey(relation, ?ordinal, ?position) == [:ordinal, :position] {
  [0, 0],
  [1, 1]
}
```

Positions are zero-based tuple positions. An ordinal describes where that position occurs in the
key or index definition. For example, an index declared as `[2, 0]` has position 2 at ordinal 0 and
position 0 at ordinal 1. Bindings for position 2 can use its leading prefix; a binding for position 0
alone cannot use that prefix. An index controls access to rows; a functional key also constrains
stored facts and coordinates concurrent writes.

`ArgumentName` contains optional schema metadata. Query variables supply the heading of each
query result independently. Writing `TeamLabel(?team, ?locale, ?label)` chooses those three answer
column names; it does not update the relation's argument metadata.

`Index` and `IndexPosition` describe declared indexes. `IndexStorageKind` reports the physical
representation in the task's committed snapshot: `:btree` or `:radix`. A small tuple store uses a
B-tree, and larger stores use a radix tree; secondary indexes use radix trees. The reported kind
can therefore change as facts are committed. Computed relations have no kernel-managed physical
index representation, so their declared indexes have no `IndexStorageKind` row.

```mica,eval
let exactly {relation} = RelationName(?relation, :RelationName)
let declared = Index(relation, ?index)
require project(declared) == ()
require project(natural_join(declared, IndexStorageKind(?index, ?kind))) == [] {}
```

### Rule Definitions and Activation

Disabling a rule retains its definition and source in the catalogue. Its `ActiveRule` row changes
to `false`; the presence of a `Rule` row alone does not mean that rule contributes answers. Join
the activation predicate when listing rules that currently derive a relation:

```mica
let enabled = ActiveRule(?rule, true)
let heads = RuleHead(?rule, ?relation)
let sources = RuleSource(?rule, ?source)
return natural_join(natural_join(enabled, heads), sources)
```

Several rules can have the same head relation. Inspecting all of them explains the possible sources
of its derived rows. The stored source describes the definition; evaluating the head relation
computes its answers against the current facts.

## Fact Neighbourhoods

Three computed relations support inspectors and provenance tools:

| Relation                                                       | Meaning                                                               |
| -------------------------------------------------------------- | --------------------------------------------------------------------- |
| `SubjectFact(subject, relation, tuple)`                        | facts whose first value is the subject                                |
| `MentionedFact(subject, relation, position, tuple)`            | facts in the current extensional view mentioning the subject anywhere |
| `ExtensionalMentionedFact(subject, relation, position, tuple)` | stored facts mentioning the subject anywhere                          |

The tuple is returned as a list value. These relations describe facts visible through the current
snapshot and still obey relation read authority. `MentionedFact` and `ExtensionalMentionedFact`
currently expose the same stored-fact rows; the latter names the extensional-only contract
explicitly for code that must exclude rule-derived results.

The subject convention is positional. In `Assigned(job, worker)`, the job is the subject because it
occupies position 0. A mention query for the worker finds the same fact at position 1:

```mica,eval
make_relation(:Assigned, 2)
assert Assigned(:job17, :alice)
let exactly {relation} = RelationName(?relation, :Assigned)
require SubjectFact(:job17, relation, ?tuple) == [:tuple] { [[:job17, :alice]] }
require MentionedFact(:alice, relation, ?position, ?tuple) == [:position, :tuple] {
  [1, [:job17, :alice]]
}
```

A value repeated in two positions produces two mention rows, one for each position. Mention
queries compare whole tuple cells. A list containing `:alice` is a different cell from `:alice`,
so these queries do not recursively search inside lists, maps, or nested relation values. Use an
explicit relation for references that inspectors need to follow directly.

## Installed Behaviour and Source Ownership

Installed methods are described by `MethodSelector`, `Param`, `Delegates`, `MethodProgram`,
`ProgramBytes`, and `MethodSource`. Filein ownership is recorded by `SourceOwnsFact`,
`SourceOwnsRule`, and `SourceOwnsRelation`. These are primarily tooling surfaces; author-facing
definitions should normally use `verb`, relation rules, and filein units.

Use `fileout(:unit)` to recover one unit's owned source, or `fileout_rules()` to render active rule
source. See [Filein and Fileout](./filein-fileout.md).

## Runtime Context and Tasks

`actor()`, `principal()`, and `endpoint()` expose the current task context. Actor and principal
return `option<identity>` because either may be absent; every task has an endpoint.

Endpoint state is represented by volatile relations: `Endpoint`, `EndpointActor`,
`EndpointPrincipal`, `EndpointProtocol`, and `EndpointOpen`. It is process-lifetime state and is not
restored as durable session authority after restart.

`tasks()` returns snapshots of currently managed tasks. Each map has an `:id` and a `:state` of
`:running` or `:suspended`. Treat these maps as observational runtime data, not durable identities
or a task-control API. Coordinate tasks through dispatch, mailboxes, and durable progress facts.

Catalogue subscriptions let root-authority tooling observe later schema and rule changes. See
[Subscriptions](./subscriptions.md).
