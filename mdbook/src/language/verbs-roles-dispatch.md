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

## Selecting a Branch

Dispatch first finds applicable branches, then compares their restrictions. A branch is more
specific when it accepts a subset of another branch's role values and narrows at least one role. For
named calls, requiring an additional supplied role can also make a branch more specific.

```mica,eval
make_identity(:instrument)
make_identity(:thermometer)
make_identity(:probe17)
assert Delegates(#thermometer, #instrument, 0)
assert Delegates(#probe17, #thermometer, 0)

verb describe(item)
  return "value"
end

verb describe(item @ #instrument)
  return "instrument"
end

verb describe(item @ #thermometer)
  return "thermometer"
end

require :describe(item: #probe17) == "thermometer"
require :describe(item: #instrument) == "instrument"
require :describe(item: 17) == "value"
```

The unrestricted `item` parameter still requires an `item` argument. It accepts any value in that
role. The instrument branch narrows the role to matching identities and values, and the thermometer
branch narrows it further through delegation. Method definition order does not establish priority.

Named calls supply roles by name, so argument order at the call site does not affect matching. Every
declared parameter must be supplied; additional call roles can be present. A branch receives its own
declared parameters in their declaration order. Positional calls instead require the same number of
arguments as the branch and associate them with parameter positions.

## Ambiguity Across Roles

More specific on one role does not automatically mean more specific overall. Consider these two
signatures:

```mica
verb handle(actor @ #reviewer, item)
  return :reviewer_action
end

verb handle(actor, item @ #change_request)
  return :change_action
end
```

For a reviewer handling a change request, both branches apply. The first narrows `actor`; the second
narrows `item`. Neither accepts a subset of the other's complete role combinations, so the call is
ambiguous. Define a branch for their intersection when that combination has a deliberate meaning:

```mica
verb handle(actor @ #reviewer, item @ #change_request)
  return :review_change
end
```

The intersection branch is more specific than both. If no branch applies, dispatch fails with
`NoApplicableMethod`. If several incomparable branches remain, it fails with `AmbiguousDispatch`.
Those failures stop the task segment; they do not run every matching branch or choose by method
identity. Prototype cycles can make restrictions equivalent, so they do not establish a strict
preference between otherwise matching branches either.

## Restrictions and Live Context

A role restriction matches the supplied value itself or a reachable prototype. Primitive values also
match their primitive prototype, and a frob matches through its delegate as well as `#frob`. All
reachable prototypes participate. Delegation rank records an ordering of prototype facts;
applicability follows reachability across those facts.

Dispatch reads the task's world view. An assertion or retraction affecting `Delegates` can therefore
change a later call in the same task. After commit, later tasks use the resulting world definition.
The runtime scopes cached dispatch answers to that view and invalidates affected answers when the
task changes dispatch facts.

Supplying a value in a role named `actor` or `agent` does not change the task's actor, principal, or
authority. Roles select behaviour and pass arguments. Runtime authority still comes from the task
context, and the selected method must be invokable under that authority. See
[Authority and Capabilities](./authority.md).
