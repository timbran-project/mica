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

use fjall::{Database, Keyspace, KeyspaceCreateOptions};
use std::path::Path;

const FJALL_FORMAT_VERSION: &str = "mica-relation-kernel-state-2.0.0";
const FJALL_SHAPE: &str =
    "relations:v1;rules:v1;facts:v1;commits:v2;buffers:v1;encoding:mica-binary-v1";
const FJALL_METADATA_KEYSPACE: &str = "metadata";
const FJALL_RELATIONS_KEYSPACE: &str = "relations";
const FJALL_RULES_KEYSPACE: &str = "rules";
const FJALL_FACTS_KEYSPACE: &str = "facts";
const FJALL_COMMITS_KEYSPACE: &str = "commits";
const FORMAT_VERSION_KEY: &[u8] = b"format_version";
const SHAPE_KEY: &[u8] = b"shape";
pub(super) const STATE_VERSION_KEY: &[u8] = b"state_version";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FjallFormatStatus {
    Fresh,
    Uninitialized,
    Current,
    MigrationRequired {
        stored_version: Option<String>,
        stored_shape: Option<String>,
        current_version: &'static str,
        current_shape: &'static str,
    },
}

#[derive(Clone)]
pub(super) struct FjallKeyspaces {
    pub(super) metadata: Keyspace,
    pub(super) relations: Keyspace,
    pub(super) rules: Keyspace,
    pub(super) facts: Keyspace,
    pub(super) commits: Keyspace,
    pub(super) buffers: Keyspace,
}

impl FjallKeyspaces {
    pub(super) fn open(database: &Database) -> Result<Self, String> {
        Ok(Self {
            metadata: open_keyspace(database, FJALL_METADATA_KEYSPACE)?,
            relations: open_keyspace(database, FJALL_RELATIONS_KEYSPACE)?,
            rules: open_keyspace(database, FJALL_RULES_KEYSPACE)?,
            facts: open_keyspace(database, FJALL_FACTS_KEYSPACE)?,
            commits: open_keyspace(database, FJALL_COMMITS_KEYSPACE)?,
            buffers: open_keyspace(database, "buffers")?,
        })
    }

    fn is_uninitialized(&self) -> bool {
        !has_entries(&self.relations)
            && !has_entries(&self.rules)
            && !has_entries(&self.facts)
            && !has_entries(&self.commits)
            && !has_entries(&self.buffers)
    }
}

pub(super) fn check_format(path: impl AsRef<Path>) -> Result<FjallFormatStatus, String> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(FjallFormatStatus::Fresh);
    }

    let database = Database::builder(path)
        .open()
        .map_err(|error| format!("failed to open fjall database for format check: {error}"))?;
    let keyspaces = FjallKeyspaces::open(&database)?;
    let stored_version = read_marker(&keyspaces.metadata, FORMAT_VERSION_KEY)?;
    let stored_shape = read_marker(&keyspaces.metadata, SHAPE_KEY)?;

    match (&stored_version, &stored_shape) {
        (None, None) if keyspaces.is_uninitialized() => Ok(FjallFormatStatus::Uninitialized),
        (Some(version), Some(shape)) if version == FJALL_FORMAT_VERSION && shape == FJALL_SHAPE => {
            Ok(FjallFormatStatus::Current)
        }
        _ => Ok(FjallFormatStatus::MigrationRequired {
            stored_version,
            stored_shape,
            current_version: FJALL_FORMAT_VERSION,
            current_shape: FJALL_SHAPE,
        }),
    }
}

pub(super) fn write_format_markers(metadata: &Keyspace) -> Result<(), String> {
    metadata
        .insert(FORMAT_VERSION_KEY, FJALL_FORMAT_VERSION.as_bytes())
        .map_err(|error| format!("failed to write fjall format marker: {error}"))?;
    metadata
        .insert(SHAPE_KEY, FJALL_SHAPE.as_bytes())
        .map_err(|error| format!("failed to write fjall shape marker: {error}"))?;
    Ok(())
}

fn open_keyspace(database: &Database, name: &str) -> Result<Keyspace, String> {
    database
        .keyspace(name, KeyspaceCreateOptions::default)
        .map_err(|error| format!("failed to open fjall keyspace `{name}`: {error}"))
}

fn read_marker(metadata: &Keyspace, key: &[u8]) -> Result<Option<String>, String> {
    metadata
        .get(key)
        .map_err(|error| format!("failed to read fjall metadata marker: {error}"))?
        .map(|bytes| {
            String::from_utf8(bytes.to_vec())
                .map_err(|error| format!("invalid utf-8 in fjall metadata marker: {error}"))
        })
        .transpose()
}

fn has_entries(keyspace: &Keyspace) -> bool {
    keyspace.iter().next().is_some()
}
