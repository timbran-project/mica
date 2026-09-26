# Exact retrieval preparation capture

Rust `8fc4589` and Odin `bfb368c` run the same fixture and protocol as `../2026-09-26-retrieval-before`.
All nine processes pass. Full provenance and raw samples are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-retrieval-cache \
  --rust-revision 8fc4589 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 60 \
  --case retrieval_repeated_exact
```

Rust interpreter median invocation latency falls from 52.90 ms to 2.78 ms, about 19 times faster.
Odin takes 6.54 ms in this capture. Rust peak RSS is 21,924–21,996 KiB, compared with 21,972–22,080 KiB before.
Each process performs two warmup and fourteen timed invocations.

Each invocation prepares its candidates once and reuses them for 32 exact searches in one transaction.
The cache does not persist between invocations. The result measures repeated preparation reuse and exact CPU scoring.
It does not measure GPU execution, approximate ranking, or cache reuse across transactions.
