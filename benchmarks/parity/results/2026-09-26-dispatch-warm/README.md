# Warm dispatch and full corpus capture

Rust `3eef87b` and Odin `bfb368c` pass all 207 processes across 23 fixtures.
The corpus includes the 22 adapted donor fixtures and the shared exact-retrieval workload.
Full provenance, raw samples, and fixed-work RSS are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-dispatch-warm \
  --rust-revision 3eef87b --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 30
```

Rust interpreter helper-call latency is 10.80 ms, compared with 13.14 ms in the earlier call-storage capture.
Odin takes 1.20 ms. The other original Rust fixtures stay within five percent of the comprehension capture.
No helper-call process reaches the timeout. The cold captures separately measure first-invocation cost.

All processes use one pinned CPU, one worker, root authority, in-memory storage, and no accelerator.
Each process performs two warmup and fourteen timed invocations.
The read/write-lock cache has not yet been measured under contention from multiple task workers.
