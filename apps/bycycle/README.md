# Bycycle CycL census and sample

This port covers the capabilities implemented at Odin Mica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The census counts dump assertions. The sample file loads seven microtheory-scoped facts.
The full dump router, non-atomic term encoding, microtheory visibility, and Cyc logical inference are not implemented.

Count a KB5022 dump:

```sh
cargo run --release --bin mica -- cycl-census kb5022.cycl
cargo run --release --bin mica -- cycl-census kb5022.cycl --limit 10000
```

Each non-empty line must contain one five-element assertion: `(Mt formula truth direction strength)`.
The formula must start with an atom predicate. The parser validates nested lists, strings, variables, and numbers without constructing an AST.
The JSON report contains assertion counts, malformed-line counts, predicate frequencies, and whole-command elapsed seconds.
No store opens and no assertions are written. Comments and multi-line forms count as malformed lines.
Trailing input, truncated strings, invalid UTF-8, and nesting beyond 256 levels are rejected.
A line larger than 8 MiB stops the census with an error.

Load the sample into a persistent store:

```sh
cargo run --bin mica -- --store cyc-db filein \
  apps/bycycle/00_schema.mica apps/bycycle/10_sample.mica
cargo run --bin mica -- --store cyc-db eval \
  'return Cyc/Isa(?person, #cyc/dentist, #cyc/people_data_mt)'
```

The schema uses `Cyc/` relation names. Identities in the sample use `cyc/` names.
These namespaces avoid collisions with the Rust catalogue and the OWL application's unscoped relations.
For example, `Cyc/Arity/3` describes a predicate within a microtheory. The runtime catalogue retains `Arity/2`.
The sample returns Alice and Bob for the dentist query. The same query in `#cyc/base_kb` returns no rows.

Tests load both application schemas together and preserve their distinct arities and facts.
The [pinned ingestion measurements](../../benchmarks/parity/results/2026-09-26-02653be/ingestion-summary.json) include a 100,000-assertion census.
The [capture manifest](../../benchmarks/parity/results/2026-09-26-02653be/ingestion-manifest.json) records process latency, peak RSS, and the Odin schema adaptation.
