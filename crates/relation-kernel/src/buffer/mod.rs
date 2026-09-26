// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

mod delta;
pub(crate) mod store;
mod text;

pub use delta::{Delta, DeltaBudget, DeltaError, InsertionAffinity, Replacement};
pub use text::{Text, TextError};

pub use store::{
    BufferChange, BufferConflictPolicy, BufferError, BufferMetadata, BufferState,
    PersistedBufferState,
};
