# Authority

When Mica runs code, the first practical question is: who is this task running for, and what is it
allowed to do?

A user at a console, an HTTP request, a background agent, and a tool bridge may all submit tasks.
Those tasks can read facts, change facts, invoke verbs, send messages, and ask hosts to perform
outside work. Mica cannot treat "the code mentions `#alice`" as permission to act as Alice, and it
cannot afford to run a full policy query for every tuple scanned by a hot relation read.

So Mica keeps two questions separate:

- What does the world say this actor should be allowed to do?
- What permissions did this particular running task actually receive?

The first question is answered by durable facts and rules. The second is answered by the authority
context attached to the task. A verb may receive `#alice` as an argument, store `#alice` in a
relation, or emit text mentioning Alice. None of that means the task is running with Alice's
permissions. The task has only the authority context it was given when it started or resumed.

Durable relations describe policy:

```mica
assert GrantInvoke(#alice, #polish_verb)
assert GrantEffect(#alice)
```

Those low-level grant facts are useful for bootstrap, but most worlds should not stop at one grant
fact per operation. Coarser policy relations are usually easier to author and review:

```mica
assert HasRole(#alice, #builder)
assert RelationInSurface(:inspection, :Name)
assert RoleCanRead(#builder, :inspection)
```

Rules can then derive effective authority:

```mica
CanRead(actor, relation) :-
  HasRole(actor, role),
  RoleCanRead(role, surface),
  RelationInSurface(surface, relation)
```

Runtime tasks receive an `AuthorityContext` derived from current policy. Checks for read, write,
invoke, grant, and effect authority use that context rather than querying policy relations on every
operation.

The shape is:

```text
durable Grant* policy facts
  -> derived Can* facts
  -> AuthorityContext for the task/session
  -> cheap read/write/invoke/grant/effect checks
```

This keeps checks cheap in hot paths. A relation read should not need to run a fresh policy query
every time it scans a tuple. Instead, the runtime builds an authority context at a task or session
boundary and uses that context for the task's operations.

Read and write grants apply to a whole named relation. They do not filter rows according to the
task's actor, principal, endpoint, or any specially named tuple position. This rule is the same for
durable and volatile relations: an authorized unbound query can observe every fact currently in the
relation. Applications that need ownership or isolation must store an explicit owner identity and
bind it in their queries, or expose a derived relation whose rules enforce the intended policy.

Capability values are ephemeral runtime tokens. They can authorize specific operations within the
running task or session, but they are not durable policy facts and are not persistable values.

For example, mailbox send and receive caps are values that can be handed to tasks. Possessing the
send cap authorizes sending to that mailbox. Possessing the receive cap authorizes receiving from
it. The durable world does not store those live tokens as policy.

For an agent host, external credentials should enter the runtime as session or task authority.
Durable policy can say that `#planner` may use `:search`, but a live tool credential is not itself a
durable fact to be filed out.

Policy changes take effect at task or session boundaries when authority is rebuilt from the current
snapshot. Suspended tasks resume with explicitly supplied fresh authority.

This boundary is important in a system that runs untrusted author code. A verb should not be able to
keep a stale broad authority context forever after policy has changed. Conversely, a hot relation
scan should not pay for a complete policy derivation on every tuple. Rebuilding authority at clear
boundaries keeps both properties visible.

Authority is also separate from identity. `#alice` is a durable identity. A task running "as Alice"
receives authority derived for Alice at that boundary. Code cannot gain authority merely by
mentioning `#alice` in a role map or fact.

## Administrative Source and Ordinary Tasks

Creating identities, declaring relations, and changing the catalogue are administrative operations.
The declaration builtins check grant authority. Method, rule, and type-alias installation runs
through the runner's administrative source path; source submitted with an actor or principal runs
as task code instead.

A host that submits an anonymous `TaskRequest` must still supply the intended `AuthorityContext`.
Omitting actor and principal identities does not confer administrative permission. A request with
empty authority can calculate `1 + 2`, but it cannot create an identity, declare a relation, or
install a method. Requests without that authority are rejected before declaration names are reserved
or catalogue changes are published.

Keep bootstrap and filein entry points with the part of the host that administers the world. For
user input, construct a request for the authenticated endpoint or actor so the runtime can derive
its policy. The choice of entry point controls installation; the authority context controls the
operations performed by the executing task.
