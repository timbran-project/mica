# List-append capture

This capture compares Rust `44019c7` with Odin `bfb368c` on six selected fixtures.
All 54 processes pass. Full provenance, raw samples, and fixed-work RSS are in `manifest.json`.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-list-append \
  --rust-revision 44019c7 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 30 \
  --case language_list_build --case language_list --case language_sort \
  --case runtime_callable --case language_string --case relation_scan_large
```

The 2,000-element scalar-list construction fixture improves from 4.515 ms to 0.427 ms in Rust's interpreter.
Odin takes 0.265 ms in this capture. The Rust median is about 10.6 times faster than the earlier copying implementation.
Rust peak RSS for that fixture is 21,292–21,312 KiB across the three fixed-work processes.
Before values come from `../2026-09-26-strings/summary.json`.
The intervening activation-buffer change did not change this fixture's list-building loop.

The other selected Rust workloads remain within about two percent of the earlier capture.
This fixture appends integer scalars. Appends containing nested lists still copy to prevent reference-count cycles.
The result does not establish append performance for all collection shapes.

The protocol uses one pinned CPU, one worker, in-memory storage, root authority, and no accelerator.
Each process includes two warmup and fourteen timed invocations.
