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
  --filein apps/native/types.mica \
  --filein apps/native/numeric.mica \
  --filein apps/native/layout.mica \
  --filein apps/native/storage.mica \
  --filein apps/native/contracts.mica \
  --filein apps/native/check.mica \
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
apps/native/value/string_append.mica
apps/native/value/string_search.mica
apps/native/value/heap.mica
apps/native/value/compare.mica
apps/native/value/maps.mica
```

`native_value/program()` returns its IR. `native/emit_module(native_value/program(), "value")` returns the C artifacts.
The platform boundary remains the allocation and release wrappers in `native/platform/allocation.c`.

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

This module is in progress. Relations, recursive hash/copy, symbol interning, display, codecs, and list operations beyond construction and comparison remain unimplemented.
Comparison supports the empty relation sentinel; other relation comparisons remain unimplemented and return `ok = false`.
Heap layouts are local to this implementation. Matching immediate tags does not make heap pointers interchangeable with Odin or Rust.

## Rust and generated C comparison

The [comparison harness](../../crates/testing/value-comparison/src/main.rs) constructs equivalent values independently in Rust and generated C.
It compares complete semantic results through a test protocol. It does not exchange heap pointers or use the persistence codec.
The harness covers implemented constructors, recursive comparison, mixed numeric comparison, arithmetic, map lookup, and map construction.
String checks cover malformed bytes, Unicode scalars, indexing, slicing, search, concatenation, and append chains.
Symbol tests use matching numeric IDs; they do not test symbol interning. Nonempty relations, hashing, and codecs remain outside its coverage.

Run fixed boundary cases and a seeded corpus with shrinking:

```sh
cargo run --release -p mica-value-comparison -- --seed 7 --cases 512
```

A mismatch exits with failure and prints a reduced JSON case. Pass that JSON to `--case` to replay it.
Add `--sanitize` for C address and undefined-behaviour checks, or set `CC=clang` to select Clang.
The test suite checks passing cases and verifies detection of a known floating-point remainder discrepancy:

```sh
cargo test -p mica-value-comparison
```

Rust computes float remainder with `%`. The generated implementation uses Odin's `a - trunc(a / b) * b` calculation.
These calculations differ for some finite inputs. Random arithmetic cases can expose this discrepancy; the harness does not suppress it.

Use the string corpus to exercise this module independently of arithmetic discrepancies:

```sh
cargo run --release -p mica-value-comparison -- --strings --cases 2048 --seed 17 --sanitize
```

`--strings` selects generated string and Unicode cases. Fixed cases and benchmark correctness checks still cover all implemented operations.

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
The harness calibrates repetitions toward the requested sample duration and alternates implementation order.
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
