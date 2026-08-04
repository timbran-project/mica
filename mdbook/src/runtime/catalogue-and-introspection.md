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

## Installed Behaviour and Source Ownership

Installed methods are described by `MethodSelector`, `Param`, `Delegates`, `MethodProgram`,
`ProgramBytes`, and `MethodSource`. Filein ownership is recorded by `SourceOwnsFact`,
`SourceOwnsRule`, and `SourceOwnsRelation`. These are primarily tooling surfaces; author-facing
definitions should normally use `verb`, relation rules, and filein units.

Use `fileout(:unit)` to recover one unit's owned source, or `fileout_rules()` to render active rule
source. See [Filein and Fileout](./filein-fileout.md).

## Runtime Context and Tasks

`actor()`, `principal()`, and `endpoint()` expose the current task context. Missing actor or
principal values return `nothing`; every task has an endpoint.

Endpoint state is represented by volatile relations: `Endpoint`, `EndpointActor`,
`EndpointPrincipal`, `EndpointProtocol`, and `EndpointOpen`. It is process-lifetime state and is not
restored as durable session authority after restart.

`tasks()` returns snapshots of currently managed tasks. Each map has an `:id` and a `:state` of
`:running` or `:suspended`. Treat these maps as observational runtime data, not durable identities
or a task-control API. Coordinate tasks through dispatch, mailboxes, and durable progress facts.

Catalogue subscriptions let root-authority tooling observe later schema and rule changes. See
[Subscriptions](./subscriptions.md).
