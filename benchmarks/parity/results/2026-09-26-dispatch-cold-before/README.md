# Cold dispatch baseline

Rust `8fc4589` and Odin `bfb368c` each run one helper-call invocation in a fresh process, with no warmup.
All nine processes pass. Full provenance and raw samples are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-dispatch-cold-before \
  --rust-revision 8fc4589 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 1 --iterations 1 --warmup 0 --workers 1 --timeout 90 \
  --case language_call
```

Rust interpreter median cold invocation latency is 29.14 seconds, with process medians from 29.00 to 29.64 seconds.
Odin takes 1.28 ms. Rust peak RSS is 32,560–32,564 KiB in the interpreter runs.
Loading is outside the invocation timer. Cold dispatch population occurs inside it.
The earlier 30-second process limit was close to the time needed by this first invocation alone.
