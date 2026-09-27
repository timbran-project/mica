# Mica compiler

The lexer and parser are written in Mica. They are ported from omica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
Load `lex.mica`, `parse.mica`, and `emit.mica` in that order.

- `lex(source)` returns `[tokens, errors]`. Tokens record Unicode scalar offsets, lines, and columns.
- `parse_rows(source)` returns `{:root, :rows, :errors}`. Each AST row is `[node, role, target, ordinal]`.
- `parse(source)` returns `{:root, :nodes, :errors}`. Its nodes are a relation value with the same four columns.

The port gives mutable parser state a local binding because Rust Mica method parameters are immutable.
It renames the parser's `exactly` local and constructs AST relations with `relation_from_rows`.
The parser retains the donor's AST vocabulary. Parser acceptance alone does not establish executable language support.

The Rust runtime provides `assemble(description)` for validated register programs. Its format is documented in [Program Assembly](../../mdbook/src/language/assembly.md).
The emitter produces Rust artifacts directly. It does not translate Odin bytecode.

`emit_source(source)` returns `{:ok -> true, :entry -> bytes, :methods -> definitions, :errors -> []}`.
Each method definition contains a selector, an ordered parameter list, and program bytes.
A parse or emission diagnostic returns `{:ok -> false, :errors -> diagnostics}`.
`emit_program(rows, root)` returns assembly descriptions before serialization.

The current emitter covers literals, local bindings, list destructuring, collection construction, indexing, assignment, calls, conditionals, loops, and required verb parameters.
Calls include builtins, explicit selectors, named roles, and local function values.
`invoke` and `mailbox_recv` use runtime instructions, including spliced argument lists and arity checks.
Mailbox receive retains local values across suspension and resumes with the delivered value.
Receiver calls support positional arguments, named roles, splices, and dynamic selectors.
Static role lists preserve duplicate roles. Spliced role maps use ordinary map replacement semantics.
Spawn emission preserves receiver, selector, argument, and delay evaluation order, followed by the parent continuation.
The spawn tests compare task requests and parent results. They do not execute the child through the driver.
Forward calls and recursive verbs resolve through the runtime method catalogue.
DOM emission supports attributes, text, nested elements, interpolation, and child splices for the runtime's supported tags.
The lexer rejects bare `&` in DOM text. Interpolation supports text that contains ampersands.
Structural literals retain scalar, list, and map payloads, including singleton lists and splices.
List destructuring supports optional defaults and one rest binding. Range values support list slicing.
Exact row bindings require a singleton relation with the stated heading and raise `E_CARDINALITY` otherwise.
Row iteration requires an exact relation heading; maps in a collection can have additional keys.
Loops and comprehensions support list and row patterns, one or two names, typed names, and wildcards.
Comprehensions support lazy filters, value sorting, and keyed sorting. Sort keys run before body expressions; equal keys sort by body value.
The accumulator and captured iteration values survive suspension. Loop exits retain ordinary `finally` handling.
`raise` and `try` support error-code catches, error bindings, and `finally`. Error fields retain Rust's option-valued message and payload.
Local functions support nested closures, typed parameters, dependent optional defaults, and rest arguments.
Closures capture referenced outer names at creation time. A shadowed name can add an unused capture, but unrelated locals are excluded.
The parser accepts function result annotations and places parameter annotations before optional defaults.
Basic type annotations produce runtime checks. The emitter does not implement Rust's static type analysis.
Unsupported syntax produces diagnostics, including declarations, self-recursive local functions, catch patterns and guards, default verb parameters, and rest verb parameters.
The emitter reads `RelationName` once per compilation and resolves existing relations in that snapshot.
Compilation requires permission to read that catalogue. Relations used by the source must already exist.
Emitted scans support output variables, repeated variables, holes, and splices. Assertions and retractions use the runtime transaction.
The artifact retains relation identities from the compilation world and performs ordinary read/write checks when it executes.
Identity literals also resolve in the compilation world. Compilation does not install methods or grant authority.

The bootstrap test compiles all three compiler sources, installs their emitted artifacts, then compiles another program with the emitted compiler.
It compares the target artifacts byte for byte and executes the target in interpreter and native-enabled modes.
The test installer writes ordinary method-catalogue facts with root authority and assigns a fresh identity to each program version.
A public module-installation workflow and broader donor feature coverage remain pending.

For example, compile an expression without installing it:

```sh
cargo run --bin mica -- eval \
  --filein apps/compiler/lex.mica \
  --filein apps/compiler/parse.mica \
  --filein apps/compiler/emit.mica \
  'return emit_source("return 2 + 3")[:ok]'
```
