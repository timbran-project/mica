# One Fjall open after 32,768 buffer edits

Eight of 12 processes passed. Four strict-mode processes exceeded the 120-second whole-process limit: two in each revision.
The manifest retains these failures. Each timed-out process group was terminated by its capture harness.
The single successful strict process per revision does not establish a stable strict-mode comparison.

All six relaxed-mode processes passed text and revision checks.
The revisions and protocol match the [4,096-edit capture](../2026-09-26-buffer-single-open/README.md), with `--edits 32768`.

| Relaxed-mode median across three processes | Before | Bounded checkpoints, one open |
| --- | ---: | ---: |
| Edits and flush | 520 ms | 604 ms |
| Edit p95 | 18.1 µs | 18.7 µs |
| Recovery | 328 ms | 211 ms |
| Peak RSS | 46,344 KiB | 61,528 KiB |

One database open reduces recovery time. Checkpoints retain an edit-time and memory cost.
Startup still scales with Fjall journal recovery and retained commit records, despite bounded canonical buffer replay.
Full stores and process logs remain under `/tmp/mica-buffer-single-open-history-2026-09-26`.
