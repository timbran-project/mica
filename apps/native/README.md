# Native code generator

This Mica library constructs typed programs for C11 and libgccjit. Rust Mica supplies the bootstrap host.
The core IR has no dependency on the Mica compiler app or a runtime value layout.
The [managed heap](memory/program.mica) and [value library](value/program.mica) use that IR to define their implementations.
The [relation kernel](kernel/program.mica) adds snapshots, transactional tuple storage, and concurrent commits.
Task scheduling, rules, dispatch, and general native source execution remain separate runtime work.

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
  --filein apps/native/generation.mica \
  --filein apps/native/sequences.mica \
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

## Transactional relation kernel

`native_kernel/program()` generates values, managed memory, the in-memory relation store, and query execution as one module.
All storage, indexing, conflict checks, publication, and tracing algorithms come from Mica source.
The platform bindings supply allocation and pthread primitives.

Each relation has persistent AVL indexes over immutable tuples.
An update copies the search paths and shares unchanged subtrees.
The catalogue is a sorted linked list. Catalogue updates copy the prefix before the changed entry.
Snapshots retain catalogue roots. They have no parent links that retain obsolete versions.

Transactions read a fixed snapshot and their own staged writes.
Declarations and fact changes publish atomically.
Set, functional-key, and event-append policies use the existing kernel conflict rules.
A functional replacement requires an explicit retraction before the assertion.
A retraction of a tuple absent from the base snapshot does not erase a concurrent insertion.
Failed commits leave the published snapshot unchanged and retain the transaction for inspection.
After an allocation failure, the caller can retry the same transaction.
After a conflict, the caller must begin a fresh transaction and repeat its work.

Commit preparation runs outside the publication mutex against a retained snapshot.
It validates the transaction, builds persistent index paths, and calculates net deltas.
Publication copies the candidate graph into mature storage and reuses immutable published subgraphs.
The heap mutex protects this copy and its allocation metadata. Other workers can continue execution.
Allocation failure preserves the source graph and the previous published root.

The kernel mutex protects snapshot capture and the final compare-and-install operation.
Workers leave the active heap set before they wait for this mutex.
If another writer publishes first, the transaction revalidates and rebuilds against that snapshot outside the mutex.
Disjoint writes within one relation preserve both commits. Existing conflict policies still apply.
After 64 lost publication attempts, the commit returns a conflict status.
Every managed pointer used after a possible collection comes from the registered transaction root.

Major collection runs on explicit requests or when managed page allocation reaches the heap growth limit, independently of successful commits.
Nursery overflow uses managed pages and does not independently request collection.
Mature allocation requests collection after page storage reaches twice its size after the last collection, with a 1 MiB minimum.
Major collection still stops active workers and scans the full live heap.
Publication copying does not reclaim storage or reset nurseries.

### Kernel interface

The generated C names have the `mica_` prefix. Controls must start as zero-initialized structs.
A `Kernel` and its `MemoryHeap` must outlive all associated workers and transactions.
The heap uses `kernel_heap_init`, which installs both kernel and value descriptors.

| Function | Contract |
| --- | --- |
| `kernel_init(kernel, worker)` | Create the empty published snapshot on an active worker |
| `kernel_begin(kernel, worker, transaction)` | Retain the current snapshot and register transaction roots |
| `kernel_declare(transaction, id, name, arity, policy, keys, indexes)` | Stage a relation declaration with caller-assigned identity and symbol IDs |
| `kernel_write(transaction, id, tuple, asserted)` | Stage an assertion or retraction of a list-valued tuple |
| `kernel_scan(transaction, id, pattern, mask, after, output, capacity)` | Read a bounded batch into a caller-owned array of value words |
| `kernel_commit(transaction)` | Validate, publish, and retain net fact deltas on success |
| `kernel_version(transaction)` | Read the base version, or the published version after commit |
| `kernel_deltas(transaction)` | Read committed additions and removals until the transaction ends |
| `kernel_end(transaction)` | Release transaction roots and discard any uncommitted changes |
| `kernel_release(kernel, worker)` | Remove the shared snapshot root under exclusive ownership of the kernel control |

The policy codes are 0 for set, 1 for functional keys, and 2 for event append.
Arity ranges from 0 through 64. Masks use one bit per column.
`keys` selects the functional key columns in column order.
`indexes` selects additional single-column indexes. The natural tuple index and functional key index exist automatically.

A scan pattern is a list with the declared arity. Only columns selected by `mask` constrain the scan.
The result contains `status`, `count`, `visited`, and `more`.
The first batch uses a zero value word for `after`.
Subsequent batches use the final tuple from the previous batch, with the same pattern and mask.
Index choice determines result order. The continuation follows that order.
Tuples, continuations, and delta pointers remain valid until the next safepoint unless the caller registers roots for them.

Operations require an active worker. A transaction belongs to one worker and has no concurrent mutation support.
Nested transactions on one worker end in reverse creation order, as required by the root stack.
Other workers can read and commit concurrently. Callers park workers before external blocking operations.

| Status | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Unknown relation |
| 2 | Tuple or pattern arity mismatch |
| 3 | Non-persistable tuple |
| 4 | Functional key violation |
| 5 | Commit conflict |
| 6 | Duplicate relation name |
| 7 | Invalid schema, identity, mask, or output buffer |
| 8 | Allocation or collection failure |
| 9 | Closed transaction or invalid lifecycle operation |

`kernel_work(transaction)` exposes the last commit status and its conflicting relation and tuple.
The current layer supports relation creation. Relation removal, transactional buffers, durable storage, rules, computed relations, authority, and dispatch remain separate work.

### Query execution

The Mica generators in `apps/native/query/` produce immutable query plans and a native executor.
Plans compose at runtime. Scan leaves read the transaction snapshot and its staged writes.

| Plan constructor | Operation |
| --- | --- |
| `kernel_query_scan(worker, relation, pattern, mask)` | Read a stored relation with bound columns |
| `kernel_query_input(worker, value)` | Read a first-class relation value in canonical heading order |
| `kernel_query_project(worker, input, positions)` | Select, reorder, or repeat columns; eliminate duplicate rows |
| `kernel_query_join(worker, left, right, left_positions, right_positions)` | Match exact keys and concatenate both rows |
| `kernel_query_semi(worker, left, right, left_positions, right_positions)` | Keep left rows with a matching right key |
| `kernel_query_anti(worker, left, right, left_positions, right_positions)` | Keep left rows without a matching right key |
| `kernel_query_union(worker, left, right)` | Combine rows with the same arity and eliminate duplicates |
| `kernel_query_difference(worker, left, right)` | Remove matching right rows from the left input |

Constructors return a managed `KernelQuery` pointer, or null on allocation failure.
Positions are Mica lists of zero-based integers. Join position lists must have equal lengths.
Empty join keys produce a Cartesian product. Projection onto no columns produces at most one empty row.
Equality uses canonical value equality, including distinct integer and float keys.

`kernel_query_execute(transaction, plan)` returns `KernelQueryResult` with `status`, `arity`, and `rows`.
Successful rows form a sorted, duplicate-free Mica list of row lists.
Errors use kernel status codes and return no partial result.
Invalid columns, missing operands, and excessive plan depth return status 7.
Incompatible arities return status 2. Plans permit at most 64 levels and results permit at most 64 columns.
Query errors leave the transaction unchanged.

Projection over a stored relation consumes its scan directly.
Joins, semi-joins, and anti-joins against a stored relation probe its existing indexes.
Other equality joins build a temporary index on the right projected key.
Remaining operator boundaries materialize intermediate results. Join reordering and shared-subplan caching remain future work.

Scans and projections traverse the selected tree once, polling every 64 visited rows, including rows rejected by residual bindings.
Join probes share a polling counter. Each probe seek is bounded by the AVL tree height.
Other row loops and result assembly also poll at bounded row intervals.
These are row-count bounds, not time bounds: comparing a large value can still take longer.
Temporary indexes and result buffers belong exclusively to one execution.
They never mutate persistent relation indexes or published list views.

Query execution can collect and relocate objects inside the call.
The executor roots its live inputs and accumulators, reloads them after polls, and restores the caller's root chain on return.
Plans and results use the generated heap and tracing descriptors.
Before query execution or another safepoint, register roots for every plan and value retained across that call.
After the call, reload retained values through those roots. A rooted result can outlive its transaction.

The authoring layer separates row mechanics from operator definitions:

- `each_row` traverses materialized rows and polls for collection.
- `scan_rows` traverses an index without materializing its input.
- `retain` and `root_value` express live managed values across polls.
- `build_row` and `project_row` allocate and fill row storage.
- `bind_probe` combines join keys with existing scan bindings.
- `join_index` and `join_matches_body` handle joins with materialized right inputs.
- `row_set`, `offer`, and `finish_rows` produce canonical results.
- `checked_query` and `status_call` propagate errors without partial results.

These builders specialize Mica callbacks during generation. Native execution does not allocate callback objects.
Operator definitions live in `query/operators.mica`; source access and plan execution have separate modules.

### Kernel checks and measurements

Run the sanitizer fixture through C, libgccjit object code, and libgccjit shared code:

```sh
cargo run --release -p mica-value-comparison --bin native-backend -- \
  --output target/native-gccjit --check-fixtures --fixture kernel --sanitize
cargo run --release -p mica-value-comparison --bin mica-kernel-comparison -- \
  --native-executable target/native-gccjit/kernel/check --seed 19 --cases 256
cargo test -p mica-runtime --test native_codegen native_relation_kernel_executes_on_both_mica_tiers
```

The fixture covers snapshots, catalogue atomicity, allocation failure, heap-valued tuples, AVL rotations, bounded scans, and concurrent commits.
The Rust comparison checks composed queries, scan results, and net commit deltas for reproducible transaction sequences.
It includes aborted transactions, collection between operations, and competing functional writes.
The integration test requires identical generated C from both bootstrap execution tiers.

Rebuild the fixture without sanitizers before measuring:

```sh
cargo run --release -p mica-value-comparison --bin native-backend -- \
  --reuse-backend --output target/native-gccjit --check-fixtures --fixture kernel
cargo run --release -p mica-value-comparison --bin mica-kernel-comparison -- \
  --native-executable target/native-gccjit/kernel/check --bench --samples 5 --rounds 128
```

The benchmark measures indexed reads, small updates, disjoint writes, and contended functional writes against Rust.
It includes native collection and thread creation for parallel workloads. Initial store construction stays outside the measured interval.
Samples alternate implementation order and report elapsed time, operation count, and conflicts.
Write workloads verify the final stored tuples outside the measured interval.
For query measurements, replace `--bench` with `--query-bench`.
This measures projection, equality joins, semi joins, and join-plus-projection over two 1,024-row relations.
Plan construction and store population stay outside the timer. Native allocation and collection remain inside it.
The query benchmark checks result counts; the differential corpus checks complete rows.
Generated modules, executables, and measurement output belong under ignored `target/` paths.

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

### Typed authoring

Load [generation.mica](generation.mica) and [sequences.mica](sequences.mica) after `builders.mica`.
The `native_gen` API constructs typed expressions and locations, then lowers them into the checked structured body.
A binding has a generated identity and a lexical scope. Its label is a readable hint, not a lookup key.
Use `ref` for declared parameters and constants; retain the handle returned by `bind` for generated locals.

```mica
let builder: map = native_gen/context(context, "sum")
let [total: map, initialized: map] = native_gen/bind(builder, "total", native_gen/zero(:U64))
builder = native_gen/for_range(initialized, "index", native_gen/zero(:U64),
  native_gen/ref(initialized, "limit"), fn(body, index)
    return native_gen/assign(body, total, native_gen/add(total, index))
  end)
builder = native_gen/return_value(builder, total)
context = native_gen/finish(context, builder)
```

The module must declare `sum` and a U64 `one` constant before constructing this body.
Callbacks run during generation. Each returns updated builder state; it becomes ordinary target control flow, without runtime closures.
Range operands evaluate once, in order. `continue` advances the index, including inside branches; nested loops keep their own continuation.

`field` derives the member type from the record schema. For pointer bases, it returns a location with the pointer's access mode.
`read` and `write` distinguish loading from storing. Writes through const locations and references escaping their scope fail during generation.
`bind` evaluates an expression once; reusing an unbound expression evaluates it at each use.
The existing IR checker still validates effects, ownership, safepoints, and tail transfers.
The bootstrap tools give generation a 256-frame host call budget for composed callbacks, alongside their instruction budgets.

`native_sequence` supplies spans, copying, ordered scans, stable merging, and binary search.
Comparators specialize into the generated function and can propagate failure explicitly.
Map construction and relation sorting share the merge implementation. Map lookup and update share binary search.
Typed allocation helpers in `memory/program.mica` check extents before reserving storage and introduce no safepoints.
String indexing and scalar lookup share a cursor for validated UTF-8 backing storage; untrusted input still uses the decoder.

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
  @native/for_range("i", "zero", "limit", [
    native/set_statement("total", native/binary_expression(:Add, "total", "i"))
  ]),
  native/return_value("total")
])
```

Here, `constants` supplies U64 zero and one from `native/constants`.
The [value generators](value/) also use structured bodies directly for operations such as UTF-8 decoding, string search, and recursive comparison.
They declare control flow with branches, loops, and early returns; the shared builder creates the blocks and branch targets.
Mica kind annotations check generator interfaces, while explicit IR types describe the generated values.

### Semantic builders

These Mica functions construct the structured representation used by the authoring and lowering layers.
They introduce no runtime helper calls or allocations beyond the operations they describe.

| Builder | Behaviour |
| --- | --- |
| `native/for_range(index, start, limit, body, ?step)` | Iterate an unsigned index and advance it on normal completion or `continue` |
| `native/walk_chain(cursor, type, first, next, body, exhausted)` | Traverse a pointer chain with an explicit exhaustion exit |
| `native/try_call(name, result, function, arguments, failure)` | Bind a result once and run the failure body when its `ok` field is false |
| `native/initialize_record(schema, fields)` | Construct a typed record from named fields in declaration order |
| `native/record_copy_arguments(schema, source, changes)` | Read unchanged fields and substitute named changes for a record copy |
| `native/member`, `native/set_member` | Derive a field type from the schema for pointer access |
| `native/copy_elements`, `native/checked_element` | Copy typed spans or form a slot after checking its index |
| `native_memory/allocate`, `native_memory/allocate_record` | Allocate managed storage and handle failure before initialization |
| `native_memory/allocate_array`, `native_memory/allocate_with_tail` | Check array extents before allocating standalone or trailing storage |
| `native_memory/record_constructor` | Define a complete managed allocation and initialization function with explicit field ownership |
| `native_value/open_heap` | Validate a value tag, unpack its pointer, and load its header |
| `native_value/scalar_field`, `native_value/value_field` | Describe fixed heap fields, including ownership and optional presence flags |
| `native_value/encode_word`, `native_value/encode_header`, `native_value/encode_bytes`, `native_value/encode_child` | Write wire values and stop on failure |
| `native_jit/row_arguments`, `native_jit/builtin_call`, `native_jit/indirect_call` | Resolve serialized operands, assemble calls, and enforce indirect-call checks or required tail calls |
| `native_kernel/require_transaction`, `native_kernel/check_tuple` | Check transaction state and tuple shape with caller-selected failure returns |
| `native_kernel/construct`, `native_kernel/copy_record` | Allocate a kernel record from named fields or copy it with named changes |
| `native_kernel/columns`, `native_kernel/scan_plan`, `native_kernel/scan_offer` | Traverse tuple columns, choose the scan order, and admit rows into bounded output |
| `native_kernel/copy_chain`, `native_kernel/apply_write_phases` | Rebuild persistent chains and order transaction write phases |
| `native_jit/row_field`, `native_jit/row_tail` | Read serialized rows using the encoder's schema |
| `native_jit/consume_arguments`, `native_jit/consume_operands` | Prepare the argument buffer and consume it before another call can overwrite it |

A schema contains a record name and its field declarations.
Named construction rejects missing, extra, and duplicate fields. Record copies reject unknown field names.
Record copies take a named source pointer, so field access cannot repeat a source expression with side effects.
Kernel layouts, constructors, field access, and tracing share these declarations.
For example, a catalogue update copies a relation and replaces its successor:

```mica
native/return_value(native_kernel/copy_record("Relation", "head", {"next" -> "suffix"}))
```

The value generators use the same approach for storage, sequences, and codecs.
For example, list construction copies its elements with:

```mica
@native/copy_elements(word, "data", "source", "length")
```

A list allocation checks the complete trailing-array extent before allocating:

```mica
@native_memory/allocate_with_tail("raw", "prefix", "element_size", "capacity",
  "layout_list", [native/return_value("null_list_header")])
```

`prefix`, `element_size`, and `capacity` are named U64 bindings.
The caller establishes that the prefix fits `allocation_limit` and the element width is nonzero.
The builder checks the count before multiplying, then handles allocation failure.
These operations do not collect or introduce safepoints.

`copy_elements` copies in ascending order. Regions must be disjoint or share the same base.
Empty spans touch neither pointer. `checked_element` rejects an index equal to the length before forming its pointer.
These builders take named lengths and indexes, so repeated checks cannot repeat an expression with side effects.
Allocation failure bodies must exit before initialization. The enclosing value functions supply `worker`, `allocation_limit`, `null_bytes`, `zero`, and `one`.
Builders expose their local names as arguments; `allocate_array` derives temporary names from its destination name.

Fixed heap records share one declaration across C layout, constructor ownership, initialization, and GC tracing.
For example, the range descriptor contains:

```mica
[native_value/value_field("start"),
 native_value/scalar_field("has_end", :Bool),
 native_value/value_field("end", "has_end")]
```

The collector traces `end` only when `has_end` is true.
Codec writers express wire operations directly, such as `native_value/encode_header("tag", "range_flags")` followed by `native_value/encode_child("start")`.
The GCC JIT generator uses `native_jit/indirect_call("callee", 4, true)` to check the callee, resolve arguments, and emit a required tail call.
JIT argument scratch storage must be consumed by its call before another argument list is assembled.

`for_range` supplies the increment, including when a branch or switch executes `continue`.
Nested loops retain their own advancement. `break` and `return` do not advance the index.
The default step is the `one` binding. The caller must supply a positive step that does not wrap the index.
`walk_chain` checks for null before the body. Its exhaustion body must leave the traversal through `break`, `return`, or tail transfer.
The enclosing function supplies the `true` binding for traversal.
The current node must remain valid until the next-link expression completes.

The range and chain builders introduce only the local names supplied by their callers.
Expression arguments retain their types when nested. Statement constructors remove the outer type wrapper where the statement supplies the expected type.
The kernel and value generators use these constructs for indexes, codecs, hashing, comparisons, collections, and tracing.

### Statement and expression constructors

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
`native/function_address_expression(name)` takes a declared function's address.
`native/indirect_call_expression(callee, arguments)` calls a typed function pointer; the callee can itself be a typed expression.

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

Standalone C places callees and address targets before callers. Prototypes remain for foreign functions and backward references within recursive cycles.
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
| `[:FunctionPointer, name]` | Named callable signature with argument, result, effect, and ownership contracts |

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
| `:FunctionAddress` | Take a declared function's address, with its ID as the only operand |
| `:CallIndirect` | Call a function pointer, with its value ID before the argument values |

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

### Typed function pointers

Declare a signature before validation:

```mica
let [unary, state] = native/function_pointer_type(native/program(), "Unary", :U64,
  [["input", :U64, :Value]], [], :Value, "")
```

The arguments after the name match function declarations: result type, parameters, allowed effects, result ownership, and result owner.
Each parameter contains its name, type, and ownership mode. Borrow regions can name a root parameter in that signature.
Taking an address checks argument types, result type, and ownership regions by parameter position.
The target's declared effects must fit within the signature's allowed effects.
Indirect calls check their arguments and use the signature's full effect allowance when checking the caller.
Borrowed results retain their argument's origin; consuming calls reject borrowed or stack storage.

Function pointers can appear in parameters, results, records, arrays, and globals.
They support copying, equality, zero initialization, and `"null"` constants. Calling null aborts before invocation.
C uses named function-pointer typedefs; recursive typedef dependencies are rejected.
Callbacks can refer to records that contain callbacks. Forward struct declarations preserve those types during C emission.

A code address has no captured data and uses `:Value` ownership. Pass a captured environment as a separate, explicitly borrowed argument.
Data-pointer casts and tagged-pointer packing do not accept code pointers.
The caller must keep the defining module loaded while any code address remains reachable.
This checker does not manage code lifetimes or establish continuation, suspension, collection, or tail-call semantics.
The [callable fixture](tests/callables.mica) exercises aggregate storage, nested calls, borrowed results, and memory effects.

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
apps/native/memory/program.mica
apps/native/memory/allocation.mica
apps/native/memory/collection.mica
apps/native/memory/lifecycle.mica
apps/native/memory/platform.mica
apps/native/value/symbol_storage.mica
apps/native/value/memory.mica
apps/native/value/tracing.mica
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
`native_memory/platform()` emits the allocation and pthread bindings for the managed heap.
Link these bindings, both symbol-table shims, and the value module with `-pthread`.

Implemented behaviour includes:

- All immediate constructors and accessors, signed 56-bit bounds, finite binary32 values, and canonical positive zero.
- Checked numeric operations, exact integer division, float remainder, and explicit numeric conversions.
- Per-worker bump nurseries, copying promotion, and a shared non-moving mark-and-sweep heap.
- UTF-8 validation, scalar encoding and decoding, indexed strings, slicing, append, concatenation, and substring search.
- Copying byte and list constructors, plus range, error, and frob constructors and checked accessors.
- Recursive canonical comparison for these values and maps, with separate exact integer/float comparison for language expressions.
- Map construction with stable key sorting, the last duplicate value retained, and binary-search lookup.

Initialize zeroed heap and worker controls with `mica_value_heap_init` and `mica_memory_worker_init`.
Heap, worker, and registered root controls must keep stable addresses until released.
The worker initializer accepts the nursery capacity in bytes. Zero capacity sends allocations directly to mature pages for allocation-failure tests.
Enter the worker with `mica_memory_enter` before accessing managed values.
Each worker has one mutator thread at a time. Workers in the same heap can execute concurrently.

Value helpers do not collect. Nursery exhaustion uses mature storage and requests collection at the next safepoint.
These overflow allocations remain worker-private until a successful collection, so string and list appends can reuse their spare capacity.
Allocation failure returns a null pointer or `{false, 0}`.
Before a safepoint, register live values with `mica_memory_root_push` and `mica_value_root_set`.
Start each root control zeroed. A raw root pointer must identify an allocation base, not an interior address.
After a safepoint, reload them with `mica_value_root_get`.
Direct value pointers remain valid between safepoints. Promotion can change their addresses.

`mica_memory_safepoint(worker, false)` joins a pending collection or collects when managed page allocation reaches the heap growth limit.
Nursery overflow uses managed pages and does not independently trigger a full collection.
An explicit `true` requests collection. Collection waits for active workers to reach a safepoint or leave.
Idle workers retain their registered roots but do not join this wait.
Promotion reserves destinations before it rewrites references. Failed reservation preserves the original graph, roots, and nursery contents.
It also preserves the private state of overflow allocations.

Child values must belong to the same managed heap. Worker-private values cannot cross thread boundaries.
`mica_memory_share` copies a private graph into immutable mature storage without a safepoint or source relocation.
It preserves cycles and shared edges, and reuses previously published immutable subgraphs.
The caller must retain the result in a registered root before its next safepoint.
`mica_memory_publish` first polls for collection, then copies a registered root and retains it in the shared root registry.
It updates the source root to the immutable copy. Other private aliases keep their original objects.
Another worker uses `mica_memory_acquire` to obtain its own registered root.
`mica_memory_unpublish` removes the shared root. Acquired roots continue to retain the graph.
Published graphs must be immutable. Mutable scratch buffers remain private to their owning worker.
Mature GC survivors and published immutable objects have separate states.
A mutable transaction record can survive collection and then receive fresh nursery references.
A successful collection freezes surviving string and list backing storage, including overflow allocations. Their subsequent append operations allocate fresh backing storage.

Remove private roots in reverse registration order with `mica_memory_root_pop`.
Leave the worker with `mica_memory_leave` before blocking outside the runtime.
An inactive worker cannot access managed payloads or change its roots.
Release an inactive worker without roots with `mica_memory_worker_release`.
Release the heap after all workers and shared roots are gone, using `mica_memory_heap_release`.

Strings and bytes copy their input storage.
Error messages are string values. Their presence and the optional error payload have separate flags.

`mica_value_compare` and `mica_value_language_compare` return `IntResult { ok, number }`, where `number` is −1, 0, or 1.
Canonical comparison distinguishes integer and float keys. Language comparison compares their numeric values without rounding integers to binary32.
Comparison ignores absent range ends, error messages, and error payloads.
`mica_value_equal` and `mica_value_language_equal` return the corresponding equality result as `BoolResult { ok, value }`.

`mica_value_map` copies its input and uses a stable merge sort before duplicate compaction.
Construction takes O(n log n) comparisons and O(n) temporary storage. Lookup takes O(log n) comparisons.
`mica_value_map_get` returns `{false, 0}` for a missing key or a non-map input.
Failed construction leaves temporary storage for the next collection. It never publishes a partial map or changes the input.

Lists support `value_list_length`, `value_list_get`, `value_list_slice`, `value_list_append`, and `value_list_set`.
Indices start at zero. Slices exclude the end position and permit empty ranges, including the end of a list.
Invalid types, reversed ranges, and out-of-range indices return `ok = false`.
Length returns `IdResult`. The other operations return `ValueResult`.

List slices allocate a header and share the backing array. Replacement copies the array before it writes the element.
Append claims spare capacity only at the current tail of worker-private storage.
Otherwise, it copies the visible prefix into another backing with geometric growth.
Every earlier view retains its elements and length. The collector preserves shared backing storage and traces its initialized elements.

The IR uses `:Gc` contracts for managed values. It rejects managed locals that survive a relocating call without a root reload.
Callers of generated C must obey the same root and safepoint rules.
Nested children share storage within one heap. `mica_value_copy` creates independent storage in the destination worker's heap.
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
The scanner checks ASCII in bounded sixteen-byte groups, with eight-byte and scalar handling for shorter tails.
Unaligned input is valid, and no read extends beyond its supplied length.

String positions count Unicode scalars, not grapheme clusters. The APIs use these conventions:

| Function after `mica_` | Result |
| --- | --- |
| `value_string_length(value)` | Scalar count as `IdResult { ok, number }` |
| `value_string_byte_offset(value, position)` | Byte offset, including the end position |
| `value_string_scalar_at(value, position)` | `RuneResult { ok, rune }`; the end position fails |
| `value_string_slice(worker, value, start, end)` | Shared view of the end-exclusive scalar range |
| `value_string_append(worker, value, bytes, length)` | String with validated suffix bytes |
| `value_string_concat(worker, left, right)` | String with the right string appended |
| `value_string_find(value, needle, start)` | First matching scalar position at or after `start` |

Invalid types, reversed ranges, and out-of-range positions return `ok = false`.
Search also returns `ok = false` for an absent match. An empty needle matches any valid position, including the end.
Search uses byte comparisons, with a stack skip table for needles of at least four bytes.
The worst-case search cost is O(haystack bytes × needle bytes).
Converting a non-ASCII match to a scalar position counts boundaries between the requested start and the match.

Each string view stores its byte length, scalar count, and ASCII flag.
Backings with at least 128 bytes of capacity reserve one byte-offset sample per 32 scalar positions.
Samples use eight bytes each. Capacity reserves enough samples for an eventual ASCII suffix.
ASCII indexing uses direct offsets. Non-ASCII indexing starts at a sample and steps past at most 31 preceding scalars.
Index construction and navigation use leading-byte widths in validated text. Raw byte inputs still receive full UTF-8 validation.
Slices share the backing bytes and samples. Small backings without samples use a bounded scan.

Append allocates a separate view header. Only a view at the current tail of worker-private storage can reuse spare capacity.
Other appends allocate another backing with geometric growth. Earlier views retain their bytes and lengths.
Overlapping suffix bytes are valid. Invalid UTF-8 leaves the input unchanged.
Concatenation uses the existing suffix string's scalar count and ASCII flag without validating its bytes again.
The owning worker controls private storage mutation. Other workers receive collected values through shared roots.
The collector preserves backing storage while a live view retains it. Allocation errors can leave temporary storage for collection.
The allocation, index-building, and validated-append helpers are internal construction steps; callers must not publish incomplete headers.
`value_string_append_validated` requires valid UTF-8 and exact suffix metadata.

Finite relations contain a symbol heading and a canonical set of tuples.
`value_relation(worker, heading, arity, rows, length)` copies the heading, tuple descriptors, and cell arrays into managed storage.
The collector traces child values and repairs interior row pointers during promotion.
The constructor sorts columns by symbol ID and permutes every row to match.
It sorts rows lexicographically and removes duplicates. Duplicate column names and rows with the wrong arity fail.
Headings can contain at most 65,535 columns. Allocation failure returns `{false, 0}` without changes to the input.

`ValueTuple` contains `data` and `arity` fields. `value_tuple` copies a supplied cell array and returns `TupleResult { ok, tuple }`.
`value_tuple_get` returns a checked cell. `value_tuple_compare` compares cells lexicographically, then compares the arities.
A tuple has no standalone value tag or heading.
Relation row views borrow their cell arrays. Root the relation across a safepoint, then obtain the row view again.
Raw `MemoryRoot.pointer` slots contain allocation bases, not interior pointers.

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
The next collection reclaims unreachable sort storage. Published views remain immutable.

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
Pinned symbol storage retains names and earlier table arrays until table release. Symbol IDs do not retain pointers into the managed heap.
Geometric growth keeps the total array size below twice the current array size.
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
The hash follows Omica's canonical value algorithm. Equal values in one symbol namespace have equal hashes, regardless of their allocation or backing storage.
Integer and float kinds remain distinct. Absent optional fields do not contribute their stored contents.
Hashes are neither cryptographic identifiers nor persistence encodings. Unknown tags return `ok = false`.

`value_copy(worker, value)` and `value_tuple_copy(worker, tuple)` recursively copy visible storage into the destination heap.
The result can outlive the source heap. Strings rebuild their scalar indexes, and lists receive independent append storage.
Maps and relations retain canonical order without another sort. Absent optional fields become zero instead of retaining source pointers.
Immediate IDs retain their original symbol, identity, capability, or function namespace.

Copy returns `ValueResult` or `TupleResult`. Allocation failure returns `ok = false` and leaves the source unchanged.
The next collection reclaims unreachable partial allocations. The caller must use its active worker for destination allocation.
Traversal uses native recursion. Copy duplicates shared subtrees and does not preserve source aliasing or spare capacity.

`value_is_persistable(value)` rejects capabilities and function handles, including nested children.
It ignores absent optional fields. This predicate does not check whether symbols have registered names.

`value_encode(worker, table, value, options)` returns a `ValueResult` containing managed bytes.
`ValueCodecOptions { symbol_ids, allow_capabilities }` defaults to persistence-safe options when zero-initialized: names, with capabilities disabled.
The format matches Rust's owned value codec: little-endian words, structural heap records, and UTF-8 symbol names.
Heap pointers are never encoded. Function handles are always rejected.
Set `symbol_ids` only when producer and consumer share the same symbol namespace.
Set `allow_capabilities` only for transient transfer within the same authority namespace; this does not make capabilities persistable.

`value_decode(worker, table, data, length, options)` returns `ValueDecodeResult { ok, value, consumed }`.
`value_decode_exact` returns `ValueResult` and rejects trailing bytes.
Decoding checks tags, flags, lengths, UTF-8, and relation headings before publishing a value.
Maps and relations use their ordinary constructors to normalize order and duplicates.
Names require an initialized symbol table. ID mode permits a null table.
Named decoding interns into the destination table, so resulting IDs can differ from the source IDs.

Encoded bytes and decoded values belong to the destination heap. Neither borrows source storage or symbol-name bytes.
The owning worker controls destination allocation. Symbol-table access uses its existing lock.
A failure returns `ok = false` without publishing a partial result. The next collection reclaims unreachable partial allocations.
Names interned before a decode failure remain in the symbol table.
The codec uses native recursion, with no separate nesting limit. Callers must bound untrusted input depth before using it.

This module is in progress. Display remains unimplemented.
Heap layouts are local to this implementation. Matching immediate tags does not make heap pointers interchangeable with Odin or Rust.

## libgccjit backend

The [libgccjit generator](gccjit/program.mica) describes a general backend in native IR.
The C emitter bootstraps that backend into an executable, called B0.
B0 accepts different modules as data without recompilation.

Mica owns serialization, declaration ordering, type construction, operation lowering, and control flow.
The [API bindings](gccjit/bindings.mica) also come from Mica.
Rust only hosts the generators, writes artifacts, invokes compilers, and runs the comparison harness.
The platform shims supply allocation and mutex operations.

`native_jit/data(state)` validates the module and serializes its tables, names, and types.
The binary input contains IR operations, not a sequence of libgccjit API calls.
B0 trusts this locally generated input. The format is not an external module protocol.
The current target requires a little-endian LP64 host and libgccjit 14 or later.
The qualified host uses AArch64 and libgccjit 14.2.0.

The backend covers the native IR, including checked arithmetic, tagged pointers, aggregates, globals, TLS, roots, and indirect calls.
Array wrappers preserve the C ABI. Pointer casts and tagged operations retain their runtime guards.
GCC 14 supplies `sizeof` but no `alignof` construction API.
For the supported natural C types, the backend derives alignment from a padded probe record.

The backend uses `gcc_jit_context_compile_to_file` to produce objects and shared libraries.
It does not use `gcc_jit_context_compile` or implement code-module retention.
The shared-library check measures `dlopen(RTLD_NOW)` and symbol lookup separately from compilation.
The fixture consumers also execute shared-library code.
The same backend compiles the managed heap and value library. Task scheduling and transaction execution remain unimplemented.

Build B0 and run the backend, memory, value, and execution fixtures:

```sh
cargo run --release -p mica-value-comparison --bin native-backend -- \
  --sanitize --check-fixtures
```

Each fixture runs through emitted C, a libgccjit object, and a libgccjit shared library.
The checks cover allocation failures, ABI layouts, TLS isolation, deliberate aborts, tracing, publication, and required tail calls.
The execution fixture requires Clang for emitted C. `CLANG` selects that compiler.

Compile the complete value module with the same B0:

```sh
cargo run --release -p mica-value-comparison --bin native-backend -- \
  --reuse-backend --sanitize --module 'native_value/program()' \
  --load-symbol mica_value_tag
cargo run --release -p mica-value-comparison -- \
  --backend gccjit --native-module target/native-gccjit \
  --all-corpora --cases 128 --seed 19 --sanitize
```

`--reuse-backend` reuses the executable. Rebuild it after changes to the backend sources or its compilation flags.
Artifacts stay under `target/native-gccjit`.
The bootstrap produces `backend.ir`, `backend.c`, `backend.h`, `support.c`, their objects, platform objects, and the `backend` executable.
Each input module produces `module.ir`, `module.h`, `module.c`, `module.o`, and optional `module.so`.
Timing metadata and compiler diagnostics also stay there.
`GCCJIT_INCLUDE` can select the directory containing `libgccjit.h`.

On the qualified host, LeakSanitizer reports retained allocations inside libgccjit after context release.
The harness suppresses those library stacks only in the compiler process.
The generated programs retain ASan, UBSan, and leak checks without that suppression.
This result does not establish bounded memory use for a persistent compiler service.

For unsanitized measurements, build a separate B0 and value module:

```sh
cargo run --release -p mica-value-comparison --bin native-backend -- \
  --output target/native-gccjit/performance --module 'native_value/program()' \
  --load-symbol mica_value_tag
```

Use a C compiler that matches the libgccjit version. Apply the same CPU affinity to both runs.
The supplied module keeps both value implementations in a separate translation unit, with `-O3`, `-fPIC`, and no LTO.

```sh
CC=gcc-14 cargo run --release -p mica-value-comparison -- \
  --backend c --native-module target/native-gccjit/performance \
  --all-corpora --cases 128 --seed 19 --bench --samples 7 --sample-ms 50
CC=gcc-14 cargo run --release -p mica-value-comparison -- \
  --backend gccjit --native-module target/native-gccjit/performance \
  --all-corpora --cases 128 --seed 19 --bench --samples 7 --sample-ms 50
```

Both runs compare results and checksums against Rust.
The existing `c_*` timing fields refer to the selected native backend.
Without `--native-module`, the harness retains its existing single-translation-unit C comparison.
The [design document](../../sketches/NATIVE_TASKS_AND_MEMORY_DESIGN.md#151-can-the-existing-native-ir-drive-libgccjit) records the experiment findings and limits.

### Backend reproduction

The harness uses B0 to compile its own IR into B1. B1 compiles the same IR into B2.
B2 compiles that input once more to check reproduction.
All three executables link the same generated bindings, driver, and platform objects.
Each rebuilt backend independently compiles all maintained fixtures and the complete value module.
The harness checks the fixtures through C, objects, and shared libraries, then compares the value object against Rust.

Build both harness executables and run the experiment:

```sh
cargo build --release -p mica-value-comparison --bins
target/release/native-backend --self-rebuild \
  --output target/native-gccjit/rebuild
```

By default, each compiler performs 40 cycles for each of three inputs: its backend, the value module, and the callable fixture.
Each cycle constructs and releases a fresh GCC context, then loads and unloads its shared library.
The callable cycle also executes `mica_chain(41)` and checks that it returns 42.
The other cycles resolve their entry symbols without execution.
`--lifecycle-rounds` accepts 1 through 1,000. Apply CPU affinity when comparing timings.

For sanitizer qualification, use a separate output directory:

```sh
target/release/native-backend --self-rebuild --sanitize \
  --output target/native-gccjit/rebuild-sanitized
```

On the tested libgccjit 14.2.0, both rebuilt backends pass the sanitized corpus.
The subsequent repeated callable compilation fails on its second cycle with a GCC internal error in `assemble_external_libcall`.
The command exits unsuccessfully and records the lifecycle failure separately from successful reproduction.
`--lifecycle-rounds 1` limits that phase to one cycle, but does not qualify repeated sanitized compilation.

Each output directory contains `b1/`, `b2/`, and `lifecycles/`, plus `self-rebuild.json`.
Lifecycle JSON lines record construction, compilation, loading, execution, release, RSS, and allocator measurements.
RSS requires Linux. Allocator measurements require glibc and report only malloc-managed storage.
Sanitized runs exercise only the callable cycles. Their memory figures are unsuitable for retention measurements.

The unsanitized backend objects reproduce byte-for-byte on the tested host.
The 40-cycle tests pass, but compiler RSS rises to about 310 MiB.
A 120-cycle object-only control reaches 317 MiB, with most growth in its first 12 cycles.
This result establishes backend reproduction from fixed IR. Native source compilation and persistent compiler-service qualification remain separate work.
The [experiment 2 results](../../sketches/NATIVE_TASKS_AND_MEMORY_DESIGN.md#152-can-the-generated-backend-rebuild-itself) give the measurements and remaining limits.

### Execution regression

The [execution fixture](tests/execution.mica) uses the current managed heap, explicit roots, safepoints, and required tail calls.
It runs a million steps on a 256 KiB native stack.
One thread publishes a checkpoint and releases its worker. A fresh thread resumes from that checkpoint.
The checks cover the result, checkpoint immutability, repeated collection, and final reclamation.
They run through emitted C, a libgccjit object, and a libgccjit shared library under `--check-fixtures`.

This fixture checks execution and memory primitives. It does not implement a task scheduler, transactions, or code-module lifetime management.
The [recorded continuation comparison](../../sketches/NATIVE_TASKS_AND_MEMORY_DESIGN.md#153-which-continuation-and-nursery-model-works) explains the choice of heap nurseries and required tail calls.

### Execution contracts

The [execution checker](execution.mica) distinguishes `:Tail` and `:Collector` functions.
`native/execution_function` assigns a policy. `native/execution_pointer_type` declares a callable with a matching transfer policy.

`native/tail_call_statement` requires identical signatures and rejects arguments that retain stack addresses.
The C backend requires Clang `musttail`; the libgccjit backend marks the call as required-tail.

`:Gc` parameter and result contracts describe managed provenance.
`native/manage_expression` introduces that provenance in collector code.
Collector code cannot manage its own stack storage. Tail entries allocate through collector helpers.
`native/root_store_statement` publishes managed references through explicit root slots.
`native/root_load_expression` reloads them after collection.
Ordinary stores retain their existing stack-escape checks.

The `:Relocate` effect invalidates managed locals and derived addresses across a call.
A fresh assignment restores the destination. Control-flow joins reject a reference invalidated on either incoming path.
These checks require declared provenance and relocation effects.
Collector code remains responsible for root registration, descriptors, bounds, and complete relocation.
They do not prove that an arbitrary raw load refers to registered GC storage.

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
Copy checks remove the source roots and release the source worker before they collect and inspect the result under AddressSanitizer.
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
Operation workloads reuse prepared values. Timings include Rust result destruction and C safepoint polls.
Each C sample includes a final collection to reclaim its temporary outputs. Worker initialization stays outside the timer.
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
