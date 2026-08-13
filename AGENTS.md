# AGENTS.md

This is quick-start context for AI coding agents working in this repository. Human contribution
rules live in [CONTRIBUTING.md](./CONTRIBUTING.md), and Rust style rules live in
[CODING-STYLE.md](./CODING-STYLE.md). Follow both.

## Project Shape

Mica is a live, programmable system for domains whose data and behaviour evolve over time:
collaborative worlds, simulations, knowledge bases, authoring systems, agent workspaces, games, and
operational models.

The core idea is a relation-first object system:

- objects are durable identity values described by facts, not records;
- behaviour is installed into the live world as verbs/methods;
- dispatch matches named roles through prototype delegation;
- rules define derived relations, including recursive relationships;
- tasks run transactionally over a live relation store;
- authority is derived from relation policy into ephemeral runtime capabilities.

Mica has no backwards-compatibility constraint with MOO, mooR, Self, etc. Those are influences, not
compatibility targets.

Do not add legacy adapters, migration shims, parallel old/new APIs, or compatibility branches unless
explicitly asked. This is a new product; prefer one coherent current API.

## Repository Structure

This is a Rust Cargo workspace:

```text
crates/
├── var/              # one-word values, heap values, identities, symbols, caps
├── relation-kernel/  # MVCC relation store, rules, dispatch support, providers
├── vm/               # bytecode format and register VM execution core
├── compiler/         # parser, HIR, lowering, bytecode compilation
├── runtime/          # live environment, tasks, builtins, filein/fileout
├── driver/           # compio task driver, wakeups, input, emissions
├── runner/           # CLI and REPL binary
└── daemon/           # minimal TCP endpoint transport
```

Supporting material:

- [apps/](./apps/) contains runnable application and shared Mica fileins.
- [sketches/](./sketches/) contains design notes; treat them as informative, not automatically
  authoritative over current code.
- [CODING-STYLE.md](./CODING-STYLE.md) is binding for code style.

## Engineering Policy

- Keep changes narrow, explicit, and grounded in the current architecture.
- **Prefer cohesive APIs over compatibility layers**. If an API shape is wrong, change it cleanly
  rather than preserving stale call paths. **- If working on apps/ type code and you identify a
  mishaped API, missing feature, missing language syntax or a bug in the underlying Mica runtime it
  is imperative that you stop development and alert the user with your observations and
  recommendations.**
- Do not design for hypothetical downstream customers unless the maintainer asks for that
  constraint. This repo is still taking shape.
- Names must describe what code does now, not its history or implementation accident.
- Avoid "legacy", "new", "old", "v2", "compat", and similar temporal naming.
- Avoid generic abstractions until they remove real duplication or clarify a stable boundary.
- Generated text, comments, commit messages, and docs must be factual and specific. Avoid marketing
  prose and vague AI filler.
- Use Canadian English in documentation and comments:
  - use `-ize` rather than `-ise`, e.g. optimize, organize, recognize;
  - keep `-our` in nouns, e.g. colour, neighbour, flavour, honour;
  - prefer `-re` endings for nouns, e.g. centre, theatre, kilometre;
  - double `l` when adding suffixes to verbs ending in `l`, e.g. travelling, cancellation,
    enrolment;
  - use `-ce` for nouns and `-se` for verbs, e.g. licence/license.

## Performance Orientation

Performance is a design constraint, not a later cleanup task.

- Keep hot-path runtime, value, query, dispatch, and storage code allocation conscious.
- Prefer cache-friendly data layouts and compact representations.
- Do not add per-operation relation lookups where a snapshot-derived cache or compiled
  authority/context can do the work once.
- Measure optimizations when making performance claims.
- Be conservative about dependencies, especially in `mica-var`, `mica-relation-kernel`, `mica-vm`,
  and `mica-runtime`.
- Keep dependency versions centralised in the root workspace `Cargo.toml`; member crates should use
  `workspace = true`.

## Security And Authority

Capabilities are ephemeral runtime values. Durable state stores policy facts, not live authority
tokens.

The intended shape is:

```text
durable policy relations -> effective Can* relations -> AuthorityContext
```

Runtime checks should be cheap checks against `AuthorityContext`, not fresh policy queries on every
read, write, invoke, or effect. Policy changes should take effect at task/session boundaries by
rebuilding authority from the current snapshot. Suspended tasks should resume with explicitly
supplied fresh authority, not stale checkpointed authority.

## Rust Style Reminders

- Rust 2024 edition.
- Format with `cargo fmt --all`.
- Keep `use` statements at the top of the file/module.
- Prefer early returns, `let else`, match guards, and match let-chains to deep nesting.
- Add focused tests for new functionality and regressions.
- Prefer real logic in tests over mocks.
- Keep test output clean.
- Use custom error types and precise messages where appropriate.

## Common Commands

```sh
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --bin mica
cargo run --bin mica -- filein apps/shared/capabilities.mica
```

`cargo nextest run --workspace` is also supported. Its repository configuration serializes tests
from crates that construct Compio drivers.

For example fileins that need persistence:

```sh
cargo run --bin mica -- --storage fjall --store demo-db filein --unit caps --replace apps/shared/capabilities.mica
cargo run --bin mica -- --storage fjall --store demo-db --actor alice eval ':polish(actor: #alice, item: #lamp)'
```

## Review Checklist

Before handing work back:

1. Does the change fit the current relation/kernel/runtime/compiler/runner boundaries?
2. Did you avoid compatibility scaffolding and stale API paths?
3. Are performance-sensitive paths free of unnecessary allocation and repeated lookups?
4. Are new behaviours tested at the right layer?
5. Did `cargo fmt --all`, relevant tests, and clippy run, or is any gap stated?
6. Are docs and comments concise, current, and written in Canadian English?
