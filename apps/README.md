# Mica Apps

`apps/` holds runnable application fileins plus shared support fileins. These examples demonstrate
relation-first application design: durable facts, derived relations, delegated behaviour, authority
policy, task effects, and host integration.

## Directories

- `shared/`: reusable libraries and host policy used by more than one app.
- `examples/`: compact, tested guide examples for equipment tracking, approval workflows, and
  recursive dependency planning.
- `mud/`: a compact multi-user room/object world. It is the broadest example in this directory:
  world state, command parsing, event delivery, HTTP documents, and browser UI authored mostly in
  Mica with live WebTransport DOM sync.
- `agent/`: a relation-first LLM coding agent shell. It reuses the MUD's sync contract and
  tool-window patterns for a transcript, workspace, inspector, and command bar, and is intended to
  drive the source-provider computed relations.
- `chat/`: a smaller WebTransport DOM sync chat example.
- `web/`: HTTP host handlers and relational route fileins.

## Guide Examples

The examples under `examples/` are self-contained fileins designed for readers learning Mica without
an application-specific background. The guide provides persistent-store walkthroughs for each one:

- [shared equipment service](../mdbook/src/examples/equipment-service.md);
- [approval workflow](../mdbook/src/examples/approval-workflow.md);
- [recursive dependency planner](../mdbook/src/examples/dependency-planner.md).

Their focused runtime tests load the checked-in source directly:

```sh
cargo test -p mica-runtime guide_example_stays_executable
```

## MUD Web App

The MUD web app is the best place to see the current application model working end to end. Rooms,
objects, sessions, narrative events, available actions, and authority policy are Mica relations. UI
verbs query those relations and return DOM node values. The WebTransport host diffs each rendered
tree and sends patches to the browser; browser interactions return as sync events handled by Mica
verbs.

Run the DOM-synced MUD web app:

```sh
scripts/mud.sh
```

Open the printed `/mud` URL in a browser. The wrapper enables local password auth with seeded Alice
and Bob users; after sign-in, DOM sync renders the server-owned room view and command input.

The MUD app is described in more detail in [`mud/README.md`](./mud/README.md).

## Agent Web App

The agent web app is a coding agent shell that reuses the MUD's sync and tool-window patterns for a
transcript, workspace, inspector, and command bar. Run it with:

```sh
scripts/agent.sh
```

Open the printed `/agent` URL in a browser. The shell is described in more detail in
[`agent/README.md`](./agent/README.md).

## MUD Telnet

This path exercises command parsing and routed effects without the browser sync stack:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/shared/string.mica \
  --filein apps/shared/events.mica \
  --filein apps/mud/core.mica \
  --filein apps/mud/event-substitutions.mica \
  --filein apps/mud/command-parser.mica \
  --telnet-bind 127.0.0.1:7777
```

Commands such as `look`, `get coin`, `put coin box`, `north`, and `say hello` exercise telnet
endpoint input and world updates.

## Web Routing

`web/http-core.mica` is the minimal default HTTP filein. `web/relational-router.mica` shows the same
web-host request facts routed through relations and stratified negation. It keeps route matching,
access policy, forbidden responses, and not-found fallback in Mica source.

Run the router demo with an explicit filein list so it replaces the default HTTP filein:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/shared/string.mica \
  --filein apps/shared/events.mica \
  --filein apps/mud/core.mica \
  --filein apps/mud/event-substitutions.mica \
  --filein apps/mud/command-parser.mica \
  --filein apps/web/relational-router.mica \
  --web-bind 127.0.0.1:8080
curl -i http://127.0.0.1:8080/hello
curl -i http://127.0.0.1:8080/admin
curl -i http://127.0.0.1:8080/missing
```

## Capabilities

`shared/capabilities.mica` shows the intended bootstrap shape for capabilities. It describes durable
policy through roles and surfaces; derived `CanRead`, `CanWrite`, `CanInvoke`, and `CanEffect`
relations are the effective policy consumed by the runner. Those facts are not live capability
values. When source is run with `--actor`, the runner resolves the actor identity, reads the
effective policy, and mints ephemeral task capabilities for that run.

Try the capability example with a persistent store:

```sh
cargo run --bin mica -- --storage fjall --store demo-db filein --unit caps --replace apps/shared/capabilities.mica
cargo run --bin mica -- --storage fjall --store demo-db --actor alice eval ':polish(actor: #alice, item: #lamp)'
cargo run --bin mica -- --storage fjall --store demo-db --actor bob eval 'return #lamp.name'
cargo run --bin mica -- --storage fjall --store demo-db --actor bob eval '#lamp.name = "stolen"'
```

Alice has the `#builder` role, so her invocation succeeds and emits an effect. Bob has the
`#visitor` role, so he can read the lamp name, but the write attempt is denied.
