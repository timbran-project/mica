# Parser fact chunks

Rust `f3ca3e291e5a84c12c2f49f53a77457be17f5cc0` against Odin `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The [baseline](../2026-09-26-compiler-emission/README.md) uses Rust `174e017`.
Both captures use CPU 5, one worker, in-memory storage, no accelerator, and the same fixed invocation counts.
Each process runs one warmup and ten timed invocations in five samples. Limits are 100 million instructions and 1,024 call frames.
All 18 processes pass. No build or test ran during measurement.

| Workload | Rust baseline | Rust fact chunks | Odin, latest capture |
| --- | ---: | ---: | ---: |
| Frontend, interpreter | 36.037 ms | 19.590 ms | 37.116 ms |
| Source to artifact, interpreter | 23.400 ms | 19.455 ms | 8.807 ms |
| Frontend, native enabled | 36.091 ms | 19.618 ms | — |
| Source to artifact, native enabled | 23.473 ms | 19.575 ms | — |

Rust frontend latency falls by approximately 46%; source-to-artifact latency falls by approximately 17%.
On these inputs, Rust is approximately 1.9 times faster for the frontend and 2.2 times slower for source-to-artifact compilation.
The frontend uses 200 declarations and two lexer passes; emission uses 100 declarations, a comprehension, and one lexer pass.
Their times cannot be subtracted to isolate emitter cost. Native-enabled execution remains close to the interpreter result.

Frontend peak RSS is 26,844–26,992 KiB for Rust's interpreter and 37,000–37,068 KiB for Odin.
Emission peak RSS is 32,212–32,320 KiB for Rust's interpreter and 34,976–34,980 KiB for Odin.
RSS includes loading, warmup, and fixed timed work. These are process peaks, not allocation counts.

The parser stores facts in 64-row chunks, then flattens them once before returning its public result.
This reduces nested-list prefix copying while preserving immutable values, AST row order, and the public list/relation representation.
The separate instruction-chunk experiment was reverted after it produced no material timing improvement.

All eleven compiler application tests pass, including the shared corpus, generated-program execution, suspension, and bootstrap artifact equality in both modes.
Workspace clippy passes. The full workspace passed before these parser-only changes.
The debug compiler test run falls from roughly 182 seconds to 52 seconds; that observation is separate from the release measurements above.

Reproduce in a fresh directory:

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-compiler-fact-chunks \
  --rust-revision f3ca3e291e5a84c12c2f49f53a77457be17f5cc0 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 5 --iterations 2 --warmup 1 --workers 1 \
  --case compiler_emission --case compiler_frontend
```

Raw logs, builds, binaries, and exports remain in `/tmp/mica-parity-compiler-fact-chunks`.
Compiler installation, broader language coverage, and further emission performance remain separate work.
