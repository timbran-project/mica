# Comprehension capture

This capture compares Rust `731583f` with Odin `bfb368c` on all 22 repeated-invocation fixtures.
196 of 198 processes pass. Full provenance, samples, and failures are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-comprehensions \
  --rust-revision 731583f --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 30
```

All nine loop-pattern/comprehension processes return the expected result, 47.
Rust interpreter invocation latency is 19.7 microseconds; Odin takes 59.3 microseconds.
The fixture is small and includes invocation overhead. It does not establish general comprehension throughput.

Two helper-call processes time out: Rust interpreter repetition 0 and native-enabled repetition 1.
The other four Rust helper-call processes pass. These failures remain visible in the summary.
The process timeout includes loading and cold warmup, whereas reported samples measure warmed invocations.
Cold dispatch-cache population needs separate investigation; this capture does not identify the timeout cause.

The protocol uses one pinned CPU, one worker, in-memory storage, root authority, and no accelerator.
Each successful process includes two warmup and fourteen timed invocations.
