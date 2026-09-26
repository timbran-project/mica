# Transactional buffers

The relation kernel stores buffers beside relation state in each immutable snapshot.
A buffer has an identity, a unique name, a durability policy, a conflict policy, and a revision.
Buffer names share the relation namespace. Deleted buffers retain tombstones that reserve their identities and names.

Language builtins use the current task transaction and its authority context.
Computed buffer relations, history, compaction, and editor integration remain pending.

## Language interface

`make_buffer(:name, :durable | :volatile[, :reject | :span | :whole])` stages an empty buffer and returns `true`.
Creation requires grant authority, like relation creation. Repeating the declaration adopts a buffer only when its metadata matches.
The default conflict policy is `:reject`. A retired name raises `E_KILLED`.

| Builtin | Result |
| --- | --- |
| `buffer_insert(name, at, text)` | Insert text at a scalar position. |
| `buffer_delete(name, at, count)` | Remove a scalar count. |
| `buffer_replace(name, start, stop, text)` | Replace the half-open scalar range. |
| `kill_buffer(name)` | Retire the buffer, its identity, and its name. |
| `buffer_len(name)` | Scalar count. |
| `buffer_line_count(name)` | Logical line count, including a final empty line after a newline. |
| `buffer_revision(name)` | Committed base revision, or zero during creation. |
| `buffer_text(name)` | Complete transaction text. |
| `buffer_slice(name, start, stop)` | Text in the half-open scalar range. |
| `buffer_find(name, pattern, from, limit)` | First scalar position, or `none`. Zero limit searches the remaining text. |
| `buffer_lines(name, first, count)` | Bounded relation with `buffer`, `line`, `start`, `stop`, and `text` columns. |
| `buffer_viewport(name, first, count, budget)` | Bounded lines with a total scalar budget and an additional `complete` column. |
| `buffer_line_span(name, line)` | Map with `start` and `stop`, or `none` past the final line. |
| `buffer_position_line_column(name, offset)` | Map with `line` and `column`. Oversized offsets clamp to the text end. |
| `buffer_line_column_offset(name, line, column)` | Scalar offset. Oversized columns clamp to the line end. |

Mutations return `true` after staging. They become visible after the task commits, including at suspension boundaries.
Line spans exclude the trailing newline. Viewport reads stop after the first incomplete row and never materialize text outside their scalar budget.
Empty search patterns return `none`.

Read operations require a read grant for the buffer identity. Mutations require a write grant.
The existing `GrantRead`, `GrantWrite`, and derived policy relations resolve active buffer names when the runtime builds authority.
Suspended tasks resume with fresh authority. Buffer reads are available to read-only queries, and mutations are rejected.

## Client revisions and markers

`buffer_apply(name, expected_revision, edits[, token])` checks the revision before it stages any edit.
Each edit is a map with optional `at`, `remove`, and `text` fields, which default to zero, zero, and an empty string.
Edits execute sequentially against the private transaction text. An invalid batch leaves that text unchanged.
The result is `:stale` when the revision differs, or `:staged` when the batch succeeds.
An apply must precede other text mutations. An empty buffer created in the same transaction can receive an apply at revision zero.
After an apply, further mutations of that buffer raise `E_STATE` until the transaction ends.

A positive token reserves a completion result. Zero requests no completion result.
`buffer_apply_result(token)` returns `:pending` before settlement or when the token is unknown.
After settlement, it returns a map with `status` and `revision`:

| Status | Meaning |
| --- | --- |
| `:ok` | The transaction committed. The `applied` field contains the complete delta against the client's original revision. |
| `:conflict` | A concurrent change prevented the commit. |
| `:aborted` | The transaction ended without publication, including a persistence error. |
| `:resync` | Reconciliation exceeded its work budget or could not express the result against the client's text. |

A `:resync` result can follow a successful commit when its acknowledgement delta exceeds the result budget.
Clients must read authoritative text before they submit another edit after `:resync`.
An `:ok` delta includes merged concurrent changes. Its ranges use the original client revision, and clients apply them from the highest position down.
Tagged conflicts do not automatically re-execute the task. A resubmission uses a fresh token and the required current revision.

Result reads require read authority for the buffer identity. Tokens remain reserved while their result is retained.
The cache holds at most 1,024 entries and 8 MiB of delta storage. It evicts only completed entries.
When all slots are pending, another tagged apply raises `E_STATE` without staging changes.
Results are ephemeral and disappear after restart. An unknown or evicted token returns `:pending`, so clients need a timeout and resynchronization path.

`buffer_marker_rebase(edits, position, :stick_before | :stick_after)` moves a marker through a committed, base-relative delta.
The delta must contain sorted, non-overlapping ranges. Insertions at the marker follow its selected affinity.
A removed interior position collapses to the start of the removed range. Marker state remains application-owned.

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
Each buffer has a checkpoint and fewer than 4096 delta records totalling less than 1 MiB.
Reaching either limit writes a checkpoint and removes those deltas in the same atomic batch.
Ordinary edits write one delta and a small counter. Checkpoints reconstruct and serialize the full buffer, which can increase edit latency.
Startup reads these bounded records. The separate commit history remains available for inspection and explicit replay.

The text tree shares immutable UTF-8 chunks and balanced nodes across snapshots.
Splices copy the affected tree paths. Line coordinates use cached subtree counts, and search streams across chunks.
Commit normalization still examines buffer pieces. These implementation properties do not establish measured editor performance.
