# Transactional buffers

The relation kernel stores buffers beside relation state in each immutable snapshot.
A buffer has an identity, a unique name, a durability policy, a conflict policy, and a revision.
Buffer names share the relation namespace. Deleted buffers retain tombstones that reserve their identities.

The current interface is the Rust kernel API. Language builtins and editor integration remain pending.

## Transaction interface

`Transaction::create_buffer` stages an empty buffer. `replace_buffer` replaces a half-open range of Unicode scalar positions.
`buffer_text`, `buffer_metadata`, and `buffer_revision` read the transaction view. `delete_buffer` stages a tombstone.
New buffers have revision zero inside the creation transaction. Their first committed revision is one.
Each later text commit advances the buffer revision once, regardless of the number of local replacements.
An edit sequence that retains the original text provenance does not advance the revision.

Facts and text publish together after persistence succeeds. A failed commit publishes neither.
Earlier snapshots retain their text. Dropping a transaction discards its private edits.
Staged kernel publication includes buffer creation, changes, and deletion.

## Concurrent changes

| Policy | Concurrent commit behaviour |
| --- | --- |
| `Reject` | Reject any change since the transaction snapshot. |
| `Span` | Merge disjoint edits by their original character positions. |
| `Whole` | Replace the current buffer with the complete transaction view. |

Concurrent deletion always causes a conflict. Deletion also conflicts with concurrent edits, regardless of policy.
Two insertions at the same position conflict under `Span`. Insertions inside a removed range also conflict.
Insertions at range boundaries and adjacent replacements can merge.
The merge uses retained chunk provenance, so identical characters at different positions remain distinct.

Concurrent span merges share a transaction budget of 4096 pieces, 1 MiB of replacement text, and 1024 replacements.
Exhausting this budget rejects the commit. Ordinary commits currently normalize all pieces without this budget.

## Persistence

Fjall stores buffer deltas in the same atomic batch as facts, catalogue changes, and the commit version.
Strict mode waits for the durable write. Relaxed mode uses the existing asynchronous writer and flush boundary.
Durable buffers recover their text and revision. Volatile buffers recover empty text and advance their revision to invalidate earlier client revisions.
Both modes retain buffer metadata and tombstones.

The store format is `mica-relation-kernel-state-2.1.0`, with commit encoding `MICACMT3`.
Earlier store formats are rejected. There is no migration adapter.
Each buffer has a checkpoint and fewer than 64 delta records totalling less than 1 MiB.
Reaching either limit writes a checkpoint and removes those deltas in the same atomic batch.
Ordinary edits write one delta and a small counter. Checkpoints reconstruct and serialize the full buffer, which can increase edit latency.
Startup reads these bounded records. The separate commit history remains available for inspection and explicit replay.

The text tree shares immutable UTF-8 chunks and balanced nodes across snapshots.
Splices copy the affected tree paths. Line coordinates use cached subtree counts, and search streams across chunks.
Commit normalization still examines buffer pieces. These implementation properties do not establish measured editor performance.
