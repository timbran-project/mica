# mica-driver

`mica-driver` is the host-facing boundary for embedding Mica in a Compio application. It keeps
sockets, windows, renderers, and platform resources in the host while owning Mica task scheduling,
relation execution, suspended-work wakeups, and lifecycle events.

See [`examples/compio_host.rs`](examples/compio_host.rs) for a complete minimal host. It translates
a native action into a named-role invocation, handles a Mica effect as a redraw request, services a
Mica external request, and shuts down cleanly.

## Dependency

Mica's internal crates are currently consumed together from the Git workspace rather than published
independently. Pin an exact revision so all internal path dependencies resolve from the same
checkout:

```toml
[dependencies]
mica-driver = { git = "https://github.com/timbran-project/mica.git", rev = "<commit>", default-features = false }
```

Add only the features the host needs. Branch dependencies are unsuitable for persisted worlds
because a branch can change the storage and program-artifact formats without changing the host's
manifest.

## Ownership model

A host constructs one process-long `DriverOwner` with an explicit `DriverResources` policy. The
owner is not cloneable and has sole authority to take the event pump and perform final shutdown. It
owns:

- a Compio dispatcher and its configured worker threads for synchronous runtime work;
- admission budgets for dispatched work, parallel relation execution, and external requests;
- Mica tasks and their suspended timer, mailbox, spawn, and external-request workers;
- endpoint registrations, subscription mailboxes, and the bounded event queue; and
- the selected in-memory or Fjall relation store.

A cloneable `DriverClient` provides process services such as endpoint creation and name lookup, but
cannot consume events or shut down the process. Driver asynchronous methods and internal workers run
on the embedding application's Compio executor. The host therefore keeps its Compio runtime alive
until `DriverOwner::shutdown` completes.

`DriverClient::allocate_ephemeral_identity` is the single allocator for endpoint, request, and other
process-local identities in Mica's reserved host range. Identities in that range must be allocated
by the same driver before they are used to open an endpoint. The allocation count is bounded by
`DriverResources::ephemeral_identity_capacity` and exposed in the resource snapshot; protocol-owned
identities outside the reserved range remain valid.

An `EndpointSession` is the lifetime boundary for a session or native client. The driver owns its
endpoint context, invocations, input, subscriptions, cancellation, and named volatile fact scopes.
Closing it rejects further submissions, waits for admitted endpoint operations to reach a stable
outcome, cancels its suspended tasks and external work, removes its subscriptions, and retracts its
scoped volatile relations. `replace_volatile_scope` performs a validated atomic relation
transaction, allowing a host to reconcile one complete endpoint-owned fact set without exposing an
intermediate state.

An `InvocationHandle` carries the task ID, selector, endpoint context, cancellation, initial report,
and independently awaitable terminal outcome. A host either awaits it, registers it with
`DriverEventRouter::watch_invocation`, or explicitly calls `detach` to transfer it to the background
event stream. There is no second implicit terminal-event path competing with the handle. The unique,
non-cloneable `DriverEventPump` preserves ordered effects and subscription readiness.
`drive_invocation`, endpoint close, and owner shutdown drive that same pump while awaiting their
result, so a saturated bounded event queue cannot deadlock control flow. A multi-host process can
run the pump through `DriverEventRouter`; each registered handler has an ordered bounded queue and a
tracked Compio worker. An asynchronous handler can therefore call back into Mica without blocking
the sole event pump. If a handler falls behind its explicit queue bound, the router disconnects it
and logs the saturation instead of deadlocking every other host.

`DriverOwner::shutdown` rejects new work, cancels tracked asynchronous work and suspended tasks,
closes endpoints and subscriptions, flushes persistence, and joins dispatcher threads. Unexpected
owner drop starts a best-effort event-discarding shutdown so queue saturation cannot strand worker
threads, but normal hosts should always await explicit pump-driven shutdown.

## Host boundary

Native gestures, process notifications, and file-watch events enter Mica through source, named-role
invocation, or endpoint input. Committed `DriverEvent::Effect` values notify the host of observable
changes. External requests ask the host to perform native work and resume the Mica task with a
value.

Native buffers, GPU objects, file descriptors, and subprocess handles remain host-owned. Store
durable editor state and policy as relations; do not put live native resources or capability values
in durable Mica data. `AuthorityContext` values are rebuilt from relation policy at task and session
boundaries.

External handlers receive an `ExternalRequestContext` containing the task, principal, actor,
endpoint, and cancellation signal. `DriverResources::external_request_capacity` bounds concurrent
handlers with FIFO admission. A request timeout covers both admission and execution. Task
cancellation, endpoint closure, timeout, and driver shutdown signal the context and drop the handler
future. A handler that starts its own operating-system work must use
`ExternalRequestCancellation::cancelled` to stop that child work. Stream emitters reject values
after cancellation.

The event pump is the sole consumer of the bounded driver event queue. Effects committed by a task
precede its completion, and subscription readiness caused by that outcome follows it. Terminal
events for watched or detached invocations and effects apply backpressure and are retained; repeated
equivalent suspension and subscription-ready events may be coalesced. Endpoints, active tasks,
suspended tasks, timers, ephemeral identities, retained terminal tombstones, queued events,
asynchronous workers, subscription mailboxes, subscriptions, external requests, and subscription
deliveries all have explicit host resource limits.

`DriverEventPump::set_wake_handler` supplies the readiness edge needed by Winit event-loop proxies.
`wait` and `drive_until` are ordinary Compio futures and can be selected with socket, terminal, or
application futures without polling sleeps.

## Features

The default feature set is `cranelift`, `fjall`, `source-provider`, and `wgpu`.

| Feature           | Adds                                                                                                                                                              |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cranelift`       | Cranelift compilation through `mica-vm-cranelift`.                                                                                                                |
| `fjall`           | Fjall-backed durable relation storage and `DriverStorage::Fjall`.                                                                                                 |
| `source-provider` | Git/Jujutsu and tree-sitter source browsing providers. This provider also brings its own Tokio-based dependencies; it does not replace the host's Compio runtime. |
| `wgpu`            | WGPU 30 relation acceleration and `RelationAcceleration::Automatic`.                                                                                              |

With `default-features = false`, the in-memory CPU implementation remains complete. A host must
still select `RelationAcceleration::Disabled`; `DriverResources::new` does this by default. With
`wgpu` enabled, `HostProvided` accepts a relation accelerator constructed from host-owned WGPU
device and queue values, allowing a renderer and Mica to share one WGPU device.

## Persisted data and compiled programs

Mica does not yet promise forward or backward compatibility for stores or compiled program
artifacts. Fjall stores carry an exact format and shape marker. Opening a mismatched store fails
with a migration-required error; it is never silently interpreted or rewritten. Program bytes carry
the current program-artifact magic and stale artifact versions are rejected.

Before upgrading a persisted deployment, back up the store and retain the exact Mica revision that
created it. Use that revision to file out named programmable units before an explicit migration,
then check and file them into a fresh store with the target revision. Fileout is not a complete
backup of arbitrary operational facts, so applications with additional durable state need a
version-specific migration or export path. Do not copy `ProgramBytes` facts between format versions;
recompile them from source through filein.
