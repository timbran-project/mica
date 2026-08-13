# Program Structure

Mica code is compiled and run as tasks. A task can be as small as one expression typed at the REPL,
or it can be the execution of an installed verb. In either case, the code runs with a transaction
and an authority context.

This is deliberately different from a language where source files are the only place definitions
live. Mica has source text, but the running world is the primary environment. Identities, relations,
rules, and verbs are installed into that world and can be changed while it is running.

At the REPL or through `eval`, ordinary source is compiled as one task and then executed. Names for
identities, relations, dot sugar, and installed methods are resolved before the task starts running.
That means this is normally a two-submission sequence:

```mica
make_identity(:sensor)
make_functional_relation(:Label, 2, [0])
```

then:

```mica
assert Label(#sensor, "temperature sensor")
let exactly {label} = Label(#sensor, ?label)
return label
```

The first task creates an identity and a functional relation keyed by position 0. After it commits,
the runner refreshes its compile context. The second task can then compile references to `Label` and
`#sensor`, assert a fact, and ask for the single label attached to `#sensor`.

Filein uses the same language, but the runner may split root source into definition and task chunks
when installation needs to happen before later code can compile. That makes this shape valid in a
filein:

```mica
make_identity(:sensor)
make_functional_relation(:Label, 2, [0])
assert Label(#sensor, "temperature sensor")
```

The same distinction shows up when authoring larger systems. A line such as:

```mica
assert Label(#sensor, "temperature sensor")
```

changes stored world state when the task commits. A verb definition such as:

```mica
verb describe(actor, item)
  let exactly {text} = Description(item, ?text)
  return text
end
```

changes the set of behaviours later dispatches can find. Both are executable world mutations.

Filein files use the same language inside a surrounding import/export flow. The importer runs source
in an order that lets committed definitions update the compiler context before later code depends on
them. Verb bodies inside a filein use ordinary Mica syntax.

Definitions such as verbs, rules, identities, and relations are not external metadata. They become
facts and installed definitions in the live store.

Root source may mix relation rules, method/verb definitions, and executable task code. The runner
handles the installation boundaries internally, refreshes compiler context after committed
definitions, and then compiles later task code against the updated world.

There is one current implementation boundary to keep in mind: a contextual source submission, such
as code run on behalf of an actor, cannot install rules or methods. Contextual submissions run as
tasks under an actor or principal's authority; installation still belongs to root/system source.

That means program structure has two layers:

- task code, which runs now and may return a value;
- installed world definitions, which change what future tasks can query or dispatch to.

For example, this source installs a verb:

```mica
verb describe(actor, item)
  let exactly {text} = Description(item, ?text)
  return text
end
```

After the task that installs it commits, later tasks can invoke `:describe`.

## Installed Definitions

An installed definition is not a separate Rust-side registry entry or a hidden compiler artefact. It
is part of the live world model. The implementation may cache compiled programs, dispatch tables, or
authority contexts, but those caches are derived from installed definitions and committed facts.

That matters for fileout and multi-author systems. If an author adds a relation, rule, or verb, the
definition can be inspected, exported, reviewed, and replaced as world state. The system does not
need to reconstruct meaning from an external application source tree before the world can run.

## Task Bodies

A task body is ordinary Mica code. It may compute values, query relations, assert and retract facts,
call builtins, emit effects, or invoke verbs:

```mica
let exactly {work} = AssignedTo(?work, actor)
let exactly {text} = Description(work, ?text)
emit(actor, text)
```

The task body is expression-oriented. Forms such as `assert`, `emit`, and assignment still produce
values, even when callers usually ignore those values. Use `return` when a body should stop and
produce a specific result.
