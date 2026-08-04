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

use crate::{RelationId, RuleError, Tuple, Version};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelError {
    UnknownRelation(RelationId),
    UnknownRule(crate::FactId),
    RelationAlreadyExists(RelationId),
    ReadOnlyRelation(RelationId),
    MissingRequiredBindings {
        relation: RelationId,
        positions: Vec<u16>,
    },
    ArityMismatch {
        relation: RelationId,
        expected: u16,
        actual: usize,
    },
    InvalidComputedRelation {
        relation: RelationId,
        message: String,
    },
    NonPersistentValue {
        relation: RelationId,
        tuple: Tuple,
    },
    InvalidIndex {
        relation: RelationId,
        position: u16,
        arity: u16,
    },
    StaleStagedSnapshot {
        expected: Version,
        actual: Version,
    },
    Persistence(String),
    DifferentialWeightOverflow {
        relation: RelationId,
        operation: &'static str,
        version: Version,
        left: i64,
        right: i64,
    },
    NegativeDifferentialSupport {
        relation: RelationId,
        tuple: Tuple,
        version: Version,
        support: i64,
    },
    Rule(RuleError),
    Conflict(Conflict),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub relation: RelationId,
    pub tuple: Tuple,
    pub kind: ConflictKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConflictKind {
    AssertRetract,
    FunctionalKeyChanged,
}
