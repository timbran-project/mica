# Compiler instruction chunks: rejected

Candidate Rust `e1a02c2e29f05e994d7a3e6656a79dc2d1965406` against Odin `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The [baseline](../2026-09-26-compiler-emission/README.md) uses Rust `174e017`.
Both captures use CPU 5, one worker, in-memory storage, no accelerator, and the same fixed invocation counts.
All 18 candidate processes pass. No build or test ran during measurement.

| Emission | Baseline | Instruction chunks |
| --- | ---: | ---: |
| Rust interpreter | 23.400 ms | 23.301 ms |
| Rust native enabled | 23.473 ms | 23.431 ms |
| Odin interpreter | 8.984 ms | 8.880 ms |

Rust changes by less than 0.5%, with similar process peak RSS. The frontend control remains near 36.1 ms.
The added chunk bookkeeping does not produce a material improvement on this workload, so the instruction-storage change is reverted.
All eleven compiler application tests passed for the candidate, including bootstrap artifact equality.
The result rejects this implementation for the measured input; it does not establish behaviour at every program size.

Raw evidence remains in `/tmp/mica-parity-compiler-chunks`.
Reproduce with the baseline command, using Rust `e1a02c2e29f05e994d7a3e6656a79dc2d1965406` and a fresh output directory.
