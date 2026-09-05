# Changing Worlds and Differential Updates

Facts change while Mica runs, and rules update their consequences at each successful commit. The
meaning remains ordinary set semantics: a derived tuple is either present or absent. Differential
evaluation is how the runtime avoids recomputing unchanged results; it does not add syntax or alter
the answers a rule produces.

Consider a dependency relation and its transitive closure:

```mica
assert DependsOn(#release, #package)
assert DependsOn(#package, #library)

Requires(item, dependency) :-
  DependsOn(item, dependency)

Requires(item, dependency) :-
  DependsOn(item, intermediate),
  Requires(intermediate, dependency)
```

Adding `DependsOn(#library, #compiler)` introduces only the consequences reachable from that new
edge. Removing an edge retracts consequences that no longer have any supporting path.

## Additions, Retractions, and Support

For a non-recursive rule, Mica tracks how many distinct derivations support a tuple. The public
relation still contains one copy. A tuple appears when its support crosses from zero to non-zero and
disappears only when its final support is removed. Removing one explanation therefore does not
remove a fact that another explanation still proves.

Recursive rules use deletion and rederivation. The runtime first overdeletes recursive consequences
that may depend on a removed input, then restores those with an independent proof. Evaluation moves
through change frontiers until no new consequence changes.

## One Commit Produces Settled Answers

A transaction may change several inputs at once. Mica evaluates their combined effect against the
resulting world. If one reason for a conclusion disappears while another appears in that same
commit, the conclusion can remain present throughout the published change. The internal work may
visit that tuple several times; a subscriber receives the settled additions and removals.

For example, suppose `CanCollect(person, instrument)` can follow from either a shared site or a
shared project. A transaction can move the instrument away from the person's site and assign it to
their project. The causes change, but the same permission still has support. A subscription to
`CanCollect` concerns the resulting permission rows. Subscribe to `LocatedAt` or `AssignedTo` when
the application needs to observe changes to those causes themselves.

Recursive overdeletion is also internal evaluation work. Other tasks do not see a published graph
with all possibly affected paths temporarily removed. The runtime settles rederivation before
publishing the next snapshot. Tasks holding the previous snapshot retain its coherent answers.

## Maintained Execution State

The runtime can retain execution structures for warmed rule results:

- arrangements index rows needed by joins;
- traces retain consolidated, versioned changes; and
- support weights distinguish one proof from several for non-recursive results.

These structures are ephemeral implementation state. Durable facts and rules remain authoritative,
and a snapshot still presents an ordinary coherent relation. If maintained state is unavailable or
unsuitable, Mica can evaluate the complete result and produce the same answers.

The first query for a derived relation computes a complete answer and can prepare maintained state
for that relation and its dependencies. Later commits advance that state through the affected rule
components. Installing or disabling rules changes the definition itself, so the runtime rebuilds the
corresponding execution state against the resulting rule set.

An in-progress task can also query derived relations after making draft writes. Its private
evaluation incorporates those writes without publishing them or changing another task's snapshot.
Aborting the task discards that draft; completing it commits the settled changes. An immutable query
result saved in a local binding remains the answer from the moment of that query, even when the same
task subsequently changes its inputs.

Differential maintenance is most useful when relations are large, commits change a small fraction of
their rows, rules remain stable, and derived results are read repeatedly. A small input change can
still have a large consequence: removing a central dependency edge may alter much of a graph.

Measure both the size of an input change and the size of its consequences when investigating query
cost. A small number of asserted facts does not imply a small join or recursive frontier. Project
only the columns the definition needs, use selective bound values in queries, and retain explicit
base facts for decisions or events whose history matters. Internal support counts track current
proofs; they are not an application event history.

To receive settled additions and removals in task code, use the runtime
[subscription API](../runtime/subscriptions.md). Subscription messages expose asserted and retracted
rows; programs do not inspect arrangements, traces, or support counts.
