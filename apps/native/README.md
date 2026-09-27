# C generator

This Mica library constructs typed programs and emits C11 source. Rust Mica is the host.
The library has no dependency on the Mica compiler app or a runtime value layout.

The [value examples](examples/values.mica) implement three operations:

- Extract the high-byte tag from a value word.
- Add signed 56-bit integers, with explicit failure for invalid operands or overflow.
- Copy borrowed bytes into an arena and publish an immutable string view.

Generated functions also create and release the arena.
The [platform shim](../../native/platform/allocation.c) supplies only `malloc` and `free` wrappers.
The examples use a contiguous bump arena. They are executable construction examples, not replacements for the runtime's value implementation.

## Run the examples

From the repository root, run:

```sh
cargo run --bin mica -- eval \
  --filein apps/native/ir.mica \
  --filein apps/native/builders.mica \
  --filein apps/native/types.mica \
  --filein apps/native/numeric.mica \
  --filein apps/native/layout.mica \
  --filein apps/native/storage.mica \
  --filein apps/native/contracts.mica \
  --filein apps/native/check.mica \
  --filein apps/native/flow.mica \
  --filein apps/native/c_syntax.mica \
  --filein apps/native/c_scopes.mica \
  --filein apps/native/c_flow.mica \
  --filein apps/native/c.mica \
  --filein apps/native/examples/scalars.mica \
  --filein apps/native/examples/values.mica \
  'return native/emit_c(native/value_examples())'
```

The result is C source as a Mica string. The CLI prints the quoted string in its task report.
An embedding can load the same files with `SourceRunner::run_filein`, then evaluate the expression with `SourceRunner::run_source`.

Run the integration tests with a C11 compiler named `cc` on `PATH`, or set `CC` to the compiler executable:

```sh
cargo test -p mica-runtime --test native_codegen
```

The value tests require AddressSanitizer and UBSan. The surface test also uses POSIX threads and GCC/Clang overflow intrinsics as test oracles.
They compile and execute generated C from both Mica execution tiers, with leak detection enabled.
Tests cover integer boundaries, Unicode and empty strings, invalid lengths, overlapping input, capacity exhaustion, allocation failure, and arena release.
The surface tests also cover f32 representations, checked arithmetic, tagged heap headers, typed arrays, shared borrow regions, and thread-local storage.
They compile a generated module separately from its C consumer. Compiler overflow intrinsics provide independent integer arithmetic checks.
Separate tests reject invalid types, control flow, effects, and ownership contracts.
Fresh processes vary symbol creation order. Other tests reverse relation rows and change node IDs.
Generated source, binaries, and measurement output stay outside source control.

## Construction API

`native/program()` returns an empty construction state.
Node-producing `native/add_*` builders return `[node_id, updated_state]`.
Other row builders return the updated state.
Builders batch rows without changing the world.

| Builder | Arguments after `state` |
| --- | --- |
| `native/add_function` | name string, result type |
| `native/add_parameter` | function, position, name string, type |
| `native/add_local` | function, name string, type |
| `native/add_block` | function, name string, entry boolean |
| `native/add_constant` | type, encoding string |
| `native/add_record` | name string |
| `native/add_global` | name string, type, storage, initializer |
| `native/add_field` | record, position, name string, type |
| `native/add_instruction` | block, position, opcode, destination local, operand list |
| `native/terminate` | block, opcode, operand list |
| `native/foreign` | function |
| `native/contract` | function, allowed effects, result ownership, owner parameter name |
| `native/parameter_contract` | parameter, ownership mode |

`native/function_scope`, `native/block_body`, `native/record_type`, and `native/constants` batch common construction sequences.
The examples show their argument shapes. These helpers produce the same rows as the individual builders.

### Structured bodies

Load [builders.mica](builders.mica) after `ir.mica` to construct bodies without naming blocks or branch targets.
`native/function_body(state, function, bindings, statements)` returns the updated state.
Declare the function with `native/function_scope`, with an empty block list.
The bindings map contains parameter and local IDs, plus named constants and called functions.

For example, this body sums unsigned integers in `[0, limit)`:

```mica
let [function, names, declared] = native/function_scope(state, "sum", :U64,
  [["limit", :U64, :Value]], [], [])
names["zero"] = constants["zero"]
names["one"] = constants["one"]
state = native/function_body(declared, function, names, [
  native/let_statement("total", :U64, "zero"),
  native/let_statement("i", :U64, "zero"),
  native/while_statement(native/binary_expression(:Less, "i", "limit"), [
    native/set_statement("total", native/binary_expression(:Add, "total", "i")),
    native/set_statement("i", native/binary_expression(:Add, "i", "one"))
  ]),
  native/return_value("total")
])
```

Here, `constants` supplies U64 zero and one from `native/constants`.
The [value generators](value/) use these builders throughout, including UTF-8 decoding, string search, recursive comparison, and map sorting.
They declare control flow with branches, loops, and early returns; the shared builder creates the blocks and branch targets.
Mica kind annotations check generator interfaces, while explicit IR types describe the generated values.

| Statement | Behaviour |
| --- | --- |
| `native/let_statement(name, type, expression)` | Declare and initialize a function local |
| `native/let_field`, `native/let_load`, `native/let_offset`, `native/let_call` | Bind a field, load, pointer offset, or call result with an explicit type |
| `native/set_statement(name, expression)` | Assign an existing local |
| `native/do_statement(expression)` | Emit an operation with no result |
| `native/store_statement(pointer, value)`, `native/copy_bytes_statement(destination, source, length)` | Write a value or copy bytes |
| `native/if_statement(condition, yes, no)` | Select one statement list |
| `native/when_statement(condition, body)`, `native/unless_statement(condition, body)` | Run a body when its condition is true or false, respectively |
| `native/while_statement(condition, body)` | Re-evaluate the condition before each iteration |
| `native/switch_statement(type, selector, cases, fallback)` | Evaluate an integer selector once; select a constant case or the fallback |
| `native/break_statement()`, `native/continue_statement()` | Exit or repeat the innermost loop |
| `native/return_value(expression)`, `native/return_void()` | Return a value or return from a Void function |
| `native/return_zero()`, `native/return_record(fields)` | Return a zero-initialized value or a record with the supplied fields |
| `native/note_statement(kind, text)` | Annotate the current block |

An expression is a binding name or a node from an expression constructor.
Use named constructors for bodies and expressions. The constructors own the positional list schema consumed by the shared lowering pass.

`native/unary_expression(op, operand)` and `native/binary_expression(op, left, right)` construct operations.
`native/field_expression`, `native/field_pointer_expression`, `native/load_expression`, and `native/offset_expression` construct field and memory access.
`native/call_expression(name, arguments)`, `native/record_expression(fields)`, and `native/zero_expression()` construct calls, records, and zero values.

Nested operands retain explicit types through `native/typed_expression(type, expression)`.
`native/typed_field`, `native/typed_load`, `native/typed_call`, `native/typed_record`, and `native/typed_zero` take the result type first.
For example, `native/typed_call(:U64, "next", [])` supplies a typed call result.
This switch evaluates `next` once:

```mica
let cases: list = [native/case_arm(name, [native/return_value(name)]) for name in ["one", "two", "three"]]
let body: list = [native/switch_statement(:U64, native/call_expression("next", []), cases, [native/return_value("zero")])]
```

Case names resolve to constants of the selector type. Duplicate cases are rejected. Cases do not fall through.
`Break` and `Continue` still refer to the enclosing loop, including inside a switch.
`native/module_context(state, constants)` creates a module construction context.
`native/declare_function(context, name, result, parameters, locals, effects, mode, owner)` registers a function and its contract.
`native/define_function(context, name, statements)` resolves constants, functions, and parameters without manual binding-map assembly.
Declare mutually recursive functions before defining their bodies. The value generators use this shared API throughout.
Operands evaluate once, from left to right. Each operation lowers to a separate instruction before C emission.
Field names and the type operands of `SizeOf` and `AlignOf` retain their existing metadata forms.

Local names are unique throughout the function. Branches do not introduce separate name scopes.
The `body_` prefix is reserved for generated locals and blocks.
Builders reject statements after a return or loop transfer, loop control outside loops, and bodies that can fall through.
The existing checker still validates types, definite assignment, effects, and ownership after lowering.

### Information retained in generated C

`native/annotate(state, node, kind, text)` attaches a `:Source`, `:Purpose`, or `:Invariant` note to an existing node.
`native/relations(state)[:Annotation]` exposes these notes for inspection.
Notes survive lowering and appear beside the corresponding declaration, block, or instruction.
Constants are inlined, so their notes appear together in the declarations section.
Annotation text is escaped before C emission, including comment delimiters, line breaks, and trigraph characters.

Function declarations and definitions include allowed effects, result ownership, and parameter borrow regions as comments.
Nominal scalar names remain typedefs. Tagged types retain comments describing their tag bits, heap tags, and pointer access.
Structured blocks retain their control-flow roles, such as loop condition and exit.
Output ordering uses semantic names and annotation text, independent of node IDs or symbol insertion order.

### Declaration order and control flow

Standalone C places callees before callers. Prototypes remain for foreign functions and backward calls within recursive cycles.
Self-recursion needs no separate prototype. Module headers retain public function declarations for callers in other translation units.
Record and array definitions follow their layout dependencies; pointer fields can refer to incomplete struct tags.

The emitter analyzes the checked control-flow graph, including bodies built directly from blocks.
Dominators identify natural loops; postdominators identify shared branch continuations.
The resulting C syntax tree uses `if`, `else`, `while`, `break`, `continue`, and early returns.
When both branches exit, the emitter puts the smaller branch first and moves the larger branch after the guard.
Branch size counts executable syntax, including nested blocks. Comments do not affect this choice.
Integer equality chains become switches when intermediate blocks contain no effects or outside entries.
Pure loop comparisons become `while (condition)`; headers with computations remain inside `while (true)`.
Calls retain their original execution order and count. Only direct returns and zero-result exits may be duplicated.
Shared tails and irreducible cycles retain targeted gotos. Labels appear only when referenced.

After control-flow restructuring, `c_scopes.mica` places each local in the smallest scope that contains all its reads and writes.
Declarations precede the first use in that scope. Simple assignments become initialized declarations, and switch arms have separate braces.
Locals whose addresses are taken retain function lifetime because aliases can outlive their last direct use.
Transfers into nested blocks can also require wider scopes to preserve values across block exits and re-entry.
Locals removed by comparison folding need no declaration.

Tagged-word operations use inline helpers for the high 8-bit tag and the low 56-bit payload.
The helpers evaluate each argument once. Heap-tag, pointer-alignment, null-pointer, and payload-range checks remain at their operation sites.

The constructors in `c_syntax.mica` own the syntax-map schema and rendering rules.
Control-flow analysis builds nodes through these helpers instead of assembling C braces and indentation itself.

Invariant notes describe author intent. They are not proofs and do not remove checks or introduce optimizer assumptions.
Comments do not affect C optimization. Existing types and operations continue to determine executable semantics.

Each function has one entry block. Each block has one terminator.
Positions start at zero and have no gaps.
Parameters are immutable. Every incoming path must assign a local before a read.
An instruction with no result uses `:Discard` as its destination.
Terminators are `:Return [value]`, `:Return []` for Void, `:Jump [block]`, and `:Branch [condition, true_block, false_block]`.

`native/relations(state)` exposes the construction rows as named relation values.
Their headings describe the schemas. Duplicate keys remain visible to validation before relation construction removes duplicate rows.
Node IDs identify references within one build. Stable names and explicit positions determine emitted C order.
Generated functions, fields, locals, and labels use `mica_`, `f_`, `v_`, and `b_` prefixes.
Foreign functions use `mica_foreign_`.

## Types and operations

Scalar types are `:U8`, `:U16`, `:U32`, `:U64`, `:I64`, `:F32`, and `:Bool`.
`:Void` is a function result type.

| Compound type | Representation |
| --- | --- |
| `[:Record, name]` | Nominal record with ordered fields |
| `[:Pointer, pointee, access]` | Typed pointer, including nested pointers. Access is `:Mutable` or `:Const` |
| `[:Array, element, count]` | Fixed array with value semantics, emitted as a struct containing `elements[count]` |
| `[:Scalar, name, representation]` | Distinct scalar type with explicit wrapping and unwrapping |
| `[:Tagged, name, access, heap_tags]` | Nominal U64 word with an eight-bit tag and 56-bit payload |

Heap tags are increasing, unique byte values. Zero remains an immediate tag, so zero initialization is valid.
The tagged type's access controls unpacked pointers. Mutable access permits updates to heap metadata, such as append ownership and string indexes.
The checker rejects empty records, conflicting type names, and by-value cycles through records or arrays. Pointer cycles are valid.

Integer constants use exactly 16 lowercase hexadecimal digits without `0x`.
I64 constants permit a minus sign before the magnitude. The checker enforces each type's range.
F32 constants use eight hexadecimal digits that specify binary32 bits, including signed zero, infinities, and NaNs.
Bool constants are `"true"` or `"false"`. Pointer constants are `"null"`.

| Instruction | Meaning |
| --- | --- |
| `:Copy`, `:Zero` | Copy a value of the same type, or initialize a result to zero |
| `:Add`, `:Subtract`, `:Multiply` | Unsigned arithmetic modulo the result width, or F32 arithmetic |
| `:Divide`, `:Remainder` | Unsigned division or remainder. Divide also accepts F32 |
| `:BitAnd`, `:BitOr`, `:BitXor` | Unsigned bit operations |
| `:ShiftLeft`, `:ShiftRight` | Unsigned shift, including runtime counts |
| `:Equal`, `:Less` | Scalar equality or numeric comparison, producing Bool |
| `:Convert` | Integer narrowing/widening, signed-to-unsigned conversion, or integer-to-F32 conversion |
| `:Bitcast` | Copy representation between U64/I64 or U32/F32 with `memcpy` |
| `:Negate`, `:Truncate`, `:IsFinite` | F32 negation, truncation, or finite-value test |
| `:CheckedAdd`, `:CheckedSubtract`, `:CheckedMultiply` | Checked I64 arithmetic |
| `:CheckedDivide`, `:CheckedRemainder`, `:CheckedNegate` | Checked I64 division, remainder, or negation |
| `:CheckedFloatToInt` | Checked F32-to-I64 conversion, truncating toward zero |
| `:Wrap`, `:Unwrap` | Convert between a nominal scalar and its declared representation |
| `:Record`, `:Field` | Construct a complete record or read one field |
| `:Address`, `:FieldPointer` | Address an initialized variable or a field through a pointer |
| `:Load`, `:Store` | Read or write through a typed pointer |
| `:Adopt` | Store an owned allocation in an owner slot and return a pointer borrowed from that owner |
| `:Offset` | Offset a typed pointer by a U64 element count |
| `:ElementPointer` | Address a fixed-array element, preserving pointer access |
| `:SizeOf`, `:AlignOf` | Query a type's C size or alignment as U64 |
| `:PointerCast` | Convert between byte storage and typed pointers, with an alignment check |
| `:Freeze` | Convert a pointer to const access |
| `:CopyBytes` | Copy a byte span with overlap support. Zero length performs no access |
| `:PackPointer`, `:UnpackPointer` | Pack or unpack a heap pointer, preserving its ownership origin |
| `:PackImmediate`, `:Payload`, `:Tag` | Construct an immediate word, extract an immediate payload, or extract any tag |
| `:GlobalAddress` | Address module state, thread-local state, or constant data |
| `:Call` | Call a declared function, with its ID before the argument values |

Checked operations return a record with fields `ok: Bool` and `number: I64`, in that order.
Failure returns `{false, 0}` without evaluating an invalid C arithmetic expression.
Checked division truncates toward zero. Checked remainder accepts `INT64_MIN % -1` and returns zero.
The Value implementation must impose its 56-bit range and exact-division rules.
It must also reject non-finite float results and normalize negative zero where required.

Unsigned division by zero, invalid runtime shift counts, and out-of-bounds fixed-array indexes abort.
Pointer packing checks the heap tag, non-null address, and 56-bit fit. Unpacking checks the heap tag, non-null payload, and alignment.
Immediate packing rejects heap tags. Payload extraction rejects heap words, so it cannot erase pointer ownership through a numeric conversion.
These checks enforce primitive preconditions. They do not replace language-level error paths.

The target has eight-bit bytes, binary32 floats, and 64-bit pointers, `size_t`, and `ptrdiff_t`.
C floating-point operations assume the default rounding environment. Fast-math transformations and floating-point contraction must remain disabled.
There are no raw C fragments or implicit pointer conversions.

## Module state and C artifacts

`native/add_global` accepts storage `:Mutable`, `:Constant`, or `:ThreadLocal`.
The initializer is `:Zero`, a non-F32 constant ID, or a list of bytes for a U8 array.
Mutable globals require external synchronization when shared between threads.
OS synchronization remains available through foreign functions and their effect contracts.

`native/emit_c(state)` returns standalone C source.
`native/emit_module(state, name)` returns a map with these entries:

- `:header_name` and `:header`: guarded declarations, types, and target assertions.
- `:source_name` and `:source`: definitions that include the generated header.
- `:symbols`: ordered pairs of IR function names and C symbol names.
- `:c_flags`: C11, disabled floating-point contraction, and disabled fast math.
- `:link_flags`: the math library.

The caller writes the artifacts and invokes the C compiler. Emission has no filesystem effects.
A generated module and its consumers can compile as separate translation units.

## Effects and ownership

`native/emit_c(state)` always validates before returning source. Invalid programs raise `E_INVARG`.
Instruction type errors include the function, block, and position.
Effect errors include a witness call path.

Contracts permit a subset of `:ReadMemory`, `:WriteMemory`, `:Allocate`, `:Release`, and `:Block`.
The checker derives effects from memory operations and actual calls, including recursive calls.
Foreign effects are trusted declarations. Missing foreign contracts and unknown effects are errors.
An omitted contract on a generated scalar function permits no effects.

Every pointer-bearing parameter declares `:Borrow` or `:Consume`.
Pointer-bearing results declare `:Owned`, `:Borrow`, or `:Static`; tagged results can also declare `:Immediate`.
Borrowed results name their owner parameter.
Static results refer to module or thread-local storage. Thread-local pointers remain valid only during their originating thread’s lifetime.
Scalar results use `:Value` with an empty owner name.
`:Immediate` results contain tagged words but no heap references or native pointers, including inside records and arrays.
Every return path must have no pointer origins. Calls preserve this guarantee; foreign implementations must honour it.
This lets numeric constructors supply container elements without inventing an arena dependency.
Tagged values and aggregates containing them also require ownership contracts.
The checker propagates possible pointer origins through records, arrays, tagged values, and calls.
`Adopt` requires an owned or consumed allocation, a mutable slot of the matching pointer type, and the `:WriteMemory` effect.
It writes the allocation into that slot. The result inherits the slot's owner instead of retaining independent ownership.
A null allocation aborts before the slot changes. The owner must release its adopted allocations.
The checker does not prove that other aliases stop using a consumed allocation.

Address-taking introduces a stack origin. Stack addresses cannot escape through returns, stores, or consuming calls.

A parameter mode `[:Borrow, "arena"]` binds its lifetime to a root `:Borrow` parameter named `arena`.
This permits containers and values to share an arena lifetime without treating their parameter names as unrelated owners.
Generated callers must supply matching origins. Foreign callers must honour the same lifetime contract.
The checker rejects consumption of borrowed storage, invalid result origins, and stores that move borrows into unrelated storage.

These checks do not prove bounds, exclusive ownership, alias validity, or liveness after release.
They do not prevent every double release or use after release.
Callers must supply valid, suitably aligned pointers and truthful readable extents.
Foreign implementations must obey their declared contracts.

## Example contracts

`mica_checked_add` returns `IntResult { ok, number }`. Failure returns `ok = false` and zero.
Both operands and a successful result must fit −2⁵⁵ through 2⁵⁵−1.

`mica_arena_create(capacity)` returns an owned arena or a canonical empty failure result.
Zero capacity succeeds without allocation. `mica_arena_release(&arena)` frees storage and clears the arena.
Every string borrowed from that arena becomes invalid at release.

`mica_string_copy(&arena, source, source_length, length)` returns a status and a const byte view.
Status 0 means success, 1 means invalid length or null nonempty input, and 2 means insufficient or unusable arena storage.
Failure leaves the arena cursor unchanged and returns a null, zero-length view.
Successful copying finishes before cursor advancement and publication.
Length counts bytes. The operation preserves UTF-8 and embedded zero bytes; it does not validate UTF-8.

## Value implementation

The [value module](value/program.mica) builds on this generator. Load its files after the generator files:

```text
apps/native/value/program.mica
apps/native/value/immediates.mica
apps/native/value/numbers.mica
apps/native/value/arena.mica
apps/native/value/utf8.mica
apps/native/value/strings.mica
apps/native/value/symbols.mica
apps/native/value/string_append.mica
apps/native/value/string_search.mica
apps/native/value/heap.mica
apps/native/value/compare.mica
apps/native/value/maps.mica
apps/native/value/collections.mica
apps/native/value/relations.mica
apps/native/value/hash.mica
apps/native/value/copy.mica
apps/native/value/buffer.mica
apps/native/value/codec.mica
apps/native/value/codec_decode.mica
apps/native/value/persistence.mica
```

`native_value/program()` returns its IR. `native/emit_module(native_value/program(), "value")` returns the C artifacts.
The platform boundary uses [allocation wrappers](../../native/platform/allocation.c) and [POSIX mutex wrappers](../../native/platform/mutex.c).
Link the value module with both shims and `-pthread`.

Implemented behaviour includes:

- All immediate constructors and accessors, signed 56-bit bounds, finite binary32 values, and canonical positive zero.
- Checked numeric operations, exact integer division, float remainder, and explicit numeric conversions.
- A growing arena with stable addresses, eight-byte alignment, and release of all chunks.
- UTF-8 validation, scalar encoding and decoding, indexed strings, slicing, append, concatenation, and substring search.
- Copying byte and list constructors, plus range, error, and frob constructors and checked accessors.
- Recursive canonical comparison for these values and maps, with separate exact integer/float comparison for language expressions.
- Map construction with stable key sorting, the last duplicate value retained, and binary-search lookup.

Initialize `struct mica_ValueArena` to zero before use. Release it with `mica_value_arena_release` after its last borrowed value becomes unused.
Allocation failure returns a null pointer or `{false, 0}`. Failed arena growth preserves the existing allocation list and cursor.
Child values stored in a container must share the destination arena's lifetime. Strings and bytes copy their input storage.
Error messages are string values. Their presence and the optional error payload have separate flags.

`mica_value_compare` and `mica_value_language_compare` return `IntResult { ok, number }`, where `number` is −1, 0, or 1.
Canonical comparison distinguishes integer and float keys. Language comparison compares their numeric values without rounding integers to binary32.
Comparison ignores absent range ends, error messages, and error payloads.
`mica_value_equal` and `mica_value_language_equal` return the corresponding equality result as `BoolResult { ok, value }`.

`mica_value_map` copies its input and uses a stable merge sort before duplicate compaction.
Construction takes O(n log n) comparisons and O(n) arena storage. Lookup takes O(log n) comparisons.
`mica_value_map_get` returns `{false, 0}` for a missing key or a non-map input.
Failed construction can retain temporary storage until arena release. It never publishes a partial map or changes the input.

Lists support `value_list_length`, `value_list_get`, `value_list_slice`, `value_list_append`, and `value_list_set`.
Indices start at zero. Slices exclude the end position and permit empty ranges, including the end of a list.
Invalid types, reversed ranges, and out-of-range indices return `ok = false`.
Length returns `IdResult`. The other operations return `ValueResult`.

List slices allocate a header and share the backing array. Replacement copies the array before it writes the element.
Append claims spare capacity only at the backing's current tail. Otherwise, it copies the visible prefix into another backing with geometric growth.
Every earlier view retains its elements and length. Nested lists can share a backing because arena release does not traverse their elements.

Serialize arena mutation, including list append. All borrowed child values and shared backings must outlive the result's use.
The ownership contracts tie these borrows to the destination arena. The caller must enforce the lifetime when it calls generated C directly.
Cross-arena operations can borrow from a longer-lived source. They do not copy nested values into the destination arena.
Allocation failure leaves published views unchanged. Internal allocation helpers must not publish a header before its elements are initialized.

Maps support `value_map_length` and `value_map_set` in addition to construction and lookup.
Length returns `IdResult`. Set returns a `ValueResult` containing a map with the supplied key and value.
Set searches for the key, copies the sorted entries, and inserts or replaces one entry without another sort.
Existing maps remain unchanged. Keys use canonical comparison, including nested values and distinct integer and float keys.

Strings contain valid UTF-8. Byte values can contain arbitrary bytes. Neither type requires a terminating zero byte.
`mica_unicode_scalar` rejects surrogates and numbers greater than U+10FFFF.
`mica_utf8_decode` returns one scalar and its byte width. Malformed or incomplete input returns `ok = false` and width zero.
`mica_utf8_encode` returns up to four bytes. `mica_utf8_scan` validates the entire input and returns its scalar count and ASCII flag.
Validation accepts Unicode noncharacters and embedded zero bytes. It rejects overlong encodings, surrogates, isolated continuation bytes, and out-of-range scalars.
The scanner checks ASCII in bounded eight-byte groups. Unaligned input is valid, and no read extends beyond its supplied length.

String positions count Unicode scalars, not grapheme clusters. The APIs use these conventions:

| Function after `mica_` | Result |
| --- | --- |
| `value_string_length(value)` | Scalar count as `IdResult { ok, number }` |
| `value_string_byte_offset(value, position)` | Byte offset, including the end position |
| `value_string_scalar_at(value, position)` | `RuneResult { ok, rune }`; the end position fails |
| `value_string_slice(arena, value, start, end)` | Shared view of the end-exclusive scalar range |
| `value_string_append(arena, value, bytes, length)` | String with validated suffix bytes |
| `value_string_concat(arena, left, right)` | String with the right string appended |
| `value_string_find(value, needle, start)` | First matching scalar position at or after `start` |

Invalid types, reversed ranges, and out-of-range positions return `ok = false`.
Search also returns `ok = false` for an absent match. An empty needle matches any valid position, including the end.
Search uses byte comparisons, with a stack skip table for needles of at least four bytes.
The worst-case search cost is O(haystack bytes × needle bytes). Converting a non-ASCII match to a scalar position scans its prefix.

Each string view stores its byte length, scalar count, and ASCII flag.
Backings with at least 128 bytes of capacity reserve one byte-offset sample per 32 scalar positions.
Samples use eight bytes each. Capacity reserves enough samples for an eventual ASCII suffix.
ASCII indexing uses direct offsets. Non-ASCII indexing starts at a sample and decodes at most 31 preceding scalars.
Slices share the backing bytes and samples. Small backings without samples use a bounded scan.

Append allocates a separate view header. It reuses spare capacity only when the input view ends at the backing's current tail.
Other appends allocate another backing with geometric growth. Earlier views retain their bytes and lengths.
Overlapping suffix bytes are valid. Invalid UTF-8 leaves the input unchanged.
Serialize mutation of an arena and its string backings, including append. This module does not provide concurrent arena mutation.
All views become invalid when their owning arena is released. Allocation errors can retain temporary storage until release.
The allocation and index-building helpers are internal construction steps; callers must not publish incomplete headers.

Finite relations contain a symbol heading and a canonical set of tuples.
`value_relation(arena, heading, arity, rows, length)` copies the heading, tuple descriptors, and cell arrays into the arena.
Child values remain borrowed under the arena lifetime contract.
The constructor sorts columns by symbol ID and permutes every row to match.
It sorts rows lexicographically and removes duplicates. Duplicate column names and rows with the wrong arity fail.
Headings can contain at most 65,535 columns. Allocation failure returns `{false, 0}` without changes to the input.

`ValueTuple` contains `data` and `arity` fields. `value_tuple` copies a supplied cell array and returns `TupleResult { ok, tuple }`.
`value_tuple_get` returns a checked cell. `value_tuple_compare` compares cells lexicographically, then compares the arities.
A tuple has no standalone value tag or heading.

| Function after `mica_` | Result |
| --- | --- |
| `value_relation_view(value)` | `RelationResult { ok, relation }`, with the header returned by value |
| `value_relation_arity(value)` | Column count as `IdResult` |
| `value_relation_length(value)` | Unique row count as `IdResult` |
| `value_relation_column(value, symbol)` | Canonical column position as `IdResult` |
| `value_relation_column_at(value, index)` | Column symbol ID as `IdResult` |
| `value_relation_row(value, index)` | Borrowed `TupleResult` |
| `value_is_unit(value)` | True for exactly one zero-column row |

The zero-column relation with no rows uses the immediate empty sentinel.
The zero-column relation with at least one input row canonicalizes to unit: exactly one empty tuple.
An empty relation with a nonempty heading retains its heading and has a heap representation.
The relation view handles the immediate sentinel without a heap pointer.
Invalid kinds, missing columns, and out-of-range indices return `ok = false`.

Relation comparison orders the canonical headings first, then the canonical tuple sequences.
Nested relations participate in list, map, range, error, and frob comparison.
Column and row sorts use stable merge passes over descriptors. They skip merge storage when the descriptors are already ordered.
Construction costs O(columns log columns + rows log rows × tuple comparison), plus the cost of copying cells.
Temporary sort storage remains arena-owned until release. Published views remain immutable.

The [symbol table](value/symbols.mica) interns UTF-8 names into stable 32-bit IDs.
Start with a zero-initialized `struct mica_SymbolTable`, then call `value_symbol_table_init` before sharing it.
Its API uses these generated functions:

| Function after `mica_` | Result |
| --- | --- |
| `value_symbol_table_init(table)` | Boolean success; allocates and initializes the table mutex |
| `value_symbol_intern(table, bytes, length)` | `IdResult { ok, number }`; successful IDs fit `uint32_t` |
| `value_symbol_text(table, id)` | `SymbolText { ok, data, length, scalars, ascii }` |
| `value_symbol_table_release(table)` | Releases storage and resets the table |

Names are case-sensitive byte sequences. Empty names, embedded NUL bytes, and all Unicode scalars are valid.
Interning rejects malformed UTF-8, null pointers with nonzero lengths, allocation failure, and exhausted IDs with `ok = false`.
Reverse lookup returns `ok = false` for unknown IDs. Names are not NUL-terminated.
Use `value_symbol((uint32_t)id.number)` to construct a symbol value, or `value_error_code` for an error code.
IDs are local to one table and start at zero. Values from different tables must not share a symbol namespace.

The table copies new names and caches their byte length, scalar count, and ASCII flag.
Repeated interning does not allocate. Text addresses and IDs stay valid through table growth.
FNV-1a hashes select buckets with linear probing. Matching hashes still require equal bytes.
At most half the buckets contain entries. Reverse lookup indexes entries directly.
The arena retains earlier table arrays until release. Geometric growth keeps their total size below twice the current array size.
Allocation failure preserves published names and IDs, but can retain unused capacity until release.

Interning and reverse lookup acquire the table's mutex internally. Concurrent calls share one namespace and cannot publish duplicate IDs for equal names.
Returned text is immutable and stays valid after unlock, including during concurrent insertion and growth.
The `symbol_*_locked` helpers require the caller to hold that mutex. Application code uses the `value_symbol_*` functions.

Initialization and release require exclusive ownership. Do not copy an initialized table.
Finish all calls and borrowed text reads before release.
Initialization returns false on allocation or mutex initialization failure, or when the table is already initialized.
Interning and lookup fail on an uninitialized table. Failed initialization permits retry.
Release destroys the mutex, frees storage, and resets the table. Reinitialize the reset table before reuse.
After release, all borrowed text and table-local IDs are invalid. Reinitialization starts IDs at zero.

The native tests run eight threads through shared-name interning, distinct-name insertion, reverse lookup, and growth.
To run these tests with ThreadSanitizer instead of address and undefined-behaviour sanitizers:

```sh
MICA_NATIVE_THREAD_SANITIZER=1 CC=clang cargo test -p mica-runtime --test native_codegen native_value_layer_executes_on_both_mica_tiers
```

`value_hash(value)` and `value_tuple_hash(tuple)` return `IdResult { ok, number }` with a 64-bit hash.
The hash follows Omica's canonical value algorithm. Equal values in one symbol namespace have equal hashes, regardless of their arena or backing storage.
Integer and float kinds remain distinct. Absent optional fields do not contribute their stored contents.
Hashes are neither cryptographic identifiers nor persistence encodings. Unknown tags return `ok = false`.

`value_copy(arena, value)` and `value_tuple_copy(arena, tuple)` recursively copy visible heap storage into the destination arena.
The result can outlive the source arena. Strings rebuild their scalar indexes, and lists receive independent append storage.
Maps and relations retain canonical order without another sort. Absent optional fields become zero instead of retaining source pointers.
Immediate IDs retain their original symbol, identity, capability, or function namespace.

Copy returns `ValueResult` or `TupleResult`. Allocation failure returns `ok = false` and leaves the source unchanged.
Partial allocations remain in the destination arena until release. The caller must serialize destination arena mutation.
Traversal uses native recursion. Copy duplicates shared subtrees and does not preserve source aliasing or spare capacity.

`value_is_persistable(value)` rejects capabilities and function handles, including nested children.
It ignores absent optional fields. This predicate does not check whether symbols have registered names.

`value_encode(arena, table, value, options)` returns a `ValueResult` containing owned bytes.
`ValueCodecOptions { symbol_ids, allow_capabilities }` defaults to persistence-safe options when zero-initialized: names, with capabilities disabled.
The format matches Rust's owned value codec: little-endian words, structural heap records, and UTF-8 symbol names.
Heap pointers are never encoded. Function handles are always rejected.
Set `symbol_ids` only when producer and consumer share the same symbol namespace.
Set `allow_capabilities` only for transient transfer within the same authority namespace; this does not make capabilities persistable.

`value_decode(arena, table, data, length, options)` returns `ValueDecodeResult { ok, value, consumed }`.
`value_decode_exact` returns `ValueResult` and rejects trailing bytes.
Decoding checks tags, flags, lengths, UTF-8, and relation headings before publishing a value.
Maps and relations use their ordinary constructors to normalize order and duplicates.
Names require an initialized symbol table. ID mode permits a null table.
Named decoding interns into the destination table, so resulting IDs can differ from the source IDs.

Encoded bytes and decoded heap storage belong to the destination arena. Neither borrows source storage or symbol-name bytes.
The caller must serialize destination arena mutation; symbol-table access uses its existing lock.
A failure returns `ok = false` without publishing a partial result. Partial arena allocations remain until release.
Names interned before a decode failure remain in the symbol table.
The codec uses native recursion, with no separate nesting limit. Callers must bound untrusted input depth before using it.

This module is in progress. Display remains unimplemented.
Heap layouts are local to this implementation. Matching immediate tags does not make heap pointers interchangeable with Odin or Rust.

## Rust and generated C comparison

The [comparison harness](../../crates/testing/value-comparison/src/main.rs) constructs equivalent values independently in Rust and generated C.
It compares complete semantic results through a test protocol. Codec cases additionally exchange Rust-compatible encoded bytes; heap pointers never cross implementations.
The harness covers implemented constructors, recursive comparison, mixed numeric comparison, arithmetic, map lookup, and map construction.
Collection checks cover nested values, indexing, slicing, append chains, replacement, and map updates.
Use `--collections` to select only collection property cases.
String checks cover malformed bytes, Unicode scalars, indexing, slicing, search, concatenation, and append chains.
Symbol sequences compare interning, deduplication, reverse lookup, and cached metadata against Rust `Symbol`.
The comparison normalizes Rust IDs by first occurrence because IDs belong to their table.
These checks read every name after the complete sequence, including growth, and include malformed UTF-8 and repeated names.
Use `--symbol-case` to replay the JSON byte arrays from a failed sequence.
Relation checks cover reordered columns, duplicate rows, nested values, empty relations, unit, accessors, and invalid headings or row widths.
Use `--relations` to select relation property cases.

`--codecs` selects encoding, decoding, malformed-input, and persistence cases.
ID-mode checks compare exact encoded bytes and decode Rust encodings in C.
Name-mode checks decode and re-encode with a different C symbol namespace, then compare values after Rust decoding.
Codec lifetime checks release source storage before inspecting results. Codec benchmarks use ID mode.

`--traversal` selects hash and copy cases across all value kinds, including nested relations.
Copy checks release the actual decoded source arena before they inspect the result under AddressSanitizer.
The Rust hash reference implements Omica's algorithm over Rust values. It does not use Rust's unspecified standard hash algorithm.
The Rust copy reference recursively reconstructs values. Copy benchmarks include this reconstruction and result destruction, rather than an `Arc` clone.

Run fixed boundary cases and a seeded corpus with shrinking:

```sh
cargo run --release -p mica-value-comparison -- --seed 7 --cases 512
```

A mismatch exits with failure and prints a reduced JSON case. Pass that JSON to `--case` to replay it.
Add `--sanitize` for C address and undefined-behaviour checks, or set `CC=clang` to select Clang.
The test suite includes float remainder regressions with subnormal divisors and large exponent differences:

```sh
cargo test -p mica-value-comparison
```

Generated float remainder uses `fmodf`, which matches Rust `%` for finite operands.
A zero divisor fails. The value constructor canonicalizes negative zero.

Use the string corpus to focus generated cases on strings and Unicode:

```sh
cargo run --release -p mica-value-comparison -- --strings --cases 2048 --seed 17 --sanitize
```

`--strings` selects generated string and Unicode cases. Symbol sequences and concurrent loads run independently with the same seed and case count.
Fixed cases and benchmark correctness checks still cover all implemented operations.

Run performance measurements for workloads that pass their own full result checks:

```sh
cargo run --release -p mica-value-comparison -- --cases 0 --bench --samples 7 --sample-ms 100
```

Here, `--cases 0` skips random cases. Fixed cases and every benchmark workload still undergo correctness checks before timing.
This command does not establish full conformance. With random cases enabled, a mismatch prevents benchmarking.

Measurements cover integer addition, mixed numeric comparison, nested comparison, map lookup, and map construction with reclamation.
String workloads cover ASCII and Unicode length, indexing, slicing, search, construction from bytes, and 64-part append chains.
String inputs contain 4 KiB of bytes. Index workloads reuse cached metadata; construction workloads include validation and metadata creation.
The Rust search reference uses standard string search and scalar-offset conversion; it is not a runtime substring-search builtin.
Symbol workloads run on one shared table with 1, 2, 4, and 8 worker threads by default.
Even the one-worker case creates a thread. This prevents glibc from using its single-thread mutex shortcut.
Use `--symbol-threads` to select worker counts and `--symbol-loads` to exclude unrelated value workloads.

| Symbol workload | Operations |
| --- | --- |
| `one` | Repeated interning of one existing name |
| `hot` | Existing names: 90% of requests select eight hot names, 10% select from 1,024 names |
| `uniform` | Existing names selected uniformly from 1,024 names |
| `text` | Text lookup through each implementation's public API |
| `metadata` | Cached metadata lookup through each implementation's public API |
| `unique` | 65,536 distinct names partitioned among workers |
| `shared` | Every worker interns the same 8,192 names in shuffled order |
| `mixed` | 20% new-name insertion, 20% existing-name interning, 40% text lookup, 20% metadata lookup |

The catalog mixes ASCII, Unicode, embedded NUL bytes, and different name lengths.
The seed fixes operation sequences. Thread scheduling and numeric ID assignment remain nondeterministic.
Correctness runs check every result during execution, then check deduplication, stable IDs, text, metadata, and table cardinality after workers finish.
Seeded stress cases also include malformed UTF-8 and concurrent insertion of an empty name.
Failures print replay inputs for `--symbol-load-case`. Replay preserves inputs, not thread scheduling.

Run concurrent correctness checks with address and undefined-behaviour sanitizers:

```sh
CC=clang cargo run --release -p mica-value-comparison -- --symbol-loads --sanitize --cases 128 --seed 17
```

Run the generated C workers under ThreadSanitizer:

```sh
CC=clang cargo run --release -p mica-value-comparison -- --symbol-loads --thread-sanitize --cases 128 --seed 17
```

Run paired throughput measurements:

```sh
CC=clang cargo run --release -p mica-value-comparison -- --symbol-loads --symbol-threads 1,2,4,8 --bench --cases 64 --seed 17
```

Give the process enough CPUs for the requested workers. Pin the parent and its children to the same CPU set for repeatable comparisons.
The JSON metadata records Linux CPU affinity. Pinning every worker to one CPU measures scheduling and contention, not multicore scaling.

Each sample uses a fresh process and table. Only names in the existing prefix undergo warmup on each worker before timing.
New-name workloads make one pass, without repetition calibration. Other symbol workloads calibrate Rust and C separately toward the requested duration.
Independent calibration keeps large contention differences from producing excessive sample times. Output records each implementation's operation count.
Start and finish barriers are timed. Thread creation, warmup, joins, complete result validation, and table cleanup are outside the timer.
Timed workers consume results and check ID stability across repetitions. A separate correctness pass checks every operation before performance sampling.
Every timed sample also checks complete final identities, text, and metadata after timing.

Symbol results include operations per second and aggregate nanoseconds per operation: wall time divided by total operations across all workers.
These measure throughput, not per-call latency. Rust uses its global read-write lock and thread-local cache; generated C uses a mutex per table.
Text and metadata workloads each make one public API call per operation. No claim about memory efficiency follows from these timings.
The harness calibrates other workloads toward the requested sample duration and alternates implementation order.
It prints JSON lines with compiler details, all samples, median nanoseconds per operation, and C/Rust ratios. Ratios below one favour C.
Samples include operation dispatch and result consumption, so very small operation timings also include harness costs.
These are workload measurements, not isolated instruction costs or whole-runtime benchmarks.

Generation, compilation, subprocess startup, input decoding, and input construction stay outside timed intervals.
Operation workloads reuse prepared values. Construction, slice, and append workloads include Rust result destruction and C arena release on each iteration.
C uses `-O3` without sanitizers; Rust requires a release build. Sanitizer mode cannot run benchmarks.
For stable comparisons, use the same idle machine and CPU affinity for the parent process and its children.

To inspect the generated standalone C source, request an explicit output file:

```sh
mkdir -p target/native
cargo run --release -p mica-value-comparison -- --cases 0 --emit-c target/native/value.c
```

`--emit-c` writes the source before compilation. The `target/` directory stays outside source control.
Other generated C and executables live in temporary directories that the harness removes on exit.
Results go to stdout. The harness does not create result files or property-test persistence files in the repository.
