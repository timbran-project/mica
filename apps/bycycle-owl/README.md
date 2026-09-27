# Bycycle OWL ingestion

The ontology and predicate mapping come from Odin Mica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The Rust command streams XML and gzip input. A Mica verb commits each batch through the ordinary runtime transaction.

Count subjects and predicates without opening a store:

```sh
cargo run --release --bin mica -- owl --owl opencyc.owl.gz --census
```

Initialize a store and load the first 1,000 subjects:

```sh
cargo run --release --bin mica -- --store bycycle-db --durability strict \
  owl --owl opencyc.owl.gz --init --limit 1000 --retrieval-actor reader
```

Resume the same import:

```sh
cargo run --release --bin mica -- --store bycycle-db --durability strict \
  owl --owl opencyc.owl.gz --retrieval-actor reader
```

`--init` installs the bundled schema, retrieval relations, rules, and loader verbs.
Each invocation accepts a list of subjects. The loader does not parse source or commit separately for each fact.
`--commit-batch` defaults to 20,000 input facts, including one count per subject. Batches end at subject boundaries.
The command also flushes after 8 MiB of decoded subject content. One subject can exceed the fact threshold.
A subject cannot exceed 8 MiB, 65,536 facts, or 256 nesting levels.

`LoaderState(:resume, progress)` stores `[source_sha256, retrieval_actor_name, subject_count, complete]`.
Facts, identity mappings, grants, and progress commit together. A stale expected cursor aborts the batch.
Malformed XML discards the pending batch. Earlier committed batches remain available.
Resume requires the same input bytes and actor. Recompressing a gzip file changes its hash.
The command scans from the beginning to restore XML namespaces, skips committed subjects, and loads the remainder.
A completed import makes no further fact commits. A different input requires a separate store.
`--durability strict` uses the existing Fjall durable commit path. The default is `relaxed`.

GUIDs map to exact `bycycle/guid/<fragment>` symbols and `GuidOf(identity, fragment)` facts.
This mapping preserves punctuation, so distinct GUIDs do not collide after sanitization.
Labels, Cyc labels, and comments retain their first value. Later values become `Alias` facts.
Wiki names and URLs retain their first value and discard repeats.
GUID resource objects become identities. Other resources survive only as strings in `SameAs`.
XML entities decode normally. CDATA remains literal text.
`CanRetrieveSubject` grants access only to the configured actor.

The schema stores direct facts. Rules derive taxonomy, symmetric disjointness, contradictions, rewrite chains, and text units.
To load facts before installing rules, initialize the schema and loader explicitly:

```sh
cargo run --release --bin mica -- --store bycycle-db filein \
  apps/bycycle-owl/00_schema.mica apps/bycycle-owl/40_loader.mica
cargo run --release --bin mica -- --store bycycle-db owl --owl opencyc.owl.gz
cargo run --release --bin mica -- --store bycycle-db filein \
  apps/shared/retrieval.mica apps/bycycle-owl/10_taxonomy.mica \
  apps/bycycle-owl/20_constraints.mica apps/bycycle-owl/30_graph.mica
```

This sequence delays rule installation. It does not disable derivation in a world that already has rules.
Rust does not expose Odin's `--defer-derivation` or manual checkpoint flags here.

Tests cover the pinned fixture, Unicode, aliases, namespace prefixes, gzip, inference, retraction, aborted batches, and persistent resume.
The CLI test resumes a strict store across separate processes.
The [pinned ingestion measurements](../../benchmarks/parity/results/2026-09-26-02653be/ingestion-summary.json) include a strict-store load of 10,001 subjects without inference rules.
The [capture manifest](../../benchmarks/parity/results/2026-09-26-02653be/ingestion-manifest.json) records process latency, peak RSS, and loader differences.
The separate Odin CycL census tool counts assertions but does not load the full dump.
