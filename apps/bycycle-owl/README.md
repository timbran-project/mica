# Bycycle OWL ontology

These fileins describe OpenCyc facts and derive taxonomy, constraints, and retrieval relations.
They come from Odin Mica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.

Load the files into a persistent store in this order:

```sh
cargo run --bin mica -- --storage fjall --store bycycle-db filein \
  apps/bycycle-owl/00_schema.mica apps/shared/retrieval.mica \
  apps/bycycle-owl/10_taxonomy.mica apps/bycycle-owl/20_constraints.mica \
  apps/bycycle-owl/30_graph.mica
```

The schema stores direct facts. Rules derive transitive taxonomy, symmetric disjointness, contradictions, rewrite chains, and text units.
`CanRetrieveSubject` requires explicit actor grants.
The ontology test checks derived results and their removal after committed retractions in both execution modes.

The Rust OWL loader is not yet available. `LoaderState` reserves a relation for transactional import progress.
The separate Odin CycL census tool counts assertions but does not load the full dump.
