# Buffer checkpoints: 64-delta interval

This capture rejected the initial checkpoint interval because edit time, recovery time, and memory use increased.
The raw manifest retains all 12 successful processes. Each process used a fresh Fjall store and verified recovered text and revision.

The baseline is `a44d3ac`; the checkpoint implementation is `e5aae9c`.
Both probes used release builds, one caller, CPU 5, 4,096 committed edits, and a 131,072-scalar initial buffer (256 KiB).
Each edit replaced one scalar. Creation occurred before timing. Total edit time includes the final persistence flush.
Recovery time includes opening the provider and loading the kernel. GNU time RSS covers the complete process.
The capture rotated before/after order across three processes per durability mode.

| Median across processes | Before | 64-delta checkpoints |
| --- | ---: | ---: |
| Strict edits and flush | 2,324 ms | 8,069 ms |
| Strict edit p95 | 1,265 µs | 4,563 µs |
| Strict recovery | 43.1 ms | 96.1 ms |
| Relaxed edits and flush | 63.1 ms | 140.3 ms |
| Relaxed recovery | 42.8 ms | 94.4 ms |
| Strict peak RSS | 16,168 KiB | 47,324 KiB |

Full-buffer checkpoints caused excessive write amplification at this interval.
The result does not establish editor performance or compare Rust buffers against Odin.
The follow-up changes the interval while retaining explicit recovery limits.

The capture command was:

```sh
python3 benchmarks/parity/capture_buffers.py /tmp/mica-buffer-checkpoints-2026-09-26 \
  --before /tmp/mica-buffer-checkpoint-before \
  --after /tmp/mica-buffer-checkpoint-after \
  --before-revision a44d3ac69d6b2b0d097141d7c63cd5c382da1eb5 \
  --after-revision e5aae9c1b30eeb2b120a8b07250838f3f4a7d310
```

The binaries came from each pinned revision. They have distinct SHA-256 hashes in the manifest.
Full stores and process RSS files remain in the capture directory under `/tmp`.
