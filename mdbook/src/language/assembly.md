# Program assembly

`assemble(description)` validates a register program and returns its Rust bytecode artifact as `bytes`.
The description is a map with exactly two fields: `:registers` and `:code`.
The code is a nonempty list of instruction lists. Each instruction starts with an operation symbol.

```mica
let artifact = assemble({:registers -> 3, :code -> [
  [:Load, 0, 20],
  [:Load, 1, 22],
  [:Binary, 2, :Add, 0, 1],
  [:Return, [:Register, 2]]
]})
```

Assembly is a pure operation. It does not install methods, run instructions, or grant authority.
The runtime checks authority when the resulting program executes through its ordinary task and method interfaces.
An artifact contains the current Rust program format. It is not portable across program-format versions or worlds with different relation identities.

`is_builtin(name)` reports whether a symbol names a builtin in the executing task's registry.
It includes host-added builtins. Compiler forms such as `commit` are not builtins.
The query confers no authority to call the named operation.

## Operands and bounds

A register operand is `[:Register, index]`. A constant operand is `[:Constant, value]`.
Constants must support artifact serialization. Function values and capabilities cannot become artifact constants.
An optional operand uses `none` for absence.

Registers and instruction targets are zero-based integers. Jump targets are absolute instruction indices within their containing program.
The register count is at most 65,535. Each register reference must be below that count.
A description can contain at most 65,536 instructions across all nested programs. At most 16 program levels are permitted.
Malformed descriptions and invalid bytecode raise catchable `E_INVARG` errors.

In the table, `dst`, `src`, `left`, `right`, `collection`, and `index_reg` are register indices.
`operand` means a tagged register or constant operand. `target` means an instruction index.
An `items` list contains operands or `[:Splice, operand]` entries.

## Instructions

| Instruction | Behaviour |
| --- | --- |
| `[:Load, dst, value]` | Load a constant value. |
| `[:CheckKind, register, kind, site, subject]` | Check a value kind. The site is `:Binding`, `:Parameter`, or `:Builtin`; the subject is a symbol. |
| `[:Move, dst, src]` | Copy a register value. |
| `[:Unary, dst, operation, src]` | Apply `:Not` or `:Neg`. |
| `[:Binary, dst, operation, left, right]` | Apply `:Eq`, `:Ne`, `:Lt`, `:Le`, `:Gt`, `:Ge`, `:Add`, `:Sub`, `:Mul`, `:Div`, or `:Rem`. |
| `[:BuildList, dst, items]` | Build a list, including spliced lists. |
| `[:BuildMap, dst, entries]` | Build a map from `[key_operand, value_operand]` pairs and `[:Splice, operand]` entries. |
| `[:BuildRelation, dst, heading, cells, row_count]` | Build a relation from a symbol heading and row-major cell operands. |
| `[:RelationPattern, dst, relation, heading, row_count, equalities]` | Test an exact relation heading and row count, with optional first-row equalities. |
| `[:RelationCell, dst, relation, column]` | Read a column from the first row; raise `E_MATCH` if absent. |
| `[:BuildRange, dst, start_operand, end_operand]` | Build a range. The end can be `none`. |
| `[:Index, dst, collection, key_operand]` | Read a collection element. |
| `[:SetIndex, dst, collection, key_operand, value_operand]` | Produce an updated collection. |
| `[:CollectionLen, dst, collection]` | Read the collection length. |
| `[:CollectionKeyAt, dst, collection, index_reg]` | Read an iteration key. |
| `[:CollectionValueAt, dst, collection, index_reg]` | Read an iteration value. |
| `[:CollectionFieldAt, dst, collection, index_reg, heading, column]` | Read a field from an iteration row; raise `E_MATCH` if the row does not match. |
| `[:Branch, condition, true_target, false_target]` | Branch on the condition register. |
| `[:Jump, target]` | Jump to an instruction. |
| `[:BuiltinCall, dst, name, items]` | Call the builtin named by a symbol. |
| `[:PositionalDispatch, dst, selector_operand, items]` | Dispatch positional arguments through the runtime method catalogue. |
| `[:DynamicDispatch, dst, selector_operand, roles_operand]` | Dispatch a runtime role map. |
| `[:SpawnDispatch, dst, selector_operand, roles_operand, delay_operand]` | Request a child invocation with a role map; resume with the child task identifier in `dst`. |
| `[:SpawnPositionalDispatch, dst, selector_operand, items, delay_operand]` | Request a child invocation with positional arguments, including splices. |
| `[:LoadFunction, dst, description, captures, min_arity, max_arity]` | Create a function from a nested program and capture operands. |
| `[:CallValue, dst, callee_operand, items]` | Call a function value. |
| `[:ScanDynamic, dst, relation, arguments]` | Scan a relation identity with dynamic arguments. |
| `[:AssertDynamic, relation, arguments]` | Assert a tuple with dynamic arguments. |
| `[:RetractDynamic, relation, arguments]` | Retract tuples with dynamic arguments. |
| `[:EnterTry, catches, finally_target, end_target]` | Enter an exception region. |
| `[:ExitTry]` | Leave an exception region through its normal path. |
| `[:EndFinally]` | Finish the current finally handler. |
| `[:Raise, error_operand, message_operand, value_operand]` | Raise an error. Message and value can be `none`. |
| `[:ErrorField, dst, error_register, field]` | Read `:Code`, `:Message`, or `:Value` from an error. |
| `[:Abort, operand]` | Abort the task with the operand as its error value. |
| `[:Return, operand]` | Return a value. |
| `[:Emit, target_operand, value_operand]` | Stage an emission. |
| `[:CommitValue, dst]` | Commit and suspend; receive the continuation value in `dst`. |
| `[:SuspendValue, dst, duration_operand]` | Suspend; duration can be `none`. |
| `[:Read, dst, metadata_operand]` | Request input; metadata can be `none`. |
| `[:MailboxRecv, dst, receivers_operand, timeout_operand]` | Receive mailbox input; timeout can be `none`. |
| `[:ExternalRequest, dst, service_operand, payload_operand, timeout_operand]` | Request a host service; timeout can be `none`. |
| `[:RollbackRetry]` | Roll back and retry the task. |

Dynamic relation arguments accept ordinary operands, `[:Splice, operand]`, `[:Query, symbol]`, and `[:Hole]`.
Runtime relation operations retain their normal arity and binding checks.

Row-pattern headings contain symbols in canonical sorted order. A relation must have exactly that heading.
`RelationPattern` equalities are `[column_symbol, constant_value]` pairs checked against the first row.
The instruction returns `false` for other value kinds or a failed match.
`RelationCell` does not check cardinality; use `RelationPattern` first when a singleton is required.
For non-relation collections, `CollectionFieldAt` reads the named key from the map at the iteration position.
Those maps can contain additional keys. The relation form requires the exact heading.

Each catch entry is `[error_code, binding_register, target]`. The code and binding can be `none`.
The finally target can be `none`. Exception targets belong to the containing program.

Spawn instructions suspend through the ordinary task interface. The delay is in seconds; `none` means no specified delay.
Child invocation checks and continuation authority remain the host task driver's responsibility.

A function receives its captures in the first registers, followed by one register containing its complete argument list.
Its nested program must reserve those registers and unpack the argument list itself.
The arity bounds must be ordered. A maximum arity of 65,535 means no upper bound.
