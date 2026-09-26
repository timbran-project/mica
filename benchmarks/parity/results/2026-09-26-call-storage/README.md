# Call-storage capture

This capture compares Rust `a00cab6` with Odin `bfb368c` on six selected fixtures.
All 54 processes pass. Full provenance, raw samples, and fixed-work RSS are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-call-storage \
  --rust-revision a00cab6 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 30 \
  --case language_call --case runtime_callable --case language_string \
  --case language_string_slice --case runtime_dynamic_dispatch --case language_arithmetic
```

| Workload | Rust interpreter before | Rust interpreter after |
| --- | ---: | ---: |
| helper calls | 13.659 ms | 13.135 ms |
| closure calls | 6.042 ms | 5.995 ms |
| dynamic dispatch | 1.116 ms | 1.100 ms |
| string construction | 0.264 ms | 0.266 ms |
| short-string slicing | 0.734 ms | 0.730 ms |
| arithmetic control | 14.747 ms | 14.669 ms |

Before values come from `../2026-09-26-strings/summary.json`.
The helper-call improvement is modest. The other changes are too small to establish useful performance differences.
Cleared argument and frame buffers remove repeated allocation, but dispatch and interpreter costs remain.
These results do not establish general call-performance parity.

The protocol uses one pinned CPU, one worker, in-memory storage, root authority, and no accelerator.
Each process includes two warmup and fourteen timed invocations.
