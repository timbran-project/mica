# Identity-value index and larger OWL loading

Before: Rust `f09d390bfa2e16c1f4b9712de11128200bc4d952` with retained transaction scan indexes.
After: Rust `dfebf6d74ddea290aa26f311c8bc8903c0870095` with an additional `NamedIdentity` index on `[1, 0]` in fresh catalogues.
Both captures use Odin `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.

Each capture has three fresh processes per case and implementation on CPU 5, with one worker and no accelerator.
All 24 processes pass. No build or test ran during timed commands.
The OWL input contains 10,001 subjects and 20,001 facts.
Stores are fresh, initialized before timing, and use strict durability. No inference rules are installed.
Whole-command time includes opening, parsing, loading, commit, and shutdown. Persisted-result verification occurs after timing.

| OWL command | Median | Peak RSS |
| --- | ---: | ---: |
| Rust, retained local indexes | 596.762 ms | 64,300–64,368 KiB |
| Rust, identity-value index | 231.751 ms | 66,476–66,604 KiB |
| Odin, first capture | 61.947 ms | 49,128–49,572 KiB |
| Odin, second capture | 61.753 ms | 48,808–49,452 KiB |

The Rust median falls by approximately 2.6 times, with approximately 2.2 MiB more peak RSS.
Identity allocation keeps its collision check. The new index avoids scanning every pending name for each generated identity.
A profile identified that scan after the earlier retained-index improvement.
The remaining Rust timing gap is approximately 3.8 times Odin's result at this size.
Rust uses a Mica loader and Fjall. Odin uses a native host loader and its own store.
This result does not establish parity for rule derivation, resumed large stores, or concurrent ingestion.

Existing persisted catalogues retain their stored index definitions. This comparison applies to fresh catalogues.
The full workspace passed after the transaction-index change.
After the catalogue-index addition, all 325 runtime library tests, ingestion application tests, runner tests, and workspace clippy passed.

The 100,000-assertion CycL control also passes.
Rust medians are 12.338 ms and 15.668 ms, with overlapping process ranges of 12.336–16.811 ms and 13.183–17.014 ms.
Odin medians are 92.148 ms and 91.741 ms. The catalogue change does not affect Rust's census path.
The [documented schema adaptation](../2026-09-26-ingestion/README.md) still applies to Odin's census.

Reproduce each capture in a fresh output directory:

```sh
python3 benchmarks/parity/capture_ingestion.py /tmp/mica-ingestion-10k-before \
  --rust-revision f09d390 --odin-revision bfb368c --cpu 5 --runs 3 \
  --subjects 10000 --assertions 100000
python3 benchmarks/parity/capture_ingestion.py /tmp/mica-ingestion-10k-after \
  --rust-revision dfebf6d --odin-revision bfb368c --cpu 5 --runs 3 \
  --subjects 10000 --assertions 100000
```

Raw logs, inputs, stores, binaries, and source exports remain in those capture directories on the host.
