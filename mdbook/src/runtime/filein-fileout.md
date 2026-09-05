# Filein and Fileout

Filein and fileout provide a human-readable import/export surface for live world state.

Filein runs ordinary Mica source:

```mica
make_identity(:sensor)
make_functional_relation(:Label, 2, [0])
assert Label(#sensor, "temperature sensor")

verb inspect(actor, subject)
  let exactly {label} = Label(subject, ?label)
  return label
end
```

Fileout emits readable source that can be reviewed, edited, version controlled, and filed back in.

This is useful for more than object worlds. A fileout can capture the schema, rules, seed facts, and
verb definitions for an agent workspace, including relations such as `Task`, `Artifact`,
`Observation`, `ToolResult`, `AssignedTo`, and `DependsOn`. The result is an auditable bootstrap and
review format for live memory, not a copy of a hidden object heap.

Units group filed-in state so replacement workflows can update an imported source unit over top of a
live workspace. The runtime stores the resulting identities, relations, facts, rules, and verb
definitions. It does not rely on storing the original file text as the source of truth.

Replacement is atomic at the unit boundary. The runtime stages a replacement against an isolated
kernel and publishes one durable commit. A failed filein leaves the previously committed unit
intact.

Filein can include text files into compiled source with `include_text("path")`. The path is resolved
relative to the filed-in source file by the `mica filein` command. This is intended for large text
assets such as CSS and JavaScript inside verbs:

```mica
verb page_style()
  return include_text("style.css")
end
```

The path uses ordinary Mica string escaping, including `\u{...}` for Unicode characters. The loader
inserts the file's contents as one string value. Quotes, backslashes, newlines, and control
characters in the asset retain their meaning as text; they do not become Mica expressions.

Fileout preserves the `include_text(...)` call in stored verb source rather than emitting the
included text inline. Filing the output back in therefore requires the referenced asset file to be
present beside the fileout source.

Filein also has a grant block surface for durable authorization policy facts. It is source sugar
over the ordinary policy relations, so the stored world still contains `CanRead`, `CanWrite`,
`CanInvoke`, `CanEffect`, and their `RoleCan*` counterparts:

The following complete filein creates its subjects and policy relations before granting authority:

```mica,filein
make_identity(:web)
make_identity(:reviewer)
make_relation(:CanRead, 2)
make_relation(:CanWrite, 2)
make_relation(:CanInvoke, 2)
make_relation(:CanEffect, 1)
make_relation(:RoleCanRead, 2)
make_relation(:RoleCanInvoke, 2)

grant #web
  read:
    :HttpRequest
    :RequestPath
  write:
    :RequestBody
  invoke:
    :http_request
    :http_response
  effect
end

grant role #reviewer
  read:
    :Label
    :ReviewStatus
  invoke:
    :approve
end
```

The first block expands to `Can*` assertions for `#web`; the second expands to `RoleCan*` assertions
for `#reviewer`. Fileout recognizes owned policy facts and emits grant blocks instead of long runs
of repeated assertions.
