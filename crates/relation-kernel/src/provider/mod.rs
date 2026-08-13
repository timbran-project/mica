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

mod memory;

#[cfg(feature = "fjall-provider")]
mod fjall;

use crate::{Commit, RelationId, RelationMetadata, RuleDefinition, Tuple, Version};

#[cfg(feature = "fjall-provider")]
pub use fjall::{FjallDurabilityMode, FjallFormatStatus, FjallStateProvider};
pub use memory::InMemoryCommitProvider;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedKernelState {
    pub version: Version,
    pub relations: Vec<RelationMetadata>,
    pub rules: Vec<RuleDefinition>,
    pub facts: Vec<(RelationId, Tuple)>,
}

pub trait CommitProvider: Send + Sync {
    fn persist_commit(&self, commit: &Commit) -> Result<(), String>;

    fn flush(&self) -> Result<(), String> {
        Ok(())
    }
}
