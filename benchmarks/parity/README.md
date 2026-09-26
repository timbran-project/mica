# Shared Mica measurements

`corpus.json` records independent expected results and source hashes for fixtures from Odin revision `bfb368c`.
Adaptations use public language syntax and make commit workloads publish changes.
The manifest records each adaptation and excluded fixture.

The Rust driver invokes installed methods through `DriverAdministrator`.
The Odin driver invokes them through `world_call`.
Both use root authority, serial CPU relation execution, and the requested number of task workers.
Every timed invocation includes task submission, execution, suspension, completion, and result verification.
Loading and optional `setup()` execution occur before the timer.

## One Rust workload

```sh
cargo build --release -p mica-runner
target/release/mica bench benchmarks/parity/mica/language_arithmetic.mica \
  --expected 14999850000 --tier interpreter --workers 1 \
  --warmup 2 --samples 7 --iterations 8
```

`--expected` accepts JSON values in Rust. The Odin comparison driver currently accepts integer results.
`--setup` invokes `setup()` exactly once. A missing or failed setup is an error.
`--tier native` permits Cranelift execution. It does not assert that every operation executes as native code.
The default tier disables native execution for initial, spawned, and resumed tasks.

The Rust command also accepts the existing global storage and durability arguments.
The shared comparison uses in-memory storage on both implementations.
Its metadata reports durability as `none` and accelerator placement as disabled.

## Pinned comparison

Select CPUs from the current affinity mask:

```sh
python3 -c 'import os; print(sorted(os.sched_getaffinity(0)))'
```

Capture results in a fresh directory:

```sh
python3 benchmarks/parity/capture.py /tmp/mica-parity-baseline \
  --rust-revision HEAD --odin-revision bfb368c --cpus 5 \
  --runs 3 --samples 7 --iterations 8 --warmup 2 --workers 1
```

Replace `5` with an allowed CPU. For worker scaling, supply enough CPU ids for the worker count.
The command requires Linux, `taskset`, GNU `time`, Cargo, and Odin.
It does not change the governor or stop existing processes.

The launcher exports committed source into isolated directories.
It copies the current measurement harness and corpus, with hashes, into the capture.
It builds Rust with `--locked --release` and Odin with `-o:speed`.
The source exports, binaries, compiler identities, commands, and build logs remain in the capture directory.

Each workload runs in fresh processes with identical warmup and timed invocation counts.
Workload and implementation order rotate across repetitions.
The launcher records all failures and returns a nonzero status if any workload fails.
A known capability gap remains a failed result. It is not silently skipped.
`--case NAME` restricts a capture explicitly, and the manifest records the selected names.

## Interpreting results

`manifest.json` contains raw elapsed samples, process peak RSS, and provenance.
`summary.json` contains the median of process medians, their range, and the 95th percentile of sample averages.
Sample averages are not individual-request tail latency measurements.
No summary timing is accepted unless every process for that workload and implementation passes.

Peak RSS includes startup, fixture loading, setup, warmup, and fixed timed work.
It is not an allocation count or a measurement of retained memory alone.
The fixed work avoids faster implementations performing more allocations during adaptive calibration.

`relation_rule_closure` measures a warm derived scan.
Initial derivation and incremental maintenance require separate workloads and remain part of the parity plan.
The single-use identity and installation fixtures also require a separate protocol.

Run the harness contract tests:

```sh
python3 -m unittest discover -s benchmarks/parity -p 'test_*.py'
cargo test -p mica-runner --test bench
```
