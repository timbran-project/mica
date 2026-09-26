# Persistent maintained tuple collections

Rust `15cd6ff` and Odin `bfb368c` pass all 18 processes.
The protocol matches the initial and indexed-rederivation captures.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-rule-persistent \
  --rust-revision 15cd6ff --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case relation_rule_initial --case relation_rule_small_update --timeout 120
```

| Workload | Rust interpreter before | Rust interpreter after | Odin after |
| --- | ---: | ---: | ---: |
| Initial load and derivation | 27.16 ms | 28.87 ms | 2.01 ms |
| 16 small updates | 212.87 ms | 166.54 ms | 14.44 ms |

Maintained collections now use the existing tuple store, including persistent radix storage above its 4,096-row threshold.
Recursive rounds share unchanged storage instead of cloning each tuple reference in a large ordered set.
Stored extensional rows also share their tuple storage when maintenance begins.

Update latency decreases by approximately 22 percent. Initial latency increases by approximately 6 percent.
Initial RSS increases from approximately 24.5 MiB to 25.3 MiB; update RSS increases from approximately 27 MiB to 28.7 MiB.
This is a tradeoff for repeated updates, not an improvement to initial derivation.
The total update improvement from the original 761.9 ms baseline is approximately 4.6 times.
Rust remains substantially slower than Odin on both workloads.

Tests cover ordering and retained branches through radix promotion, small and large recursive collections, and comparison with complete recomputation.
Workspace tests, final focused kernel tests, and clippy pass.
Raw samples, process ranges, RSS, hashes, and compiler identities are in `manifest.json`.
