# Structural Relation Types Stage 0

Date: 2026-08-13

## Outcome

Proceed with structural relation types independently of optimization. The grammar probe found no
unresolved parser collision, and the representation benchmark found a large enough local cost gap
to retain scalar replacement as a later measured optimization. Correctness continues to use ordinary
relation values at observable boundaries.

The executable parser does not yet accept structural types, row bindings, type declarations, or
unit. It now diagnoses empty parentheses explicitly instead of accepting `()` and lowering an error
node that could be mistaken for `nothing`.

## Grammar Probe

The test-only parser in `crates/compiler/src/structural_syntax_prototype.rs` accepts the proposed
forms without exposing them to executable source:

```mica
relation<{:person -> identity, :name -> string}>
relation<{:value -> string}> where rows in 0..1
relation<
  {:case -> :ok, :value -> option<string>}
  | {:case -> :error, :value -> error}
> where rows in 1

let exactly {label} = Label(#sensor, ?label)
if let {:label -> sensor_label} = Label(#sensor, ?label)
for {work} in AssignedTo(?work, assignee)
()
```

The probe covers nested closing angle brackets, row alternatives, cardinality intervals, `*`, unit,
the explicit empty relation, punned and renamed row fields, and all three binding delimiters. Tests
also confirm that existing maps, relation literals, ranges, optional parameters, splices, query
variables, annotations, role restrictions, and calls still parse unchanged.

Decisions from the probe:

- Keep `relation<{:column -> type}>` and include row alternatives in the first syntax release.
- Keep `where rows in CARDINALITY`; accept `∈` as an input synonym when the production lexer lands.
- Keep `{field}` and `{:column -> binding}` as relation-row patterns in the shared lexical pattern
  model.
- Keep the current contextual meanings of `?` and `@` for this implementation. Their uses are
  distinguishable at the parser entry points tested by the probe.
- Reserve `()` for unit. Until unit lands, empty parentheses are a parse error.

## Live Alias And Result Decisions

Aliases will be installed live-world declarations, not file-local aliases. The catalogue stores one
canonical current declaration, fileout reproduces it, replacement invalidates dependent compilation
caches, and compiled programs carry erased descriptors needed for execution. There is no parallel
file-local final API.

The standard result contract is:

```mica
type result<T> = relation<
  {:case -> :ok, :value -> T}
  | {:case -> :error, :value -> error}
> where rows in 1
```

The failure discriminator is `:error`, the failure payload is Mica's structured `error` value, and
the constructors/patterns are `ok(value)` and `err(problem)`. A general `either<L, R>` remains a
possible ordinary alias rather than broadening the standard result contract.

## Application Absence Audit

Reproduce the parsed inventory with:

```sh
cargo run -q -p mica-compiler --example absence_audit -- apps
```

The tool masks filein-only `grant` blocks while preserving byte positions, then parses every `.mica`
file. Its 630 `nothing` literals exactly match the textual token count and it reports no parse
errors.

| Parsed surface | Count | Migration class |
| --- | ---: | --- |
| `nothing` in equality tests | 329 | `option<T>` presence tests |
| `nothing` stored in lists, maps, or relation cells | 158 | typed option fields, except explicitly relational empty inputs |
| `nothing` passed as a call argument | 96 | explicit option arguments or an explicit `[] {}` where the API is relational |
| `return nothing` | 32 | unit, option, or result according to the enclosing API contract |
| local initialized or assigned to `nothing` | 15 | `none` with a type inferred from later assignments |
| query-shaped `one` | 76 | 57 optional bindings and 19 exact bindings |
| functional dot reads | 188 | strict scalar reads; optional code uses `if let` over the relation query |
| index operations | 339 | strict indexing; callers needing absence use `get` or `get_or` |
| existing `for` bindings | 135 | structural row bindings for relations; ordinary element patterns otherwise |
| bare returns | 0 | future bare returns still become unit |

The `one` split was reviewed by use: 52 expressions or their bindings are compared directly with
`nothing`, two participate in explicit fallback assignment, and three are returned from
option-shaped helper APIs. The remaining 19 are required facts and migrate to `let exactly`.

The audit reports per-file counts so Stage 8 can migrate one application at a time without a blind
replacement. No application use remains classified as a universal sentinel.

## Runtime And Durable Absence Audit

Every internal empty-relation use falls into one of these migration groups:

| Current use | Locations | Required replacement |
| --- | --- | --- |
| Immediate zero word, relation codec, hashing, ordering, and actual empty relations | `mica-var`, program value codec | retain the representation and rename APIs to `empty_relation` |
| VM register initialization and compiler scratch operands | compiler, VM, Cranelift helpers and tests | internal placeholder; never expose it as absence or unit |
| Missing functional scan result | `Opcode::ScanValue` | raise `E_CARDINALITY`; optional queries use row binding |
| `one` extraction and ambiguity | compiler and VM | remove the opcode and use exact/optional row binding |
| Collection iteration end markers | VM collection key/value helpers | keep internal iteration state out of source value semantics |
| Missing error message/value | VM error-field access, reports, task maps | typed options |
| Missing actor, principal, task value, task error, dependency relation, or subscription cursor | runtime, driver, host protocol, auth, source provider | typed options at the boundary |
| Empty dispatch restriction and frob-only marker | relation-kernel dispatch facts | explicit restriction alternative; preserve durable meaning during migration |
| Resume-without-value and input metadata omission | task manager and driver | unit for no result, option for omitted metadata |
| JSON `null` | runtime JSON decoder and HTTP adapters | explicit tagged conversion helpers; no implicit relation conversion |
| Empty HTTP/browser response | browser and web host | unit or an explicit host-protocol response alternative |
| Tests, fuzz seeds, and benchmarks | all crates | migrate with the production surface they exercise |

The relation codec continues to decode the zero-column empty relation as a real relation value. The
program artefact codec changes in place when structural descriptors land; no compatibility decoder
is retained.

### Builtin contracts

| Builtin/API | Current absence behaviour | Migration |
| --- | --- | --- |
| `log`, `mailbox_close`, `cancel_subscription`, `disable_rule` | return empty relation after an effect | return unit |
| `frob_delegate` | empty relation for a non-frob | raise `E_TYPE` |
| `from_literal` | empty relation for syntax or unsupported literal | `result<Value>` with a structured parse/type error |
| `parse_ordinal` | empty relation for invalid text | `result<int>` |
| `os_getenv` | empty relation for a missing variable | `option<string>` |
| `actor`, `principal`, and internal context identity lookup | empty relation when unavailable | `option<identity>` where absence is valid |
| `index_or` | caller-supplied fallback | retain as explicit `get_or`; ordinary indexing stays strict |
| `subscribe_changes` relation, bindings, and cursor sentinels | empty relation marks omitted fields | typed options or an explicit catalogue subject alternative |
| error fields and read-only query report fields | empty relation marks omission | typed options |
| JSON decode | `null` becomes empty relation | reject implicit conversion; use explicit JSON conversion helpers |

`emit` and `mailbox_send` already return the emitted/sent value and are not unit operations. Parsing
APIs use result when they can explain failure; absence-only lookup APIs use option.

## Representation Benchmark

The retained benchmark is:

```sh
cargo bench -p mica-var --bench small_relation_representations
```

It runs `none`, `some`, `ok`, and `error` construction and extraction for ordinary boxed relations
and a presence/tag-plus-payload prototype, both serially and with one/four coordinated workers. The
2026-08-13 baseline ran on the 20-core aarch64 Cortex-X925/Cortex-A725 host with PMU collection.

Representative medians:

| Operation | Boxed relation | Components | Relative throughput |
| --- | ---: | ---: | ---: |
| construct `some` serial | 140.04 ns | 8.82 ns | 15.9x |
| construct `ok` serial | 185.65 ns | 8.72 ns | 21.3x |
| construct `some`, one concurrent worker | 163.44 ns | 31.75 ns | 5.1x |
| construct `some`, four workers combined | 40.95 ns | 7.93 ns | 5.2x |

The prototype proves that general relation construction dominates this isolated workload. It does
not prove an end-to-end win or justify a general escape-analysis system by itself. Stage 7 may add
known-shape boxed construction and bounded scalar replacement only after correctness, direct
matching, side-exit materialization tests, and application-shaped benchmarks exist.

## Stage Gates

- Structural types proceed even if later optimization is removed; their semantics do not depend on
  the component representation.
- Unit, empty relation, option, result, and raised error remain observably distinct.
- Dynamic structural checks will use immutable relation summaries and cache successful descriptors;
  repeated boundaries will not rescan every cell.
- Matching and row binding will load cells by known heading position and will not allocate row maps.
- Installed live aliases and structured `error` result payloads are fixed requirements for the next
  stages.
