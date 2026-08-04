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

Rules are installed world state. They can be inspected, disabled, subscribed to through their head
relations, and filed out. See [Changing Worlds and Differential Updates](./differential-updates.md)
for how derived results respond to later fact changes.
