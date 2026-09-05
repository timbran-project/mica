# Program Structure

Mica code is compiled and run as tasks. A task can be as small as one expression typed at the REPL,
or it can be the execution of an installed verb. In either case, the code runs with a transaction
and an authority context.

This is deliberately different from a language where source files are the only place definitions
live. Mica has source text, but the running world is the primary environment. Identities, relations,
rules, and verbs are installed into that world and can be changed while it is running.

Names for identities, relations, dot access, and installed methods are resolved against the
compiler's view of the world before a task starts. An already running task uses its compiled
references; creating a name does not retroactively change that task's bytecode.

The administrative source runner coordinates installation and compilation. It recognizes root calls
such as `make_identity(:sensor)` and `make_functional_relation(:Label, 2, [0])`, installs their
literal names, and refreshes the compiler context before compiling dependent expressions. This
allows a complete administrative submission to declare names and then use them:

```mica,eval
make_identity(:sensor)
make_functional_relation(:Label, 2, [0])
assert Label(#sensor, "temperature sensor")
let exactly {label} = Label(#sensor, ?label)
require label == "temperature sensor"
```

The declaration pass recognizes these direct root calls with literal names and schema arguments. A
name computed while executing ordinary task code becomes available to later compilations after it is
committed. For example, constructing an identity name from text does not make a matching `#name`
reference available earlier in the compilation of that same task.

When a submission mixes executable code with verb or rule installation, the runner separates the
installation stages and refreshes its context between them. Filein uses this coordination while
staging the imported unit. See [Filein and Fileout](../runtime/filein-fileout.md) for atomic unit
replacement and ownership.

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

Administrative installation requires a source request with no actor or principal and with
administrative authority. Requests on behalf of an actor or principal execute task code against
installed definitions. The same task path applies to anonymous requests without administrative
authority. A host should select the request context and authority explicitly; the absence of an
actor alone does not authorize installation.

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

Stored method programs contain bytecode and constants. Persistable constants include nested lists,
maps, relations, ranges, frobs, and structured errors, as well as scalar values. Symbols are encoded
by name so loading a program in another process preserves their meaning. Program loading validates
the artifact before execution and reconstructs its type facts from the instructions.

A local `fn` value has a different lifetime from an installed verb. It belongs to its running
execution environment and can capture local bindings. Installing a verb stores its definition and
compiled program for later tasks; it does not store a live closure from the importing task.
Capabilities likewise belong to runtime authority, which each task receives separately from the
stored program.

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
