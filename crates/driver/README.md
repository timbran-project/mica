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

A host should normally construct one process-long `CompioTaskDriver` with an explicit
`DriverResources` policy. The driver owns:

- a Compio dispatcher and its configured worker threads for synchronous runtime work;
- admission budgets for dispatched work, parallel relation execution, and external requests;
- Mica tasks and their suspended timer, mailbox, spawn, and external-request workers;
- endpoint registrations, subscription mailboxes, and the bounded event queue; and
- the selected in-memory or Fjall relation store.

Driver clones share this state; they do not create more worker pools. Driver asynchronous methods
and internal workers run on the embedding application's Compio executor. The host therefore keeps
its Compio runtime alive until `CompioTaskDriver::shutdown` completes.

An endpoint is the lifetime boundary for a session or native client. Allocate its identity with
`CompioTaskDriver::allocate_ephemeral_identity`, open it before accepting input, and close it when
the native owner disappears. Closing an endpoint rejects further submissions, cancels its suspended
tasks and external work, removes its subscriptions, and retracts its volatile endpoint relations.

`shutdown` is shared and idempotent. It rejects new work, cancels tracked asynchronous work and
suspended tasks, closes endpoints and subscriptions, flushes persistence, and joins dispatcher
threads. The host must continue draining driver events while shutdown is in progress if producers
have already filled the bounded event queue.

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

The driver event queue has one logical consumer. `wait_events` and `drain_events` divide the same
stream if called concurrently. Effects committed by a task precede its lifecycle event, and
subscription readiness caused by that outcome follows it. Terminal task events and effects apply
backpressure and are retained; repeated equivalent suspension and subscription-ready events may be
coalesced. A submission also returns its immediate outcome for request/response control flow, while
the event stream remains the host's source for asynchronous lifecycle changes.

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
the `MICAPRG4` magic and stale artifact versions are rejected.

Before upgrading a persisted deployment, back up the store and retain the exact Mica revision that
created it. Use that revision to file out named programmable units before an explicit migration,
then check and file them into a fresh store with the target revision. Fileout is not a complete
backup of arbitrary operational facts, so applications with additional durable state need a
version-specific migration or export path. Do not copy `ProgramBytes` facts between format versions;
recompile them from source through filein.
