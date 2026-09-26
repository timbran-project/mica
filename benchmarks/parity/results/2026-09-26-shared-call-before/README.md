# Shared collection call workload: before

Rust `59c39a2` and Odin `bfb368c` pass all nine processes.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-shared-call-before \
  --rust-revision 59c39a2 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case language_call_shared_list --timeout 120
```

Each invocation builds 2,048 scalar values and makes 400 calls with fresh outer lists that share that input.
The independently calculated result is 899,000. Input-preservation checks run inside the timer.
The repeated protocol includes cache state from earlier invocations in the same process.

| Implementation | Before | After |
| --- | ---: | ---: |
| Rust interpreter | 21.49 ms | 13.54 ms |
| Rust native enabled | 21.47 ms | 13.55 ms |
| Odin | 0.331 ms | 0.349 ms |

Value equality and ordering now recognize identical value words before traversing their immutable payloads.
Rust improves by approximately 37 percent; peak RSS remains approximately 21 MiB.
Rust remains approximately 39 times slower than Odin in the after capture. This change does not establish call-performance parity.
Separately allocated values still require structural comparisons, including equal input lists retained from earlier invocations.

Workspace tests and clippy pass. Tests cover shared graphs, canonical ordering, retained list views, and the Mica frontend corpus.
Raw samples, process ranges, RSS, source hashes, and compiler identities are in `manifest.json`.
