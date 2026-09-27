# mica-runner

`mica-runner` is the command-line executable crate for Mica. It provides the `mica` binary, the
REPL, source evaluation, filein, and fileout commands.

This crate is not the live runtime environment. That code lives in `mica-runtime`; this crate is a
command-line consumer of it. Ordinary source evaluation is submitted through `mica-driver` so timed
suspensions, commits, input waits, and emitted effects use the same driver path as future daemons
and listeners.

## What's Here

- `src/main.rs`: command-line entry point for `mica run`, `mica filein`, `mica eval`, and
  `mica repl`.

## Commands

Run one source file as a single source chunk:

```sh
cargo run --bin mica -- run path/to/file.mica
```

File in a source file as a sequence of chunks:

```sh
cargo run --bin mica -- filein apps/shared/capabilities.mica
```

Evaluate a one-line source string:

```sh
cargo run --bin mica -- eval "1 + 1"
```

Start the REPL:

```sh
cargo run --bin mica
```

## Role In Mica

The runner opens a `mica-runtime` `SourceRunner`, gives it to the compio driver, feeds source from
files, command-line arguments, or the REPL through that driver, and renders reports for humans.
Filein/fileout still use the runtime directly because they are import/export operations rather than
ordinary task submissions.

Local `run`, `eval`, and REPL sessions use administrative authority by default.
With `--actor IDENTITY`, the session uses that actor's policy through an endpoint.
Loading a file with `eval --filein` does not grant the actor permission to invoke its verbs.

## Licence

Mica is licensed under the GNU Affero General Public License v3.0. See the repository root
`LICENSE`.
