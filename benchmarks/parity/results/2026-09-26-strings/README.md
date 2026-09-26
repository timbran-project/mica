# String and collection capability capture

This capture compares Rust `a938ee4` with Odin `bfb368c`, using the initial baseline's protocol.
Full hashes, raw samples, commands, and machine details are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-a938ee4 \
  --rust-revision a938ee4 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 30
```

Of 198 processes, 192 passed. Only Rust's `language_for_pattern` fixture still fails.
The five fixtures previously blocked by `len` and `string_append` now pass in both Rust tiers.

| Workload | Rust interpreter before | Rust interpreter after | Odin in this capture |
| --- | ---: | ---: | ---: |
| string construction | 304.3 µs | 263.7 µs | 170.1 µs |
| string append | unsupported | 254.0 µs | 163.5 µs |
| short-string slicing | 741.1 µs | 734.0 µs | 415.7 µs |
| list construction | unsupported | 4514.8 µs | 270.2 µs |
| sorting | unsupported | 372.1 µs | 249.4 µs |
| warm recursive closure scan | unsupported | 82.3 µs | 65.1 µs |

Before values come from `../2026-09-26-baseline/summary.json`.
Numbers are medians of three process medians for complete invocations.
The short-string slicing change is too small to establish a useful improvement.
List construction remains a substantial performance gap despite passing conformance.

Each process includes two warmup and fourteen timed invocations.
Loading and setup are excluded from timing but included in peak RSS.
Both implementations use one task worker, one pinned CPU, in-memory storage, root authority, and no accelerator.
This capture does not establish durable-storage or worker-scaling performance.
