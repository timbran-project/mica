# Retained visible rows during recursive deletion

Rust `84be391` and Odin `bfb368c` pass all 18 processes under the same rule-measurement protocol.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-rule-retained-visible \
  --rust-revision 84be391 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case relation_rule_initial --case relation_rule_small_update --timeout 120
```

| Workload | Rust interpreter before | Rust interpreter after | Odin after |
| --- | ---: | ---: | ---: |
| Initial load and derivation | 28.87 ms | 28.25 ms | 2.03 ms |
| 16 small updates | 166.54 ms | 118.83 ms | 14.90 ms |

Recursive maintenance retains the current visible collection and removes overdeleted rows that lack stored support.
If no derived rows remain, it starts directly from stored facts. It no longer rebuilds the entire visible collection before each small update.
The update workload improves by approximately 29 percent, or 6.4 times from the original 761.9 ms baseline.
Rust remains approximately eight times slower than Odin for updates, and initial derivation remains a separate gap.
Update RSS falls from approximately 28.7 MiB to 27.5 MiB. Initial RSS remains approximately 25.4 MiB.

Kernel tests and clippy pass. A focused regression checks stored support asserted in the same commit as an overdeletion.
It also checks later removal, complete recomputation, and retained snapshots.
Raw samples, process ranges, RSS, source hashes, and compiler identities are in `manifest.json`.
