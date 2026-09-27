# Mica compiler

The lexer and parser are written in Mica. They are ported from omica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
Load `lex.mica`, `parse.mica`, and `emit.mica` in that order.
Load `install.mica` afterward to install and run compiled modules.

- `lex(source)` returns `[tokens, errors]`. Tokens record Unicode scalar offsets, lines, and columns.
- `parse_rows(source)` returns `{:root, :rows, :errors}`. Each AST row is `[node, role, target, ordinal]`.
- `parse(source)` returns `{:root, :nodes, :errors}`. Its nodes are a relation value with the same four columns.

The port gives mutable parser state a local binding because Rust Mica method parameters are immutable.
It renames the parser's `exactly` local and constructs AST relations with `relation_from_rows`.
The parser retains the donor's AST vocabulary. Parser acceptance alone does not establish executable language support.

The Rust runtime provides `assemble(description)` for validated register programs. Its format is documented in [Program Assembly](../../mdbook/src/language/assembly.md).
The emitter produces Rust artifacts directly. It does not translate Odin bytecode.

`emit_source(source)` returns `{:ok -> true, :entry -> bytes, :methods -> definitions, :source -> source, :errors -> []}`.
Each method definition contains a selector, an ordered parameter list, and program bytes.
Each parameter records its `:role` and `:restriction`. Prototype restrictions use `value @ #prototype` syntax.
Overloads retain separate definitions and use ordinary runtime dispatch. Duplicate signatures produce a diagnostic.
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
Match expressions support literals, bindings, wildcards, nested list and map patterns, option and result patterns, and guards.
List patterns support one rest binding, including between fixed elements. Map patterns permit additional keys; relation patterns require an exact singleton heading.
The subject runs once. Guard side effects and suspended locals survive selection of the matching case.
An unmatched value raises `E_MATCH`; the emitter does not perform static exhaustiveness analysis.
Row iteration requires an exact relation heading; maps in a collection can have additional keys.
Loops and comprehensions support list and row patterns, one or two names, typed names, and wildcards.
Comprehensions support lazy filters, value sorting, and keyed sorting. Sort keys run before body expressions; equal keys sort by body value.
The accumulator and captured iteration values survive suspension. Loop exits retain ordinary `finally` handling.
`raise` and `try` support error-code catches, error bindings, and `finally`. Error fields retain Rust's option-valued message and payload.
Local functions support nested closures, typed parameters, dependent optional defaults, and rest arguments.
Named local functions can recurse and return their own callable value. Reassigning the outer name leaves existing aliases and recursive calls bound to the original function.
Closures capture referenced outer names at creation time. A shadowed name can add an unused capture, but unrelated locals are excluded.
The parser accepts function result annotations and places parameter annotations before optional defaults.
Basic type annotations produce runtime checks. The emitter does not implement Rust's static type analysis.
Unsupported syntax produces diagnostics, including declarations, catch patterns and guards, default verb parameters, and rest verb parameters.
The emitter reads `RelationName` once per compilation and resolves existing relations in that snapshot.
Functional field syntax also reads `Arity` and `FunctionalKey` once, then uses the cached metadata throughout compilation.
Fields require a binary relation functional on position zero. Reads require exactly one fact; assignments replace the fact within the transaction.
Compilation requires permission to read these catalogues. Relations used by the source must already exist.
Emitted scans support output variables, repeated variables, holes, and splices. Assertions and retractions use the runtime transaction.
The artifact retains relation identities from the compilation world and performs ordinary read/write checks when it executes.
Identity literals also resolve in the compilation world. Compilation does not install methods or grant authority.

`compiler/install(module, name)` installs a successful `emit_source` result. The symbol `name` identifies the module and selects its entry program.
Its result contains `:ok`, `:entry` (the selector), and `:methods` (the installed identities).
`compiler/run(source, name)` compiles, installs, and executes the entry, returning `{:ok -> true, :value -> result, :errors -> []}`.
Both return compilation diagnostics without installing a failed module.
Installation uses the caller's authority and transaction. It requires administrative identity creation and writes to the method catalogues.
The installer grants no authority. Unhandled entry failures roll back installation and entry effects in the same transaction.
An explicit commit or suspension in the entry retains its normal transaction semantics.
Reinstalling a module replaces its owned methods and removes their obsolete signatures. It preserves methods owned by other modules.
Method identities remain stable for unchanged signatures. Each program version receives a fresh identity; earlier program bytes remain available to suspended tasks.

The bootstrap test compiles all four compiler sources, installs their emitted artifacts, then compiles another program with the emitted compiler.
It compares the target artifacts byte for byte and executes the target in interpreter and native-enabled modes.
Tests use the application installer. Bootstrap tests retire the native compiler's method selectors after installing the emitted compiler.

Use administrative filein for compilation and installation. Ordinary endpoint evaluation needs explicit policy grants.
To compile an expression without installing its artifact:

```sh
cargo run --bin mica -- filein \
  apps/compiler/lex.mica apps/compiler/parse.mica apps/compiler/emit.mica \
  /dev/stdin <<'MICA'
return emit_source("return 2 + 3")[:ok]
MICA
```

To compile, install, and run an expression:

```sh
cargo run --bin mica -- filein \
  apps/compiler/lex.mica apps/compiler/parse.mica apps/compiler/emit.mica \
  apps/compiler/install.mica /dev/stdin <<'MICA'
return compiler/run("return 2 + 3", :example)[:value]
MICA
```
