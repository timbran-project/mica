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

//! Register-based bytecode VM for Mica.
//!
//! This crate defines Mica bytecode programs and the register interpreter that
//! executes them until a host boundary is reached. Task scheduling, source
//! execution, filein/fileout, and live-environment concerns live in
//! `mica-runtime`.

mod authority;
mod builtin;
mod effect;
mod error;
pub mod metrics;
mod program;
mod vm;

#[cfg(all(test, feature = "cranelift"))]
mod cranelift_tests;

pub use authority::{AuthorityContext, CapabilityGrant, CapabilityOp, CapabilityScope};
pub use builtin::{
    Builtin, BuiltinContext, BuiltinRegistry, BuiltinResultKind, ClientBuiltin,
    ClientBuiltinContext, ClientBuiltinRegistry, MailboxRuntime, RuntimeContext, RuntimePorts,
    SYSTEM_ENDPOINT, SubscriptionInitialDelivery, SubscriptionOperation, SubscriptionRequest,
    SubscriptionSubject,
};
pub use effect::Emission;
pub use error::RuntimeError;
pub use program::{
    CatchHandler, ErrorField, ExternalRequest, Instruction, KindCheckSite, ListItem,
    MailboxRecvRequest, MailboxSend, MapItem, Operand, Program, ProgramBuilder, ProgramResolver,
    QueryBinding, Register, RelationArg, RelationRowContract, RelationTypeContract,
    RuntimeBinaryOp, RuntimeUnaryOp, SpawnRequest, SpawnTarget, SuspendKind, TypeContract,
    TypeLiteralContract,
};
pub use vm::{
    Frame, ProjectedVmHostContext, RegisterVm, VmHost, VmHostContext, VmHostResponse, VmState,
};
