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

mod codec;
mod layout;
mod loader;
mod writer;

pub use self::layout::FjallFormatStatus;
use self::layout::{FjallKeyspaces, check_format, write_format_markers};
use self::loader::{load_commits, load_last_commit_version, load_state, load_state_version};
use self::writer::FjallCommitWriter;
use super::{CommitProvider, PersistedKernelState};
use crate::Commit;
use fjall::Database;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FjallDurabilityMode {
    Relaxed,
    Strict,
}

pub struct FjallStateProvider {
    keyspaces: FjallKeyspaces,
    writer: FjallCommitWriter,
}

impl FjallStateProvider {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::open_with_durability(path, FjallDurabilityMode::Relaxed)
    }

    pub fn open_strict(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::open_with_durability(path, FjallDurabilityMode::Strict)
    }

    pub fn open_with_durability(
        path: impl AsRef<Path>,
        durability: FjallDurabilityMode,
    ) -> Result<Self, String> {
        let path = path.as_ref();
        match Self::check_format(path)? {
            FjallFormatStatus::Fresh
            | FjallFormatStatus::Uninitialized
            | FjallFormatStatus::Current => {}
            FjallFormatStatus::MigrationRequired {
                stored_version,
                stored_shape,
                current_version,
                current_shape,
            } => {
                return Err(format!(
                    "fjall relation-kernel store needs migration: version {:?}, shape {:?}; current version {current_version}, shape {current_shape}",
                    stored_version, stored_shape
                ));
            }
        }

        let database = Database::builder(path)
            .open()
            .map_err(|error| format!("failed to open fjall database: {error}"))?;
        let keyspaces = FjallKeyspaces::open(&database)?;
        write_format_markers(&keyspaces.metadata)?;
        let initial_version = load_state_version(&keyspaces.metadata)?
            .unwrap_or(load_last_commit_version(&keyspaces.commits)?);
        let writer = FjallCommitWriter::spawn(
            database.clone(),
            keyspaces.clone(),
            initial_version,
            durability,
        )?;
        Ok(Self { keyspaces, writer })
    }

    pub fn check_format(path: impl AsRef<Path>) -> Result<FjallFormatStatus, String> {
        check_format(path)
    }

    pub fn load_commits(&self) -> Result<Vec<Commit>, String> {
        load_commits(&self.keyspaces.commits)
    }

    pub fn load_state(&self) -> Result<PersistedKernelState, String> {
        load_state(&self.keyspaces)
    }

    pub fn completed_version(&self) -> u64 {
        self.writer.completed_version()
    }

    pub fn queued_version(&self) -> u64 {
        self.writer.queued_version()
    }

    pub fn durability(&self) -> FjallDurabilityMode {
        self.writer.durability()
    }

    pub fn last_write_error(&self) -> Option<String> {
        self.writer.last_write_error()
    }
}

impl CommitProvider for FjallStateProvider {
    fn persist_commit(&self, commit: &Commit) -> Result<(), String> {
        self.writer.persist_commit(commit)
    }

    fn flush(&self) -> Result<(), String> {
        self.writer.flush()
    }
}
