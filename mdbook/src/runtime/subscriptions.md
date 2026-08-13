# Subscriptions

Subscriptions deliver committed relation changes through a mailbox. They support stored facts,
complete relation results including rule derivations, and root-only catalogue changes.

## Registering a Subscription

Create a mailbox and pass its sender capability to `subscribe_changes`:

```mica
let [receiver, sender] = mailbox()
let subscription = subscribe_changes(
  sender,
  :relation,
  some(:ReadyForReview),
  [some(#reviewer), none],
  :snapshot
)
commit()
```

The complete signature is:

```text
subscribe_changes(sender, subject, relation, bindings, initial[, cursor[, queue_budget]])
```

`subject` is one of:

| Subject      | Observed surface                                           |
| ------------ | ---------------------------------------------------------- |
| `:facts`     | asserted changes in a stored relation only                 |
| `:relation`  | changes to the complete stored-and-derived relation result |
| `:catalogue` | relation and rule catalogue changes; root authority only   |

For `:facts` and `:relation`, `relation` is `some(identity-or-name)`. `bindings` has exactly one
entry per relation position. `some(value)` restricts that position; `none` leaves it open. The
example observes rows of `ReadyForReview` whose first position is `#reviewer`.

For `:catalogue`, pass `none` as the relation and `[]` as the bindings.

`initial` is `:snapshot` to receive current matching rows followed by changes, or `:changes` to
receive only changes after registration. A non-negative cursor may resume change delivery from
retained commit history. Pass `none` when no cursor is available. The queue budget defaults to 64
messages and must be a non-negative integer.

Registration becomes active when the surrounding task commits.

## Message Shapes

A relation snapshot or change message is a map:

```mica
{
  :kind -> :changes,
  :subscription -> subscription,
  :cursor -> 42,
  :subject -> :relation,
  :assertions -> [[#reviewer, #change_request]],
  :retractions -> []
}
```

An initial relation snapshot uses `:subject -> :snapshot`; its current rows appear in `:assertions`.
A stored-fact change uses `:subject -> :facts`.

Catalogue snapshots use `:kind -> :snapshot`, `:subject -> :catalogue`, a cursor, and an `:entries`
list. Later catalogue messages use `:kind -> :changes`, `:subject -> :catalogue`, and the same
`:entries` field. Entry kinds include `:relation_created`, `:rule_installed`, and `:rule_disabled`.

Two marker messages require recovery rather than row processing:

- `:kind -> :resynchronize` means retained history or the bounded queue could not preserve a
  complete incremental stream. Read a fresh snapshot and register again.
- `:kind -> :revoked` means refreshed authority no longer permits the read. No later rows are
  disclosed.

Both marker maps include `:subscription` and `:cursor`.

## Consuming and Cancelling

`mailbox_recv` returns ready groups shaped as `[receiver, messages]`:

```mica
let ready = mailbox_recv([receiver])
for group in ready
  for message in group[1]
    if message[:kind] == :changes
      for row in message[:assertions]
        emit(#reviewer, row[1])
      end
    end
  end
end
```

`mailbox_recv` is a commit boundary. Effects and writes performed while processing one delivery are
published when the task next receives, commits, suspends, reads, or spawns.

Cancellation is also transactional:

```mica
cancel_subscription(subscription)
commit()
mailbox_close(receiver)
```

Cancel before closing a mailbox-backed operation. Subscriptions and mailbox capabilities are
ephemeral; store durable progress, such as the last processed work item or cursor, as relation
facts.

## Authority and Backpressure

The runtime checks read authority from the task's current runtime context. It does not use an
ordinary argument that merely happens to identify an actor. Authority is refreshed at boundaries;
loss of permission produces a revocation marker.

Bind every stable column you can. A subscription with `[none, none]` observes the entire
binary relation and consumes queue capacity for unrelated rows. Bounded delivery prevents a slow
consumer from growing memory without limit; resynchronization is the explicit recovery path.

## Synchronized DOM Views

A synchronized view can declare the relation patterns that determine its rendered tree:

```mica
verb sync_view_dependencies(view)
  let exactly {project} = ProjectView(view, ?project)
  return [{
    :subject -> :relation,
    :relation -> some(:ProjectStatus),
    :bindings -> [some(project), none]
  }]
end
```

The host registers the subscriptions, rerenders `sync_view_tree(view)` after a relevant change, and
sends a structural DOM diff. Rules determine what is true, subscriptions identify changes that can
affect the view, and the synchronizer turns the new tree into a browser update. The renderer itself
rerenders the affected view; it is the triggering relation evaluation that is maintained
differentially.

Rule evaluation supplies settled relation changes. See
[Changing Worlds and Differential Updates](../language/differential-updates.md) for how Mica
maintains derived results between commits.
