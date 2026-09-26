# Exact retrieval baseline

Rust `3bce027` and Odin `bfb368c` run 32 exact searches over 512 eight-dimensional vectors per invocation.
The fixture checks scores and sums subject ids, including deterministic tie selection.
All nine processes pass. Full provenance and raw samples are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-retrieval-before \
  --rust-revision 3bce027 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 60 \
  --case retrieval_repeated_exact
```

Rust interpreter median invocation latency is 52.90 ms; Odin takes 6.72 ms.
Rust rebuilds candidate membership and parses vectors on each search in this baseline.
One invocation uses one transaction. Loading and index population are outside the timer.
All variants use serial CPU execution, root authority, in-memory storage, and no accelerator.
