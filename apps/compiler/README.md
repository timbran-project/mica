# Mica compiler

The lexer and parser are written in Mica. They are ported from omica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
Load `lex.mica` before `parse.mica`.

- `lex(source)` returns `[tokens, errors]`. Tokens record Unicode scalar offsets, lines, and columns.
- `parse_rows(source)` returns `{:root, :rows, :errors}`. Each AST row is `[node, role, target, ordinal]`.
- `parse(source)` returns `{:root, :nodes, :errors}`. Its nodes are a relation value with the same four columns.

The port gives mutable parser state a local binding because Rust Mica method parameters are immutable.
It renames the parser's `exactly` local and constructs AST relations with `relation_from_rows`.
The parser retains the donor's AST vocabulary. Parser acceptance alone does not establish executable language support.

The Rust runtime provides `assemble(description)` for validated register programs. Its format is documented in [Program Assembly](../../mdbook/src/language/assembly.md).
The Mica emitter and bootstrap execution remain under implementation.
