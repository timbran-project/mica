# Changing Worlds and Differential Updates

Facts change while Mica runs, and rules update their consequences at each successful commit. The
meaning remains ordinary set semantics: a derived tuple is either present or absent. Differential
evaluation is how the runtime avoids recomputing unchanged results; it does not add syntax or alter
the answers a rule produces.

Consider a dependency relation and its transitive closure:

```mica
DependsOn(#release, #package)
DependsOn(#package, #library)

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

## Maintained Execution State

The runtime can retain execution structures for warmed rule results:

- arrangements index rows needed by joins;
- traces retain consolidated, versioned changes; and
- support weights distinguish one proof from several for non-recursive results.

These structures are ephemeral implementation state. Durable facts and rules remain authoritative,
and a snapshot still presents an ordinary coherent relation. If maintained state is unavailable or
unsuitable, Mica can evaluate the complete result and produce the same answers.

Differential maintenance is most useful when relations are large, commits change a small fraction of
their rows, rules remain stable, and derived results are read repeatedly. A small input change can
still have a large consequence: removing a central dependency edge may alter much of a graph.

To receive settled additions and removals in task code, use the runtime
[subscription API](../runtime/subscriptions.md). Subscription messages expose asserted and retracted
rows; programs do not inspect arrangements, traces, or support counts.
