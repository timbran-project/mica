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

## Read-Only Candidate Rows

Computed relation rows are not durable truth. They are candidate rows produced from the current
reader and whatever implementation backs the relation.

You cannot assert into a computed relation:

```mica
assert NearestEmbedding(#index, [1.0, 0.0], 1, #unit, 0.9, 42)
```

That is a write to a read-only relation. Instead, assert the ordinary facts the computed relation
reads from, such as `EmbeddingOf`, `EmbeddingVector`, and `VectorIndexContains`.

Because computed rows are candidates, callers must validate them before using them as application
state. Retrieval code should check that the subject still exists, that the source is fresh enough,
and that the actor is allowed to use the subject.

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

## Rules And Computed Relations

Computed relations are scanned through the same `RelationRead` path as stored relations. They may
therefore be visible to rule evaluation.

That does not make their rows facts. A rule may use a computed relation to derive candidate rows,
but any workflow that records durable state should validate the candidate through ordinary relations
and authority rules first.

This distinction matters for search. Vector similarity can propose "this looks nearby"; it should
not by itself establish that the subject is visible, trusted, current, or relevant.
