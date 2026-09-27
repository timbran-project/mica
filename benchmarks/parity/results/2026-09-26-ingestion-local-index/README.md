# Retained transaction scan indexes

Rust: `f09d390bfa2e16c1f4b9712de11128200bc4d952`. Odin: `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
This capture repeats the [initial ingestion protocol](../2026-09-26-ingestion/README.md) on CPU 5 with three fresh processes per case and implementation.
All 12 processes pass. No build or test ran during timed commands.

| Whole command | Rust median | Odin median | Rust peak RSS | Odin peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Strict OWL load: 1,001 subjects, 2,001 facts | 33.796 ms | 15.448 ms | 21,408–21,472 KiB | 11,116–11,120 KiB |
| CycL census: 100,000 assertions | 12.160 ms | 93.329 ms | 4,876–4,996 KiB | 115,396–115,460 KiB |

Rust's OWL median falls from 162.648 ms to 33.796 ms, an approximately 4.8-times improvement.
Peak RSS increases by roughly 0.3 MiB. Retained indexes replace repeated reconstruction after every local write.
The remaining timing gap is approximately 2.2 times Odin's result in this capture.
The CycL result is essentially unchanged and remains a whole-command comparison with different schema-initialization costs.

A separate 10,001-subject profile identified repeated index construction, radix-key encoding, and allocation in the transaction overlay.
That profiling run is diagnostic evidence and does not contribute a timing sample here.
Regressions verify ordered lookup after writes, retractions, reassertions, Unicode keys, and compact-to-radix promotion.
Workspace tests and clippy pass.

Reproduce:

```sh
python3 benchmarks/parity/capture_ingestion.py /tmp/mica-ingestion-local-index \
  --rust-revision f09d390 --odin-revision bfb368c --cpu 5 --runs 3 \
  --subjects 1000 --assertions 100000
```

Raw logs, inputs, stores, binaries, and source exports remain in `/tmp/mica-ingestion-local-index` on the capture host.
