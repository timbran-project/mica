# Buffer recovery after 32,768 edits

All 12 processes passed. The revisions and protocol match the
[4,096-edit capture](../2026-09-26-buffer-checkpoints-4096/README.md), with `--edits 32768`.
Each process starts with 131,072 Unicode scalars (256 KiB) and replaces one scalar per committed edit.

| Median across three processes | Before | 4,096-delta checkpoints |
| --- | ---: | ---: |
| Strict edits and flush | 29,637 ms | 31,713 ms |
| Strict edit p95 | 4,205 µs | 4,698 µs |
| Strict recovery | 325 ms | 420 ms |
| Relaxed edits and flush | 503 ms | 605 ms |
| Relaxed recovery | 325 ms | 423 ms |
| Strict peak RSS | 46,408 KiB | 61,184 KiB |

Bounding buffer delta replay does not bound all startup work. Fjall still recovers its journal, including retained commit entries.
These revisions also open the database twice during startup: once for format validation and once for normal use.
The follow-up removes the duplicate open. This capture retains the measured cost of checkpoints before that change.

Full stores remain under `/tmp/mica-buffer-checkpoints-history-2026-09-26`.
