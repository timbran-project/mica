# Recursive Dependency Planner

This example models four software components:

```text
web frontend -> API service -> database
job worker -----------------> database
```

A direct dependency is stored. The complete transitive dependency relation and the components
affected by an outage are derived with recursive rules.

The complete source is [`apps/examples/dependency-planner.mica`](../../../apps/examples/dependency-planner.mica).

## Load the Planner

```sh
export MICA_EXAMPLE_STORE="$(mktemp -d)"

cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" \
  filein --unit dependencies --replace \
  apps/examples/dependency-planner.mica
```

Olivia is the operator identity used by the walkthrough. The filein grants her read access to the
dependency relations, write access to `Unavailable`, and invoke access to the two operational verbs.

## Derive Transitive Dependencies

The first rule copies direct edges into `DependsOn`:

```mica
DependsOn(component, dependency) :-
  DirectDependency(component, dependency)
```

The recursive rule follows another edge:

```mica
DependsOn(component, dependency) :-
  DirectDependency(component, intermediate),
  DependsOn(intermediate, dependency)
```

Ask what the web frontend depends on:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor olivia \
  eval 'return DependsOn(#web_frontend, ?dependency)'
```

The result contains both the direct API dependency and the indirect database dependency:

```text
[:dependency] {[#api_service], [#database]}
```

The program did not write a graph traversal loop. The recursive definition states what a dependency
path means, and Mica evaluates it to a fixpoint.

## Mark a Component Unavailable

Invoke the operator action:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor olivia \
  eval 'return :mark_unavailable(actor: #olivia, component: #database)'
```

It returns `:marked_unavailable` and asserts one stored fact:

```mica
Unavailable(#database)
```

Impact is derived through two rule branches:

```mica
Affected(component) :-
  Unavailable(component)

Affected(component) :-
  DependsOn(component, dependency),
  Unavailable(dependency)
```

Query the result:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor olivia \
  eval 'return Affected(?component)'
```

The returned set is:

```text
[:component] {[#web_frontend], [#api_service], [#job_worker], [#database]}
```

The database is affected directly. The API and worker depend on it directly. The frontend is
affected through the API's transitive dependency.

## Restore the Component

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor olivia \
  eval 'return :restore(actor: #olivia, component: #database)'
```

The verb retracts `Unavailable(#database)` and returns `:restored`. Querying `Affected(?component)`
again returns an empty relation:

```text
[:component] {}
```

No cleanup loop retracts four impact facts. They disappear because their only support disappeared.

## Cycles and Set Semantics

Real dependency graphs may contain cycles. Positive recursive evaluation uses set semantics: once a
fact has been derived, encountering it again does not add another logical copy. Evaluation stops
when a pass adds nothing new.

A cycle can still produce surprising modelling results, such as a component depending transitively
on itself. Decide whether cycles are valid in your domain and add validation or a separate diagnostic
relation when they are not.

## What the Example Demonstrates

- a graph represented as ordinary binary facts;
- positive recursion without an imperative traversal;
- transitive results queried through the same relation interface;
- stored outage observations separated from derived impact;
- a single retraction removing every unsupported conclusion;
- named-role operator verbs with explicit invoke and write authority;
- set semantics providing a finite fixpoint for a finite active domain.

The [Rules](../language/rules.md) reference covers recursion, safety, and stratified negation in more
detail.
