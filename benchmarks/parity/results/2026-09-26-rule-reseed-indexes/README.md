# Indexed recursive rederivation

Rust `0e06a7f` and Odin `bfb368c` pass all 18 processes.
The protocol and workloads match the rule-maintenance baseline.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-rule-reseed-indexes \
  --rust-revision 0e06a7f --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case relation_rule_initial --case relation_rule_small_update --timeout 120
```

| Workload | Rust interpreter before | Rust interpreter after | Odin after |
| --- | ---: | ---: | ---: |
| Initial load and derivation | 27.73 ms | 27.16 ms | 1.98 ms |
| 16 small updates | 761.90 ms | 212.87 ms | 15.18 ms |

Recursive deletion now uses maintained join indexes when finding replacement derivations.
The deletion algorithm, support counts, guards, and negation semantics remain unchanged.
Kernel regressions compare retained snapshots and deletion results with full recomputation.
Workspace tests and clippy pass.

The update workload improves by approximately 3.6 times, but Rust remains approximately 14 times slower than Odin.
Initial derivation remains a separate gap. Native-enabled Rust produces similar results to the interpreter on these workloads.
Fixed-work Rust RSS remains approximately 24.5 MiB for initial derivation and 27 MiB for updates.
Raw samples, ranges, RSS, source hashes, build commands, and toolchains are in `manifest.json`.
