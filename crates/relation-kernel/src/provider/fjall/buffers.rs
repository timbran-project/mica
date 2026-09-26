// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::codec::{decode_buffer_changes_record, encode_buffer_changes_record};
use crate::RelationDurability;
use crate::buffer::store::BufferStates;
use crate::buffer::{BufferChange, Delta, PersistedBufferState, Replacement};
use fjall::{Keyspace, OwnedWriteBatch};
use mica_var::Identity;

const MAX_DELTAS: u64 = 4096;
const MAX_DELTA_BYTES: u64 = 1024 * 1024;
const COUNTER: u8 = 0;
const CHECKPOINT: u8 = 1;
const DELTA: u8 = 2;

#[derive(Default)]
struct Counter {
    revision: u64,
    deltas: u64,
    bytes: u64,
}

impl Counter {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 24 {
            return Err("invalid buffer checkpoint counter length".to_owned());
        }
        let counter = Self {
            revision: u64::from_be_bytes(bytes[..8].try_into().unwrap()),
            deltas: u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
            bytes: u64::from_be_bytes(bytes[16..].try_into().unwrap()),
        };
        if (counter.revision == 0 && (counter.deltas != 0 || counter.bytes != 0))
            || counter.deltas >= MAX_DELTAS
            || counter.bytes >= MAX_DELTA_BYTES
        {
            return Err("invalid buffer checkpoint counter".to_owned());
        }
        Ok(counter)
    }

    fn encode(&self) -> [u8; 24] {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&self.revision.to_be_bytes());
        bytes[8..16].copy_from_slice(&self.deltas.to_be_bytes());
        bytes[16..].copy_from_slice(&self.bytes.to_be_bytes());
        bytes
    }
}

fn key(tag: u8, id: Identity) -> [u8; 9] {
    let mut key = [tag; 9];
    key[1..].copy_from_slice(&id.raw().to_be_bytes());
    key
}

fn decode_change(bytes: &[u8], id: Identity) -> Result<BufferChange, String> {
    let mut changes = decode_buffer_changes_record(bytes)?;
    if changes.len() != 1 || changes[0].metadata.id != id {
        return Err("invalid buffer checkpoint identity".to_owned());
    }
    Ok(changes.pop().unwrap())
}

fn load_buffer(space: &Keyspace, id: Identity, counter: &Counter) -> Result<BufferStates, String> {
    let checkpoint = space
        .get(key(CHECKPOINT, id))
        .map_err(|error| format!("failed to read buffer checkpoint: {error}"))?
        .ok_or_else(|| "missing buffer checkpoint".to_owned())?;
    let mut buffers = BufferStates::default();
    buffers
        .replay(&decode_change(&checkpoint, id)?)
        .map_err(|error| format!("invalid buffer checkpoint: {error:?}"))?;
    let mut deltas = 0;
    let mut bytes = 0;
    for entry in space.prefix(key(DELTA, id)) {
        let (key, value) = entry
            .into_inner()
            .map_err(|error| format!("failed to read buffer delta: {error}"))?;
        if key.len() != 17 || deltas >= MAX_DELTAS {
            return Err("invalid buffer delta key or count".to_owned());
        }
        let change = decode_change(&value, id)?;
        if key[9..] != change.revision.to_be_bytes() {
            return Err("buffer delta key does not match revision".to_owned());
        }
        buffers
            .replay(&change)
            .map_err(|error| format!("invalid buffer delta: {error:?}"))?;
        deltas += 1;
        bytes += value.len() as u64;
    }
    if deltas != counter.deltas
        || bytes != counter.bytes
        || buffers.get(id).unwrap().revision() != counter.revision
    {
        return Err("buffer checkpoint counter does not match state".to_owned());
    }
    Ok(buffers)
}

pub(super) fn load_buffers(space: &Keyspace) -> Result<Vec<PersistedBufferState>, String> {
    let mut records = Vec::new();
    for entry in space.prefix([COUNTER]) {
        let (key, value) = entry
            .into_inner()
            .map_err(|error| format!("failed to read buffer counter: {error}"))?;
        if key.len() != 9 {
            return Err("invalid buffer counter key".to_owned());
        }
        let id = Identity::new(u64::from_be_bytes(key[1..].try_into().unwrap()))
            .ok_or_else(|| "invalid buffer counter identity".to_owned())?;
        records.extend(load_buffer(space, id, &Counter::decode(&value)?)?.persisted());
    }
    Ok(records)
}

/// Adds canonical buffer state to the same batch as the commit and relation facts.
/// Ordinary edits write a small counter and one delta. Checkpoints bound recovery work.
pub(super) fn write_changes(
    batch: &mut OwnedWriteBatch,
    space: &Keyspace,
    changes: &[BufferChange],
) -> Result<(), String> {
    for change in changes {
        let id = change.metadata.id;
        let prior = space
            .get(key(COUNTER, id))
            .map_err(|error| format!("failed to read buffer counter: {error}"))?;
        let mut counter = prior
            .as_ref()
            .map(|bytes| Counter::decode(bytes))
            .transpose()?
            .unwrap_or_default();
        if (prior.is_some() && change.revision <= counter.revision)
            || (prior.is_none() && change.base_revision != 0)
            || change.base_revision < counter.revision
            || (change.metadata.durability == RelationDurability::Durable
                && change.base_revision != counter.revision)
        {
            return Err("buffer commit does not follow persisted revision".to_owned());
        }
        let encoded = encode_buffer_changes_record(std::slice::from_ref(change))?;
        let deltas = counter.deltas + 1;
        let bytes = counter.bytes.saturating_add(encoded.len() as u64);
        if prior.is_none() || change.deleted || deltas >= MAX_DELTAS || bytes >= MAX_DELTA_BYTES {
            let mut buffers = if prior.is_some() {
                load_buffer(space, id, &counter)?
            } else {
                BufferStates::default()
            };
            buffers
                .replay(change)
                .map_err(|error| format!("invalid buffer commit: {error:?}"))?;
            let state = buffers.get(id).unwrap();
            let checkpoint = BufferChange {
                metadata: state.metadata().clone(),
                base_revision: 0,
                revision: state.revision(),
                deleted: state.is_deleted(),
                delta: Delta::new(vec![Replacement {
                    range: 0..0,
                    text: state.text().to_text(),
                }])
                .map_err(|error| format!("invalid buffer checkpoint delta: {error:?}"))?,
            };
            batch.insert(
                space,
                key(CHECKPOINT, id),
                encode_buffer_changes_record(&[checkpoint])?,
            );
            for entry in space.prefix(key(DELTA, id)) {
                let (key, _) = entry.into_inner().map_err(|error| {
                    format!("failed to remove checkpointed buffer delta: {error}")
                })?;
                batch.remove(space, key);
            }
            counter.deltas = 0;
            counter.bytes = 0;
        } else {
            let mut delta_key = key(DELTA, id).to_vec();
            delta_key.extend_from_slice(&change.revision.to_be_bytes());
            batch.insert(space, delta_key, encoded);
            counter.deltas = deltas;
            counter.bytes = bytes;
        }
        counter.revision = change.revision;
        batch.insert(space, key(COUNTER, id), counter.encode());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferConflictPolicy, BufferMetadata};
    use crate::{FjallStateProvider, RelationKernel};
    use mica_var::Symbol;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Store(PathBuf);
    impl Store {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "mica-buffer-checkpoint-{}-{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            )))
        }
    }
    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn empty_buffer_catalogue_survives_reopen_at_revision_zero() {
        let store = Store::new();
        {
            let provider = Arc::new(FjallStateProvider::open_strict(&store.0).unwrap());
            let kernel = RelationKernel::with_provider(provider);
            let mut tx = kernel.begin();
            for (raw, name, durability) in [
                (10, "durable", RelationDurability::Durable),
                (11, "volatile", RelationDurability::Volatile),
            ] {
                tx.create_buffer(BufferMetadata {
                    id: Identity::new(raw).unwrap(),
                    name: Symbol::intern(name),
                    durability,
                    conflict: BufferConflictPolicy::Span,
                })
                .unwrap();
            }
            tx.commit().unwrap();
        }
        {
            let provider = Arc::new(FjallStateProvider::open_strict(&store.0).unwrap());
            let kernel =
                RelationKernel::load_from_state(provider.load_state().unwrap(), provider).unwrap();
            let mut tx = kernel.begin();
            for raw in [10, 11] {
                let id = Identity::new(raw).unwrap();
                assert_eq!(tx.buffer_revision(id).unwrap(), 0);
                assert!(tx.buffer_text(id).unwrap().is_empty());
                tx.replace_buffer(id, 0..0, "é🦀").unwrap();
            }
            let result = tx.commit().unwrap();
            for raw in [10, 11] {
                assert_eq!(
                    result
                        .snapshot()
                        .buffer(Identity::new(raw).unwrap())
                        .unwrap()
                        .revision(),
                    1
                );
            }
        }
        let provider = Arc::new(FjallStateProvider::open_strict(&store.0).unwrap());
        let kernel =
            RelationKernel::load_from_state(provider.load_state().unwrap(), provider).unwrap();
        let snapshot = kernel.snapshot();
        let durable = snapshot.buffer(Identity::new(10).unwrap()).unwrap();
        assert_eq!(durable.text().to_text(), "é🦀");
        assert_eq!(durable.revision(), 1);
        let volatile = snapshot.buffer(Identity::new(11).unwrap()).unwrap();
        assert!(volatile.text().is_empty());
        assert_eq!(volatile.revision(), 2);
    }

    #[test]
    fn checkpoints_bound_recovery_by_delta_count_and_bytes() {
        let store = Store::new();
        let id = Identity::new(10).unwrap();
        let expected;
        {
            let provider = Arc::new(FjallStateProvider::open_strict(&store.0).unwrap());
            let kernel = RelationKernel::with_provider(provider.clone());
            let mut tx = kernel.begin();
            tx.create_buffer(BufferMetadata {
                id,
                name: Symbol::intern("notes"),
                durability: RelationDurability::Durable,
                conflict: BufferConflictPolicy::Span,
            })
            .unwrap();
            tx.replace_buffer(id, 0..0, "héllo→").unwrap();
            tx.commit().unwrap();
            for index in 0..(2 * MAX_DELTAS + 22) {
                let mut tx = kernel.begin();
                tx.replace_buffer(id, 2..3, if index % 2 == 0 { "🦀" } else { "λ" })
                    .unwrap();
                tx.commit().unwrap();
            }
            let space = &provider.keyspaces.buffers;
            let counter = Counter::decode(&space.get(key(COUNTER, id)).unwrap().unwrap()).unwrap();
            assert_eq!(counter.deltas, 22);
            assert_eq!(space.prefix(key(DELTA, id)).count(), 22);
            assert_eq!(
                provider.load_state().unwrap().buffers[0].text,
                kernel.snapshot().buffer(id).unwrap().text().to_text()
            );
            let mut tx = kernel.begin();
            tx.replace_buffer(id, 0..0, &"🦀".repeat(MAX_DELTA_BYTES as usize / 4))
                .unwrap();
            tx.commit().unwrap();
            assert_eq!(space.prefix(key(DELTA, id)).count(), 0);
            let mut tx = kernel.begin();
            tx.replace_buffer(id, 1..2, "after checkpoint").unwrap();
            tx.commit().unwrap();
            expected = kernel.snapshot().buffer(id).unwrap().text().to_text();
            assert_eq!(space.prefix(key(DELTA, id)).count(), 1);
            assert_eq!(provider.load_state().unwrap().buffers[0].text, expected);
        }
        let provider = Arc::new(FjallStateProvider::open_strict(&store.0).unwrap());
        let kernel =
            RelationKernel::load_from_state(provider.load_state().unwrap(), provider.clone())
                .unwrap();
        assert_eq!(
            kernel.snapshot().buffer(id).unwrap().text().to_text(),
            expected
        );
        let mut tx = kernel.begin();
        tx.delete_buffer(id).unwrap();
        tx.commit().unwrap();
        assert_eq!(provider.keyspaces.buffers.prefix(key(DELTA, id)).count(), 0);
        let state = provider.load_state().unwrap();
        assert!(state.buffers[0].deleted);
        assert!(state.buffers[0].text.is_empty());
    }
}
