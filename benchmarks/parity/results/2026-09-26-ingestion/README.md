# Ingestion command comparison

Rust: `64e4bd4f582e9000d4f0d64820ebbe302ba915b8`.
Odin: `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The capture uses CPU 5, one worker, no accelerator, zero warmup, and three fresh processes per implementation and case.
All 12 corrected runs pass. No build or test ran concurrently with the timed commands.

| Whole command | Rust median | Odin median | Rust peak RSS | Odin peak RSS |
| --- | ---: | ---: | ---: | ---: |
| OWL: 1,001 subjects, 2,001 facts | 162.648 ms | 21.435 ms | 21,152–21,164 KiB | 11,120–11,184 KiB |
| CycL: 100,000 assertions | 12.065 ms | 94.735 ms | 4,804–4,876 KiB | 115,460 KiB |

OWL stores are fresh and initialized before timing. Both loaders use strict durability and one input batch, without inference rules.
The timer includes process launch, input processing, store opening, loading, commit, and shutdown.
Persisted identity counts, labels, edges, and a Unicode value are checked after timing through administrative authority.
Rust executes the Mica loader with native execution enabled. Odin executes a native host loader.
Rust uses Fjall and commits facts with progress. Odin uses its own store and a separate progress commit.
The Rust command is approximately 7.6 times slower on this workload. This result does not establish large-dataset scaling.

The CycL commands count assertions without writing facts.
Odin initializes an in-memory schema, while Rust runs only its parser. The result is a whole-command comparison, not an isolated parser comparison.
The pinned Odin schema conflicts with its system `Arity/2` relation, so the corrected capture supplies Rust's namespaced schema.
That adaptation and its input hash appear in the manifest.

The initial capture is retained in `initial-failed-manifest.json`.
Rust OWL verification lacked read grants because it used an ordinary endpoint. The corrected capture uses administrative filein.
The initial Odin CycL command failed during schema initialization. Neither failed result contributes a summary timing.

Reproduce:

```sh
python3 benchmarks/parity/capture_ingestion.py /tmp/mica-ingestion-verified \
  --rust-revision 64e4bd4 --odin-revision bfb368c --cpu 5 --runs 3 \
  --subjects 1000 --assertions 100000
```

Raw logs, generated inputs, source exports, and binaries remain in `/tmp/mica-ingestion-verified` on the capture host.
