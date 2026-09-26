# Cold dispatch insertion capture

Rust `3eef87b` and Odin `bfb368c` use the same cold protocol as `../2026-09-26-dispatch-cold-before`.
All nine processes pass. Full provenance and raw samples are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-dispatch-cold-after \
  --rust-revision 3eef87b --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 1 --iterations 1 --warmup 0 --workers 1 --timeout 90 \
  --case language_call
```

Rust interpreter median cold invocation latency falls from 29.14 seconds to 27.6 ms.
Odin takes 1.36 ms. Rust peak RSS falls to 25,268–25,424 KiB.
Dispatch cache insertion now updates ordered maps instead of cloning all prior entries.
The remaining call overhead is still substantial; this result does not establish general call parity.
