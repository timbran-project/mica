# Rules

Rules derive relation facts from other relation facts:

```mica
ReadyForReview(reviewer, change) :-
  AssignedReviewer(change, reviewer),
  Completed(change),
  not ReviewRecorded(change)
```

The head before `:-` describes the facts being derived. Every body item must hold for one set of
logical variable bindings. Rule variables are conventionally bare names; they are not imperative
locals assigned in source order.

Task-code queries mark free variables with `?`:

```mica
return ReadyForReview(#alice, ?change)
```

The compiler also accepts `?name` in rule atoms, but bare names are the preferred rule style. A
relation with both asserted facts and rule heads reads as the union of stored and derived facts.

## Reading a Rule as a Query

Each positive body predicate supplies candidate values. A name shared between predicates joins
their matching cells. A name used only in the body helps establish the conclusion without appearing
in the answer:

```mica,eval
make_relation(:WorksAt, 2)
make_relation(:LocatedAt, 2)
make_relation(:CanCollect, 2)
assert WorksAt(:alice, :north)
assert WorksAt(:bob, :south)
assert LocatedAt(:sensor17, :north)

CanCollect(person, instrument) :-
  WorksAt(person, site),
  LocatedAt(instrument, site)

require CanCollect(?person, ?instrument) == [:person, :instrument] {
  [:alice, :sensor17]
}
```

Here `site` connects the two sources. It is not a returned column because the head contains only
`person` and `instrument`. The head can reorder variables or include literal values. A literal
in a body predicate selects matching facts, just as a bound argument does in a task query.

Repeated occurrences of one variable must refer to the same value. `Link(item, item)` selects
self-links; `Link(from, to)` permits different endpoints. The evaluator may reorder body predicates
to use available bindings and indexes. Write the order that explains the definition clearly.

## Comparison Guards

Use `==`, `!=`, `<`, `<=`, `>`, or `>=` to filter values bound by positive predicates:

```mica,eval
make_relation(:Reading, 2)
make_relation(:ColdSite, 1)
assert Reading(:north, -5)
assert Reading(:south, 3)

ColdSite(site) :-
  Reading(site, temperature),
  temperature < -1.5

require ColdSite(?site) == [:site] { [:north] }
```

A guard tests a candidate binding; it does not assign a variable. In particular, `temperature == 5`
does not supply values for an otherwise unbound `temperature`. Give that variable a positive source
such as `Reading(site, temperature)`.

Predicate matching uses canonical value identity, while comparison guards use the language's
numeric comparisons. This distinction matters when integer and float facts coexist:

```mica,eval
make_relation(:Measurement, 2)
make_relation(:IntegerOne, 1)
make_relation(:NumericOne, 1)
assert Measurement(:count, 1)
assert Measurement(:scale, 1.0)

IntegerOne(name) :- Measurement(name, 1)
NumericOne(name) :- Measurement(name, value), value == 1

require IntegerOne(?name) == [:name] { [:count] }
require NumericOne(?name) == [:name] { [:count], [:scale] }
```

The first rule matches the integer literal exactly. The second binds either numeric kind and then
compares its value with 1. Use consistent numeric kinds when an argument participates in identity
matching, keys, or joins.

## Safety and Negation

Rules must be range-restricted. Every head variable must be bound by a positive body predicate.
Variables in a negated predicate must also be bound positively:

```mica
ReadyForReview(reviewer, change) :-
  AssignedReviewer(change, reviewer),
  Completed(change),
  not ReviewRecorded(change)
```

This is unsafe because neither variable has a finite positive source:

```mica
ReadyForReview(reviewer, change) :-
  not ReviewRecorded(change)
```

`not` means "not derivable in the current snapshot". Negation is stratified: the runtime must be
able to compute positive dependencies before the relations that negate them. Mutual negative cycles
are rejected because neither side has a stable evaluation order.

The positive source also makes absence meaningful. A rule that derives an available instrument
from `Instrument(instrument), not Reserved(instrument)` considers known instruments. It says nothing
about values that never appear in `Instrument`.

Negation can refer to another derived relation. Mica completes that relation's dependencies before
checking for absence. Adding a fact can therefore remove a conclusion: asserting a reservation
removes that instrument from the available result. Retracting the reservation lets it qualify
again. No explicit negative fact is stored by `not`.

## Recursion

Positive recursion expresses transitive relationships:

```mica
Requires(item, dependency) :-
  DependsOn(item, dependency)

Requires(item, dependency) :-
  DependsOn(item, intermediate),
  Requires(intermediate, dependency)
```

Mica computes the finite, set-based least fixpoint: it starts with direct dependencies, repeatedly
adds newly implied dependencies, and stops when another pass adds nothing. Cycles do not produce
duplicate facts.

A path can have more than one reason to exist. Removing one edge removes a derived path only when
no remaining path supports it. A query in the writing task sees the effect of its draft changes:

```mica,eval
make_relation(:DependsOn, 2)
make_relation(:Requires, 2)
assert DependsOn(:app, :library)
assert DependsOn(:library, :core)
assert DependsOn(:app, :core)

Requires(item, dependency) :- DependsOn(item, dependency)
Requires(item, dependency) :-
  DependsOn(item, intermediate),
  Requires(intermediate, dependency)

require Requires(:app, :core)
retract DependsOn(:app, :core)
require Requires(:app, :core)
retract DependsOn(:library, :core)
require !Requires(:app, :core)
```

The recursive rule describes paths with at least one edge. It does not automatically make every
item require itself. A cycle such as `a -> b -> a` does supply a path back to its starting point,
so self-pairs can follow from actual cycles. Define a separate reflexive rule over an explicit
domain relation if every known item should relate to itself even without an edge.

## Stored Facts and Derived Support

Retracting a head fact removes a stored assertion of that fact. It does not suppress a conclusion
still supported by a rule body. Change the causes or the definition when a derived answer should
disappear. Conversely, an explicitly asserted head fact remains present after its derived support
disappears, until that assertion is retracted too.

Several rules for one head express alternative reasons. Body predicates within one rule express
conditions that must hold together. Duplicate conclusions from different bindings, paths, or rules
still appear as one row. Query results describe which facts hold; their row count does not count
how many proofs support each fact.

Rules are installed world state. They can be inspected, disabled, subscribed to through their head
relations, and filed out. See [Changing Worlds and Differential Updates](./differential-updates.md)
for how derived results respond to later fact changes.
