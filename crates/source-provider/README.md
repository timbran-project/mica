# mica-source-provider

`mica-source-provider` exposes source code browsing as Mica computed relations. It provides
repository entry listings, file text, syntax highlighting, semantic symbol search, definition-at and
references-of lookups, text search, and VCS history through the same relation interface that Mica
code uses for all other data.

## What's Here

- `src/index.rs`: persistent semantic index format, loading, and source index file building. Indexes
  symbols, references, and text chunks from a set of source root directories.
- `src/syntax.rs`: `SyntaxDocument` parsing with tree-sitter (Rust, JavaScript, Markdown) and Mica's
  native lexer for syntax highlighting and outline extraction.
- `src/rust_analyzer.rs`: `RustAnalyzerProvider`, a managed rust-analyzer LSP process pool for
  definition and references queries with session reuse and automatic document synchronization.
- `src/navigation.rs`: semantic location resolution, symbol identity encoding, and
  byte-offset-to-LSP-position conversion.
- `src/provider.rs`: the public native provider contract, bounded local-worktree provider, provider
  result types, capabilities, and explicit per-runtime configuration.
- `src/dispatcher.rs`: relational provider selection, precedence and fallback handling, bounded
  listing composition, document caching, and repository-root admission.
- `src/relations.rs`: computed relations that bind source dispatch, syntax parsing, semantic index
  queries, rust-analyzer LSP results, and VCS history into the Mica relation model. Includes
  `RepositoryEntry(7)`, `FileText(7)`, `FileLines(9)`, `SyntaxLine(10)`,
  `SyntaxOutline(12)`, `SyntaxNodeAt(13)`, `DefinitionAt(13)`, `ReferencesOf(10)`,
  `SymbolSearch(11)`, `IndexedTextUnit(9)`, `TextSearch(11)`, index metadata, and VCS relations for
  commits, diffs, logs, blame, and file history.
- `src/vcs.rs`: `VcsProvider`, a git-backed version control reader using `jj-lib` (GitBackend).
  Supports commit metadata, tree traversal, file content, diffs with unified diff output, blame,
  commit log walking, and commit text search.
- `src/util.rs`: shared helpers for relation binding extraction, path validation, content hashing,
  row filtering, and error construction.

## Role In Mica

This crate produces computed relations through `computed_relations(SourceConfig)`. `SourceRunner`
and `DriverBuilder` provide host-facing constructors for installing them while retaining the
standard runtime relations and host-request functions.

Native providers are registered explicitly in `SourceConfig`. Mica facts in
`source/RepositoryProvider(repository, provider, capability, precedence)` and
`source/ProviderEnabled(provider, enabled)` select them. Exact reads reject equal precedence,
continue only after `Absent`, and retain provider, content-hash, and source-version provenance.
Listings merge by relative path with higher-precedence providers shadowing lower-precedence ones.

Local filesystem and VCS access is restricted to roots in `SourceConfig`. The daemon accepts
`--source-root`, `--source-index`, and `--rust-analyzer`; it also recognizes `MICA_SOURCE_ROOTS`,
`MICA_SOURCE_ROOT`, `MICA_SOURCE_INDEX`, and `MICA_RUST_ANALYZER` as convenience defaults.

## Licence

Mica is licensed under the GNU Affero General Public License v3.0. See the repository root
`LICENSE`.
