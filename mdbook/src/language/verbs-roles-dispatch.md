# Verbs, Roles, and Dispatch

Verbs install behaviour into the live world. They are the closest Mica concept to methods in
class-based languages, but they are not stored "inside" a receiver object.

A verb declares a selector and a set of named parameters:

```mica
verb approve(actor @ #reviewer, request @ #change_request)
  ReadyForReview(actor, request) || return false
  assert ApprovedBy(request, actor)
  return true
end
```

The selector is `:approve`. The parameter names are `actor` and `request`. The restrictions after
`@` describe what values may fill those roles for this verb branch to apply.

The `@ #reviewer` and `@ #change_request` parts are role restrictions. They say that this verb
applies when the supplied values match those prototypes. Matching can use prototype delegation, so a
concrete identity such as `#alice` can match `#reviewer` if the world says Alice delegates to that
prototype.

The setup is ordinary relation data:

```mica
make_relation(:Delegates, 3)

assert Delegates(#alice, #reviewer, 0)
assert Delegates(#release_change, #change_request, 0)
```

The third position gives delegation order. It allows multiple prototypes to be ordered without
making parentage a built-in object-table field.

Dispatch uses named roles:

```mica
:approve(actor: #alice, request: #release_change)
```

This is different from positional function calls. The call site says which value is the actor and
which value is the request. That makes dispatch able to consider several domain roles without making
one of them the privileged receiver.

The dispatcher looks for installed verbs whose selector is `:approve` and whose role restrictions
match the supplied role values. There is no privileged `self` argument in the dispatch model. A call
can dispatch on actor, request, tool, target, or any other role the domain cares about.

The compiler also supports positional dispatch syntax:

```mica
split("a b")
```

If `split` is not a local function or registered runtime builtin, the compiler treats the call as
dispatch to selector `:split` and binds the supplied values by the installed method parameter
positions. This is convenient for primitive or library-style operations, but named-role dispatch is
clearer when several domain roles are involved.

Primitive values can also appear in roles when the language has a prototype for that primitive
family:

```mica
verb split(text @ #string)
  // string-specific behaviour
end
```

The exact primitive prototypes are part of the standard environment, not ordinary durable objects
created by user code.

A verb parameter may also have a [value-kind annotation](./value-kind-annotations.md), written after
its dispatch restriction when both are present:

```mica
verb words(text @ #string: string) -> list
  return [text]
end
```

The `@ #string` restriction participates in method selection. The `: string` annotation checks the
selected method's parameter value and does not make that method more or less applicable. The result
annotation proves the kind of every normal result; it does not add a dispatch restriction.

The same model works for agent workflows:

```mica
verb summarize(agent @ #agent, artifact @ #artifact)
  emit(agent, #tool_call<{:tool -> :summarize, :artifact -> artifact}>)
  return true
end

:summarize(agent: #planner, artifact: #release_notes)
```

Here dispatch considers both an agent and an artefact.

Receiver-call sugar may be used where it reads naturally:

```mica
#bucket:pour_into(quantity: 500, unit: :ml, to: #other_bucket)
```

This is sugar for a named-role dispatch where the receiver is just another role:

```mica
:pour_into(receiver: #bucket, quantity: 500, unit: :ml, to: #other_bucket)
```

It is not a privileged object slot lookup. There is no hidden `self` whose fields or method table
are searched first.

A verb definition installs one or more method identities in the world. In this specific sense, a
method is an identity representing an applicable compiled branch of a verb: it has facts describing
its selector, parameters, restrictions, and compiled program. That means behaviour can be inspected
and filed out through the same world mechanism as other definitions.

The author-facing `verb` form generates method identities as needed. Fileout and lower-level tooling
may also use an explicit method form:

```mica
method #approve_change :approve
  roles actor @ #reviewer, request @ #change_request
do
  return request
end
```

That form exists to preserve explicit method identity in import/export workflows. Most handwritten
code should use `verb`.

If multiple methods match a dispatch, the dispatcher must choose according to the current dispatch
rules or report ambiguity. Authoring should avoid relying on accidental ties.

When ambiguity is useful, authors should make it explicit at a higher level: query for applicable
behaviours, inspect the candidates, or install a more specific dispatch rule. Silent accidental
choice is the part to avoid.
