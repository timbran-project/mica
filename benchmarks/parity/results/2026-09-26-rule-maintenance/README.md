# Initial derivation and small-update baseline

Rust `7346fcb` and Odin `bfb368c` pass all 18 processes.
Both use one worker, CPU 5, root authority, memory storage, and serial CPU relation execution.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-rule-maintenance \
  --rust-revision 7346fcb --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case relation_rule_initial --case relation_rule_small_update --timeout 120
```

Initial derivation overrides the repeated protocol: one invocation per fresh process, with no setup or warmup.
It inserts 512 edges, commits, and reads 8,448 closure rows across 16 disjoint chains.
The update fixture materializes this closure during setup, then performs 16 publishing updates per invocation.
Each update changes one edge and 32 closure rows. Bound reads check changed results and an untouched component.

| Workload | Rust interpreter | Rust native enabled | Odin |
| --- | ---: | ---: | ---: |
| Initial load and derivation | 27.73 ms | 27.89 ms | 2.03 ms |
| 16 small updates | 761.90 ms | 761.75 ms | 15.22 ms |

Values are medians of process medians. Raw samples, process ranges, RSS, hashes, and toolchains are in `manifest.json`.
Initial-process RSS is approximately 24.5 MiB for Rust and 6.1 MiB for Odin.
Update-process RSS is approximately 27 MiB for Rust and 9–10 MiB for Odin.
Rust's incremental architecture does not establish a performance advantage on this workload.

A separate `perf record -F 499 --call-graph dwarf` run used the same Rust binary and update fixture.
The sampled stacks put unification, value equality, and allocation under `differential::evaluate_rule_full` during commit maintenance.
Recursive deletion performs a full replacement-derivation seed using nested scans, despite already having maintained join indexes.
The profile is diagnostic only; its elapsed samples are excluded from the table.
