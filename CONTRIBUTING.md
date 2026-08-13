# Contributing to Mica

Mica is a correctness-sensitive language and runtime for live relational object systems.
Contributions are welcome, but the bar is intentionally high: changes should be understandable,
tested, and owned by the person submitting them.

## Coding Style

Follow [`CODING-STYLE.md`](./CODING-STYLE.md).

In particular:

- Prefer small, correct changes over broad rewrites.
- Keep names factual and descriptive; avoid names based on implementation history or temporary
  context.
- Use ordinary Rust formatting with `cargo fmt --all`.
- Keep the workspace clippy-clean where practical.
- Avoid deep nesting; prefer early returns, `let else`, and small focused functions where they
  clarify the code.
- Add tests for new behaviour and regressions.
- Avoid gratuitous crate dependencies. New dependency versions belong in the root workspace
  `Cargo.toml`, and member crates should inherit them with `workspace = true`.

Before submitting non-trivial changes, run the relevant checks where practical:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets
```

`cargo-nextest` 0.9.48 or later is also supported and is usually faster:

```bash
cargo nextest run --workspace
```

The repository nextest configuration puts crates that construct a Compio driver in a
single-threaded resource group. The driver crate also serializes its Compio-owning unit tests within
the ordinary Cargo test harness, so both commands avoid exhausting thread or address-space limits.

For language or runner changes, also consider exercising the current filein example:

```bash
cargo run --bin mica -- filein apps/shared/capabilities.mica
```

Use narrower commands when the full suite is not practical, and say what you did and did not run.

## Project Shape

Mica is still early, but it is not a throwaway experiment. Treat the existing crate boundaries as
meaningful:

- `mica-var`: compact value representation.
- `mica-relation-kernel`: relations, transactions, rules, queries, and dispatch matching.
- `mica-vm`: bytecode format and register VM execution core.
- `mica-compiler`: lexer, parser, lowering, semantic analysis, and bytecode compilation.
- `mica-runtime`: live environment, task manager, builtins, filein/fileout, and rendered reports.
- `mica-driver`: compio task driver, wakeups, input, and emissions.
- `mica-runner`: CLI and REPL binary.

Keep design notes, crate READMEs, and app fileins aligned with implemented syntax. When documenting
future syntax or semantics, mark it clearly as planned rather than current.

## Agentic Code Conduct

AI assistants and coding agents are acceptable tools on this project. They do not reduce the
contributor's responsibility for the result.

- Review and understand all generated code before submitting it.
- Write your own commit messages, pull-request descriptions, issue comments, and code review
  comments.
- Do not submit code you cannot explain.
- Verify generated claims against the codebase, tests, docs, or primary sources.
- Strip vague marketing language, filler, and generic AI prose from documentation and comments.
- Call out confused, overcomplicated, or poorly reasoned agentic output when you see it.
- Be honest about uncertainty, test coverage, and remaining risks.

You are responsible for your work. Obvious AI slop will be rejected.

The standard is directed engineering work with human accountability.

## Conduct

Be direct, factual, and constructive. Technical disagreement is expected; personal contempt is not.

Do not be a dick.

## Licence

Mica is free software licensed under the GNU Affero General Public License v3.0. By submitting a
contribution, you agree to license that contribution under the same terms.

Do not submit work owned by an employer, client, or another project unless you have the right to
contribute it under these terms. Substantial contributions may require separate written confirmation
before they are accepted.
