# Unrestricted dispatch and compiler frontend measurements

Rust `3d5c166` and Odin `bfb368c` pass all 18 processes.

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-arity-cache \
  --rust-revision 3d5c166 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 3 --iterations 1 --warmup 1 --workers 1 \
  --case language_call_shared_list --case compiler_frontend --timeout 120
```

Each process uses one worker, root authority, in-memory storage, serial CPU relation execution, and no accelerator.
The timer includes task submission, execution, and result checks. RSS includes loading, warmup, and fixed timed work.

| Workload | Rust interpreter | Rust native enabled | Odin |
| --- | ---: | ---: | ---: |
| Shared collection calls | 0.671 ms | 0.682 ms | 0.344 ms |
| Mica compiler frontend | 36.112 ms | 36.286 ms | 37.059 ms |

The shared-call workload previously took 13.539 ms in the Rust interpreter at `0ad9d57`, with the same protocol.
Caching unrestricted positional dispatch by arity removes argument retention and structural key comparisons from the snapshot cache.
The workload improves by approximately 20 times. Rust still takes approximately twice Odin's time.
Rust peak RSS stays near 21 MiB, compared with approximately 5.3 MiB for Odin.

The frontend workload constructs and lexes 200 Unicode declarations, then parses the same source.
It verifies 1,001 tokens, source positions, zero diagnostics, and 200 binding nodes inside each invocation.
Each implementation loads its own pinned Mica lexer and parser. Their hashes and the generated fixture hashes appear in the manifest.
Rust peak RSS is 26.2–26.3 MiB. Odin peak RSS is approximately 19.0 MiB.
This is a frontend comparison; it does not measure emission, installation, bootstrap, or native host compilers.
The Rust and Odin frontend medians are close on this workload. This result does not establish general compiler parity.

Workspace tests and clippy pass for the cache change. Regression coverage includes overload restrictions, local catalogue writes, and retained snapshots.
The separate compiler bootstrap check passes in both execution modes.
Raw samples, process ranges, source hashes, and compiler identities are in `manifest.json`.
