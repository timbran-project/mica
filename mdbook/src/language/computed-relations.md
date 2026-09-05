# Computed Relations

Most relations in Mica are stored or derived. Stored relations contain asserted facts. Derived
relations are rule heads computed from other relations.

Computed relations are different: they are read-only relation surfaces whose rows are produced by
runtime code when they are scanned.

They still look like relations to Mica source:

```mica
NearestEmbedding(index, query_embedding, limit, ?subject, ?score, ?snapshot_version)
```

The difference is that Mica does not store every possible nearest-neighbour answer as a fact. The
runtime computes candidate rows from ordinary embedding facts and returns them through the normal
relation query path.

## What Computed Relations Are For

Computed relations are useful when a relation-shaped question has a specialized access path:

- system reflection, such as relation metadata and object-neighbourhood views;
- search, such as nearest embedding candidates;
- bounded host-backed views that should still compose with ordinary relation reads.

The important property is that the result is still relation-shaped. A computed relation returns
tuples, not an opaque host object. That means task code can iterate, filter, record, and join the
result with ordinary facts.

## Read-Only Results

Computed rows are produced from the current reader by a registered implementation. System reflection
reports the catalogue or stored facts visible to that reader; nearest-neighbour search reports a
selection calculated from embedding facts. Neither requires storing the answer rows in the queried
relation.

You cannot assert into a computed relation:

```mica
assert NearestEmbedding(#index, [1.0, 0.0], 1, #unit, 0.9, 42)
```

That is a write to a read-only relation. Instead, assert the ordinary facts the computed relation
reads from, such as `EmbeddingOf`, `EmbeddingVector`, and `VectorIndexContains`.

Search results require an application decision before they become workflow state. Retrieval code
should check that the subject still exists, that the source is fresh enough, and that the actor is
allowed to use the subject.

For example, `apps/shared/retrieval.mica` records retrieved context only after checking:

```mica
CanRetrieveSubject(actor, subject)
```

An application might derive that relation from document ownership, project membership, or an
explicit access grant.

## Required Bindings

Some computed relations require certain positions to be bound. `NearestEmbedding` requires the
index, query embedding, and limit:

```mica
NearestEmbedding(index, query_embedding, limit, ?subject, ?score, ?snapshot_version)
```

This is an access pattern, not just documentation. An unconstrained query for all nearest-neighbour
answers is not meaningful, because the answer depends on the query vector and limit.

A bound argument is a value supplied by the caller, including a local expression. A query variable
such as `?index` or a wildcard `_` leaves the position open and cannot satisfy a required binding.
Missing required inputs produce a query error. Source code can catch it as `E_DB`:

```mica,eval
make_relation(:NearestEmbedding, 6)
let rejected = try
  NearestEmbedding(?index, [1.0, 0.0], 1, ?subject, ?score, _)
  false
catch E_DB
  true
end
require rejected
```

Other positions can still be bound to filter the answer. For nearest-neighbour search, Mica first
selects the best `limit` subjects for the given index and vector, then applies bindings on subject,
score, and snapshot version. Binding a subject therefore asks whether it occurs among those selected
candidates. It does not start a separate search restricted to that subject.

## Results Are Ordinary Values

A query with named output variables produces an immutable relation value. Its heading comes from
those variable names, and duplicate rows collapse just as they do in a stored-relation query. A
fully bound query, or one containing only wildcards, produces a boolean existence result.

This example supplies vectors directly so the search can be reproduced without an embedding service:

```mica,eval
make_relation(:NearestEmbedding, 6)
make_relation(:VectorIndexContains, 2)
make_functional_relation(:EmbeddingOf, 2, [0])
make_functional_relation(:EmbeddingVector, 2, [0])

assert VectorIndexContains(:manuals, :calibration_vector)
assert VectorIndexContains(:manuals, :cleaning_vector)
assert EmbeddingOf(:calibration_vector, "calibration")
assert EmbeddingOf(:cleaning_vector, "cleaning")
assert EmbeddingVector(:calibration_vector, [1.0, 0.0])
assert EmbeddingVector(:cleaning_vector, [0.0, 1.0])

let best = NearestEmbedding(:manuals, [1.0, 0.0], 1, ?subject, ?score, _)
require best == [:subject, :score] { ["calibration", 1.0] }
require !NearestEmbedding(:manuals, [1.0, 0.0], 1, "cleaning", _, _)
```

Relation values have canonical row order. A search implementation's ranking determines which rows
are selected; it does not turn the relation into an ordered list. Use an explicit score sort when
presenting several candidates in ranked order. Saving `best` also saves that answer: another query
can reflect later input changes, while the saved value retains its contents.

## Rules And Computed Relations

Computed relations are scanned through the same `RelationRead` path as stored relations. They may
therefore be visible to rule evaluation.

That does not make their rows facts. A rule may use a computed relation to derive candidate rows,
but any workflow that records durable state should validate the candidate through ordinary relations
and authority rules first.

This distinction matters for search. Vector similarity can propose "this looks nearby"; it should
not by itself establish that the subject is visible, trusted, current, or relevant.
