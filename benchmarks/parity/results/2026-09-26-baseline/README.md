# Initial release baseline

This capture compares Rust `2d80218` with Odin `bfb368c` on one CPU from the current machine's affinity mask.
`manifest.json` contains full revision hashes, compiler identities, machine details, fixture hashes, commands, raw samples, and failures.
`summary.json` contains the derived summaries. Both files retain the capture's original data as compact JSON.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-2d80218 \
  --rust-revision 2d80218 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 2 --warmup 2 --workers 1 --timeout 30
```

Each process performs two warmup invocations and fourteen timed invocations.
Both implementations use in-memory storage, root authority, serial CPU relation execution, and no accelerator.
Rust runs with native execution disabled and enabled in separate processes.
Enabling native execution does not prove that a fixture uses compiled native code.

Of 198 processes, 162 passed and 36 failed. All Odin processes passed.
Six Rust fixtures failed in both tiers across all repetitions:

| Fixture | Observed failure |
| --- | --- |
| `language_for_pattern` | loop destructuring and comprehension parse errors |
| `language_list_build` | missing `len` builtin |
| `language_sort` | missing `len` builtin |
| `language_string_append` | missing `string_append` builtin |
| `relation_rule_closure` | missing `len` builtin |
| `relation_scan_large` | missing `len` builtin |

The release helper-call fixture passes despite the earlier debug probe timeout.
Its median invocation takes 13.32 ms in Rust's interpreter and 1.19 ms in Odin.
The arithmetic fixture takes 14.46 ms and 2.27 ms respectively.
The large-map fixture takes 0.0375 ms and 0.0710 ms respectively.
These examples show why parity requires separate workload measurements.

Peak RSS includes loading, setup, warmup, and timed work.
The percentile statistic describes sample averages, not individual-request tail latency.
This capture does not measure durable storage, initial derivation, incremental maintenance, or worker scaling.
The original source exports, binaries, logs, and process outputs remain under `/tmp/mica-parity-2d80218` on the capture host.
