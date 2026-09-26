// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use super::codec::{decode_commit, decode_relation_metadata, decode_rule_definition, decode_tuple};
use super::layout::{FjallKeyspaces, STATE_VERSION_KEY};
use crate::provider::PersistedKernelState;
use crate::{Commit, RelationId};
use fjall::Keyspace;
use mica_var::Identity;

pub(super) fn load_state_version(metadata: &Keyspace) -> Result<Option<u64>, String> {
    metadata
        .get(STATE_VERSION_KEY)
        .map_err(|error| format!("failed to read fjall state version: {error}"))?
        .map(|bytes| {
            let bytes = bytes.as_ref();
            if bytes.len() != 8 {
                return Err(format!(
                    "invalid fjall state version length {}",
                    bytes.len()
                ));
            }
            Ok(u64::from_be_bytes(bytes.try_into().unwrap()))
        })
        .transpose()
}

pub(super) fn load_last_commit_version(commits: &Keyspace) -> Result<u64, String> {
    let Some(entry) = commits.iter().next_back() else {
        return Ok(0);
    };
    let (key, _) = entry
        .into_inner()
        .map_err(|error| format!("failed to read fjall commit key: {error}"))?;
    let key = key.as_ref();
    if key.len() != 8 {
        return Err(format!("invalid fjall commit key length {}", key.len()));
    }
    Ok(u64::from_be_bytes(key.try_into().unwrap()))
}

pub(super) fn load_commits(commits: &Keyspace) -> Result<Vec<Commit>, String> {
    let mut out = Vec::new();
    for entry in commits.iter() {
        let (_, value) = entry
            .into_inner()
            .map_err(|error| format!("failed to read fjall commit entry: {error}"))?;
        out.push(decode_commit(value.as_ref())?);
    }
    Ok(out)
}

pub(super) fn load_state(keyspaces: &FjallKeyspaces) -> Result<PersistedKernelState, String> {
    let version = match load_state_version(&keyspaces.metadata)? {
        Some(version) => version,
        None => load_last_commit_version(&keyspaces.commits)?,
    };

    Ok(PersistedKernelState {
        buffers: super::buffers::load_buffers(&keyspaces.buffers)?,
        version,
        relations: load_relations(&keyspaces.relations)?,
        rules: load_rules(&keyspaces.rules)?,
        facts: load_facts(&keyspaces.facts)?,
    })
}

fn load_relations(relations: &Keyspace) -> Result<Vec<crate::RelationMetadata>, String> {
    let mut out = Vec::new();
    for entry in relations.iter() {
        let (_, value) = entry
            .into_inner()
            .map_err(|error| format!("failed to read fjall relation entry: {error}"))?;
        out.push(decode_relation_metadata(value.as_ref())?);
    }
    Ok(out)
}

fn load_rules(rules: &Keyspace) -> Result<Vec<crate::RuleDefinition>, String> {
    let mut out = Vec::new();
    for entry in rules.iter() {
        let (_, value) = entry
            .into_inner()
            .map_err(|error| format!("failed to read fjall rule entry: {error}"))?;
        out.push(decode_rule_definition(value.as_ref())?);
    }
    Ok(out)
}

fn load_facts(facts: &Keyspace) -> Result<Vec<(RelationId, crate::Tuple)>, String> {
    let mut out = Vec::new();
    for entry in facts.iter() {
        let (key, value) = entry
            .into_inner()
            .map_err(|error| format!("failed to read fjall fact entry: {error}"))?;
        let key = key.as_ref();
        if key.len() < 8 {
            return Err(format!("invalid fjall fact key length {}", key.len()));
        }
        let relation = Identity::new(u64::from_be_bytes(key[..8].try_into().unwrap()))
            .ok_or_else(|| "invalid relation identity in fjall fact key".to_owned())?;
        out.push((relation, decode_tuple(value.as_ref())?));
    }
    Ok(out)
}
