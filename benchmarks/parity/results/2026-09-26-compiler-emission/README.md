# Mica compiler emission baseline

Rust `174e0171d21ee353cc149c3a33e02c71ab530f42`; Odin `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
All 18 processes pass. CPU 5, one worker, in-memory storage, no durability or accelerator.
Each fresh process runs one warmup and ten timed invocations, grouped into five samples.
Both permit 100 million instructions and 1,024 call frames per task. No build or test ran during measurement.

| Workload | Rust interpreter | Rust native enabled | Odin interpreter |
| --- | ---: | ---: | ---: |
| Frontend, 200 declarations | 36.037 ms | 36.091 ms | 37.642 ms |
| Source to artifact, 100 declarations and comprehension | 23.400 ms | 23.473 ms | 8.984 ms |
| Emission process peak RSS | 32,432–32,460 KiB | 32,440–32,464 KiB | 34,912–35,048 KiB |

Times are medians of process medians, including invocation submission, execution, completion, and verification.
Prelude loading occurs before timing and remains included in process peak RSS.
The frontend builds and lexes source, then parses it through another lexer pass.
Emission builds source and compiles through one lexer pass, parsing, emission, and assembly.
These workloads differ in input size and work; subtracting their times does not isolate emitter cost.

Emission verifies successful assembly, a nonempty artifact, and the 1,941-scalar source length.
The Rust application test separately executes the generated artifact and checks `[6, 4]` in both modes.
Installation and generated-program execution are outside the timed workload.
The fixture reads Rust's `:entry` or Odin's `:bytes` field. Artifact formats and compiler implementations differ.

Rust is approximately 2.6 times slower on this emission workload. Native-enabled execution has no material benefit in this capture.
A separate 90-invocation profile finds substantial reference-count traffic and frame cleanup.
Its top atomic increment/decrement helpers account for approximately 31% of sampled cycles; value cloning adds approximately 5%.
The profile is diagnostic evidence and contributes no accepted timing sample.

Reproduce in a fresh directory:

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-compiler-emission \
  --rust-revision 174e0171d21ee353cc149c3a33e02c71ab530f42 --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 5 --iterations 2 --warmup 1 --workers 1 \
  --case compiler_emission --case compiler_frontend
```

Raw builds, logs, binaries, and exports remain in `/tmp/mica-parity-compiler-emission`.
The diagnostic profile is `/tmp/mica-compiler-emission.perf.data`.
