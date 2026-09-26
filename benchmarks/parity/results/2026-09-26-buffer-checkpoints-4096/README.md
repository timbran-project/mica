# Buffer checkpoints: 4,096-delta interval

All 12 processes passed text and revision checks. This capture uses the same 4,096-edit protocol as the
[64-delta capture](../2026-09-26-buffer-checkpoints-64/README.md).
The baseline is `a44d3ac`; the longer interval is `ee62c44`.
The latter also releases retained commit history iteratively and reserves retired buffer names.

| Median across three processes | Before | 4,096-delta checkpoints |
| --- | ---: | ---: |
| Strict edits and flush | 2,368 ms | 2,605 ms |
| Strict edit p95 | 1,199 µs | 1,336 µs |
| Strict recovery | 43.0 ms | 53.7 ms |
| Relaxed edits and flush | 63.9 ms | 77.5 ms |
| Relaxed recovery | 42.7 ms | 54.6 ms |
| Strict peak RSS | 16,108 KiB | 17,940 KiB |

The longer interval reduces the first implementation's regression, but the baseline remains faster at this workload.
Recovery includes provider opening and kernel loading. It does not isolate buffer delta replay from Fjall journal recovery.
The final edit reaches the checkpoint boundary. The journal limit remains fewer than 4,096 deltas and less than 1 MiB per buffer.

The capture command used `capture_buffers.py` defaults, with these arguments:

```sh
python3 benchmarks/parity/capture_buffers.py /tmp/mica-buffer-checkpoints-4096-2026-09-26 \
  --before /tmp/mica-buffer-checkpoint-before --after /tmp/mica-buffer-checkpoint-long \
  --before-revision a44d3ac69d6b2b0d097141d7c63cd5c382da1eb5 \
  --after-revision ee62c44765b9a089b8ff836ee1f0e7b3e9a546f3
```
