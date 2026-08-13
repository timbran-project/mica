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

//! Live MVCC relation storage for Mica.
//!
//! This crate is the first relation-kernel slice: cataloged n-ary set
//! relations, transaction-local assert/retract overlays, snapshot reads,
//! commit-time conflict validation, catalog commits, rule evaluation, and a
//! pluggable commit provider boundary. It intentionally follows mooR's live
//! transaction shape while keeping physical index storage narrow and replaceable.

mod batch;
mod catalog;
mod closure;
mod computed;
mod differential;
mod dispatch;
mod dispatch_cache;
mod error;
mod execution;
mod fact;
mod index;
mod kernel;
mod materialized;
mod metadata;
mod method_program_cache;
pub mod metrics;
mod neighborhood;
mod projected;
mod provider;
mod query;
mod radix_key;
pub mod relation_algebra;
mod relation_states;
mod rules;
mod snapshot;
mod transaction;
mod tuple;
mod workspace;

#[cfg(test)]
mod differential_tests;
#[cfg(test)]
mod tests;

use mica_var::Identity;

pub use catalog::{
    CatalogFact, CatalogPredicate, system_computed_relations, system_row_source_relation,
};
pub use closure::{
    delegates_reaches, delegates_star, delegates_star_from, materialize_delegates_star,
};
pub use computed::{ComputedRelation, ComputedRelationRead, ComputedRelationRegistry};
pub use dispatch::{
    ApplicableMethod, ApplicableMethodCall, DispatchRead, DispatchRelations,
    applicable_method_calls, applicable_method_calls_normalized, applicable_method_entries,
    applicable_methods, applicable_positional_methods, applicable_positional_methods_cached,
    frob_only_dispatch_restriction, method_program_id, named_method_args, normalize_dispatch_roles,
    ordered_params, positional_method_args, role_value, unrestricted_dispatch_restriction,
};
pub use error::{Conflict, ConflictKind, KernelError};
pub use execution::{
    AccelerationDecline, AccelerationOutcome, EqualityJoin, EqualityJoinMatch, ExecutionAdmission,
    ExecutionContext, MembershipSelection, RelationAccelerator,
};
pub use fact::Fact;
pub use kernel::RelationKernel;
pub use materialized::materialize_rule_set;
pub use metadata::{
    ConflictPolicy, RelationDurability, RelationMetadata, RelationSchema, TupleIndexSpec,
};
pub use neighborhood::{MentionedFact, SubjectFact};
pub use projected::{ProjectedDelta, ProjectedStore};
pub use provider::{CommitProvider, InMemoryCommitProvider, PersistedKernelState};
#[cfg(feature = "fjall-provider")]
pub use provider::{FjallDurabilityMode, FjallFormatStatus, FjallStateProvider};
pub use query::{
    PackedRelation, PreparedQuery, QueryPlan, RelationCapabilities, RelationRead, RelationSource,
    ScanControl, ValueDomain,
};
pub use rules::{
    Atom, Rule, RuleBodyItem, RuleComparisonOp, RuleDefinition, RuleError, RuleEvalError,
    RuleGuard, RuleSet, Term,
};
pub use snapshot::{CatalogChange, Commit, CommitResult, FactChange, FactChangeKind, Snapshot};
pub use transaction::Transaction;
pub use tuple::Tuple;
pub use workspace::RelationWorkspace;

pub type RelationId = Identity;
pub type FactId = Identity;
pub type Version = u64;
