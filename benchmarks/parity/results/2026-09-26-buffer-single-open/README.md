# Buffer checkpoints with one Fjall open

All 12 processes passed text and revision checks. The baseline is `a44d3ac`; the bounded-checkpoint implementation is `31e8b1b`.
The latter validates store format on the database it opens for normal use.
Both binaries came from pinned source exports. The after build used an isolated Cargo target directory.

The protocol matches the [earlier capture](../2026-09-26-buffer-checkpoints-4096/README.md):
4,096 edits, a 131,072-scalar initial buffer (256 KiB), one caller, release builds, CPU 5, and three fresh processes per mode.
Edit time includes the final flush. Recovery includes opening Fjall and loading kernel state.

| Median across three processes | Before | Bounded checkpoints, one open |
| --- | ---: | ---: |
| Strict edits and flush | 2,647 ms | 2,819 ms |
| Strict edit p95 | 1,414 µs | 1,492 µs |
| Strict recovery | 42.8 ms | 29.0 ms |
| Relaxed edits and flush | 69.1 ms | 81.0 ms |
| Relaxed recovery | 45.4 ms | 30.6 ms |
| Strict peak RSS | 16,168 KiB | 17,948 KiB |

Recovery improves by approximately one third. The bounded journal still increases edit time and memory use at this workload.
The final edit reaches the checkpoint boundary. This capture does not measure worst-case replay within the journal limit.
Full-buffer checkpoint cost and Fjall journal recovery remain relevant for larger documents and histories.

```sh
python3 benchmarks/parity/capture_buffers.py /tmp/mica-buffer-single-open-2026-09-26 \
  --before /tmp/mica-buffer-checkpoint-before --after /tmp/mica-buffer-single-open \
  --before-revision a44d3ac69d6b2b0d097141d7c63cd5c382da1eb5 \
  --after-revision 31e8b1bee6d0ed2aff250c473aa72a13748c43b3
```
