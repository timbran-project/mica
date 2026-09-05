# mica-relation-kernel

`mica-relation-kernel` is the live relational storage layer for Mica. It owns relations, facts,
snapshots, transactions, derived rules, dispatch matching, and the first cut of query execution.

This crate is deliberately not a SQL engine. Its job is to support Mica's object-as-identity model:
n-ary set relations, transaction-local `assert`/`retract`, read-your-own-writes, relation metadata,
rules, and inspection views.

## What's Here

- `src/kernel.rs`: `RelationKernel`, the snapshot-published entry point.
- `src/transaction.rs`: transaction overlays, assertions, retractions, conflict checks, and commit.
- `src/snapshot.rs`: immutable snapshot state, commit records, and fact changes.
- `src/metadata.rs`: relation schemas, conflict policies, and index specs.
- `src/index.rs`: in-memory relation indexes.
- `src/tuple.rs` and `src/fact.rs`: tuple and fact representations.
- `src/catalog.rs`: relational representation of relation metadata.
- `src/computed.rs`: read-only computed relation hooks for relation-shaped rows that are produced on
  scan instead of stored as facts.
- `src/rules.rs`: Datalog-style rule definitions and evaluation.
- `src/materialized.rs`: materialisation of derived rule results.
- `src/query.rs`: simple query plans and join execution.
- `src/dispatch.rs`: role-based method applicability matching.
- `src/closure.rs`: delegation closure helpers.
- `src/neighborhood.rs`: object-neighbourhood inspection views.
- `src/workspace.rs`: shared mutation interface for transactional and projected relation workspaces.
- `src/projected.rs`: non-durable projected relation store for single-user or browser-hoisted Mica
  surfaces.
- `src/provider/`: commit provider boundary. `memory.rs` contains the in-memory provider,
  `fjall/mod.rs` contains the Fjall-backed durable state provider, and `fjall/codec.rs` contains the
  persisted binary encoding.
- `src/tests.rs`: integration-style kernel tests.

## Role In Mica

The kernel is the authoritative world state for the current prototype. The runtime opens
transactions against it, the compiler installs methods and rules into it, and the runner refreshes
its compile context from its catalogue facts.

The crate also has a smaller relation-core build shape. The workspace dependency disables durable
providers by default, so compiler, VM, and browser packages can use values, tuples, indexes, query
plans, rules, dispatch, and the `ProjectedStore` without linking Fjall. Server/runtime crates opt
into the `fjall-provider` feature explicitly.

`ProjectedStore` is for relation slices that are already selected by a server or host environment.
It applies commit-shaped deltas, supports immediate local workspace mutation through
`RelationWorkspace`, and evaluates rules over the projected facts. It does not provide multiuser
conflict validation, durable commit acknowledgement, or restart recovery; those remain
`RelationKernel` responsibilities.

Computed relations are read-only relation surfaces. They are scanned through the same `RelationRead`
path as stored relations and may be visible to rule evaluation, but their rows are not durable
truth. Callers that use computed search rows must validate the candidate against ordinary relation
state for existence, freshness, and authority before recording or exposing derived context.

Persistence stores canonical relation state: relation metadata, rule definitions, current
extensional facts, and the latest committed version. Each successful mutation is still represented
as a semantic `Commit`, but the Fjall provider applies that commit to the canonical keyspaces in the
same batch that records the commit entry.

For users and operators, this means [Fjall](https://github.com/fjall-rs/fjall), a log-structured
embedded key-value storage engine, is the durability and restart boundary while the current query
path still runs from memory. Startup loads the current relation state with
`RelationKernel::load_from_state`; it is not required to replay the whole historical commit stream.
The retained commit entries are an implementation aid for inspection, testing, and future recovery
work, not the only durable representation of the world.

`FjallStateProvider::open` defaults to relaxed durability: a commit returns after it has been
accepted into the provider's ordered writer queue, and normal provider shutdown drains that queue.
`FjallStateProvider::open_strict` waits for the background writer to apply the Fjall batch and sync
its journal before returning from the commit path. `RelationKernel::flush_persistence` waits for
queued writes and syncs the journal in either mode. Strict mode adds a journal sync to each durable
commit; relaxed mode lets the host choose explicit flush boundaries.

For developers, this means the persisted representation is the state encoding in
`src/provider/fjall/codec.rs`, plus the commit encoding kept beside it. Changes to the state shape,
value encoding, or catalogue representation must update the format version or shape marker and
provide a migration path. `FjallStateProvider` records those markers so incompatible stores are
detected before opening.

## Licence

Mica is licensed under the GNU Affero General Public License v3.0. See the repository root
`LICENSE`.
