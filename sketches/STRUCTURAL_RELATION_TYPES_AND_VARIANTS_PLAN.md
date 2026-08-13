# Structural Relation Types, Variants, Option, And Result

Date: 2026-07-15
Revised: 2026-08-13

## Status

Implemented through Stages 0–8 on 2026-08-13. Structural types, live aliases, unit, option/result
values, structural matching and row binding, strict absence boundaries, and the bounded Stage 7
optimizations described here are current Mica behaviour. Broader escape analysis and specialized
calling conventions remain deliberately deferred pending application-shaped evidence.

## Executive Position

Mica should investigate structural relation types as the common foundation for:

- type constraints on relation headings and cells;
- literal discriminators inside relation rows;
- cardinality constraints;
- structural type aliases;
- option and result values;
- sum-of-products behaviour usually provided by algebraic data types;
- compiler and JIT scalar replacement of small relation values.

The central observation is:

```text
algebraic data type = finite sum of products
relation value      = finite set of named products
```

A relation with a statically described row shape and cardinality can therefore express many useful
ADT behaviours without adding `Option`, `Result`, or arbitrary variant objects to
`mica_var::ValueKind`.

This is not free. Mica currently knows only the outer `ValueKind::Relation`. Making the relational
encoding safe and ergonomic requires a compiler-owned structural type model, cardinality analysis,
literal singleton types, type aliases, narrowing, and direct matching over relation cells. The
runtime representation can remain an ordinary relation value at observable boundaries.

The most important performance hypothesis is that a cardinality-one relation with a statically known
heading is an aggregate the compiler can scalar-replace. A one-column relation can become one
payload register; an optional one-column relation can become a presence bit plus one payload
register; and a discriminated result can become a variant tag plus one payload register. The heap
relation is materialized only when the value escapes the optimized region.

## Goals

1. Let annotations describe the inside of first-class relation values rather than only their outer
   `relation` kind.
2. Express exact headings, per-column value types, literal discriminators, row alternatives, and
   cardinality.
3. Preserve Mica's existing rule that annotations constrain and validate values but never convert
   them.
4. Add erased structural aliases, including parameterized aliases sufficient to define `option<T>`
   and `result<T>`.
5. Give option and result values ordinary Mica value semantics: they can be nested, compared,
   persisted, serialized by the value codec, and carried across task, RPC, and IPC boundaries.
6. Add exhaustive matching and direct payload extraction without materializing row maps.
7. Keep raised errors separate from returned `result<T>` values.
8. Expose enough proof to eliminate small-relation allocation and repeated tag, heading, and kind
   checks in optimized code.
9. Preserve unannotated dynamic Mica behaviour except for the deliberately removed `one` and
   `nothing` surfaces and operations made strict by the absence audit.
10. Remove the `one` operator and the `nothing` sentinel from source rather than preserving their
    projection-dependent and context-dependent behaviour.
11. Give query cardinality, unit results, expected absence, recoverable failure, and relational
    emptiness distinct language surfaces.

## Non-Goals

- Do not add a new runtime `ValueKind` for every alias or variant.
- Do not make aliases nominal in the first implementation.
- Do not add user-defined object layouts, classes, or record mutation.
- Do not make structural annotations participate in installed verb selection in the first
  implementation.
- Do not change identity/prototype role dispatch into type overloading.
- Do not add SQL `NULL`, three-valued logic, or another universal sentinel in place of `nothing`.
- Do not preserve `one` or `nothing` through aliases, compatibility syntax, or silent lowering.
- Do not implicitly convert raised errors into result values or result values into raised errors.
- Do not promise recursive aliases or general recursive ADTs in the first implementation.
- Do not attach compatibility decoders or retain the preceding program artefact format when the
  bytecode type tables change.
- Do not optimize by weakening observable relation equality, heading, cardinality, persistence, or
  literal rendering semantics.

## Current Foundation

Mica already has most of the runtime value behaviour needed by this design:

- [`Value`](../crates/var/src/value.rs) is one tagged 64-bit word.
- [`RelationValue`](../crates/var/src/tuple.rs) is an immutable finite relation with a canonical
  symbol heading and canonical set of rows.
- relation values can be nested inside other values and can be encoded by the ordinary value codec;
- the zero-column empty relation is an ordinary relation value written `[] {}`;
- the zero-column unit relation is an ordinary relation value written `[] {[]}` and is the runtime
  foundation for the proposed `()` unit value;
- relation literals are parsed and lowered by the compiler;
- `BuildRelation` constructs relation literals in the VM;
- `project`, `natural_join`, `union`, and `difference` provide initial relation-value algebra;
- annotations currently enforce exact outer `ValueKind` contracts;
- builtin result metadata now publishes exact successful result kinds to the compiler, VM, and JIT;
- program kind facts currently track `Option<ValueKind>` for register results;
- Cranelift native loops already consume these outer-kind facts and support side exit to the
  interpreter.

The current limitation is stated directly in
[`value-kind-annotations.md`](../mdbook/src/language/value-kind-annotations.md): a `relation`
annotation does not constrain the heading or row shape. Compiler inference represents possible outer
kinds in a compact `KindSet`; HIR annotations and `CheckKind` carry `ValueKind` directly.

This project is the point at which that intentionally narrow model stops being sufficient.

## Semantic Model

### Runtime kind and static type become distinct

`ValueKind` remains the runtime classification of one Mica value. Every value described in this
document still has runtime kind `relation`.

The compiler gains a broader static type model. A working shape is:

```text
StaticType =
  Dynamic
  | Never
  | Kind(ValueKind)
  | Literal(Value)
  | Union([StaticType])
  | Relation(RelationType)
  | Parameter(TypeParameterId)
  | Alias(TypeAliasId, [StaticType])
```

`Literal(Value)` is initially restricted to persistable immediate discriminator values that have
stable source syntax, especially symbols, booleans, error codes, `()`, and explicitly written empty
relations. It should not become an unrestricted compile-time value evaluator.

An alias is resolved before runtime checks or program type facts are emitted. Alias names may be
retained for diagnostics, but they do not affect equality or representation.

The existing compact `KindSet` may remain as a fast outer-kind summary inside `StaticType`; it must
no longer be treated as the whole type.

### Relation types describe rows and cardinality

A relation type contains:

```text
RelationType {
    alternatives: [RowShape],
    cardinality: Cardinality,
}

RowShape {
    columns: sorted [(Symbol, StaticType)],
}

Cardinality {
    min: usize,
    max: Option<usize>,
}
```

The heading of each row shape is exact. Row shapes in one discriminated relation type should have
the same heading in the first implementation. This matches the runtime invariant that every row in
one `RelationValue` has the same heading and avoids inventing missing cells.

A row conforms to an alternative when every column value conforms to that alternative's column type.
A relation conforms when:

- its heading is the required heading;
- every row conforms to at least one alternative;
- its row count is within the declared cardinality.

This permits a relation to contain rows from more than one alternative when cardinality is greater
than one. For `result<T>`, cardinality is exactly one, so one discriminator selects one payload
type.

### Required discriminator support

Per-column kind constraints are not enough to express variants. Both `:ok` and `:error` have kind
`symbol`. The type model must support singleton literal constraints:

```text
:case -> :ok
:case -> :error
```

After a successful test that a row's `:case` cell equals `:ok`, the compiler narrows the row to the
alternatives compatible with that literal. Its `:value` cell can then have type `T` without another
kind check.

This is ordinary discriminated-union narrowing over relation cells. It is not a relation-kernel
query and must not perform a named-relation lookup.

### Cardinality is part of the type

At minimum the type model needs:

```text
0       empty
1       exactly one
0..1    optional
0..*    unrestricted finite relation
1..*    non-empty relation
```

The implementation should store general inclusive lower and optional upper bounds rather than an
enum containing only these spellings. Type syntax can initially expose only the useful common forms.

Cardinality is essential rather than decorative:

- an option is a zero-or-one-row relation;
- a result is an exactly-one-row relation;
- whole-value matching can bind cells directly only when at most one row is possible;
- scalar replacement needs a small static upper bound;
- truthiness of an exactly-one relation is statically true;
- truthiness of an empty relation is statically false;
- truthiness of a zero-or-one relation is exactly its presence test.

### Structural equivalence

Aliases are erased and structural:

```text
type First = relation<...>
type Second = relation<...>
```

If the expansions are identical, `First` and `Second` describe the same values. A future nominal
type feature must be separate and explicit. It must not be smuggled in through aliases.

Subtyping is initially conservative:

- a literal singleton is a subtype of its outer kind;
- a row alternative is a subtype of a compatible broader row alternative;
- a tighter cardinality interval is a subtype of a wider compatible interval;
- a relation type is a subtype of outer `relation`;
- an alias is equivalent to its expansion;
- a union member is a subtype of the union.

Do not add width subtyping for relation headings initially. A relation heading is part of canonical
value identity, so silently accepting extra columns would be surprising and could invalidate
equality and projection reasoning.

## Working Type Syntax

The examples in this section choose one coherent syntax so implementation work can be scoped. The
parser stage must still prototype it against existing symbol, map, relation-literal, range, and
annotation syntax before it is fixed.

### Structural relation types

`relation<...>` is a type constructor. An exact row shape uses the existing map-style association
syntax inside it:

```mica
relation<{:person -> identity, :name -> string}>
```

The braces describe an unordered mapping from heading symbols to cell types. This deliberately keeps
the outer type constructor distinct from relation value literals while reusing Mica's existing
`key -> value` notation.

`<` and `>` remain comparison operators in expression grammar. After a type constructor name, the
type parser treats them as contextual delimiters, as it does for `option<T>` and other parameterized
aliases. The parser prototype must cover nested closing delimiters before shifts are added to the
expression grammar.

Cardinality follows in a `where rows in` clause:

```mica
relation<{:value -> string}> where rows in 0..1
relation<{:case -> symbol, :value -> string}> where rows in 1
```

The Unicode membership spelling is also accepted when the source is comfortable using it:

```mica
relation<{:value -> string}> where rows ∈ 0..1
```

`in` is the canonical ASCII spelling in filed-out source and documentation. `∈` is exactly an
alternative spelling for `in`; it does not replace `where`, and epsilon is not a cardinality or
constraint operator. Leaving out the clause means `0..*`.

Literal values are singleton types in a type position, so discriminators need no additional
operator:

```mica
relation<{:case -> :ok, :value -> string}> where rows in 1
```

Discriminated row alternatives use `|` inside the relation type:

```mica
relation<
  {:case -> :ok, :value -> string}
  | {:case -> :error, :value -> error}
> where rows in 1
```

This syntax deliberately distinguishes a union of allowed row alternatives from a future union of
whole value types. The compiler representation should support both even if only relation-row
alternatives are exposed first.

### Type references and aliases

Type names remain contextual rather than global keywords. Proposed declarations are:

```mica
type PersonName = relation<{:person -> identity, :name -> string}>

type option<T> = relation<{:value -> T}> where rows in 0..1
```

The first release should support:

- non-recursive aliases;
- type parameters used only inside the alias body;
- explicit type-argument arity;
- forward references within one declaration unit if cycle detection remains clear;
- exact diagnostics for unknown aliases, unknown parameters, cycles, and wrong arity.

Parameterized aliases do not imply generic runtime functions, monomorphized code, reified runtime
types, or user-defined constructors. They are compiler substitution over type expressions.

### Alias availability in a live world

File-local aliases are insufficient for installed verbs and functions compiled by later fileins. The
compiler environment must be able to resolve installed aliases.

The implementation must choose one of these coherent models before alias syntax lands:

1. Store canonical alias declarations as ordinary live-world program metadata, load them into the
   compiler context, and include them in fileout.
2. Restrict aliases to one compilation unit and keep only a very small compiler-predefined standard
   alias set.

The first model is more consistent with Mica's live programmable world. It requires catalogue
storage, authority and replacement semantics, dependency invalidation, filein/fileout support, and
cycle checking across installed aliases. The second model is a useful implementation slice but is
not an adequate final language feature.

No compatibility representation is needed. Store one canonical current alias form.

## Unit, Absence, Failure, And Empty Relations

Mica currently overloads the zero-column empty relation, rendered as `nothing`, across unrelated
meanings. It is returned by empty `one`, missing functional dot reads, missing indexes, absent
actor/principal/environment values, some parsers, bare returns, fallthrough, and side-effect-only
builtins. Applications then use it as an untyped local sentinel and store it in otherwise unrelated
relation cells.

The Stage 0 application inventory contains 76 parsed query-shaped `one` expressions and 630 parsed
`nothing` literals across `apps/`. The retained audit tool masks filein-only `grant` blocks before
parsing and reports no parse errors. These are established surfaces rather than a coherent language
model. No compatibility constraint requires retaining them.

The replacement model is:

| Meaning | Language result | Runtime relation representation |
| --- | --- | --- |
| No meaningful result | `()` with static type `unit` | `[] {[]}` |
| Expected absence | `none` / `some(value)` with type `option<T>` | zero or one `:value` row |
| Recoverable failure | `ok(value)` / `err(error)` with type `result<T>` | one discriminated row |
| Violated operation contract | raised structured error | existing error control flow |
| An actually empty relation | `[] {}` | zero-column, zero-row relation |

These meanings must not implicitly convert into one another. In particular, `[] {}` is not unit,
none, null, false, failure, an omitted argument, or a missing field merely because it is falsey.

### Unit

Add `unit` as the structural type of the zero-column, exactly-one-row relation and `()` as its
canonical source spelling:

```mica
type unit = relation<{}> where rows in 1
```

Bare `return`, normal fallthrough, and side-effect-only operations return `()`. A function with a
non-unit result contract must continue to prove that every reachable normal exit returns that type.
Unit is truthy because it contains one row; code must not use it as an absence test.

The ordinary relation literal `[] {[]}` remains structurally equivalent to `()`. The formatter and
fileout should prefer `()` when the value is used as a scalar, while relation-oriented diagnostics
may show the expanded literal when shape detail matters.

### Remove `nothing`

Remove `nothing` from source syntax and stop rendering `[] {}` as `nothing`. Rename internal helpers
such as `Value::nothing` to describe the representation, for example `Value::empty_relation`, so
new runtime code does not accidentally recreate sentinel semantics.

This is an intentional breaking change. Do not retain `nothing` as an alias for `[] {}`, `none`, or
`()`. Authors who genuinely need the zero-column empty relation can write `[] {}`. Existing uses
must be classified and migrated according to their meaning.

### Strict, optional, and failing APIs

Default operations should be strict when absence indicates a violated contract:

- list and map indexing raises a focused bounds/key error when the entry is missing;
- functional dot reads raise a cardinality error when the required fact is absent;
- extracting a frob delegate from a non-frob raises `E_TYPE`;
- exact query binding raises a cardinality error for zero or multiple rows.

Expected absence uses `option<T>` through explicit APIs or binding forms:

- collection `get` returns an option;
- `get_or` accepts an explicit fallback;
- actor, principal, environment, and optional error fields return options when absence is valid;
- optional query binding branches on zero or one row and raises only when the result is ambiguous.

Parsing and native operations that can explain a failure should generally return `result<T>` or
raise a documented error rather than erase the reason into absence. Stage 0 must classify every
current sentinel-returning builtin before implementation begins; it must not mechanically wrap all
of them in `option<T>`.

## Option And Result As Standard Relation Aliases

### Option

The standard option type is structurally:

```mica
type option<T> = relation<{:value -> T}> where rows in 0..1
```

Its canonical runtime values are ordinary relation values:

```mica
[:value] {}                 -- none
[:value] { [42] }           -- some(42)
[:value] { [()] }           -- some(())
[:value] { [[] {}] }        -- some(the zero-column empty relation)
```

This preserves distinctions that a universal sentinel cannot preserve. In particular:

```text
none
some(())
some(none)
```

are distinct values. The last value stores the empty `[:value]` option relation as the cell of the
outer one-row relation.

The empty option value has no reified `T`. It can flow into any `option<T>` expected by an annotated
boundary, just as an empty collection can be typed from context. Outside an expected type, its
inferred payload type is `Never`.

### Result

Mica already has a structured `error` value kind with an open `E_*` code namespace, optional
message, and optional payload. The initial standard result therefore needs one success type
parameter and uses Mica `error` as its failure payload:

```mica
type result<T> = relation<
  {:case -> :ok, :value -> T}
  | {:case -> :error, :value -> error}
> where rows in 1
```

Its canonical runtime values are:

```mica
[:case, :value] { [:ok, 42] }
[:case, :value] { [:error, err] }
```

A later general `either<L, R>` alias can express arbitrary two-sided values if a real use appears.
Do not make `result<T, E>` generic over arbitrary failure payloads merely to imitate another
language; Mica already has a coherent error value.

### Standard constructors

Working constructor names are:

```mica
none
some(value)
ok(value)
err(error)
```

The exact spelling remains a language-design decision. Whatever names are chosen, they should be
compiler-known ordinary constructors with exact structural result facts. They must not call a
dynamic builtin that constructs a general relation and then asks the compiler to rediscover its
shape.

Possible implementation levels are:

1. compiler intrinsics that lower directly to `BuildRelation` while publishing structural facts;
2. ordinary installed functions whose signatures have exact structural result types;
3. dedicated bytecode only when measurement shows it improves construction or unboxing.

Start with compiler intrinsics or statically described standard functions. Dedicated runtime
representations belong to the optimization stages, not the semantic stage.

### Raised errors remain distinct

These are different operations:

```mica
raise E_NOT_FOUND, "not found", item
err(error_value)
```

The first leaves normal control flow and enters the existing catch/recover mechanism. The second
returns one ordinary relation value. A function returning `result<string>` may still raise unless a
separate future error-effect annotation says otherwise.

No constructor or match operation implicitly catches raised errors. A future expression such as
`attempt expression` could explicitly capture a raised error as `result<T>`, but that is separate
from the initial result alias.

### JSON is not the value codec

Option and result values are already serializable by the Mica value codec because they are relation
values. They can cross task, RPC, and IPC boundaries and can be persisted when their cells are
persistable.

Do not automatically map `option<T>` to JSON `null`. Remove the current implicit mapping between
the zero-column empty relation and JSON `null`; it is another instance of the sentinel problem.
JSON APIs should use an explicit tagged representation or explicit conversion helpers so `none`,
`some(())`, an empty relation payload, and nested options remain distinguishable.

## Matching, Unwrapping, And Propagation

### Exhaustive `match` is the core operation

The preferred core surface is an expression-valued `match` with patterns that bind relation cells
directly:

```mica
match maybe_name
case some(name)
  return name
case none
  return "anonymous"
end
```

```mica
match loaded
case ok(text)
  return string_len(text)
case err(problem)
  raise problem.code, problem.message, problem.value
end
```

For the standard aliases, `some`, `none`, `ok`, and `err` are pattern sugar over structural relation
patterns. They do not require nominal runtime constructors.

The general structural pattern syntax remains to be prototyped. A fully explicit form could be:

```mica
match loaded
case relation<{:case -> :ok, :value -> text}>
  return text
case relation<{:case -> :error, :value -> problem}>
  raise problem.code, problem.message, problem.value
end
```

The compiler must lower these patterns to:

- a cardinality check when it is not already proven;
- a heading or shape check when it is not already proven;
- a discriminator comparison;
- direct cell loads into bindings.

It must not create a symbol-keyed map for the row.

### Exhaustiveness and narrowing

When the scrutinee has a closed structural type, the compiler checks that every alternative is
covered. An omitted alternative is a compile error unless a wildcard case is present.

When the scrutinee is dynamic, a wildcard case is required or the match raises a focused
non-exhaustive-match error. Pattern bindings receive the narrowed payload types. Branch result types
are joined using the ordinary static type union operation.

A guard may further refine a pattern, but a guarded case does not by itself exhaust the unguarded
alternative.

### `if` binding sugar

After `match` is sound, an option-specific convenience can lower to it:

```mica
if some(name) = maybe_name
  emit(actor, name)
else
  emit(actor, "anonymous")
end
```

Do not implement separate semantics for this form. It is a two-branch match.

### Defaults and strict extraction

Option should support an explicit default operation without raising:

```mica
let name = maybe_name else "anonymous"
```

The exact operator syntax is unsettled; a standard `option_or(maybe_name, "anonymous")` function is
an acceptable first implementation if it receives structural builtin result metadata and avoids
dynamic dispatch.

An unchecked unwrap that traps is not required. If a caller wants absence to be an error, the source
should say which Mica error to raise, either with a match or a focused helper.

### Propagation operator is a later convenience

A postfix propagation operator could eventually mean:

```text
option<T>?  -> extract T or return none from the enclosing option-returning function
result<T>?  -> extract T or return the unchanged err relation from the enclosing
              result-returning function
```

This operator should not land with the first matcher. It requires:

- an enclosing result contract known to the compiler;
- exact rules for converting or preserving aliases;
- diagnostics when the enclosing result shape is incompatible;
- a decision about interaction with closures and installed verbs;
- parser validation against existing prefix `?query` and optional-parameter syntax.

It must not catch raised errors.

## Query Cardinality And Lexical Binding

Remove `one` rather than changing what it returns. A query expression always produces a relation
value with a heading derived from its free query-variable names. It never automatically becomes a
scalar, list element, or binding map.

Cardinality is measured on that canonical projected answer relation, after duplicate projected rows
have collapsed. It is not the count of hidden source derivations. The binding forms accept query
expressions, including relation algebra whose result heading remains statically known; they are not
restricted to a single named-relation call.

Cardinality and cell extraction belong to explicit binding contexts. They should not create a
second, query-only binding grammar. Ordinary lexical bindings put the pattern to be bound on the
left and the expression producing the value on the right:

```mica
let PATTERN = EXPRESSION
if let PATTERN = EXPRESSION
for PATTERN in EXPRESSION
```

Relation rows need a named structural pattern in that same pattern grammar. The current candidate
uses braces, with field punning from `label` to `:label -> label` and the existing map association
spelling for renaming:

```mica
{label}
{:label -> sensor_label}
{name, value_kind}
```

With that pattern, the cardinality forms become:

```mica
-- Exactly one row. The row pattern introduces lexical bindings.
let exactly {label} = Label(#sensor, ?label)
return label

-- Zero or one row. More than one is still a cardinality error.
if let {next} = NextView(current, ?next)
  return some(next)
else
  return none
end

-- Any number of rows. The ordinary loop pattern is bound once per row.
for {work} in AssignedTo(?work, assignee)
  emit(actor, work)
end
```

This gives query code three visibly different cardinality contracts:

| Form | Accepted rows | Binding lifetime | Zero rows | Multiple rows |
| --- | ---: | --- | --- | --- |
| `let exactly PATTERN = Query(...)` | exactly 1 | enclosing block | raise `E_CARDINALITY` | raise `E_CARDINALITY` |
| `if let PATTERN = Query(...)` | 0..1 | selected branch | take `else` | raise `E_CARDINALITY` |
| `for PATTERN in Query(...)` | 0..* | one iteration | no iterations | iterate every row |

The cardinality error should retain the query result and report expected and actual row counts. A
more specific diagnostic may distinguish missing and ambiguous results, but the language construct
has one cardinality contract.

On the right, each free `?name` declares a projected heading column. It does not also introduce a
lexical binding. Repeated occurrences of the same query variable continue to mean relational
equality. On the left, the row pattern introduces lexical bindings using the same collision and
shadowing rules as scatter binding. This keeps existing locals, repeated query variables, nested
query algebra, and renamed cells explicit instead of deriving scope from identifiers buried in an
expression.

Multi-column queries bind multiple locals directly:

```mica
let exactly {name, value_kind} = CommandArgument(command, position, ?name, ?value_kind)
return [name, value_kind]
```

There is no scalar-for-one-column/map-for-many rule and no row-map allocation. Code that wants the
first-class relation keeps it explicitly:

```mica
let answers = CommandArgument(command, position, ?name, ?value_kind)
return answers
```

Existence tests remain ordinary relation truthiness and do not bind projected cells:

```mica
if Enabled(package)
  return true
end
```

The old `for row in Query(...)` binding-map query idiom should migrate to structural row patterns.
The same `for PATTERN in EXPRESSION` form handles lists, maps, and relations; the pattern and static
type determine what one iteration binds. Relation iteration should load cells directly from the
known heading and must not allocate a map for each row. Do not retain the map-producing query loop
as a compatibility surface.

### Binding harmony has a boundary

Scatter binding and structural relation-row binding should be members of one lexical `Pattern`
model because both destructure a produced value and introduce scoped locals. Match cases should use
that model too.

Rule variables should not be forced into the lexical pattern model. A rule describes a set of
logical bindings shared across order-independent atoms; it is not assignment in source order.
Task-code `?name` and rule variables should nevertheless have one documented logical-variable
model, even if their contextual spelling differs.

Verb role restrictions should also remain separate. `actor @ #reviewer` participates in method
applicability and specificity before the body runs; it does not destructure an argument. Reusing a
single internal or surface "matcher" for row patterns, rule unification, and verb dispatch would
hide materially different semantics.

There is still a punctuation audit to perform. `?` currently means both a task query variable and
an optional parameter or scatter part. `@` means rest/splice in value syntax but prototype
restriction in a verb parameter. These uses are contextually parseable, but parseability alone is
not enough: the syntax prototype must test whether readers can predict each meaning without
remembering which compiler subsystem owns the construct.

### Functional dot reads are strict

A functional binary relation proves at most one value for a complete key, not that a value exists.
Dot reads should nevertheless be strict property assertions:

```mica
let label = #sensor.label
```

This returns the scalar cell when the fact exists and raises `E_CARDINALITY` when it does not. The
functional constraint prevents ambiguity. Optional code uses the underlying query explicitly:

```mica
if let {label} = Label(#sensor, ?label)
  use(label)
else
  use("unnamed")
end
```

Do not add an optional-dot sentinel or postfix punctuation that conflicts with future result
propagation. Dot assignment remains replacement sugar for the declared functional relation.

### `one` is not a collection operator

The current VM also applies `one` to lists, unwraps a one-entry map row, and returns the empty
relation for unsupported input. Remove all of that behaviour with the operator. Lists, maps, and
ranges use explicit collection APIs with strict and optional forms; query cardinality syntax is not
a generic collection convenience.

## Matching Is Not Verb Dispatch

Value-kind annotations currently do not affect installed verb applicability or specificity.
Structural relation annotations should preserve that rule initially.

This remains valid:

```mica
verb inspect(value @ #relation: result<string>)
  -- The role restriction selected the method.
  -- The result<string> annotation validates and narrows the value after selection.
end
```

Two installed methods must not become overloads merely because their structural annotations differ.
Adding relation-shape dispatch would affect method indexing, specificity, ambiguity, persistent
dispatch facts, and cache keys. It is not required to obtain exhaustive local matching or optimized
option/result handling.

Possible later dispatch-like surfaces are:

- multiple local function clauses compiled to one `match`;
- pattern parameters on non-installed functions;
- an explicit `match` in one installed method;
- a future structural dispatch restriction with its own syntax and cache design.

If structural dispatch is ever added, it must test the immediate relation value or a cached shape
summary. It must never issue relation-kernel queries or inspect durable world facts per call.

## Static Inference And Runtime Enforcement

### Preserve the current annotation contract

For a structural annotation boundary:

1. A proven subtype emits no runtime check.
2. A proven disjoint type is a compile error.
3. A dynamic or partially known source emits one structural check.
4. The check validates without converting or wrapping the value.
5. Failure raises catchable `E_TYPE` and retains the offending value.
6. Function and verb result annotations remain proof-only.

An outer `CheckKind(Relation)` is insufficient for case 3. Add a structural check that references a
canonical type/shape descriptor in the program artefact. The exact opcode could be
`CheckRelationType`, but the name should follow the final descriptor model.

### Structural checking must not repeatedly scan large relations

A naive dynamic check is `O(rows * columns)`. That is acceptable for a one-row result but not for a
large query answer crossing a loop or function boundary repeatedly.

`RelationValue` is immutable, so validated structural facts cannot become stale. Candidate runtime
support includes:

- compute per-column outer-kind masks during relation construction;
- retain row count and canonical heading, which are already directly available;
- lazily compute discriminator value sets or row-alternative summaries;
- cache a compact structural summary on the heap relation;
- cache successful checks by structural shape identifier when useful;
- let producers such as literals, projections, joins, and typed constructors attach trusted facts
  without rescanning.

Do not store alias identity in the value. Cached data describes structure only. Compiler or
program-local shape identifiers must not leak into persistent value encoding.

### Inference through relation construction

For a relation literal, infer:

- exact canonical heading;
- exact literal row count after duplicate removal when every cell is constant;
- otherwise a conservative cardinality upper bound based on source rows;
- one column type per row alternative;
- literal singleton cells where useful for discrimination;
- joined cell types when rows share a non-discriminated shape.

Literal duplicate removal can lower cardinality. The type checker must not claim exactly two rows
from two source rows unless it can prove they differ.

### Inference through relation algebra

Inference should begin conservative and improve operation by operation:

- `project` computes the projected heading; it cannot increase cardinality but may reduce it through
  duplicate removal;
- `natural_join` combines headings and row alternatives; its maximum cardinality is bounded by the
  product of finite input maxima;
- `union` requires identical headings and joins row alternatives; its maximum is at most the sum of
  finite input maxima;
- `difference` preserves the left heading and upper bound but usually lowers the minimum to zero;
- predicate filters preserve the upper bound and usually lower the minimum to zero;
- iteration over a typed relation gives each row the union of its row alternatives;
- a discriminator test narrows the current row alternative;
- a functional named-relation query with fully bound key positions can eventually infer `0..1`.

Named relations currently declare arity and functional keys, not column names or cell types. The
first implementation should type first-class query result headings from free-variable names while
leaving their cell types dynamic unless another proof exists. Typed named-relation declarations are
a possible later project, not a prerequisite for typed relation literals and standard aliases.

### Builtin and call result metadata

`BuiltinResultKind::Exact(ValueKind)` is sufficient for scalar builtins but cannot describe a
structural relation result. Evolve the compiler-facing contract to distinguish:

```text
Dynamic
ExactKind(ValueKind)
ExactType(StaticType or stable type descriptor)
```

The runtime VM validation needs only the descriptor required to validate an externally supplied
result. Compiler intrinsics and ordinary bytecode constructors can publish structural facts without
calling the builtin registry.

Installed function and verb result aliases erase to canonical structural types in compiled metadata.
Callers should not need the alias declaration merely to execute already compiled bytecode, but
source compilation and diagnostics do need the alias environment.

## Representation And Optimization

### Correctness representation

The initial implementation uses ordinary `RelationValue` at every observable boundary. This keeps:

- canonical equality and ordering;
- source literal round trips;
- value-codec serialization;
- persistence and nested-value traversal;
- task and host ownership;
- ordinary garbage collection through shared heap values.

Correctness stages should not add a second publicly observable option/result representation.

### Construction cost that must be measured

The current `BuildRelation` path:

1. resolves and collects the heading;
2. resolves all cells;
3. builds `Tuple` rows;
4. calls `Value::relation`;
5. canonicalizes heading and rows and removes duplicates;
6. allocates the heap relation.

For a statically known cardinality-one relation, much of that work is redundant. Before changing
representation, benchmark each component and retain a general relation baseline.

### Tier 1: known-shape boxed relations

When a small relation must escape as a `Value`, a known structural descriptor can avoid repeated
general construction work:

- use an interned canonical heading;
- skip heading sorting when the compiler already emits canonical order;
- skip row sorting and duplicate removal for cardinality at most one;
- construct one compact row directly;
- reuse one canonical empty option relation;
- cache or precompute the structural summary.

This tier retains one heap allocation for a present option or result unless the current heap model
can safely embed it elsewhere.

### Tier 2: local construction and match elimination

The compiler can eliminate a relation that is constructed and immediately matched without needing
general escape analysis:

```mica
let answer = some(value)
match answer
case some(inner)
  use(inner)
case none
  fallback()
end
```

This becomes a direct binding of `inner = value`; the absent branch is unreachable. Equivalent
constant folding applies to `none`, `ok`, and `err`.

This is the smallest experiment proving that structural facts can remove both allocation and
branching.

### Tier 3: scalar replacement

For non-escaping values, use virtual representations:

```text
relation<{:value -> T}> rows 1
    -> payload: T

option<T>
    -> present: bool
       payload: T

result<T>
    -> case: small tag
       payload: T or Error

relation with N columns and rows 1
    -> N payload registers
```

The heading and alias are compile-time shape metadata. Projection becomes register selection.
Matching becomes a presence or discriminator branch. No `RelationValue`, `Tuple`, heading vector,
row vector, `Arc`, or reference-count operation is needed inside the optimized region.

The compiler must not represent an absent option by reading an uninitialized payload. The presence
bit guards every payload use, and side-exit materialization ignores the payload when absent.

### Escape and materialization

A virtual relation must materialize when it:

- enters dynamically typed code;
- is passed to a call using the ordinary one-word `Value` ABI;
- is stored in a list, map, frob payload, error payload, or relation cell;
- is returned through an unspecialized call boundary;
- crosses task, RPC, IPC, or host boundaries;
- is compared with a dynamically shaped relation;
- is rendered, serialized, hashed, or persisted;
- is live at a JIT side exit whose interpreter state expects a boxed register value.

Cranelift side-exit metadata therefore needs materialization recipes such as:

```text
register r7 = relation shape S17 from [native value v3]
register r9 = optional shape S22 from [present p1, payload v8]
register r4 = result shape S31 from [case t2, payload v6]
```

Materialization must reproduce the exact canonical `RelationValue` that unoptimized execution would
have produced.

### Interpreter strategy

The interpreter currently stores one ordinary `Value` per register. Full scalar replacement across
arbitrary calls would require either virtual aggregate registers or compiler lowering that assigns
the scalar components to ordinary hidden registers.

Use staged experiments:

1. eliminate constructor-to-match pairs in compiler lowering;
2. add direct cardinality, discriminator, and cell-load opcodes for boxed small relations;
3. represent non-escaping typed relations as hidden component registers within one function;
4. add specialized call conventions only if measured call-heavy workloads justify them.

Do not broaden every VM register into a multi-word tagged union merely to optimize option values.

### JIT strategy

The JIT is the natural first home for general scalar replacement because Cranelift already carries
native scalar values and fast-path predicates. Required additions are:

- structural program facts rather than only `Option<ValueKind>`;
- virtual relation state in the native-loop planner;
- direct presence and discriminator branches;
- cell values carried as native `Value` words or unboxed primitive scalars;
- escape/materialization recipes;
- side-exit tests proving interpreter-visible state is exact;
- no helper or side exit on the steady success path when the shape is proven.

A result whose success payload has exact kind `int` can carry an unboxed integer on the success
path. The error alternative may remain a boxed `Value` because it is cold. Branch layout should
favour the measured common alternative rather than blindly favouring `:ok` by spelling.

### Branch and dispatch cost

Avoid replacing heap allocation with repeated structural interrogation:

- one shape or discriminator guard should dominate all payload uses in a branch;
- narrowed bindings retain their facts;
- repeated `some`/`ok` tests should common-subexpression eliminate;
- matching must load cells directly, not call generic map indexing;
- standard constructors and matchers must avoid installed verb dispatch;
- static success paths should not query alias metadata at runtime;
- a closed exhaustive match should not retain a wildcard dynamic fallback.

For small hot loops, benchmark branch prediction under both predominantly-present and
predominantly-absent distributions. An allocation win that creates an unpredictable branch per
operation may still regress realistic workloads.

### Concurrency and allocation pressure

Removing heap relations may reduce allocator traffic, atomic reference counts, and cache-line
bouncing. Measure this rather than assuming it.

Every construction, matching, and propagation benchmark should have:

- a single-worker variant;
- internally coordinated multi-worker variants;
- present/success-heavy and absent/error-heavy distributions;
- escaping and non-escaping variants;
- interpreter and JIT variants where available.

Run independent benchmark binaries serially. Do not run supposedly comparable benchmark processes at
the same time.

## Benchmark Plan

Establish baselines before correctness work changes representation:

1. Build and drop `[:value] {}`.
2. Build and drop `[:value] { [int] }`.
3. Build and drop `[:case, :value] { [:ok, int] }`.
4. Build and drop `[:case, :value] { [:error, error] }`.
5. Branch on empty/non-empty and extract one cell.
6. Branch on `:ok`/`:error` and extract one cell.
7. Construct, return, and consume across one function boundary.
8. Store each value in a list, map, and relation cell to force escape.
9. Serialize and deserialize each value through the Mica codec.
10. Compare with a small prototype carrying presence/tag plus payload without a relation allocation.

Report:

- wall time and throughput;
- allocations and allocated bytes per operation;
- reference-count operations where measurable;
- branch misses where platform tooling permits;
- JIT side exits and helper calls;
- scaling across worker counts;
- relative cost against plain scalar return and raised-error baselines.

The Stage 0 prototype is a decision gate. If a relation-backed observable value is cheap enough but
scalar replacement produces no meaningful gain in representative code, do not build a large escape
analysis system solely for Option.

## Incremental Implementation Stages

### Stage 0: Syntax and representation prototypes

- Prototype the proposed relation type grammar without accepting it in executable source.
- Prototype `()`, structural row patterns, `let exactly PATTERN = Query(...)`,
  `if let PATTERN = Query(...)`, and `for PATTERN in Query(...)` against the existing block,
  binding, query-variable, and iteration grammar.
- Confirm it does not destabilize symbols, maps, relation literals, ranges, optional parameters,
  query variables, or return annotations.
- Add a standalone/internal benchmark comparing current boxed one-row relations with
  presence/tag-plus-payload representations.
- Classify every existing `nothing`-returning builtin, `one`, dot read, missing index, bare return,
  and application sentinel as unit, option, result, raised failure, or an actual empty relation.
- Include internal and durable uses of the empty relation as a dispatch restriction, omitted error
  field, register placeholder, serialized value, or system-relation sentinel; source migration alone
  is not a complete absence audit.
- Record and group the parsed forms behind the current 76 query-shaped `one` expressions and 630
  `nothing` literals so the eventual app migration is reviewable rather than mechanical.
- Decide alias persistence scope for a live world.
- Decide the exact result failure payload contract.

Exit criteria:

- reviewed grammar examples;
- reproducible serial and concurrent benchmark baselines;
- explicit decision to proceed with structural relation types independently of optimization wins;
- no source syntax accepted but ignored.

Suggested commit if the benchmark harness is worth retaining:

```text
bench(vm): measure small relation value representations
```

### Stage 1: Compiler structural type core

- Introduce compiler-owned `StaticType`, row shape, relation type, cardinality, and type-parameter
  representations.
- Preserve a compact outer-kind summary for fast existing inference.
- Add canonicalization, union, intersection, subtype, disjointness, and diagnostic rendering.
- Represent literal singleton types for the approved discriminator values.
- Infer structural types for relation literals internally.
- Keep current source annotations restricted to exact value kinds during this stage.

Verification:

- canonical heading order does not affect type equality;
- incompatible headings are disjoint;
- cardinality subtyping is correct;
- row alternative narrowing is correct;
- `none`, `some(())`, `some([] {})`, and nested option shapes remain distinct;
- existing kind inference and JIT facts remain unchanged externally.

Suggested commit:

```text
feat(compiler): model structural relation types
```

### Stage 2: Relation type syntax and enforcement

- Parse unresolved general type expressions in every currently supported annotation position.
- Resolve exact kinds and relation types into `StaticType`.
- Add structural relation check descriptors and one explicit VM check operation.
- Change the program artefact format in place.
- Preserve proof, mismatch, and dynamic-check behaviour from exact kind annotations.
- Add runtime structural summaries or caching sufficient to prevent obvious repeated full scans.

Verification:

- exact heading, cell kind, literal discriminator, row alternative, and cardinality success/failure;
- catchable `E_TYPE` with the unchanged offending relation;
- no check for proven relation literals;
- compile failure for proven mismatches;
- repeated checks over one immutable large relation do not repeatedly scan every cell;
- artefact round trip and stale-format rejection.

Suggested commit:

```text
feat(vm): enforce structural relation contracts
```

### Stage 3: Relation operation inference

- Publish structural program facts alongside outer kind facts.
- Infer headings and conservative cardinalities through projection, join, union, difference, scans,
  iteration, and discriminator tests.
- Preserve typed row facts through iteration without materializing row maps for direct structural
  matches.
- Extend compiler and runtime builtin result metadata where structural results are stable.
- Feed shape facts to JIT planning without changing representation yet.

Verification:

- operation-by-operation type and cardinality tests;
- differential tests comparing inferred upper bounds with observed result sizes;
- no structural fact survives a control-flow merge unless valid on every incoming path;
- dynamic operations fall back conservatively rather than claiming a false shape.

Suggested commit:

```text
feat(compiler): infer relation shapes through relational operations
```

### Stage 4: Structural type aliases

- Add non-recursive parameterized alias declarations.
- Resolve, canonicalize, and erase aliases.
- Implement the chosen installed-alias environment and filein/fileout behaviour.
- Track alias dependencies so replacement invalidates affected source compilation caches.
- Keep runtime equality and dispatch independent of alias names.

Verification:

- local and installed alias resolution;
- parameter substitution and nested aliases;
- unknown names, wrong arity, duplicate parameters, and direct/indirect cycle diagnostics;
- replacement and fileout round trips;
- two structurally equal aliases interoperate without checks or conversion.

Suggested commit:

```text
feat(compiler): add structural type aliases
```

### Stage 5: Standard option and result types

- Add the `unit` structural alias and canonical `()` value.
- Install or predefine `option<T>` and `result<T>` aliases.
- Add the chosen standard constructors with exact structural result facts.
- Add literal rendering and codec round-trip tests, relying on existing relation encoding.
- Add explicit conversion helpers at JSON boundaries rather than implicit `null` coercion.
- Publish exact structural result facts for the unit-, option-, and result-returning APIs selected by
  the Stage 0 audit.

Verification:

- bare return, fallthrough, and side-effect-only operations produce `()`;
- none, some, nested options, ok, and err construction;
- `some(())` and `some([] {})` differ from none;
- returned err differs from a raised error;
- persistence, task, RPC, and IPC round trips;
- non-persistable nested values retain current rejection behaviour;
- concurrent construction baselines remain recorded.

Suggested commit:

```text
feat(runtime): define relational option and result values
```

### Stage 6: Exhaustive relation matching

- Add expression-valued `match`.
- Add general structural relation patterns and standard option/result pattern sugar.
- Implement exhaustiveness, wildcard, guards, binding, and flow narrowing.
- Lower matches to cardinality/shape guards and direct cell loads.
- Add structural row patterns to ordinary `let`, `if let`, `for`, and `match` binding positions,
  with cardinality checks and no row-map allocation.
- Make functional dot reads strict while retaining dot assignment as functional replacement sugar.
- Add `if` binding sugar only after core matching is complete.
- Defer postfix propagation until matching and result contracts have real app usage.

Verification:

- exhaustive and non-exhaustive static matches;
- dynamic wildcard requirements;
- direct payload kind facts in each case;
- no row-map allocation;
- no installed verb dispatch or relation-kernel query during matching;
- expression result type joins;
- nested match and catch/recover interaction.

Suggested commit:

```text
feat(compiler): match structural relation variants
```

### Stage 7: Small relation optimization

Implement only optimizations justified by Stage 0 and current end-to-end benchmarks:

1. known-shape boxed construction;
2. constructor-to-match elimination;
3. direct boxed match opcodes;
4. JIT scalar replacement in one bounded loop/function region;
5. side-exit materialization;
6. interpreter hidden-register scalarization if it pays off;
7. specialized call conventions only after a separate experiment.

Each optimization lands with semantic differential tests and single/concurrent benchmarks. Do not
combine the first correct matcher with allocation elimination in one review.

Possible commits should describe the measured mechanism rather than claim generic ADT speedups, for
example:

```text
perf(jit): scalar-replace non-escaping optional relations
```

### Stage 8: API migration and documentation

- Remove `one`, `nothing`, their syntax nodes, and their bytecode/runtime behaviour without aliases
  or compatibility lowering.
- Apply the Stage 0 classification to functional dot reads, indexing, parsing helpers, environment
  lookup, contextual values, error fields, host requests, JSON decode, bare returns, builtins, and
  application sentinel maps.
- Rename internal empty-relation helpers so representation names do not imply absence or unit.
- Migrate applications coherently to query binding, `()`, option/result matching, strict errors, or
  explicit `[] {}` as appropriate.
- Document structural aliases, option/result, matching, raised errors, and JSON boundaries.
- Document unit, query cardinality binding, strict indexing and dot reads, and the absence of a
  universal sentinel.
- Add app examples that exercise nested options and result handling without unnecessary annotations.

Do not treat every possible error as `result<T>`. Ordinary programming errors and exceptional task
failures should continue to raise.

## Test Matrix

### Type-system tests

- exact and dynamic outer relation kinds;
- exact headings and canonical heading order;
- missing and extra columns;
- per-column exact kinds;
- literal singleton constraints;
- row alternative union, narrowing, and exhaustiveness;
- cardinality interval subtype and disjointness;
- alias substitution, erasure, cycles, and diagnostics;
- control-flow joins and unreachable paths;
- result annotations with raised paths excluded from normal result types.

### Runtime tests

- structural check success and failure;
- large immutable relation check caching;
- relation literal and constructor equivalence;
- canonical equality and ordering;
- nested option and result traversal;
- value codec and program artefact round trips;
- persistence and non-persistable payload rejection;
- exact interpreter instruction budgeting for new checks and matches.

### JIT tests

- boxed and scalarized execution produce identical values;
- no side exit on proven common paths;
- side exits materialize exact interpreter register values;
- absent payload storage is never read;
- error alternatives preserve the original error value;
- branch-heavy distributions remain semantically identical;
- helper and side-exit counts match the intended fast path.

### App tests

- optional lookup with a default;
- strict lookup raising a chosen error;
- returned result handled exhaustively;
- raised error caught separately from returned err;
- `some(())`, `some([] {})`, and nested option handling;
- exact, optional, and iterative query binding without row maps;
- bare return and fallthrough unit results;
- serialized task/RPC response;
- explicit JSON conversion.

## Resolved Syntax Decisions

- Relation cardinality uses `where rows in CARDINALITY`. Source may use `∈` in place of `in`.
  Filed-out source and documentation use ASCII `in`. Neither spelling replaces `where`, and epsilon
  has no special meaning.

## Open Design Questions

1. Is the proposed `relation<{:column -> type}>` syntax readable enough beside map literals and
   symbol keys?
2. Are row alternatives required in the first syntax release, or can cardinality and homogeneous
   column types land first?
3. Should aliases be installed live-world definitions immediately, or may the first slice be
   compilation-unit-local while the standard aliases are compiler predefined?
4. Should the standard result failure discriminator be `:error`, `:err`, or another symbol?
5. Are `some`, `none`, `ok`, and `err` acceptable constructor and pattern names?
6. Should `result<T>` always carry a structured Mica `error`, or is a general `either<L, R>` enough
   for applications needing other failure payloads?
7. Is `{label}` the right field-punning structural row pattern, and can it join the existing scatter
   `Pattern` model without making map, relation, and collection patterns visually misleading?
8. Should task queries and rules use one logical-variable spelling, and should optional
   parameter/scatter syntax change so `?name` has only that logical meaning?
9. Should rest/splice and verb prototype restriction continue to share `@`, or should the syntax
   reserve distinct punctuation for those unrelated operations?
10. Should matching a dynamic relation with no matching case raise `E_TYPE`, a new match error, or a
    more specific structural error?
11. Which structural summaries belong in `RelationValue`, and which should remain external caches?
12. Can JIT materialization reuse canonical shape data without introducing process-local identifiers
    into persistent values?
13. Does constructor-to-match elimination deliver enough real improvement to justify general escape
    analysis?
14. Should a future postfix propagation operator work only for the standard aliases or for any
    structurally compatible relation type?
15. Is nominal distinction ever needed, or are explicit discriminator cells sufficient for Mica's
    domain modelling?
16. Should `()` always render instead of `[] {[]}` outside explicitly relation-oriented output?

## Decision Gates

Do not proceed from syntax exploration to a broad implementation merely because the relational
encoding is elegant.

Proceed to structural annotation enforcement when:

- relation shape and cardinality semantics are reviewed independently of Option and Result;
- unit, empty relation, option, result, and raised failure remain observably distinct;
- the query binding grammar has no unresolved scope or parser ambiguity;
- dynamic-check cost has a credible immutable-summary design;
- alias persistence has one coherent live-world model;
- the syntax can express discriminators without colliding with current Mica forms.

Proceed to standard Option and Result when:

- nested values and serialization are proven;
- matching can be implemented without row-map allocation;
- raised errors remain clearly distinct;
- at least two real APIs benefit from each abstraction.

Proceed to general scalar replacement when:

- the small prototype shows a material win over known-shape boxed construction;
- side-exit materialization has a bounded, testable design;
- both single-worker and concurrent benchmarks improve or remain neutral;
- the optimization does not complicate the ordinary dynamic `Value` ABI.

## Provisional Recommendation

Begin with the Stage 0 grammar, absence classification, and representation experiment. Validate
`()`, the common pattern grammar, and relation cardinality at ordinary binding sites as one
coherent surface, then implement the compiler-owned structural type core without changing runtime
representation. Remove `one` and `nothing` only as part of the typed migration that supplies unit,
option/result matching, strict operations, and explicit empty relations; do not replace them with a
differently spelled universal sentinel.

Treat `option<T>` and `result<T>` as the first demanding users of relation shape, cardinality,
singleton discrimination, aliases, and matching—not as special value kinds. Treat Roe and the
shipped applications as the migration test: their current sentinel-heavy code should become more
explicit and locally understandable, not merely longer.

The design succeeds if ordinary dynamic Mica still sees canonical relation values while typed hot
code can reduce those values to presence bits, discriminator tags, and scalar payload registers. It
fails if every annotation repeatedly scans a relation, every match allocates a row map, or every
small result is forced through general relation construction and dynamic dispatch.
