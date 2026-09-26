# Retained query indexes for derived relations

Rust `665dfb7` and Odin `bfb368c` pass all 18 processes under the same rule-measurement protocol.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-rule-derived-indexes \
  --rust-revision 665dfb7 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case relation_rule_initial --case relation_rule_small_update --timeout 120
```

| Workload | Rust interpreter before | Rust interpreter after | Odin after |
| --- | ---: | ---: | ---: |
| Initial load and derivation | 28.25 ms | 28.03 ms | 2.04 ms |
| 16 small updates | 118.83 ms | 93.76 ms | 14.65 ms |

Maintained state retains the derived relation tables and their query indexes.
Updates insert or remove a tuple only when its derived support crosses zero.
Stored facts remain separate, and retained snapshots keep their prior tables.
Metadata changes rebuild the affected table with its current indexes.

The update workload improves by 21 percent, or 8.1 times from the original 761.9 ms baseline.
Rust remains approximately 6.4 times slower than Odin for updates. Initial derivation remains approximately 14 times slower.
Update RSS stays near 27 MiB, and initial RSS stays near 25.5 MiB.

Workspace tests and clippy pass. Regressions cover support changes, secondary indexes, stored/derived overlap, unrelated commits, and retained snapshots.
Raw samples, process ranges, RSS, source hashes, and compiler identities are in `manifest.json`.
