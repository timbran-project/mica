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

use crate::RuntimeError;
use arc_swap::ArcSwap;
use mica_relation_kernel::{DispatchRelations, RelationId, RelationRead};
use mica_var::{Identity, Symbol, Value, ValueKind};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

#[cfg(feature = "cranelift")]
use mica_var::abi::{VALUE_INT_TAG, borrowed_value_bits, value_is_immediate, value_tag};
#[cfg(feature = "cranelift")]
use mica_vm_cranelift::{
    CompiledFloatLoop, CompiledIntegerLoop, CompiledNaturalLoop, FloatArithmetic, FloatComparison,
    FloatLoopPlan, IntegerComparison, NaturalLoopInstruction, NaturalLoopPlan, ScalarComparison,
};
#[cfg(feature = "cranelift")]
use std::collections::BTreeSet;
#[cfg(feature = "cranelift")]
use std::fmt::{Debug, Formatter};
#[cfg(feature = "cranelift")]
use std::sync::OnceLock;
#[cfg(feature = "cranelift")]
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Register(pub u16);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operand {
    Register(Register),
    Value(Value),
}

impl From<Register> for Operand {
    fn from(value: Register) -> Self {
        Self::Register(value)
    }
}

impl From<Value> for Operand {
    fn from(value: Value) -> Self {
        Self::Value(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListItem {
    Value(Operand),
    Splice(Operand),
}

impl ListItem {
    fn operand(&self) -> &Operand {
        match self {
            Self::Value(operand) | Self::Splice(operand) => operand,
        }
    }
}

impl From<Operand> for ListItem {
    fn from(value: Operand) -> Self {
        Self::Value(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MapItem {
    Entry(Operand, Operand),
    Splice(Operand),
}

impl MapItem {
    fn operands(&self) -> impl Iterator<Item = &Operand> {
        match self {
            Self::Entry(key, value) => [Some(key), Some(value), None].into_iter().flatten(),
            Self::Splice(operand) => [Some(operand), None, None].into_iter().flatten(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryBinding {
    pub name: Symbol,
    pub position: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelationArg {
    Value(Operand),
    Splice(Operand),
    Query(Symbol),
    Hole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatchHandler {
    pub code: Option<Value>,
    pub binding: Option<Register>,
    pub target: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorField {
    Code,
    Message,
    Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KindCheckSite {
    Binding,
    Parameter,
    Builtin,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TypeContract {
    Dynamic,
    Never,
    Kind(ValueKind),
    Literal(TypeLiteralContract),
    Union(Vec<TypeContract>),
    Relation(RelationTypeContract),
}

impl TypeContract {
    pub fn exact_outer_kind(&self) -> Option<ValueKind> {
        match self {
            Self::Dynamic | Self::Never => None,
            Self::Kind(kind) => Some(*kind),
            Self::Literal(literal) => Some(literal.outer_kind()),
            Self::Union(types) => {
                let mut kinds = types.iter().filter_map(Self::exact_outer_kind);
                let first = kinds.next()?;
                kinds.all(|kind| kind == first).then_some(first)
            }
            Self::Relation(_) => Some(ValueKind::Relation),
        }
    }
}

impl fmt::Display for TypeContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dynamic => formatter.write_str("dynamic"),
            Self::Never => formatter.write_str("never"),
            Self::Kind(kind) => formatter.write_str(kind.name()),
            Self::Literal(literal) => write!(formatter, "{literal}"),
            Self::Union(types) => {
                for (index, ty) in types.iter().enumerate() {
                    if index != 0 {
                        formatter.write_str(" | ")?;
                    }
                    write!(formatter, "{ty}")?;
                }
                Ok(())
            }
            Self::Relation(relation) => write!(formatter, "{relation}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TypeLiteralContract {
    Bool(bool),
    Symbol(Symbol),
    ErrorCode(Symbol),
    Unit,
    EmptyRelation,
}

impl TypeLiteralContract {
    pub const fn outer_kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::Symbol(_) => ValueKind::Symbol,
            Self::ErrorCode(_) => ValueKind::ErrorCode,
            Self::Unit | Self::EmptyRelation => ValueKind::Relation,
        }
    }
}

impl fmt::Display for TypeLiteralContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Symbol(value) => write!(formatter, ":{}", symbol_text(*value)),
            Self::ErrorCode(value) => formatter.write_str(symbol_text(*value)),
            Self::Unit => formatter.write_str("()"),
            Self::EmptyRelation => formatter.write_str("[] {}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RelationRowContract {
    pub columns: Vec<(Symbol, TypeContract)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RelationTypeContract {
    pub alternatives: Vec<RelationRowContract>,
    pub minimum_rows: usize,
    pub maximum_rows: Option<usize>,
}

impl fmt::Display for RelationTypeContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("relation<")?;
        for (alternative_index, alternative) in self.alternatives.iter().enumerate() {
            if alternative_index != 0 {
                formatter.write_str(" | ")?;
            }
            formatter.write_str("{")?;
            for (column_index, (name, ty)) in alternative.columns.iter().enumerate() {
                if column_index != 0 {
                    formatter.write_str(", ")?;
                }
                write!(formatter, ":{} -> {ty}", symbol_text(*name))?;
            }
            formatter.write_str("}")?;
        }
        formatter.write_str("> where rows in ")?;
        match self.maximum_rows {
            Some(maximum) if maximum == self.minimum_rows => write!(formatter, "{maximum}"),
            Some(maximum) => write!(formatter, "{}..{maximum}", self.minimum_rows),
            None => write!(formatter, "{}..*", self.minimum_rows),
        }
    }
}

fn symbol_text(symbol: Symbol) -> &'static str {
    symbol.name().unwrap_or("<unnamed>")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnRequest {
    pub selector: Symbol,
    pub target: SpawnTarget,
    pub delay_millis: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpawnTarget {
    NamedRoles(Vec<(Symbol, Value)>),
    PositionalArgs(Vec<Value>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailboxRecvRequest {
    pub receivers: Vec<Value>,
    pub timeout_millis: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailboxSend {
    pub sender: Value,
    pub value: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalRequest {
    pub service: Symbol,
    pub payload: Value,
    pub timeout_millis: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SuspendKind {
    Commit,
    Never,
    TimedMillis(u64),
    WaitingForInput(Value),
    MailboxRecv(MailboxRecvRequest),
    Spawn(SpawnRequest),
    ExternalRequest(ExternalRequest),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Instruction {
    Load {
        dst: Register,
        value: Value,
    },
    Move {
        dst: Register,
        src: Register,
    },
    CheckKind {
        value: Register,
        expected: ValueKind,
        site: KindCheckSite,
        subject: Symbol,
    },
    CheckType {
        value: Register,
        expected: TypeContract,
        site: KindCheckSite,
        subject: Symbol,
    },
    Unary {
        dst: Register,
        op: RuntimeUnaryOp,
        src: Register,
    },
    Binary {
        dst: Register,
        op: RuntimeBinaryOp,
        left: Register,
        right: Register,
    },
    BuildList {
        dst: Register,
        items: Vec<ListItem>,
    },
    BuildRelation {
        dst: Register,
        heading: Vec<Symbol>,
        cells: Vec<Operand>,
        row_count: u16,
    },
    BuildMap {
        dst: Register,
        entries: Vec<(Operand, Operand)>,
    },
    BuildMapDynamic {
        dst: Register,
        items: Vec<MapItem>,
    },
    BuildRange {
        dst: Register,
        start: Operand,
        end: Option<Operand>,
    },
    Index {
        dst: Register,
        collection: Register,
        index: Operand,
    },
    SetIndex {
        dst: Register,
        collection: Register,
        index: Operand,
        value: Operand,
    },
    ErrorField {
        dst: Register,
        error: Register,
        field: ErrorField,
    },
    One {
        dst: Register,
        src: Register,
    },
    CollectionLen {
        dst: Register,
        collection: Register,
    },
    CollectionKeyAt {
        dst: Register,
        collection: Register,
        index: Register,
    },
    CollectionValueAt {
        dst: Register,
        collection: Register,
        index: Register,
    },
    ScanExists {
        dst: Register,
        relation: RelationId,
        bindings: Vec<Option<Operand>>,
    },
    ScanBindings {
        dst: Register,
        relation: RelationId,
        bindings: Vec<Option<Operand>>,
        outputs: Vec<QueryBinding>,
    },
    ScanValue {
        dst: Register,
        relation: RelationId,
        key: Operand,
    },
    Assert {
        relation: RelationId,
        values: Vec<Operand>,
    },
    Retract {
        relation: RelationId,
        values: Vec<Operand>,
    },
    RetractWhere {
        relation: RelationId,
        bindings: Vec<Option<Operand>>,
    },
    ScanDynamic {
        dst: Register,
        relation: RelationId,
        args: Vec<RelationArg>,
    },
    AssertDynamic {
        relation: RelationId,
        args: Vec<RelationArg>,
    },
    RetractDynamic {
        relation: RelationId,
        args: Vec<RelationArg>,
    },
    ReplaceFunctional {
        relation: RelationId,
        values: Vec<Operand>,
    },
    Branch {
        condition: Register,
        if_true: usize,
        if_false: usize,
    },
    Jump {
        target: usize,
    },
    EnterTry {
        catches: Vec<CatchHandler>,
        finally: Option<usize>,
        end: usize,
    },
    ExitTry,
    EndFinally,
    Emit {
        target: Operand,
        value: Operand,
    },
    LoadFunction {
        dst: Register,
        program: Arc<Program>,
        captures: Vec<Operand>,
        min_arity: u16,
        max_arity: u16,
    },
    CallValue {
        dst: Register,
        callee: Operand,
        args: Vec<Operand>,
    },
    CallValueDynamic {
        dst: Register,
        callee: Operand,
        args: Vec<ListItem>,
    },
    Call {
        dst: Register,
        program: Arc<Program>,
        args: Vec<Operand>,
    },
    BuiltinCall {
        dst: Register,
        name: Symbol,
        result_kind: Option<ValueKind>,
        args: Vec<Operand>,
    },
    BuiltinCallDynamic {
        dst: Register,
        name: Symbol,
        result_kind: Option<ValueKind>,
        args: Vec<ListItem>,
    },
    Dispatch {
        dst: Register,
        relations: DispatchRelations,
        program_relation: RelationId,
        program_bytes: RelationId,
        selector: Operand,
        roles: Vec<(Value, Operand)>,
    },
    DynamicDispatch {
        dst: Register,
        relations: DispatchRelations,
        program_relation: RelationId,
        program_bytes: RelationId,
        selector: Operand,
        roles: Operand,
    },
    PositionalDispatch {
        dst: Register,
        relations: DispatchRelations,
        program_relation: RelationId,
        program_bytes: RelationId,
        selector: Operand,
        args: Vec<Operand>,
    },
    PositionalDispatchDynamic {
        dst: Register,
        relations: DispatchRelations,
        program_relation: RelationId,
        program_bytes: RelationId,
        selector: Operand,
        args: Vec<ListItem>,
    },
    SpawnDispatch {
        dst: Register,
        selector: Operand,
        roles: Vec<(Value, Operand)>,
        delay: Option<Operand>,
    },
    SpawnDispatchDynamic {
        dst: Register,
        selector: Operand,
        roles: Operand,
        delay: Option<Operand>,
    },
    SpawnPositionalDispatch {
        dst: Register,
        selector: Operand,
        args: Vec<Operand>,
        delay: Option<Operand>,
    },
    SpawnPositionalDispatchDynamic {
        dst: Register,
        selector: Operand,
        args: Vec<ListItem>,
        delay: Option<Operand>,
    },
    Commit,
    Suspend {
        kind: SuspendKind,
    },
    SuspendValue {
        dst: Register,
        duration: Option<Operand>,
    },
    CommitValue {
        dst: Register,
    },
    Read {
        dst: Register,
        metadata: Option<Operand>,
    },
    MailboxRecv {
        dst: Register,
        receivers: Operand,
        timeout: Option<Operand>,
    },
    ExternalRequest {
        dst: Register,
        service: Operand,
        payload: Operand,
        timeout: Option<Operand>,
    },
    RollbackRetry,
    Return {
        value: Operand,
    },
    Abort {
        error: Operand,
    },
    Raise {
        error: Operand,
        message: Option<Operand>,
        value: Option<Operand>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConstId(pub(crate) u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProgramId(pub(crate) u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RelationRef(pub(crate) u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DispatchSpecId(pub(crate) u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SuspendKindId(pub(crate) u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Target(pub(crate) u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TableRange {
    start: u16,
    len: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperandRef {
    Register(Register),
    Constant(ConstId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactListItem {
    Value(OperandRef),
    Splice(OperandRef),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactMapItem {
    Entry(OperandRef, OperandRef),
    Splice(OperandRef),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactRelationArg {
    Value(OperandRef),
    Splice(OperandRef),
    Query(Symbol),
    Hole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompactCatchHandler {
    pub(crate) code: Option<ConstId>,
    pub(crate) binding: Option<Register>,
    pub(crate) target: Target,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DispatchSpec {
    pub(crate) relations: DispatchRelations,
    pub(crate) program_relation: RelationId,
    pub(crate) program_bytes: RelationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Opcode {
    Load {
        dst: Register,
        value: ConstId,
    },
    Move {
        dst: Register,
        src: Register,
    },
    CheckKind {
        value: Register,
        expected: ValueKind,
        site: KindCheckSite,
        subject: Symbol,
    },
    CheckType {
        value: Register,
        expected: TypeContract,
        site: KindCheckSite,
        subject: Symbol,
    },
    Unary {
        dst: Register,
        op: RuntimeUnaryOp,
        src: Register,
    },
    Binary {
        dst: Register,
        op: RuntimeBinaryOp,
        left: Register,
        right: Register,
    },
    BuildList {
        dst: Register,
        items: TableRange,
    },
    BuildRelation {
        dst: Register,
        heading: TableRange,
        cells: TableRange,
        row_count: u16,
    },
    BuildMap {
        dst: Register,
        entries: TableRange,
    },
    BuildMapDynamic {
        dst: Register,
        items: TableRange,
    },
    BuildRange {
        dst: Register,
        start: OperandRef,
        end: Option<OperandRef>,
    },
    Index {
        dst: Register,
        collection: Register,
        index: OperandRef,
    },
    SetIndex {
        dst: Register,
        collection: Register,
        index: OperandRef,
        value: OperandRef,
    },
    ErrorField {
        dst: Register,
        error: Register,
        field: ErrorField,
    },
    One {
        dst: Register,
        src: Register,
    },
    CollectionLen {
        dst: Register,
        collection: Register,
    },
    CollectionKeyAt {
        dst: Register,
        collection: Register,
        index: Register,
    },
    CollectionValueAt {
        dst: Register,
        collection: Register,
        index: Register,
    },
    ScanExists {
        dst: Register,
        relation: RelationRef,
        bindings: TableRange,
    },
    ScanBindings {
        dst: Register,
        relation: RelationRef,
        bindings: TableRange,
        outputs: TableRange,
    },
    ScanValue {
        dst: Register,
        relation: RelationRef,
        key: OperandRef,
    },
    Assert {
        relation: RelationRef,
        values: TableRange,
    },
    Retract {
        relation: RelationRef,
        values: TableRange,
    },
    RetractWhere {
        relation: RelationRef,
        bindings: TableRange,
    },
    ScanDynamic {
        dst: Register,
        relation: RelationRef,
        args: TableRange,
    },
    AssertDynamic {
        relation: RelationRef,
        args: TableRange,
    },
    RetractDynamic {
        relation: RelationRef,
        args: TableRange,
    },
    ReplaceFunctional {
        relation: RelationRef,
        values: TableRange,
    },
    Branch {
        condition: Register,
        if_true: Target,
        if_false: Target,
    },
    Jump {
        target: Target,
    },
    EnterTry {
        catches: TableRange,
        finally: Option<Target>,
        end: Target,
    },
    ExitTry,
    EndFinally,
    Emit {
        target: OperandRef,
        value: OperandRef,
    },
    LoadFunction {
        dst: Register,
        program: ProgramId,
        captures: TableRange,
        min_arity: u16,
        max_arity: u16,
    },
    CallValue {
        dst: Register,
        callee: OperandRef,
        args: TableRange,
    },
    CallValueDynamic {
        dst: Register,
        callee: OperandRef,
        args: TableRange,
    },
    Call {
        dst: Register,
        program: ProgramId,
        args: TableRange,
    },
    BuiltinCall {
        dst: Register,
        name: Symbol,
        result_kind: Option<ValueKind>,
        args: TableRange,
    },
    BuiltinCallDynamic {
        dst: Register,
        name: Symbol,
        result_kind: Option<ValueKind>,
        args: TableRange,
    },
    Dispatch {
        dst: Register,
        spec: DispatchSpecId,
        selector: OperandRef,
        roles: TableRange,
    },
    DynamicDispatch {
        dst: Register,
        spec: DispatchSpecId,
        selector: OperandRef,
        roles: OperandRef,
    },
    PositionalDispatch {
        dst: Register,
        spec: DispatchSpecId,
        selector: OperandRef,
        args: TableRange,
    },
    PositionalDispatchDynamic {
        dst: Register,
        spec: DispatchSpecId,
        selector: OperandRef,
        args: TableRange,
    },
    SpawnDispatch {
        dst: Register,
        selector: OperandRef,
        roles: TableRange,
        delay: Option<OperandRef>,
    },
    SpawnDispatchDynamic {
        dst: Register,
        selector: OperandRef,
        roles: OperandRef,
        delay: Option<OperandRef>,
    },
    SpawnPositionalDispatch {
        dst: Register,
        selector: OperandRef,
        args: TableRange,
        delay: Option<OperandRef>,
    },
    SpawnPositionalDispatchDynamic {
        dst: Register,
        selector: OperandRef,
        args: TableRange,
        delay: Option<OperandRef>,
    },
    Commit,
    Suspend {
        kind: SuspendKindId,
    },
    SuspendValue {
        dst: Register,
        duration: Option<OperandRef>,
    },
    CommitValue {
        dst: Register,
    },
    Read {
        dst: Register,
        metadata: Option<OperandRef>,
    },
    MailboxRecv {
        dst: Register,
        receivers: OperandRef,
        timeout: Option<OperandRef>,
    },
    ExternalRequest {
        dst: Register,
        service: OperandRef,
        payload: OperandRef,
        timeout: Option<OperandRef>,
    },
    RollbackRetry,
    Return {
        value: OperandRef,
    },
    Abort {
        error: OperandRef,
    },
    Raise {
        error: OperandRef,
        message: Option<OperandRef>,
        value: Option<OperandRef>,
    },
}

impl Opcode {
    fn destination(&self) -> Option<Register> {
        match self {
            Self::Load { dst, .. }
            | Self::Move { dst, .. }
            | Self::Unary { dst, .. }
            | Self::Binary { dst, .. }
            | Self::BuildList { dst, .. }
            | Self::BuildRelation { dst, .. }
            | Self::BuildMap { dst, .. }
            | Self::BuildMapDynamic { dst, .. }
            | Self::BuildRange { dst, .. }
            | Self::Index { dst, .. }
            | Self::SetIndex { dst, .. }
            | Self::ErrorField { dst, .. }
            | Self::One { dst, .. }
            | Self::CollectionLen { dst, .. }
            | Self::CollectionKeyAt { dst, .. }
            | Self::CollectionValueAt { dst, .. }
            | Self::ScanExists { dst, .. }
            | Self::ScanBindings { dst, .. }
            | Self::ScanValue { dst, .. }
            | Self::ScanDynamic { dst, .. }
            | Self::LoadFunction { dst, .. }
            | Self::CallValue { dst, .. }
            | Self::CallValueDynamic { dst, .. }
            | Self::Call { dst, .. }
            | Self::BuiltinCall { dst, .. }
            | Self::BuiltinCallDynamic { dst, .. }
            | Self::Dispatch { dst, .. }
            | Self::DynamicDispatch { dst, .. }
            | Self::PositionalDispatch { dst, .. }
            | Self::PositionalDispatchDynamic { dst, .. }
            | Self::SpawnDispatch { dst, .. }
            | Self::SpawnDispatchDynamic { dst, .. }
            | Self::SpawnPositionalDispatch { dst, .. }
            | Self::SpawnPositionalDispatchDynamic { dst, .. }
            | Self::SuspendValue { dst, .. }
            | Self::CommitValue { dst }
            | Self::Read { dst, .. }
            | Self::MailboxRecv { dst, .. }
            | Self::ExternalRequest { dst, .. } => Some(*dst),
            Self::CheckKind { .. }
            | Self::CheckType { .. }
            | Self::Assert { .. }
            | Self::Retract { .. }
            | Self::RetractWhere { .. }
            | Self::AssertDynamic { .. }
            | Self::RetractDynamic { .. }
            | Self::ReplaceFunctional { .. }
            | Self::Branch { .. }
            | Self::Jump { .. }
            | Self::EnterTry { .. }
            | Self::ExitTry
            | Self::EndFinally
            | Self::Emit { .. }
            | Self::Commit
            | Self::Suspend { .. }
            | Self::RollbackRetry
            | Self::Return { .. }
            | Self::Abort { .. }
            | Self::Raise { .. } => None,
        }
    }

    fn kind_fact_register(&self) -> Option<Register> {
        match self {
            Self::CheckKind { value, .. } | Self::CheckType { value, .. } => Some(*value),
            _ => self.destination(),
        }
    }
}

fn infer_kind_facts(
    register_count: usize,
    opcodes: &[Opcode],
    constants: &[Value],
    catches: &[CompactCatchHandler],
) -> Box<[Option<ValueKind>]> {
    let mut block_entries = vec![false; opcodes.len()];
    if let Some(entry) = block_entries.first_mut() {
        *entry = true;
    }
    let mut mark_target = |target: Target| {
        if let Some(entry) = block_entries.get_mut(target.0 as usize) {
            *entry = true;
        }
    };
    for opcode in opcodes {
        match opcode {
            Opcode::Branch {
                if_true, if_false, ..
            } => {
                mark_target(*if_true);
                mark_target(*if_false);
            }
            Opcode::Jump { target } => mark_target(*target),
            Opcode::EnterTry {
                catches: handlers,
                finally,
                end,
            } => {
                for handler in table_range(catches, *handlers) {
                    mark_target(handler.target);
                }
                if let Some(finally) = finally {
                    mark_target(*finally);
                }
                mark_target(*end);
            }
            _ => {}
        }
    }

    let mut registers = vec![None; register_count];
    let mut facts = Vec::with_capacity(opcodes.len());
    for (instruction, opcode) in opcodes.iter().enumerate() {
        if block_entries[instruction] {
            registers.fill(None);
        }
        let fact = infer_opcode_kind(opcode, constants, &registers);
        if let Some(register) = opcode.kind_fact_register()
            && let Some(slot) = registers.get_mut(register.0 as usize)
        {
            *slot = fact;
        }
        facts.push(fact);
        if matches!(
            opcode,
            Opcode::Branch { .. }
                | Opcode::Jump { .. }
                | Opcode::ExitTry
                | Opcode::EndFinally
                | Opcode::Return { .. }
                | Opcode::Abort { .. }
                | Opcode::Raise { .. }
        ) {
            registers.fill(None);
        }
    }
    facts.into_boxed_slice()
}

fn infer_opcode_kind(
    opcode: &Opcode,
    constants: &[Value],
    registers: &[Option<ValueKind>],
) -> Option<ValueKind> {
    let register_kind = |register: Register| registers.get(register.0 as usize).copied().flatten();
    match opcode {
        Opcode::Load { value, .. } => constants.get(value.0 as usize).map(Value::kind),
        Opcode::Move { src, .. } => register_kind(*src),
        Opcode::CheckKind { expected, .. } => Some(*expected),
        Opcode::CheckType { expected, .. } => expected.exact_outer_kind(),
        Opcode::Unary {
            op: RuntimeUnaryOp::Not,
            ..
        } => Some(ValueKind::Bool),
        Opcode::Unary {
            op: RuntimeUnaryOp::Neg,
            src,
            ..
        } => match register_kind(*src) {
            Some(kind @ (ValueKind::Int | ValueKind::Float)) => Some(kind),
            _ => None,
        },
        Opcode::Binary {
            op:
                RuntimeBinaryOp::Eq
                | RuntimeBinaryOp::Ne
                | RuntimeBinaryOp::Lt
                | RuntimeBinaryOp::Le
                | RuntimeBinaryOp::Gt
                | RuntimeBinaryOp::Ge,
            ..
        }
        | Opcode::ScanExists { .. } => Some(ValueKind::Bool),
        Opcode::Binary {
            op, left, right, ..
        } => infer_numeric_result_kind(*op, register_kind(*left), register_kind(*right)),
        Opcode::BuildList { .. } => Some(ValueKind::List),
        Opcode::BuildRelation { .. } | Opcode::ScanBindings { .. } => Some(ValueKind::Relation),
        Opcode::BuildMap { .. } | Opcode::BuildMapDynamic { .. } => Some(ValueKind::Map),
        Opcode::BuildRange { .. } => Some(ValueKind::Range),
        Opcode::SetIndex { collection, .. } => match register_kind(*collection) {
            Some(kind @ (ValueKind::List | ValueKind::Map)) => Some(kind),
            _ => None,
        },
        Opcode::ErrorField {
            error,
            field: ErrorField::Code,
            ..
        } if matches!(
            register_kind(*error),
            Some(ValueKind::Error | ValueKind::ErrorCode)
        ) =>
        {
            Some(ValueKind::ErrorCode)
        }
        Opcode::CollectionLen { .. } => Some(ValueKind::Int),
        Opcode::LoadFunction { .. } => Some(ValueKind::Function),
        Opcode::BuiltinCall { result_kind, .. }
        | Opcode::BuiltinCallDynamic { result_kind, .. } => *result_kind,
        _ => None,
    }
}

fn infer_type_facts(
    register_count: usize,
    opcodes: &[Opcode],
    constants: &[Value],
    operands: &[OperandRef],
    query_bindings: &[QueryBinding],
    catches: &[CompactCatchHandler],
    kind_facts: &[Option<ValueKind>],
) -> Box<[Option<TypeContract>]> {
    let mut block_entries = vec![false; opcodes.len()];
    if let Some(entry) = block_entries.first_mut() {
        *entry = true;
    }
    let mut mark_target = |target: Target| {
        if let Some(entry) = block_entries.get_mut(target.0 as usize) {
            *entry = true;
        }
    };
    for opcode in opcodes {
        match opcode {
            Opcode::Branch {
                if_true, if_false, ..
            } => {
                mark_target(*if_true);
                mark_target(*if_false);
            }
            Opcode::Jump { target } => mark_target(*target),
            Opcode::EnterTry {
                catches: handlers,
                finally,
                end,
            } => {
                for handler in table_range(catches, *handlers) {
                    mark_target(handler.target);
                }
                if let Some(finally) = finally {
                    mark_target(*finally);
                }
                mark_target(*end);
            }
            _ => {}
        }
    }

    let mut registers = vec![None; register_count];
    let mut facts = Vec::with_capacity(opcodes.len());
    for (instruction, opcode) in opcodes.iter().enumerate() {
        if block_entries[instruction] {
            registers.fill(None);
        }
        let fact = infer_opcode_type(opcode, constants, operands, query_bindings, &registers)
            .or_else(|| kind_facts[instruction].map(TypeContract::Kind));
        if let Some(register) = opcode.kind_fact_register()
            && let Some(slot) = registers.get_mut(register.0 as usize)
        {
            *slot = fact.clone();
        }
        facts.push(fact);
        if matches!(
            opcode,
            Opcode::Branch { .. }
                | Opcode::Jump { .. }
                | Opcode::ExitTry
                | Opcode::EndFinally
                | Opcode::Return { .. }
                | Opcode::Abort { .. }
                | Opcode::Raise { .. }
        ) {
            registers.fill(None);
        }
    }
    facts.into_boxed_slice()
}

fn infer_opcode_type(
    opcode: &Opcode,
    constants: &[Value],
    operands: &[OperandRef],
    query_bindings: &[QueryBinding],
    registers: &[Option<TypeContract>],
) -> Option<TypeContract> {
    let operand_type = |operand: OperandRef| match operand {
        OperandRef::Register(register) => registers.get(register.0 as usize)?.clone(),
        OperandRef::Constant(constant) => constants.get(constant.0 as usize).map(value_contract),
    };
    match opcode {
        Opcode::Load { value, .. } => constants.get(value.0 as usize).map(value_contract),
        Opcode::Move { src, .. } => registers.get(src.0 as usize)?.clone(),
        Opcode::CheckKind { expected, .. } => Some(TypeContract::Kind(*expected)),
        Opcode::CheckType { expected, .. } => Some(expected.clone()),
        Opcode::BuildRelation {
            heading,
            cells,
            row_count,
            ..
        } => {
            let heading = table_range(operands, *heading)
                .iter()
                .map(|operand| match operand {
                    OperandRef::Constant(id) => constants.get(id.0 as usize)?.as_symbol(),
                    OperandRef::Register(_) => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let cells = table_range(operands, *cells);
            let arity = heading.len();
            let mut order = (0..arity).collect::<Vec<_>>();
            order.sort_by_key(|position| heading[*position]);
            let mut alternatives = if *row_count == 0 {
                vec![RelationRowContract {
                    columns: order
                        .iter()
                        .map(|position| (heading[*position], TypeContract::Never))
                        .collect(),
                }]
            } else {
                (0..usize::from(*row_count))
                    .map(|row| {
                        Some(RelationRowContract {
                            columns: order
                                .iter()
                                .map(|position| {
                                    let cell = cells.get(row * arity + position)?;
                                    Some((heading[*position], operand_type(*cell)?))
                                })
                                .collect::<Option<Vec<_>>>()?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?
            };
            alternatives.sort();
            alternatives.dedup();
            Some(TypeContract::Relation(RelationTypeContract {
                alternatives,
                minimum_rows: usize::from(*row_count),
                maximum_rows: Some(usize::from(*row_count)),
            }))
        }
        Opcode::ScanBindings { outputs, .. } => {
            let mut columns = table_range(query_bindings, *outputs)
                .iter()
                .map(|binding| (binding.name, TypeContract::Dynamic))
                .collect::<Vec<_>>();
            columns.sort_by_key(|(name, _)| *name);
            Some(TypeContract::Relation(RelationTypeContract {
                alternatives: vec![RelationRowContract { columns }],
                minimum_rows: 0,
                maximum_rows: None,
            }))
        }
        _ => None,
    }
}

fn value_contract(value: &Value) -> TypeContract {
    if let Some(value) = value.as_bool() {
        return TypeContract::Literal(TypeLiteralContract::Bool(value));
    }
    if let Some(value) = value.as_symbol() {
        return TypeContract::Literal(TypeLiteralContract::Symbol(value));
    }
    if let Some(value) = value.as_error_code() {
        return TypeContract::Literal(TypeLiteralContract::ErrorCode(value));
    }
    if value.is_empty_relation() {
        return TypeContract::Literal(TypeLiteralContract::EmptyRelation);
    }
    if let Some(contract) = value.with_relation(|relation| {
        if relation.arity() == 0 && relation.len() == 1 {
            return TypeContract::Literal(TypeLiteralContract::Unit);
        }
        let alternatives = if relation.is_empty() {
            vec![RelationRowContract {
                columns: relation
                    .heading()
                    .iter()
                    .map(|name| (*name, TypeContract::Never))
                    .collect(),
            }]
        } else {
            relation
                .rows()
                .iter()
                .map(|row| RelationRowContract {
                    columns: relation
                        .heading()
                        .iter()
                        .zip(row.values())
                        .map(|(name, value)| (*name, value_contract(value)))
                        .collect(),
                })
                .collect()
        };
        TypeContract::Relation(RelationTypeContract {
            alternatives,
            minimum_rows: relation.len(),
            maximum_rows: Some(relation.len()),
        })
    }) {
        return contract;
    }
    TypeContract::Kind(value.kind())
}

fn infer_numeric_result_kind(
    op: RuntimeBinaryOp,
    left: Option<ValueKind>,
    right: Option<ValueKind>,
) -> Option<ValueKind> {
    let (Some(left), Some(right)) = (left, right) else {
        return None;
    };
    let both_numeric = matches!(left, ValueKind::Int | ValueKind::Float)
        && matches!(right, ValueKind::Int | ValueKind::Float);
    if !both_numeric {
        return None;
    }
    match op {
        RuntimeBinaryOp::Add | RuntimeBinaryOp::Sub | RuntimeBinaryOp::Mul => {
            if left == ValueKind::Int && right == ValueKind::Int {
                Some(ValueKind::Int)
            } else {
                Some(ValueKind::Float)
            }
        }
        RuntimeBinaryOp::Div => {
            if left == ValueKind::Int && right == ValueKind::Int {
                None
            } else {
                Some(ValueKind::Float)
            }
        }
        RuntimeBinaryOp::Rem => {
            if left == ValueKind::Int && right == ValueKind::Int {
                Some(ValueKind::Int)
            } else {
                Some(ValueKind::Float)
            }
        }
        RuntimeBinaryOp::Eq
        | RuntimeBinaryOp::Ne
        | RuntimeBinaryOp::Lt
        | RuntimeBinaryOp::Le
        | RuntimeBinaryOp::Gt
        | RuntimeBinaryOp::Ge => Some(ValueKind::Bool),
    }
}

#[cfg(feature = "cranelift")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IntegerLoopSite {
    pub(crate) entry_ip: usize,
    pub(crate) branch_ip: usize,
    pub(crate) exit_ip: usize,
    pub(crate) current: Register,
    pub(crate) step: Register,
    pub(crate) limit: Register,
    pub(crate) condition: Register,
}

#[cfg(feature = "cranelift")]
pub(crate) struct FloatLoopSite {
    pub(crate) entry_ip: usize,
    pub(crate) branch_ip: usize,
    pub(crate) exit_ip: usize,
    pub(crate) current: Register,
    pub(crate) step: Register,
    pub(crate) limit: Register,
    pub(crate) condition: Register,
    pub(crate) plan: FloatLoopPlan,
    compiled: OnceLock<Result<Arc<CompiledFloatLoop>, Box<str>>>,
}

#[cfg(feature = "cranelift")]
impl Debug for FloatLoopSite {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FloatLoopSite")
            .field("entry_ip", &self.entry_ip)
            .field("branch_ip", &self.branch_ip)
            .field("exit_ip", &self.exit_ip)
            .field("current", &self.current)
            .field("step", &self.step)
            .field("limit", &self.limit)
            .field("condition", &self.condition)
            .field("plan", &self.plan)
            .field("compiled", &self.compiled.get().is_some())
            .finish()
    }
}

#[cfg(feature = "cranelift")]
pub(crate) const MAX_NATURAL_LOOP_SLOTS: usize = 32;
#[cfg(feature = "cranelift")]
pub(crate) const MAX_NATURAL_LOOP_COLLECTION_VIEWS: usize = 32;

#[cfg(feature = "cranelift")]
struct NaturalLoopCollectionCandidates {
    keys: Box<[Option<ValueKind>]>,
    values: Box<[Option<ValueKind>]>,
}

#[cfg(feature = "cranelift")]
struct NaturalLoopUnboxedSlots {
    integers: u32,
    entry_integers: u32,
    floats: u32,
    entry_floats: u32,
}

#[cfg(feature = "cranelift")]
pub(crate) struct NaturalIntegerLoopSite {
    pub(crate) header_ip: usize,
    pub(crate) branch_ip: usize,
    pub(crate) body_ip: usize,
    pub(crate) exit_ip: usize,
    pub(crate) region_instruction_count: usize,
    pub(crate) registers: Box<[Register]>,
    pub(crate) collection_view_registers: Box<[Register]>,
    pub(crate) relation_shapes: Box<[(Register, RelationTypeContract)]>,
    pub(crate) current: Register,
    pub(crate) delta: i64,
    pub(crate) limit: Register,
    pub(crate) comparison: IntegerComparison,
    pub(crate) plan: NaturalLoopPlan,
    compiled: OnceLock<Result<Arc<CompiledNaturalLoop>, Box<str>>>,
}

#[cfg(feature = "cranelift")]
impl Debug for NaturalIntegerLoopSite {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NaturalIntegerLoopSite")
            .field("header_ip", &self.header_ip)
            .field("branch_ip", &self.branch_ip)
            .field("body_ip", &self.body_ip)
            .field("exit_ip", &self.exit_ip)
            .field("region_instruction_count", &self.region_instruction_count)
            .field("registers", &self.registers)
            .field("collection_view_registers", &self.collection_view_registers)
            .field("relation_shapes", &self.relation_shapes)
            .field("current", &self.current)
            .field("delta", &self.delta)
            .field("limit", &self.limit)
            .field("comparison", &self.comparison)
            .field("plan", &self.plan)
            .field("compiled", &self.compiled.get().is_some())
            .finish()
    }
}

#[cfg(feature = "cranelift")]
struct NativeProgramCacheState {
    integer_loop_sites: Box<[IntegerLoopSite]>,
    float_loop_sites: Box<[FloatLoopSite]>,
    natural_integer_loop_sites: Box<[NaturalIntegerLoopSite]>,
    integer_loop: OnceLock<Result<Arc<CompiledIntegerLoop>, Box<str>>>,
    compile_attempts: AtomicUsize,
}

#[cfg(feature = "cranelift")]
#[derive(Clone)]
struct NativeProgramCache(Arc<NativeProgramCacheState>);

#[cfg(feature = "cranelift")]
impl NativeProgramCache {
    fn recognize(
        opcodes: &[Opcode],
        constants: &[Value],
        kind_facts: &[Option<ValueKind>],
        type_facts: &[Option<TypeContract>],
    ) -> Option<Self> {
        let integer_loop_sites = (2..opcodes.len())
            .filter_map(|branch_ip| recognize_integer_loop(opcodes, branch_ip))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let float_loop_sites = (2..opcodes.len())
            .filter_map(|branch_ip| recognize_float_loop(opcodes, branch_ip))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let natural_integer_loop_sites = (0..opcodes.len())
            .filter_map(|branch_ip| {
                recognize_natural_integer_loop(
                    opcodes, constants, kind_facts, type_facts, branch_ip,
                )
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if integer_loop_sites.is_empty()
            && float_loop_sites.is_empty()
            && natural_integer_loop_sites.is_empty()
        {
            return None;
        }
        Some(Self(Arc::new(NativeProgramCacheState {
            integer_loop_sites,
            float_loop_sites,
            natural_integer_loop_sites,
            integer_loop: OnceLock::new(),
            compile_attempts: AtomicUsize::new(0),
        })))
    }

    fn integer_loop_site(&self, branch_ip: usize) -> Option<&IntegerLoopSite> {
        self.0
            .integer_loop_sites
            .binary_search_by_key(&branch_ip, |site| site.branch_ip)
            .ok()
            .map(|index| &self.0.integer_loop_sites[index])
    }

    fn compiled_integer_loop(&self, compile: bool) -> Option<Arc<CompiledIntegerLoop>> {
        if let Some(compiled) = self.0.integer_loop.get() {
            return compiled.as_ref().ok().cloned();
        }
        if !compile {
            return None;
        }
        self.0
            .integer_loop
            .get_or_init(|| {
                self.0.compile_attempts.fetch_add(1, Ordering::Relaxed);
                CompiledIntegerLoop::compile()
                    .map(Arc::new)
                    .map_err(|error| error.to_string().into_boxed_str())
            })
            .as_ref()
            .ok()
            .cloned()
    }

    fn float_loop_site(&self, branch_ip: usize) -> Option<&FloatLoopSite> {
        self.0
            .float_loop_sites
            .binary_search_by_key(&branch_ip, |site| site.branch_ip)
            .ok()
            .map(|index| &self.0.float_loop_sites[index])
    }

    fn compiled_float_loop(
        &self,
        branch_ip: usize,
        compile: bool,
    ) -> Option<Arc<CompiledFloatLoop>> {
        let site = self.float_loop_site(branch_ip)?;
        if let Some(compiled) = site.compiled.get() {
            return compiled.as_ref().ok().cloned();
        }
        if !compile {
            return None;
        }
        site.compiled
            .get_or_init(|| {
                self.0.compile_attempts.fetch_add(1, Ordering::Relaxed);
                CompiledFloatLoop::compile(site.plan)
                    .map(Arc::new)
                    .map_err(|error| error.to_string().into_boxed_str())
            })
            .as_ref()
            .ok()
            .cloned()
    }

    fn natural_integer_loop_site(&self, branch_ip: usize) -> Option<&NaturalIntegerLoopSite> {
        self.0
            .natural_integer_loop_sites
            .binary_search_by_key(&branch_ip, |site| site.branch_ip)
            .ok()
            .map(|index| &self.0.natural_integer_loop_sites[index])
    }

    fn compiled_natural_integer_loop(
        &self,
        branch_ip: usize,
        compile: bool,
    ) -> Option<Arc<CompiledNaturalLoop>> {
        let site = self.natural_integer_loop_site(branch_ip)?;
        if let Some(compiled) = site.compiled.get() {
            return compiled.as_ref().ok().cloned();
        }
        if !compile {
            return None;
        }
        site.compiled
            .get_or_init(|| {
                self.0.compile_attempts.fetch_add(1, Ordering::Relaxed);
                CompiledNaturalLoop::compile(&site.plan)
                    .map(Arc::new)
                    .map_err(|error| error.to_string().into_boxed_str())
            })
            .as_ref()
            .ok()
            .cloned()
    }

    #[cfg(test)]
    fn compile_attempts(&self) -> usize {
        self.0.compile_attempts.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "cranelift")]
impl Debug for NativeProgramCache {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeProgramCache")
            .field("integer_loop_sites", &self.0.integer_loop_sites)
            .field("float_loop_sites", &self.0.float_loop_sites)
            .field(
                "natural_integer_loop_sites",
                &self.0.natural_integer_loop_sites,
            )
            .field("compiled", &self.0.integer_loop.get().is_some())
            .finish()
    }
}

#[cfg(feature = "cranelift")]
impl PartialEq for NativeProgramCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[cfg(feature = "cranelift")]
impl Eq for NativeProgramCache {}

#[cfg(feature = "cranelift")]
fn recognize_integer_loop(opcodes: &[Opcode], branch_ip: usize) -> Option<IntegerLoopSite> {
    let entry_ip = branch_ip.checked_sub(2)?;
    let Opcode::Binary {
        dst: current,
        op: RuntimeBinaryOp::Add,
        left,
        right,
    } = opcodes.get(entry_ip)?
    else {
        return None;
    };
    let step = if left == current && right != current {
        *right
    } else if right == current && left != current {
        *left
    } else {
        return None;
    };
    let Opcode::Binary {
        dst: condition,
        op: RuntimeBinaryOp::Lt,
        left: comparison_left,
        right: limit,
    } = opcodes.get(entry_ip + 1)?
    else {
        return None;
    };
    let Opcode::Branch {
        condition: branch_condition,
        if_true,
        if_false,
    } = opcodes.get(branch_ip)?
    else {
        return None;
    };
    if comparison_left != current
        || branch_condition != condition
        || if_true.0 as usize != entry_ip
        || if_false.0 as usize <= branch_ip
        || current == limit
        || condition == current
        || condition == &step
        || condition == limit
    {
        return None;
    }
    Some(IntegerLoopSite {
        entry_ip,
        branch_ip,
        exit_ip: if_false.0 as usize,
        current: *current,
        step,
        limit: *limit,
        condition: *condition,
    })
}

#[cfg(feature = "cranelift")]
fn recognize_float_loop(opcodes: &[Opcode], branch_ip: usize) -> Option<FloatLoopSite> {
    let entry_ip = branch_ip.checked_sub(2)?;
    let Opcode::Binary {
        dst: current,
        op,
        left,
        right,
    } = opcodes.get(entry_ip)?
    else {
        return None;
    };
    let arithmetic = match op {
        RuntimeBinaryOp::Add => FloatArithmetic::Add,
        RuntimeBinaryOp::Sub => FloatArithmetic::Subtract,
        RuntimeBinaryOp::Mul => FloatArithmetic::Multiply,
        RuntimeBinaryOp::Div => FloatArithmetic::Divide,
        _ => return None,
    };
    let step = if left == current && right != current {
        *right
    } else if matches!(arithmetic, FloatArithmetic::Add | FloatArithmetic::Multiply)
        && right == current
        && left != current
    {
        *left
    } else {
        return None;
    };
    let Opcode::Binary {
        dst: condition,
        op,
        left: comparison_left,
        right: limit,
    } = opcodes.get(entry_ip + 1)?
    else {
        return None;
    };
    let comparison = match op {
        RuntimeBinaryOp::Eq => FloatComparison::Equal,
        RuntimeBinaryOp::Ne => FloatComparison::NotEqual,
        RuntimeBinaryOp::Lt => FloatComparison::LessThan,
        RuntimeBinaryOp::Le => FloatComparison::LessThanOrEqual,
        RuntimeBinaryOp::Gt => FloatComparison::GreaterThan,
        RuntimeBinaryOp::Ge => FloatComparison::GreaterThanOrEqual,
        _ => return None,
    };
    let Opcode::Branch {
        condition: branch_condition,
        if_true,
        if_false,
    } = opcodes.get(branch_ip)?
    else {
        return None;
    };
    if comparison_left != current
        || branch_condition != condition
        || if_true.0 as usize != entry_ip
        || if_false.0 as usize <= branch_ip
        || current == limit
        || condition == current
        || condition == &step
        || condition == limit
    {
        return None;
    }
    Some(FloatLoopSite {
        entry_ip,
        branch_ip,
        exit_ip: if_false.0 as usize,
        current: *current,
        step,
        limit: *limit,
        condition: *condition,
        plan: FloatLoopPlan::new(arithmetic, comparison),
        compiled: OnceLock::new(),
    })
}

#[cfg(feature = "cranelift")]
fn recognize_natural_integer_loop(
    opcodes: &[Opcode],
    constants: &[Value],
    kind_facts: &[Option<ValueKind>],
    type_facts: &[Option<TypeContract>],
    branch_ip: usize,
) -> Option<NaturalIntegerLoopSite> {
    let Opcode::Branch {
        condition,
        if_true,
        if_false,
    } = opcodes.get(branch_ip)?
    else {
        return None;
    };
    let body_ip = if_true.0 as usize;
    let exit_ip = if_false.0 as usize;
    let backedge_ip = exit_ip.checked_sub(1)?;
    let Opcode::Jump { target } = opcodes.get(backedge_ip)? else {
        return None;
    };
    let header_ip = target.0 as usize;
    if header_ip >= branch_ip
        || body_ip != branch_ip + 1
        || backedge_ip < body_ip
        || exit_ip > opcodes.len()
    {
        return None;
    }

    let header = opcodes.get(header_ip..branch_ip)?;
    let body = opcodes.get(body_ip..backedge_ip)?;
    let region = opcodes.get(header_ip..exit_ip)?;
    let collection_view_registers = region
        .iter()
        .filter_map(|opcode| match opcode {
            Opcode::CollectionKeyAt { collection, .. }
            | Opcode::CollectionValueAt { collection, .. } => Some(*collection),
            Opcode::Index { collection, .. } => Some(*collection),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if collection_view_registers.len() > MAX_NATURAL_LOOP_COLLECTION_VIEWS
        || collection_view_registers
            .iter()
            .any(|register| region.iter().any(|opcode| opcode_writes(opcode, *register)))
    {
        return None;
    }
    let collection_view_registers = collection_view_registers
        .into_iter()
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut registers = BTreeSet::new();
    registers.insert(*condition);
    for opcode in region {
        collect_natural_loop_registers(opcode, constants, &mut registers)?;
    }
    if registers.len() > MAX_NATURAL_LOOP_SLOTS {
        return None;
    }
    let registers = registers.into_iter().collect::<Vec<_>>().into_boxed_slice();
    let slot = |register: Register| {
        registers
            .binary_search(&register)
            .ok()
            .and_then(|slot| u16::try_from(slot).ok())
    };
    let collection_view = |register: Register| {
        collection_view_registers
            .binary_search(&register)
            .ok()
            .and_then(|view| u16::try_from(view).ok())
    };
    let instructions = region
        .iter()
        .map(|opcode| {
            natural_loop_instruction(
                opcode,
                constants,
                &slot,
                &collection_view,
                header_ip,
                exit_ip,
            )
        })
        .collect::<Option<Vec<_>>>()?;
    let (current, delta, limit, comparison) = recognize_natural_loop_induction(
        opcodes.get(..header_ip)?,
        header,
        body,
        constants,
        *condition,
    )?;
    let entry = u16::try_from(body_ip.checked_sub(header_ip)?).ok()?;
    let mut plan = NaturalLoopPlan::new(
        u16::try_from(registers.len()).ok()?,
        u16::try_from(collection_view_registers.len()).ok()?,
        entry,
        instructions,
    )
    .ok()?;
    let collection_candidates = natural_loop_collection_candidates(
        opcodes.get(..header_ip)?,
        constants,
        &collection_view_registers,
    );
    if let Some(slots) = natural_loop_unboxed_slots(
        &plan,
        &registers,
        opcodes.get(..header_ip)?,
        kind_facts.get(..header_ip)?,
        &collection_candidates,
        current,
        limit,
    ) && let Ok(specialized) = plan.clone().with_unboxed_slots(
        slots.integers,
        slots.entry_integers,
        slots.floats,
        slots.entry_floats,
    ) {
        plan = specialized;
    }
    let relation_shapes = relation_collection_shapes(
        opcodes.get(..header_ip)?,
        type_facts.get(..header_ip)?,
        &collection_view_registers,
    );
    Some(NaturalIntegerLoopSite {
        header_ip,
        branch_ip,
        body_ip,
        exit_ip,
        region_instruction_count: region.len(),
        registers,
        collection_view_registers,
        relation_shapes,
        current,
        delta,
        limit,
        comparison,
        plan,
        compiled: OnceLock::new(),
    })
}

#[cfg(feature = "cranelift")]
fn relation_collection_shapes(
    preheader: &[Opcode],
    type_facts: &[Option<TypeContract>],
    collection_registers: &[Register],
) -> Box<[(Register, RelationTypeContract)]> {
    let mut facts = BTreeMap::<Register, RelationTypeContract>::new();
    for (opcode, fact) in preheader.iter().zip(type_facts) {
        if let Some(register) = opcode.kind_fact_register() {
            match fact {
                Some(TypeContract::Relation(relation)) => {
                    facts.insert(register, relation.clone());
                }
                _ => {
                    facts.remove(&register);
                }
            }
        }
        if matches!(opcode, Opcode::Branch { .. } | Opcode::Jump { .. }) {
            facts.clear();
        }
    }
    collection_registers
        .iter()
        .filter_map(|register| facts.get(register).cloned().map(|ty| (*register, ty)))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

#[cfg(feature = "cranelift")]
fn natural_loop_unboxed_slots(
    plan: &NaturalLoopPlan,
    registers: &[Register],
    preheader: &[Opcode],
    preheader_facts: &[Option<ValueKind>],
    collection_candidates: &NaturalLoopCollectionCandidates,
    current: Register,
    limit: Register,
) -> Option<NaturalLoopUnboxedSlots> {
    // These are specialization candidates, not trusted type metadata. The
    // generated entry guards validate every live candidate before unboxing.
    let slot = |register: Register| {
        registers
            .binary_search(&register)
            .ok()
            .and_then(|slot| u16::try_from(slot).ok())
    };
    let mut entry_registers = linear_numeric_registers(preheader, preheader_facts);
    entry_registers.insert(current, ValueKind::Int);
    entry_registers.insert(limit, ValueKind::Int);
    let mut kinds = [None; MAX_NATURAL_LOOP_SLOTS];
    let mut entry_integer_slots = 0_u32;
    let mut entry_float_slots = 0_u32;
    for (register, kind) in entry_registers {
        let Some(slot) = slot(register) else {
            continue;
        };
        kinds[usize::from(slot)] = Some(kind);
        match kind {
            ValueKind::Int => entry_integer_slots |= 1_u32 << slot,
            ValueKind::Float => entry_float_slots |= 1_u32 << slot,
            _ => unreachable!("linear numeric facts contain only int and float"),
        }
    }
    let assign_kind =
        |kinds: &mut [Option<ValueKind>], slot: u16, kind: ValueKind| -> Option<bool> {
            let current = kinds.get_mut(usize::from(slot))?;
            match *current {
                Some(current) if current != kind => None,
                Some(_) => Some(false),
                None => {
                    *current = Some(kind);
                    Some(true)
                }
            }
        };
    loop {
        let mut changed = false;
        for instruction in plan.instructions() {
            let kind = |slot: u16| kinds.get(usize::from(slot)).copied().flatten();
            let candidate = match *instruction {
                NaturalLoopInstruction::Load { dst, value } => match value_tag(value) {
                    VALUE_INT_TAG => Some((dst, ValueKind::Int)),
                    mica_var::abi::VALUE_FLOAT_TAG => Some((dst, ValueKind::Float)),
                    _ => None,
                },
                NaturalLoopInstruction::Move { dst, src }
                | NaturalLoopInstruction::Negate { dst, src } => kind(src).map(|kind| (dst, kind)),
                NaturalLoopInstruction::Add { dst, left, right }
                | NaturalLoopInstruction::Subtract { dst, left, right }
                | NaturalLoopInstruction::Multiply { dst, left, right } => {
                    infer_numeric_result_kind(RuntimeBinaryOp::Add, kind(left), kind(right))
                        .map(|kind| (dst, kind))
                }
                NaturalLoopInstruction::Divide { dst, left, right } => {
                    let result =
                        infer_numeric_result_kind(RuntimeBinaryOp::Div, kind(left), kind(right));
                    (result == Some(ValueKind::Float)).then_some((dst, ValueKind::Float))
                }
                NaturalLoopInstruction::CheckKind { value, expected } => Some((value, expected)),
                NaturalLoopInstruction::CollectionValueAt { dst, view, .. } => {
                    collection_candidates
                        .values
                        .get(usize::from(view))
                        .copied()
                        .flatten()
                        .map(|kind| (dst, kind))
                }
                NaturalLoopInstruction::CollectionKeyAt { dst, view, .. } => collection_candidates
                    .keys
                    .get(usize::from(view))
                    .copied()
                    .flatten()
                    .map(|kind| (dst, kind)),
                _ => None,
            };
            if let Some((dst, kind)) = candidate {
                changed |= assign_kind(&mut kinds, dst, kind)?;
            }
        }
        if !changed {
            break;
        }
    }

    let mut integer_slots = 0_u32;
    let mut float_slots = 0_u32;
    for (slot, kind) in kinds.into_iter().enumerate() {
        let slot = u16::try_from(slot).ok()?;
        match kind {
            Some(ValueKind::Int) => integer_slots |= 1_u32 << slot,
            Some(ValueKind::Float) => float_slots |= 1_u32 << slot,
            Some(_) | None => {}
        }
    }
    Some(NaturalLoopUnboxedSlots {
        integers: integer_slots,
        entry_integers: entry_integer_slots,
        floats: float_slots,
        entry_floats: entry_float_slots,
    })
}

#[cfg(feature = "cranelift")]
fn linear_numeric_registers(
    opcodes: &[Opcode],
    kind_facts: &[Option<ValueKind>],
) -> BTreeMap<Register, ValueKind> {
    let mut numeric = BTreeMap::new();
    for (opcode, fact) in opcodes.iter().zip(kind_facts) {
        if let Some(register) = opcode.kind_fact_register() {
            match fact {
                Some(kind @ (ValueKind::Int | ValueKind::Float)) => {
                    numeric.insert(register, *kind);
                }
                _ => {
                    numeric.remove(&register);
                }
            }
        }
        if matches!(opcode, Opcode::Branch { .. } | Opcode::Jump { .. }) {
            numeric.clear();
        }
    }
    numeric
}

#[cfg(feature = "cranelift")]
fn natural_loop_collection_candidates(
    preheader: &[Opcode],
    constants: &[Value],
    collection_registers: &[Register],
) -> NaturalLoopCollectionCandidates {
    // Collection element kinds are guarded at every native load. The first
    // literal element is only a specialization candidate, so heterogeneous
    // collections side exit atomically rather than becoming trusted metadata.
    let mut candidates = BTreeMap::<Register, (Option<ValueKind>, Option<ValueKind>)>::new();
    for opcode in preheader {
        match opcode {
            Opcode::Load { dst, value } => {
                let candidate = constants
                    .get(value.0 as usize)
                    .map(numeric_collection_candidates)
                    .unwrap_or((None, None));
                candidates.insert(*dst, candidate);
            }
            Opcode::Move { dst, src } => {
                if let Some(candidate) = candidates.get(src).copied() {
                    candidates.insert(*dst, candidate);
                } else {
                    candidates.remove(dst);
                }
            }
            _ => {
                if let Some(dst) = opcode.destination() {
                    candidates.remove(&dst);
                }
            }
        }
        if matches!(opcode, Opcode::Branch { .. } | Opcode::Jump { .. }) {
            candidates.clear();
        }
    }
    let mut key_kinds = Vec::with_capacity(collection_registers.len());
    let mut value_kinds = Vec::with_capacity(collection_registers.len());
    for register in collection_registers {
        let (key, value) = candidates.get(register).copied().unwrap_or((None, None));
        key_kinds.push(key);
        value_kinds.push(value);
    }
    NaturalLoopCollectionCandidates {
        keys: key_kinds.into_boxed_slice(),
        values: value_kinds.into_boxed_slice(),
    }
}

#[cfg(feature = "cranelift")]
fn numeric_collection_candidates(value: &Value) -> (Option<ValueKind>, Option<ValueKind>) {
    let numeric_kind = |value: &Value| match value.kind() {
        kind @ (ValueKind::Int | ValueKind::Float) => Some(kind),
        _ => None,
    };
    if value.with_range(|_, _| ()).is_some() {
        return (Some(ValueKind::Int), Some(ValueKind::Int));
    }
    if let Some(value_kind) = value
        .with_list(|values| values.first().and_then(&numeric_kind))
        .flatten()
    {
        return (Some(ValueKind::Int), Some(value_kind));
    }
    if let Some((key_kind, value_kind)) = value
        .with_map(|entries| {
            entries
                .first()
                .map(|(key, value)| (numeric_kind(key), numeric_kind(value)))
        })
        .flatten()
    {
        return (key_kind, value_kind);
    }
    (None, None)
}

#[cfg(feature = "cranelift")]
fn collect_natural_loop_registers(
    opcode: &Opcode,
    constants: &[Value],
    registers: &mut BTreeSet<Register>,
) -> Option<()> {
    match opcode {
        Opcode::Load { dst, value } => {
            let value = constants.get(value.0 as usize)?;
            if !value_is_immediate(borrowed_value_bits(value)) {
                return None;
            }
            registers.insert(*dst);
        }
        Opcode::Move { dst, src } => {
            registers.insert(*dst);
            registers.insert(*src);
        }
        Opcode::CheckKind {
            value,
            expected: ValueKind::Int | ValueKind::Float,
            ..
        } => {
            registers.insert(*value);
        }
        Opcode::Unary { dst, src, .. } => {
            registers.insert(*dst);
            registers.insert(*src);
        }
        Opcode::Binary {
            dst,
            op,
            left,
            right,
        } if natural_binary_operation(*op).is_some() => {
            registers.insert(*dst);
            registers.insert(*left);
            registers.insert(*right);
        }
        Opcode::Branch { condition, .. } => {
            registers.insert(*condition);
        }
        Opcode::CollectionKeyAt { dst, index, .. }
        | Opcode::CollectionValueAt { dst, index, .. } => {
            registers.insert(*dst);
            registers.insert(*index);
        }
        Opcode::Index {
            dst,
            index: OperandRef::Register(index),
            ..
        } => {
            registers.insert(*dst);
            registers.insert(*index);
        }
        Opcode::Index {
            dst,
            index: OperandRef::Constant(index),
            ..
        } => {
            if !value_is_immediate(borrowed_value_bits(constants.get(index.0 as usize)?)) {
                return None;
            }
            registers.insert(*dst);
        }
        Opcode::Jump { .. } => {}
        _ => return None,
    }
    Some(())
}

#[cfg(feature = "cranelift")]
fn natural_loop_instruction(
    opcode: &Opcode,
    constants: &[Value],
    slot: &impl Fn(Register) -> Option<u16>,
    collection_view: &impl Fn(Register) -> Option<u16>,
    region_start: usize,
    region_end: usize,
) -> Option<NaturalLoopInstruction> {
    let target = |target: Target| {
        let target = usize::from(target.0);
        if !(region_start..=region_end).contains(&target) {
            return None;
        }
        u16::try_from(target - region_start).ok()
    };
    match opcode {
        Opcode::Load { dst, value } => Some(NaturalLoopInstruction::Load {
            dst: slot(*dst)?,
            value: borrowed_value_bits(constants.get(value.0 as usize)?),
        }),
        Opcode::Move { dst, src } => Some(NaturalLoopInstruction::Move {
            dst: slot(*dst)?,
            src: slot(*src)?,
        }),
        Opcode::CheckKind {
            value, expected, ..
        } if matches!(expected, ValueKind::Int | ValueKind::Float) => {
            Some(NaturalLoopInstruction::CheckKind {
                value: slot(*value)?,
                expected: *expected,
            })
        }
        Opcode::Unary {
            dst,
            op: RuntimeUnaryOp::Neg,
            src,
        } => Some(NaturalLoopInstruction::Negate {
            dst: slot(*dst)?,
            src: slot(*src)?,
        }),
        Opcode::Unary {
            dst,
            op: RuntimeUnaryOp::Not,
            src,
        } => Some(NaturalLoopInstruction::Not {
            dst: slot(*dst)?,
            src: slot(*src)?,
        }),
        Opcode::Binary {
            dst,
            op: RuntimeBinaryOp::Add,
            left,
            right,
        } => Some(NaturalLoopInstruction::Add {
            dst: slot(*dst)?,
            left: slot(*left)?,
            right: slot(*right)?,
        }),
        Opcode::CollectionValueAt {
            dst,
            collection,
            index,
        } => Some(NaturalLoopInstruction::CollectionValueAt {
            dst: slot(*dst)?,
            view: collection_view(*collection)?,
            index: slot(*index)?,
        }),
        Opcode::CollectionKeyAt {
            dst,
            collection,
            index,
        } => Some(NaturalLoopInstruction::CollectionKeyAt {
            dst: slot(*dst)?,
            view: collection_view(*collection)?,
            index: slot(*index)?,
        }),
        Opcode::Index {
            dst,
            collection,
            index: OperandRef::Register(index),
        } => Some(NaturalLoopInstruction::IndexValue {
            dst: slot(*dst)?,
            view: collection_view(*collection)?,
            index: slot(*index)?,
        }),
        Opcode::Index {
            dst,
            collection,
            index: OperandRef::Constant(index),
        } => Some(NaturalLoopInstruction::IndexValueImmediate {
            dst: slot(*dst)?,
            view: collection_view(*collection)?,
            index: borrowed_value_bits(constants.get(index.0 as usize)?),
        }),
        Opcode::Binary {
            dst,
            op: RuntimeBinaryOp::Sub,
            left,
            right,
        } => Some(NaturalLoopInstruction::Subtract {
            dst: slot(*dst)?,
            left: slot(*left)?,
            right: slot(*right)?,
        }),
        Opcode::Binary {
            dst,
            op: RuntimeBinaryOp::Mul,
            left,
            right,
        } => Some(NaturalLoopInstruction::Multiply {
            dst: slot(*dst)?,
            left: slot(*left)?,
            right: slot(*right)?,
        }),
        Opcode::Binary {
            dst,
            op: RuntimeBinaryOp::Div,
            left,
            right,
        } => Some(NaturalLoopInstruction::Divide {
            dst: slot(*dst)?,
            left: slot(*left)?,
            right: slot(*right)?,
        }),
        Opcode::Binary {
            dst,
            op: RuntimeBinaryOp::Rem,
            left,
            right,
        } => Some(NaturalLoopInstruction::Remainder {
            dst: slot(*dst)?,
            left: slot(*left)?,
            right: slot(*right)?,
        }),
        Opcode::Binary {
            dst,
            op,
            left,
            right,
        } => Some(NaturalLoopInstruction::Compare {
            dst: slot(*dst)?,
            comparison: scalar_comparison(*op)?,
            left: slot(*left)?,
            right: slot(*right)?,
        }),
        Opcode::Branch {
            condition,
            if_true,
            if_false,
        } => Some(NaturalLoopInstruction::Branch {
            condition: slot(*condition)?,
            if_true: target(*if_true)?,
            if_false: target(*if_false)?,
        }),
        Opcode::Jump {
            target: jump_target,
        } => Some(NaturalLoopInstruction::Jump {
            target: target(*jump_target)?,
        }),
        _ => None,
    }
}

#[cfg(feature = "cranelift")]
fn recognize_natural_loop_induction(
    preheader: &[Opcode],
    header: &[Opcode],
    body: &[Opcode],
    constants: &[Value],
    condition: Register,
) -> Option<(Register, i64, Register, IntegerComparison)> {
    let (current, limit, comparison) = header.iter().rev().find_map(|opcode| match opcode {
        Opcode::Binary {
            dst,
            op,
            left,
            right,
        } if *dst == condition => Some((*left, *right, integer_comparison(*op)?)),
        _ => None,
    })?;
    if current == limit || header.iter().any(|opcode| opcode_writes(opcode, current)) {
        return None;
    }

    let current_writes = body
        .iter()
        .enumerate()
        .filter(|(_, opcode)| opcode_writes(opcode, current))
        .collect::<Vec<_>>();
    if current_writes.len() != 1 {
        return None;
    }
    let (write_index, write) = current_writes[0];
    let (arithmetic_index, step_register, direction) = match write {
        Opcode::Binary {
            op, left, right, ..
        } => {
            let (step, direction) = induction_step(*op, *left, *right, current)?;
            (write_index, step, direction)
        }
        Opcode::Move { src, .. } => {
            let (arithmetic_index, op, left, right) = body[..write_index]
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, opcode)| match opcode {
                    Opcode::Binary {
                        dst,
                        op,
                        left,
                        right,
                    } if dst == src => Some((index, *op, *left, *right)),
                    _ => None,
                })?;
            let (step, direction) = induction_step(op, left, right, current)?;
            (arithmetic_index, step, direction)
        }
        _ => return None,
    };
    let step = body[..arithmetic_index]
        .iter()
        .rev()
        .find(|opcode| opcode_writes(opcode, step_register))
        .map(|opcode| loaded_integer(opcode, step_register, constants))
        .unwrap_or_else(|| {
            if header
                .iter()
                .any(|opcode| opcode_writes(opcode, step_register))
            {
                return None;
            }
            let tail_start = preheader
                .iter()
                .rposition(|opcode| matches!(opcode, Opcode::Branch { .. } | Opcode::Jump { .. }))
                .map_or(0, |index| index + 1);
            preheader[tail_start..]
                .iter()
                .rev()
                .find(|opcode| opcode_writes(opcode, step_register))
                .and_then(|opcode| loaded_integer(opcode, step_register, constants))
        })?;
    let delta = if direction > 0 {
        step
    } else {
        step.checked_neg()?
    };
    Some((current, delta, limit, comparison))
}

#[cfg(feature = "cranelift")]
fn loaded_integer(opcode: &Opcode, register: Register, constants: &[Value]) -> Option<i64> {
    let Opcode::Load { dst, value } = opcode else {
        return None;
    };
    (*dst == register).then(|| constants.get(value.0 as usize)?.as_int())?
}

#[cfg(feature = "cranelift")]
fn induction_step(
    operation: RuntimeBinaryOp,
    left: Register,
    right: Register,
    current: Register,
) -> Option<(Register, i8)> {
    match operation {
        RuntimeBinaryOp::Add if left == current && right != current => Some((right, 1)),
        RuntimeBinaryOp::Add if right == current && left != current => Some((left, 1)),
        RuntimeBinaryOp::Sub if left == current && right != current => Some((right, -1)),
        _ => None,
    }
}

#[cfg(feature = "cranelift")]
fn natural_binary_operation(operation: RuntimeBinaryOp) -> Option<()> {
    match operation {
        RuntimeBinaryOp::Add
        | RuntimeBinaryOp::Sub
        | RuntimeBinaryOp::Mul
        | RuntimeBinaryOp::Div
        | RuntimeBinaryOp::Rem => Some(()),
        operation if integer_comparison(operation).is_some() => Some(()),
        _ => None,
    }
}

#[cfg(feature = "cranelift")]
fn integer_comparison(operation: RuntimeBinaryOp) -> Option<IntegerComparison> {
    match operation {
        RuntimeBinaryOp::Eq => Some(IntegerComparison::Equal),
        RuntimeBinaryOp::Ne => Some(IntegerComparison::NotEqual),
        RuntimeBinaryOp::Lt => Some(IntegerComparison::LessThan),
        RuntimeBinaryOp::Le => Some(IntegerComparison::LessThanOrEqual),
        RuntimeBinaryOp::Gt => Some(IntegerComparison::GreaterThan),
        RuntimeBinaryOp::Ge => Some(IntegerComparison::GreaterThanOrEqual),
        _ => None,
    }
}

#[cfg(feature = "cranelift")]
fn scalar_comparison(operation: RuntimeBinaryOp) -> Option<ScalarComparison> {
    match operation {
        RuntimeBinaryOp::Eq => Some(ScalarComparison::Equal),
        RuntimeBinaryOp::Ne => Some(ScalarComparison::NotEqual),
        RuntimeBinaryOp::Lt => Some(ScalarComparison::LessThan),
        RuntimeBinaryOp::Le => Some(ScalarComparison::LessThanOrEqual),
        RuntimeBinaryOp::Gt => Some(ScalarComparison::GreaterThan),
        RuntimeBinaryOp::Ge => Some(ScalarComparison::GreaterThanOrEqual),
        _ => None,
    }
}

#[cfg(feature = "cranelift")]
fn opcode_writes(opcode: &Opcode, register: Register) -> bool {
    opcode.destination() == Some(register)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeUnaryOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeBinaryOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    register_count: usize,
    opcodes: Arc<[Opcode]>,
    kind_facts: Arc<[Option<ValueKind>]>,
    type_facts: Arc<[Option<TypeContract>]>,
    constants: Arc<[Value]>,
    list_items: Arc<[CompactListItem]>,
    map_items: Arc<[CompactMapItem]>,
    relation_args: Arc<[CompactRelationArg]>,
    map_entries: Arc<[(OperandRef, OperandRef)]>,
    operands: Arc<[OperandRef]>,
    bindings: Arc<[Option<OperandRef>]>,
    query_bindings: Arc<[QueryBinding]>,
    catches: Arc<[CompactCatchHandler]>,
    roles: Arc<[(ConstId, OperandRef)]>,
    programs: Arc<[Arc<Program>]>,
    relations: Arc<[RelationId]>,
    dispatch_specs: Arc<[DispatchSpec]>,
    suspend_kinds: Arc<[SuspendKind]>,
    #[cfg(feature = "cranelift")]
    native_cache: Option<NativeProgramCache>,
}

impl Program {
    pub fn new(
        register_count: usize,
        instructions: impl IntoIterator<Item = Instruction>,
    ) -> Result<Self, RuntimeError> {
        let mut builder = ProgramBuilder::new();
        for instruction in instructions {
            builder.emit(instruction)?;
        }
        builder.finish(register_count)
    }

    pub fn register_count(&self) -> usize {
        self.register_count
    }

    #[inline]
    pub fn instructions(&self) -> Vec<Instruction> {
        self.opcodes
            .iter()
            .map(|opcode| self.decode_instruction(opcode))
            .collect()
    }

    #[inline]
    pub(crate) fn opcodes(&self) -> &[Opcode] {
        &self.opcodes
    }

    /// Returns the exact kind established by an instruction on normal
    /// fallthrough, together with the affected register.
    pub fn kind_fact_after(&self, instruction: usize) -> Option<(Register, ValueKind)> {
        let kind = self.kind_facts.get(instruction).copied().flatten()?;
        let register = self.opcodes.get(instruction)?.kind_fact_register()?;
        Some((register, kind))
    }

    /// Returns the structural type established by an instruction on normal
    /// fallthrough, together with the affected register.
    pub fn type_fact_after(&self, instruction: usize) -> Option<(Register, &TypeContract)> {
        let ty = self.type_facts.get(instruction)?.as_ref()?;
        let register = self.opcodes.get(instruction)?.kind_fact_register()?;
        Some((register, ty))
    }

    #[inline]
    pub(crate) fn constant(&self, id: ConstId) -> &Value {
        &self.constants[id.0 as usize]
    }

    #[inline]
    pub(crate) fn list_items(&self, range: TableRange) -> &[CompactListItem] {
        table_range(&self.list_items, range)
    }

    #[inline]
    pub(crate) fn map_items(&self, range: TableRange) -> &[CompactMapItem] {
        table_range(&self.map_items, range)
    }

    #[inline]
    pub(crate) fn relation_args(&self, range: TableRange) -> &[CompactRelationArg] {
        table_range(&self.relation_args, range)
    }

    #[inline]
    pub(crate) fn map_entries(&self, range: TableRange) -> &[(OperandRef, OperandRef)] {
        table_range(&self.map_entries, range)
    }

    #[inline]
    pub(crate) fn operands(&self, range: TableRange) -> &[OperandRef] {
        table_range(&self.operands, range)
    }

    #[inline]
    pub(crate) fn bindings(&self, range: TableRange) -> &[Option<OperandRef>] {
        table_range(&self.bindings, range)
    }

    #[inline]
    pub(crate) fn query_bindings(&self, range: TableRange) -> &[QueryBinding] {
        table_range(&self.query_bindings, range)
    }

    #[inline]
    pub(crate) fn catches(&self, range: TableRange) -> &[CompactCatchHandler] {
        table_range(&self.catches, range)
    }

    #[inline]
    pub(crate) fn roles(&self, range: TableRange) -> &[(ConstId, OperandRef)] {
        table_range(&self.roles, range)
    }

    #[inline]
    pub(crate) fn program(&self, id: ProgramId) -> &Arc<Program> {
        &self.programs[id.0 as usize]
    }

    #[inline]
    pub(crate) fn relation(&self, id: RelationRef) -> RelationId {
        self.relations[id.0 as usize]
    }

    #[inline]
    pub(crate) fn dispatch_spec(&self, id: DispatchSpecId) -> DispatchSpec {
        self.dispatch_specs[id.0 as usize]
    }

    #[inline]
    pub(crate) fn suspend_kind(&self, id: SuspendKindId) -> &SuspendKind {
        &self.suspend_kinds[id.0 as usize]
    }

    #[cfg(feature = "cranelift")]
    pub(crate) fn integer_loop_site(&self, branch_ip: usize) -> Option<&IntegerLoopSite> {
        self.native_cache.as_ref()?.integer_loop_site(branch_ip)
    }

    #[cfg(feature = "cranelift")]
    pub(crate) fn compiled_integer_loop(&self, compile: bool) -> Option<Arc<CompiledIntegerLoop>> {
        self.native_cache.as_ref()?.compiled_integer_loop(compile)
    }

    #[cfg(feature = "cranelift")]
    pub(crate) fn float_loop_site(&self, branch_ip: usize) -> Option<&FloatLoopSite> {
        self.native_cache.as_ref()?.float_loop_site(branch_ip)
    }

    #[cfg(feature = "cranelift")]
    pub(crate) fn compiled_float_loop(
        &self,
        branch_ip: usize,
        compile: bool,
    ) -> Option<Arc<CompiledFloatLoop>> {
        self.native_cache
            .as_ref()?
            .compiled_float_loop(branch_ip, compile)
    }

    #[cfg(feature = "cranelift")]
    pub(crate) fn natural_integer_loop_site(
        &self,
        branch_ip: usize,
    ) -> Option<&NaturalIntegerLoopSite> {
        self.native_cache
            .as_ref()?
            .natural_integer_loop_site(branch_ip)
    }

    #[cfg(feature = "cranelift")]
    pub(crate) fn compiled_natural_integer_loop(
        &self,
        branch_ip: usize,
        compile: bool,
    ) -> Option<Arc<CompiledNaturalLoop>> {
        self.native_cache
            .as_ref()?
            .compiled_natural_integer_loop(branch_ip, compile)
    }

    #[cfg(all(feature = "cranelift", test))]
    pub(crate) fn native_compile_attempts(&self) -> usize {
        self.native_cache
            .as_ref()
            .map_or(0, NativeProgramCache::compile_attempts)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, RuntimeError> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MICAPRG5");
        write_u32(&mut out, self.register_count as u32);
        write_u32(&mut out, self.opcodes.len() as u32);
        for instruction in self.instructions() {
            write_instruction(&mut out, &instruction)?;
        }
        for fact in self.kind_facts.iter().copied() {
            write_optional_value_kind(&mut out, fact);
        }
        Ok(out)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RuntimeError> {
        let mut input = ByteReader::new(bytes);
        input.expect_magic(b"MICAPRG5")?;
        let register_count = input.read_u32()? as usize;
        let instruction_count = input.read_u32()? as usize;
        let mut instructions = Vec::with_capacity(instruction_count);
        for _ in 0..instruction_count {
            instructions.push(input.read_instruction()?);
        }
        let mut encoded_kind_facts = Vec::with_capacity(instruction_count);
        for _ in 0..instruction_count {
            encoded_kind_facts.push(input.read_optional_value_kind()?);
        }
        if !input.is_empty() {
            return Err(artifact_error("trailing program artifact bytes"));
        }
        let program = Self::new(register_count, instructions)?;
        if program.kind_facts.as_ref() != encoded_kind_facts {
            return Err(artifact_error(
                "program kind facts do not match instructions",
            ));
        }
        Ok(program)
    }

    fn decode_operand(&self, operand: OperandRef) -> Operand {
        match operand {
            OperandRef::Register(register) => Operand::Register(register),
            OperandRef::Constant(id) => Operand::Value(self.constant(id).clone()),
        }
    }

    fn decode_relation_args(&self, range: TableRange) -> Vec<RelationArg> {
        self.relation_args(range)
            .iter()
            .map(|arg| match arg {
                CompactRelationArg::Value(operand) => {
                    RelationArg::Value(self.decode_operand(*operand))
                }
                CompactRelationArg::Splice(operand) => {
                    RelationArg::Splice(self.decode_operand(*operand))
                }
                CompactRelationArg::Query(name) => RelationArg::Query(*name),
                CompactRelationArg::Hole => RelationArg::Hole,
            })
            .collect()
    }

    fn decode_list_items(&self, range: TableRange) -> Vec<ListItem> {
        self.list_items(range)
            .iter()
            .map(|item| match item {
                CompactListItem::Value(operand) => ListItem::Value(self.decode_operand(*operand)),
                CompactListItem::Splice(operand) => ListItem::Splice(self.decode_operand(*operand)),
            })
            .collect()
    }

    fn decode_map_items(&self, range: TableRange) -> Vec<MapItem> {
        self.map_items(range)
            .iter()
            .map(|item| match item {
                CompactMapItem::Entry(key, value) => {
                    MapItem::Entry(self.decode_operand(*key), self.decode_operand(*value))
                }
                CompactMapItem::Splice(operand) => MapItem::Splice(self.decode_operand(*operand)),
            })
            .collect()
    }

    fn decode_instruction(&self, opcode: &Opcode) -> Instruction {
        match opcode {
            Opcode::Load { dst, value } => Instruction::Load {
                dst: *dst,
                value: self.constant(*value).clone(),
            },
            Opcode::Move { dst, src } => Instruction::Move {
                dst: *dst,
                src: *src,
            },
            Opcode::CheckKind {
                value,
                expected,
                site,
                subject,
            } => Instruction::CheckKind {
                value: *value,
                expected: *expected,
                site: *site,
                subject: *subject,
            },
            Opcode::CheckType {
                value,
                expected,
                site,
                subject,
            } => Instruction::CheckType {
                value: *value,
                expected: expected.clone(),
                site: *site,
                subject: *subject,
            },
            Opcode::Unary { dst, op, src } => Instruction::Unary {
                dst: *dst,
                op: *op,
                src: *src,
            },
            Opcode::Binary {
                dst,
                op,
                left,
                right,
            } => Instruction::Binary {
                dst: *dst,
                op: *op,
                left: *left,
                right: *right,
            },
            Opcode::BuildList { dst, items } => Instruction::BuildList {
                dst: *dst,
                items: self.decode_list_items(*items),
            },
            Opcode::BuildRelation {
                dst,
                heading,
                cells,
                row_count,
            } => Instruction::BuildRelation {
                dst: *dst,
                heading: self
                    .operands(*heading)
                    .iter()
                    .map(|operand| {
                        let OperandRef::Constant(id) = operand else {
                            unreachable!("relation headings contain constant symbols");
                        };
                        self.constant(*id)
                            .as_symbol()
                            .expect("relation headings contain symbols")
                    })
                    .collect(),
                cells: self
                    .operands(*cells)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
                row_count: *row_count,
            },
            Opcode::BuildMap { dst, entries } => Instruction::BuildMap {
                dst: *dst,
                entries: self
                    .map_entries(*entries)
                    .iter()
                    .map(|(key, value)| (self.decode_operand(*key), self.decode_operand(*value)))
                    .collect(),
            },
            Opcode::BuildMapDynamic { dst, items } => Instruction::BuildMapDynamic {
                dst: *dst,
                items: self.decode_map_items(*items),
            },
            Opcode::BuildRange { dst, start, end } => Instruction::BuildRange {
                dst: *dst,
                start: self.decode_operand(*start),
                end: end.map(|operand| self.decode_operand(operand)),
            },
            Opcode::Index {
                dst,
                collection,
                index,
            } => Instruction::Index {
                dst: *dst,
                collection: *collection,
                index: self.decode_operand(*index),
            },
            Opcode::SetIndex {
                dst,
                collection,
                index,
                value,
            } => Instruction::SetIndex {
                dst: *dst,
                collection: *collection,
                index: self.decode_operand(*index),
                value: self.decode_operand(*value),
            },
            Opcode::ErrorField { dst, error, field } => Instruction::ErrorField {
                dst: *dst,
                error: *error,
                field: *field,
            },
            Opcode::One { dst, src } => Instruction::One {
                dst: *dst,
                src: *src,
            },
            Opcode::CollectionLen { dst, collection } => Instruction::CollectionLen {
                dst: *dst,
                collection: *collection,
            },
            Opcode::CollectionKeyAt {
                dst,
                collection,
                index,
            } => Instruction::CollectionKeyAt {
                dst: *dst,
                collection: *collection,
                index: *index,
            },
            Opcode::CollectionValueAt {
                dst,
                collection,
                index,
            } => Instruction::CollectionValueAt {
                dst: *dst,
                collection: *collection,
                index: *index,
            },
            Opcode::ScanExists {
                dst,
                relation,
                bindings,
            } => Instruction::ScanExists {
                dst: *dst,
                relation: self.relation(*relation),
                bindings: self
                    .bindings(*bindings)
                    .iter()
                    .map(|binding| binding.map(|operand| self.decode_operand(operand)))
                    .collect(),
            },
            Opcode::ScanBindings {
                dst,
                relation,
                bindings,
                outputs,
            } => Instruction::ScanBindings {
                dst: *dst,
                relation: self.relation(*relation),
                bindings: self
                    .bindings(*bindings)
                    .iter()
                    .map(|binding| binding.map(|operand| self.decode_operand(operand)))
                    .collect(),
                outputs: self.query_bindings(*outputs).to_vec(),
            },
            Opcode::ScanValue { dst, relation, key } => Instruction::ScanValue {
                dst: *dst,
                relation: self.relation(*relation),
                key: self.decode_operand(*key),
            },
            Opcode::Assert { relation, values } => Instruction::Assert {
                relation: self.relation(*relation),
                values: self
                    .operands(*values)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
            },
            Opcode::Retract { relation, values } => Instruction::Retract {
                relation: self.relation(*relation),
                values: self
                    .operands(*values)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
            },
            Opcode::RetractWhere { relation, bindings } => Instruction::RetractWhere {
                relation: self.relation(*relation),
                bindings: self
                    .bindings(*bindings)
                    .iter()
                    .map(|binding| binding.map(|operand| self.decode_operand(operand)))
                    .collect(),
            },
            Opcode::ScanDynamic {
                dst,
                relation,
                args,
            } => Instruction::ScanDynamic {
                dst: *dst,
                relation: self.relation(*relation),
                args: self.decode_relation_args(*args),
            },
            Opcode::AssertDynamic { relation, args } => Instruction::AssertDynamic {
                relation: self.relation(*relation),
                args: self.decode_relation_args(*args),
            },
            Opcode::RetractDynamic { relation, args } => Instruction::RetractDynamic {
                relation: self.relation(*relation),
                args: self.decode_relation_args(*args),
            },
            Opcode::ReplaceFunctional { relation, values } => Instruction::ReplaceFunctional {
                relation: self.relation(*relation),
                values: self
                    .operands(*values)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
            },
            Opcode::Branch {
                condition,
                if_true,
                if_false,
            } => Instruction::Branch {
                condition: *condition,
                if_true: if_true.0 as usize,
                if_false: if_false.0 as usize,
            },
            Opcode::Jump { target } => Instruction::Jump {
                target: target.0 as usize,
            },
            Opcode::EnterTry {
                catches,
                finally,
                end,
            } => Instruction::EnterTry {
                catches: self
                    .catches(*catches)
                    .iter()
                    .map(|catch| CatchHandler {
                        code: catch.code.map(|id| self.constant(id).clone()),
                        binding: catch.binding,
                        target: catch.target.0 as usize,
                    })
                    .collect(),
                finally: finally.map(|target| target.0 as usize),
                end: end.0 as usize,
            },
            Opcode::ExitTry => Instruction::ExitTry,
            Opcode::EndFinally => Instruction::EndFinally,
            Opcode::Emit { target, value } => Instruction::Emit {
                target: self.decode_operand(*target),
                value: self.decode_operand(*value),
            },
            Opcode::LoadFunction {
                dst,
                program,
                captures,
                min_arity,
                max_arity,
            } => Instruction::LoadFunction {
                dst: *dst,
                program: Arc::clone(self.program(*program)),
                captures: self
                    .operands(*captures)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
                min_arity: *min_arity,
                max_arity: *max_arity,
            },
            Opcode::CallValue { dst, callee, args } => Instruction::CallValue {
                dst: *dst,
                callee: self.decode_operand(*callee),
                args: self
                    .operands(*args)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
            },
            Opcode::CallValueDynamic { dst, callee, args } => Instruction::CallValueDynamic {
                dst: *dst,
                callee: self.decode_operand(*callee),
                args: self.decode_list_items(*args),
            },
            Opcode::Call { dst, program, args } => Instruction::Call {
                dst: *dst,
                program: Arc::clone(self.program(*program)),
                args: self
                    .operands(*args)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
            },
            Opcode::BuiltinCall {
                dst,
                name,
                result_kind,
                args,
            } => Instruction::BuiltinCall {
                dst: *dst,
                name: *name,
                result_kind: *result_kind,
                args: self
                    .operands(*args)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
            },
            Opcode::BuiltinCallDynamic {
                dst,
                name,
                result_kind,
                args,
            } => Instruction::BuiltinCallDynamic {
                dst: *dst,
                name: *name,
                result_kind: *result_kind,
                args: self.decode_list_items(*args),
            },
            Opcode::Dispatch {
                dst,
                spec,
                selector,
                roles,
            } => {
                let spec = self.dispatch_spec(*spec);
                Instruction::Dispatch {
                    dst: *dst,
                    relations: spec.relations,
                    program_relation: spec.program_relation,
                    program_bytes: spec.program_bytes,
                    selector: self.decode_operand(*selector),
                    roles: self
                        .roles(*roles)
                        .iter()
                        .map(|(role, operand)| {
                            (self.constant(*role).clone(), self.decode_operand(*operand))
                        })
                        .collect(),
                }
            }
            Opcode::DynamicDispatch {
                dst,
                spec,
                selector,
                roles,
            } => {
                let spec = self.dispatch_spec(*spec);
                Instruction::DynamicDispatch {
                    dst: *dst,
                    relations: spec.relations,
                    program_relation: spec.program_relation,
                    program_bytes: spec.program_bytes,
                    selector: self.decode_operand(*selector),
                    roles: self.decode_operand(*roles),
                }
            }
            Opcode::PositionalDispatch {
                dst,
                spec,
                selector,
                args,
            } => {
                let spec = self.dispatch_spec(*spec);
                Instruction::PositionalDispatch {
                    dst: *dst,
                    relations: spec.relations,
                    program_relation: spec.program_relation,
                    program_bytes: spec.program_bytes,
                    selector: self.decode_operand(*selector),
                    args: self
                        .operands(*args)
                        .iter()
                        .map(|operand| self.decode_operand(*operand))
                        .collect(),
                }
            }
            Opcode::PositionalDispatchDynamic {
                dst,
                spec,
                selector,
                args,
            } => {
                let spec = self.dispatch_spec(*spec);
                Instruction::PositionalDispatchDynamic {
                    dst: *dst,
                    relations: spec.relations,
                    program_relation: spec.program_relation,
                    program_bytes: spec.program_bytes,
                    selector: self.decode_operand(*selector),
                    args: self.decode_list_items(*args),
                }
            }
            Opcode::SpawnDispatch {
                dst,
                selector,
                roles,
                delay,
            } => Instruction::SpawnDispatch {
                dst: *dst,
                selector: self.decode_operand(*selector),
                roles: self
                    .roles(*roles)
                    .iter()
                    .map(|(role, operand)| {
                        (self.constant(*role).clone(), self.decode_operand(*operand))
                    })
                    .collect(),
                delay: delay.map(|operand| self.decode_operand(operand)),
            },
            Opcode::SpawnDispatchDynamic {
                dst,
                selector,
                roles,
                delay,
            } => Instruction::SpawnDispatchDynamic {
                dst: *dst,
                selector: self.decode_operand(*selector),
                roles: self.decode_operand(*roles),
                delay: delay.map(|operand| self.decode_operand(operand)),
            },
            Opcode::SpawnPositionalDispatch {
                dst,
                selector,
                args,
                delay,
            } => Instruction::SpawnPositionalDispatch {
                dst: *dst,
                selector: self.decode_operand(*selector),
                args: self
                    .operands(*args)
                    .iter()
                    .map(|operand| self.decode_operand(*operand))
                    .collect(),
                delay: delay.map(|operand| self.decode_operand(operand)),
            },
            Opcode::SpawnPositionalDispatchDynamic {
                dst,
                selector,
                args,
                delay,
            } => Instruction::SpawnPositionalDispatchDynamic {
                dst: *dst,
                selector: self.decode_operand(*selector),
                args: self.decode_list_items(*args),
                delay: delay.map(|operand| self.decode_operand(operand)),
            },
            Opcode::Commit => Instruction::Commit,
            Opcode::Suspend { kind } => Instruction::Suspend {
                kind: self.suspend_kind(*kind).clone(),
            },
            Opcode::SuspendValue { dst, duration } => Instruction::SuspendValue {
                dst: *dst,
                duration: duration.map(|operand| self.decode_operand(operand)),
            },
            Opcode::CommitValue { dst } => Instruction::CommitValue { dst: *dst },
            Opcode::Read { dst, metadata } => Instruction::Read {
                dst: *dst,
                metadata: metadata.map(|operand| self.decode_operand(operand)),
            },
            Opcode::MailboxRecv {
                dst,
                receivers,
                timeout,
            } => Instruction::MailboxRecv {
                dst: *dst,
                receivers: self.decode_operand(*receivers),
                timeout: timeout.map(|operand| self.decode_operand(operand)),
            },
            Opcode::ExternalRequest {
                dst,
                service,
                payload,
                timeout,
            } => Instruction::ExternalRequest {
                dst: *dst,
                service: self.decode_operand(*service),
                payload: self.decode_operand(*payload),
                timeout: timeout.map(|operand| self.decode_operand(operand)),
            },
            Opcode::RollbackRetry => Instruction::RollbackRetry,
            Opcode::Return { value } => Instruction::Return {
                value: self.decode_operand(*value),
            },
            Opcode::Abort { error } => Instruction::Abort {
                error: self.decode_operand(*error),
            },
            Opcode::Raise {
                error,
                message,
                value,
            } => Instruction::Raise {
                error: self.decode_operand(*error),
                message: message.map(|operand| self.decode_operand(operand)),
                value: value.map(|operand| self.decode_operand(operand)),
            },
        }
    }
}

#[inline]
fn table_range<T>(table: &[T], range: TableRange) -> &[T] {
    let start = range.start as usize;
    let end = start + range.len as usize;
    &table[start..end]
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProgramBuilder {
    opcodes: Vec<Opcode>,
    constants: Vec<Value>,
    constant_ids: BTreeMap<Value, ConstId>,
    list_items: Vec<CompactListItem>,
    map_items: Vec<CompactMapItem>,
    relation_args: Vec<CompactRelationArg>,
    map_entries: Vec<(OperandRef, OperandRef)>,
    operands: Vec<OperandRef>,
    bindings: Vec<Option<OperandRef>>,
    query_bindings: Vec<QueryBinding>,
    catches: Vec<CompactCatchHandler>,
    roles: Vec<(ConstId, OperandRef)>,
    programs: Vec<Arc<Program>>,
    relations: Vec<RelationId>,
    relation_ids: BTreeMap<RelationId, RelationRef>,
    dispatch_specs: Vec<DispatchSpec>,
    suspend_kinds: Vec<SuspendKind>,
}

impl ProgramBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.opcodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.opcodes.is_empty()
    }

    pub fn emit(&mut self, instruction: Instruction) -> Result<usize, RuntimeError> {
        let index = self.opcodes.len();
        if index > u16::MAX as usize {
            return Err(artifact_error("program has too many instructions"));
        }
        let opcode = self.encode_instruction(instruction)?;
        self.opcodes.push(opcode);
        Ok(index)
    }

    pub fn emit_branch(
        &mut self,
        condition: Register,
        if_true: usize,
        if_false: usize,
    ) -> Result<usize, RuntimeError> {
        self.emit(Instruction::Branch {
            condition,
            if_true,
            if_false,
        })
    }

    pub fn emit_jump(&mut self, target: usize) -> Result<usize, RuntimeError> {
        self.emit(Instruction::Jump { target })
    }

    pub fn patch_branch(
        &mut self,
        index: usize,
        if_true: usize,
        if_false: usize,
    ) -> Result<(), RuntimeError> {
        let true_target = self.target(if_true)?;
        let false_target = self.target(if_false)?;
        let Some(Opcode::Branch {
            if_true, if_false, ..
        }) = self.opcodes.get_mut(index)
        else {
            return Err(artifact_error("expected branch opcode"));
        };
        *if_true = true_target;
        *if_false = false_target;
        Ok(())
    }

    pub fn patch_true_target(&mut self, index: usize, target: usize) -> Result<(), RuntimeError> {
        let target = self.target(target)?;
        let Some(Opcode::Branch { if_true, .. }) = self.opcodes.get_mut(index) else {
            return Err(artifact_error("expected branch opcode"));
        };
        *if_true = target;
        Ok(())
    }

    pub fn patch_false_target(&mut self, index: usize, target: usize) -> Result<(), RuntimeError> {
        let target = self.target(target)?;
        let Some(Opcode::Branch { if_false, .. }) = self.opcodes.get_mut(index) else {
            return Err(artifact_error("expected branch opcode"));
        };
        *if_false = target;
        Ok(())
    }

    pub fn patch_jump(&mut self, index: usize, target: usize) -> Result<(), RuntimeError> {
        let target = self.target(target)?;
        let Some(Opcode::Jump { target: slot }) = self.opcodes.get_mut(index) else {
            return Err(artifact_error("expected jump opcode"));
        };
        *slot = target;
        Ok(())
    }

    pub fn patch_enter_try(
        &mut self,
        index: usize,
        new_catches: Vec<CatchHandler>,
        new_finally: Option<usize>,
        new_end: usize,
    ) -> Result<(), RuntimeError> {
        let catches = self.catches(new_catches)?;
        let finally = new_finally.map(|target| self.target(target)).transpose()?;
        let end = self.target(new_end)?;
        let Some(Opcode::EnterTry {
            catches: catch_slot,
            finally: finally_slot,
            end: end_slot,
        }) = self.opcodes.get_mut(index)
        else {
            return Err(artifact_error("expected enter-try opcode"));
        };
        *catch_slot = catches;
        *finally_slot = finally;
        *end_slot = end;
        Ok(())
    }

    pub fn finish(self, register_count: usize) -> Result<Program, RuntimeError> {
        let kind_facts = infer_kind_facts(
            register_count,
            &self.opcodes,
            &self.constants,
            &self.catches,
        );
        let type_facts = infer_type_facts(
            register_count,
            &self.opcodes,
            &self.constants,
            &self.operands,
            &self.query_bindings,
            &self.catches,
            &kind_facts,
        );
        #[cfg(feature = "cranelift")]
        let native_cache =
            NativeProgramCache::recognize(&self.opcodes, &self.constants, &kind_facts, &type_facts);
        let program = Program {
            register_count,
            opcodes: self.opcodes.into(),
            kind_facts: kind_facts.into(),
            type_facts: type_facts.into(),
            constants: self.constants.into(),
            list_items: self.list_items.into(),
            map_items: self.map_items.into(),
            relation_args: self.relation_args.into(),
            map_entries: self.map_entries.into(),
            operands: self.operands.into(),
            bindings: self.bindings.into(),
            query_bindings: self.query_bindings.into(),
            catches: self.catches.into(),
            roles: self.roles.into(),
            programs: self.programs.into(),
            relations: self.relations.into(),
            dispatch_specs: self.dispatch_specs.into(),
            suspend_kinds: self.suspend_kinds.into(),
            #[cfg(feature = "cranelift")]
            native_cache,
        };
        let instructions = program.instructions();
        for instruction in &instructions {
            validate_instruction(register_count, instructions.len(), instruction)?;
        }
        Ok(program)
    }

    fn encode_instruction(&mut self, instruction: Instruction) -> Result<Opcode, RuntimeError> {
        Ok(match instruction {
            Instruction::Load { dst, value } => Opcode::Load {
                dst,
                value: self.constant(value)?,
            },
            Instruction::Move { dst, src } => Opcode::Move { dst, src },
            Instruction::CheckKind {
                value,
                expected,
                site,
                subject,
            } => Opcode::CheckKind {
                value,
                expected,
                site,
                subject,
            },
            Instruction::CheckType {
                value,
                expected,
                site,
                subject,
            } => Opcode::CheckType {
                value,
                expected,
                site,
                subject,
            },
            Instruction::Unary { dst, op, src } => Opcode::Unary { dst, op, src },
            Instruction::Binary {
                dst,
                op,
                left,
                right,
            } => Opcode::Binary {
                dst,
                op,
                left,
                right,
            },
            Instruction::BuildList { dst, items } => Opcode::BuildList {
                dst,
                items: self.list_items(items)?,
            },
            Instruction::BuildRelation {
                dst,
                heading,
                cells,
                row_count,
            } => Opcode::BuildRelation {
                dst,
                heading: self.operands(
                    heading
                        .into_iter()
                        .map(|column| Operand::Value(Value::symbol(column)))
                        .collect(),
                )?,
                cells: self.operands(cells)?,
                row_count,
            },
            Instruction::BuildMap { dst, entries } => Opcode::BuildMap {
                dst,
                entries: self.map_entries(entries)?,
            },
            Instruction::BuildMapDynamic { dst, items } => Opcode::BuildMapDynamic {
                dst,
                items: self.map_items(items)?,
            },
            Instruction::BuildRange { dst, start, end } => Opcode::BuildRange {
                dst,
                start: self.operand(start)?,
                end: end.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::Index {
                dst,
                collection,
                index,
            } => Opcode::Index {
                dst,
                collection,
                index: self.operand(index)?,
            },
            Instruction::SetIndex {
                dst,
                collection,
                index,
                value,
            } => Opcode::SetIndex {
                dst,
                collection,
                index: self.operand(index)?,
                value: self.operand(value)?,
            },
            Instruction::ErrorField { dst, error, field } => {
                Opcode::ErrorField { dst, error, field }
            }
            Instruction::One { dst, src } => Opcode::One { dst, src },
            Instruction::CollectionLen { dst, collection } => {
                Opcode::CollectionLen { dst, collection }
            }
            Instruction::CollectionKeyAt {
                dst,
                collection,
                index,
            } => Opcode::CollectionKeyAt {
                dst,
                collection,
                index,
            },
            Instruction::CollectionValueAt {
                dst,
                collection,
                index,
            } => Opcode::CollectionValueAt {
                dst,
                collection,
                index,
            },
            Instruction::ScanExists {
                dst,
                relation,
                bindings,
            } => Opcode::ScanExists {
                dst,
                relation: self.relation(relation)?,
                bindings: self.bindings(bindings)?,
            },
            Instruction::ScanBindings {
                dst,
                relation,
                bindings,
                outputs,
            } => Opcode::ScanBindings {
                dst,
                relation: self.relation(relation)?,
                bindings: self.bindings(bindings)?,
                outputs: self.query_bindings(outputs)?,
            },
            Instruction::ScanValue { dst, relation, key } => Opcode::ScanValue {
                dst,
                relation: self.relation(relation)?,
                key: self.operand(key)?,
            },
            Instruction::Assert { relation, values } => Opcode::Assert {
                relation: self.relation(relation)?,
                values: self.operands(values)?,
            },
            Instruction::Retract { relation, values } => Opcode::Retract {
                relation: self.relation(relation)?,
                values: self.operands(values)?,
            },
            Instruction::RetractWhere { relation, bindings } => Opcode::RetractWhere {
                relation: self.relation(relation)?,
                bindings: self.bindings(bindings)?,
            },
            Instruction::ScanDynamic {
                dst,
                relation,
                args,
            } => Opcode::ScanDynamic {
                dst,
                relation: self.relation(relation)?,
                args: self.relation_args(args)?,
            },
            Instruction::AssertDynamic { relation, args } => Opcode::AssertDynamic {
                relation: self.relation(relation)?,
                args: self.relation_args(args)?,
            },
            Instruction::RetractDynamic { relation, args } => Opcode::RetractDynamic {
                relation: self.relation(relation)?,
                args: self.relation_args(args)?,
            },
            Instruction::ReplaceFunctional { relation, values } => Opcode::ReplaceFunctional {
                relation: self.relation(relation)?,
                values: self.operands(values)?,
            },
            Instruction::Branch {
                condition,
                if_true,
                if_false,
            } => Opcode::Branch {
                condition,
                if_true: self.target(if_true)?,
                if_false: self.target(if_false)?,
            },
            Instruction::Jump { target } => Opcode::Jump {
                target: self.target(target)?,
            },
            Instruction::EnterTry {
                catches,
                finally,
                end,
            } => Opcode::EnterTry {
                catches: self.catches(catches)?,
                finally: finally.map(|target| self.target(target)).transpose()?,
                end: self.target(end)?,
            },
            Instruction::ExitTry => Opcode::ExitTry,
            Instruction::EndFinally => Opcode::EndFinally,
            Instruction::Emit { target, value } => Opcode::Emit {
                target: self.operand(target)?,
                value: self.operand(value)?,
            },
            Instruction::LoadFunction {
                dst,
                program,
                captures,
                min_arity,
                max_arity,
            } => Opcode::LoadFunction {
                dst,
                program: self.program(program)?,
                captures: self.operands(captures)?,
                min_arity,
                max_arity,
            },
            Instruction::CallValue { dst, callee, args } => Opcode::CallValue {
                dst,
                callee: self.operand(callee)?,
                args: self.operands(args)?,
            },
            Instruction::CallValueDynamic { dst, callee, args } => Opcode::CallValueDynamic {
                dst,
                callee: self.operand(callee)?,
                args: self.list_items(args)?,
            },
            Instruction::Call { dst, program, args } => Opcode::Call {
                dst,
                program: self.program(program)?,
                args: self.operands(args)?,
            },
            Instruction::BuiltinCall {
                dst,
                name,
                result_kind,
                args,
            } => Opcode::BuiltinCall {
                dst,
                name,
                result_kind,
                args: self.operands(args)?,
            },
            Instruction::BuiltinCallDynamic {
                dst,
                name,
                result_kind,
                args,
            } => Opcode::BuiltinCallDynamic {
                dst,
                name,
                result_kind,
                args: self.list_items(args)?,
            },
            Instruction::Dispatch {
                dst,
                relations,
                program_relation,
                program_bytes,
                selector,
                roles,
            } => Opcode::Dispatch {
                dst,
                spec: self.dispatch_spec(DispatchSpec {
                    relations,
                    program_relation,
                    program_bytes,
                })?,
                selector: self.operand(selector)?,
                roles: self.roles(roles)?,
            },
            Instruction::DynamicDispatch {
                dst,
                relations,
                program_relation,
                program_bytes,
                selector,
                roles,
            } => Opcode::DynamicDispatch {
                dst,
                spec: self.dispatch_spec(DispatchSpec {
                    relations,
                    program_relation,
                    program_bytes,
                })?,
                selector: self.operand(selector)?,
                roles: self.operand(roles)?,
            },
            Instruction::PositionalDispatch {
                dst,
                relations,
                program_relation,
                program_bytes,
                selector,
                args,
            } => Opcode::PositionalDispatch {
                dst,
                spec: self.dispatch_spec(DispatchSpec {
                    relations,
                    program_relation,
                    program_bytes,
                })?,
                selector: self.operand(selector)?,
                args: self.operands(args)?,
            },
            Instruction::PositionalDispatchDynamic {
                dst,
                relations,
                program_relation,
                program_bytes,
                selector,
                args,
            } => Opcode::PositionalDispatchDynamic {
                dst,
                spec: self.dispatch_spec(DispatchSpec {
                    relations,
                    program_relation,
                    program_bytes,
                })?,
                selector: self.operand(selector)?,
                args: self.list_items(args)?,
            },
            Instruction::SpawnDispatch {
                dst,
                selector,
                roles,
                delay,
            } => Opcode::SpawnDispatch {
                dst,
                selector: self.operand(selector)?,
                roles: self.roles(roles)?,
                delay: delay.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::SpawnDispatchDynamic {
                dst,
                selector,
                roles,
                delay,
            } => Opcode::SpawnDispatchDynamic {
                dst,
                selector: self.operand(selector)?,
                roles: self.operand(roles)?,
                delay: delay.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::SpawnPositionalDispatch {
                dst,
                selector,
                args,
                delay,
            } => Opcode::SpawnPositionalDispatch {
                dst,
                selector: self.operand(selector)?,
                args: self.operands(args)?,
                delay: delay.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::SpawnPositionalDispatchDynamic {
                dst,
                selector,
                args,
                delay,
            } => Opcode::SpawnPositionalDispatchDynamic {
                dst,
                selector: self.operand(selector)?,
                args: self.list_items(args)?,
                delay: delay.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::Commit => Opcode::Commit,
            Instruction::Suspend { kind } => Opcode::Suspend {
                kind: self.suspend_kind(kind)?,
            },
            Instruction::SuspendValue { dst, duration } => Opcode::SuspendValue {
                dst,
                duration: duration.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::CommitValue { dst } => Opcode::CommitValue { dst },
            Instruction::Read { dst, metadata } => Opcode::Read {
                dst,
                metadata: metadata.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::MailboxRecv {
                dst,
                receivers,
                timeout,
            } => Opcode::MailboxRecv {
                dst,
                receivers: self.operand(receivers)?,
                timeout: timeout.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::ExternalRequest {
                dst,
                service,
                payload,
                timeout,
            } => Opcode::ExternalRequest {
                dst,
                service: self.operand(service)?,
                payload: self.operand(payload)?,
                timeout: timeout.map(|operand| self.operand(operand)).transpose()?,
            },
            Instruction::RollbackRetry => Opcode::RollbackRetry,
            Instruction::Return { value } => Opcode::Return {
                value: self.operand(value)?,
            },
            Instruction::Abort { error } => Opcode::Abort {
                error: self.operand(error)?,
            },
            Instruction::Raise {
                error,
                message,
                value,
            } => Opcode::Raise {
                error: self.operand(error)?,
                message: message.map(|operand| self.operand(operand)).transpose()?,
                value: value.map(|operand| self.operand(operand)).transpose()?,
            },
        })
    }

    fn constant(&mut self, value: Value) -> Result<ConstId, RuntimeError> {
        if let Some(id) = self.constant_ids.get(&value).copied() {
            return Ok(id);
        }
        let id = ConstId(narrow_index(self.constants.len(), "constant table")?);
        self.constants.push(value.clone());
        self.constant_ids.insert(value, id);
        Ok(id)
    }

    fn operand(&mut self, operand: Operand) -> Result<OperandRef, RuntimeError> {
        match operand {
            Operand::Register(register) => Ok(OperandRef::Register(register)),
            Operand::Value(value) => Ok(OperandRef::Constant(self.constant(value)?)),
        }
    }

    fn target(&self, target: usize) -> Result<Target, RuntimeError> {
        Ok(Target(narrow_index(target, "instruction target")?))
    }

    fn relation(&mut self, relation: RelationId) -> Result<RelationRef, RuntimeError> {
        if let Some(id) = self.relation_ids.get(&relation).copied() {
            return Ok(id);
        }
        let id = RelationRef(narrow_index(self.relations.len(), "relation table")?);
        self.relations.push(relation);
        self.relation_ids.insert(relation, id);
        Ok(id)
    }

    fn program(&mut self, program: Arc<Program>) -> Result<ProgramId, RuntimeError> {
        let id = ProgramId(narrow_index(self.programs.len(), "program table")?);
        self.programs.push(program);
        Ok(id)
    }

    fn dispatch_spec(&mut self, spec: DispatchSpec) -> Result<DispatchSpecId, RuntimeError> {
        let id = DispatchSpecId(narrow_index(
            self.dispatch_specs.len(),
            "dispatch spec table",
        )?);
        self.dispatch_specs.push(spec);
        Ok(id)
    }

    fn suspend_kind(&mut self, kind: SuspendKind) -> Result<SuspendKindId, RuntimeError> {
        let id = SuspendKindId(narrow_index(
            self.suspend_kinds.len(),
            "suspend kind table",
        )?);
        self.suspend_kinds.push(kind);
        Ok(id)
    }

    fn operands(&mut self, operands: Vec<Operand>) -> Result<TableRange, RuntimeError> {
        let start = self.operands.len();
        for operand in operands {
            let operand = self.operand(operand)?;
            self.operands.push(operand);
        }
        table_range_for(start, self.operands.len(), "operand table")
    }

    fn list_items(&mut self, items: Vec<ListItem>) -> Result<TableRange, RuntimeError> {
        let start = self.list_items.len();
        for item in items {
            let item = match item {
                ListItem::Value(operand) => CompactListItem::Value(self.operand(operand)?),
                ListItem::Splice(operand) => CompactListItem::Splice(self.operand(operand)?),
            };
            self.list_items.push(item);
        }
        table_range_for(start, self.list_items.len(), "list item table")
    }

    fn map_items(&mut self, items: Vec<MapItem>) -> Result<TableRange, RuntimeError> {
        let start = self.map_items.len();
        for item in items {
            let item = match item {
                MapItem::Entry(key, value) => {
                    CompactMapItem::Entry(self.operand(key)?, self.operand(value)?)
                }
                MapItem::Splice(operand) => CompactMapItem::Splice(self.operand(operand)?),
            };
            self.map_items.push(item);
        }
        table_range_for(start, self.map_items.len(), "map item table")
    }

    fn relation_args(&mut self, args: Vec<RelationArg>) -> Result<TableRange, RuntimeError> {
        let start = self.relation_args.len();
        for arg in args {
            let arg = match arg {
                RelationArg::Value(operand) => CompactRelationArg::Value(self.operand(operand)?),
                RelationArg::Splice(operand) => CompactRelationArg::Splice(self.operand(operand)?),
                RelationArg::Query(name) => CompactRelationArg::Query(name),
                RelationArg::Hole => CompactRelationArg::Hole,
            };
            self.relation_args.push(arg);
        }
        table_range_for(start, self.relation_args.len(), "relation arg table")
    }

    fn map_entries(
        &mut self,
        entries: Vec<(Operand, Operand)>,
    ) -> Result<TableRange, RuntimeError> {
        let start = self.map_entries.len();
        for (key, value) in entries {
            let key = self.operand(key)?;
            let value = self.operand(value)?;
            self.map_entries.push((key, value));
        }
        table_range_for(start, self.map_entries.len(), "map entry table")
    }

    fn bindings(&mut self, bindings: Vec<Option<Operand>>) -> Result<TableRange, RuntimeError> {
        let start = self.bindings.len();
        for binding in bindings {
            let binding = binding.map(|operand| self.operand(operand)).transpose()?;
            self.bindings.push(binding);
        }
        table_range_for(start, self.bindings.len(), "binding table")
    }

    fn query_bindings(&mut self, outputs: Vec<QueryBinding>) -> Result<TableRange, RuntimeError> {
        let start = self.query_bindings.len();
        self.query_bindings.extend(outputs);
        table_range_for(start, self.query_bindings.len(), "query binding table")
    }

    fn catches(&mut self, catches: Vec<CatchHandler>) -> Result<TableRange, RuntimeError> {
        let start = self.catches.len();
        for catch in catches {
            let code = catch.code.map(|code| self.constant(code)).transpose()?;
            let target = self.target(catch.target)?;
            self.catches.push(CompactCatchHandler {
                code,
                binding: catch.binding,
                target,
            });
        }
        table_range_for(start, self.catches.len(), "catch table")
    }

    fn roles(&mut self, roles: Vec<(Value, Operand)>) -> Result<TableRange, RuntimeError> {
        let start = self.roles.len();
        for (role, operand) in roles {
            let role = self.constant(role)?;
            let operand = self.operand(operand)?;
            self.roles.push((role, operand));
        }
        table_range_for(start, self.roles.len(), "role table")
    }
}

fn narrow_index(index: usize, table: &'static str) -> Result<u16, RuntimeError> {
    u16::try_from(index).map_err(|_| artifact_error(format!("{table} exceeds u16 capacity")))
}

fn table_range_for(
    start: usize,
    end: usize,
    table: &'static str,
) -> Result<TableRange, RuntimeError> {
    let len = end - start;
    Ok(TableRange {
        start: narrow_index(start, table)?,
        len: narrow_index(len, table)?,
    })
}

#[derive(Debug)]
pub struct ProgramResolver {
    cache: ArcSwap<BTreeMap<Value, Arc<Program>>>,
}

impl Default for ProgramResolver {
    fn default() -> Self {
        Self {
            cache: ArcSwap::new(Arc::new(BTreeMap::new())),
        }
    }
}

impl ProgramResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_program(mut self, method: Value, program: Program) -> Self {
        self.insert(method, program);
        self
    }

    pub fn insert(&mut self, method: Value, program: Program) -> Option<Arc<Program>> {
        let mut next = self.cache.load_full().as_ref().clone();
        let previous = next.insert(method, Arc::new(program));
        self.cache.store(Arc::new(next));
        previous
    }

    pub fn get(&self, method: &Value) -> Option<Arc<Program>> {
        self.cache.load().get(method).cloned()
    }

    pub fn contains(&self, method: &Value) -> bool {
        self.cache.load().contains_key(method)
    }

    pub fn len(&self) -> usize {
        self.cache.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.load().is_empty()
    }

    pub fn resolve(
        &self,
        reader: &impl RelationRead,
        program_bytes_relation: RelationId,
        program_id: &Value,
    ) -> Result<Arc<Program>, RuntimeError> {
        if let Some(program) = self.get(program_id) {
            return Ok(program);
        }

        let rows =
            reader.scan_relation(program_bytes_relation, &[Some(program_id.clone()), None])?;
        let bytes = rows
            .first()
            .and_then(|row| row.values()[1].with_bytes(<[u8]>::to_vec))
            .ok_or_else(|| RuntimeError::MissingProgramArtifact {
                program: program_id.clone(),
            })?;
        let program = Arc::new(Program::from_bytes(&bytes)?);
        self.cache.rcu(|current| {
            if current.contains_key(program_id) {
                return Arc::clone(current);
            }
            let mut next = current.as_ref().clone();
            next.insert(program_id.clone(), Arc::clone(&program));
            Arc::new(next)
        });
        Ok(program)
    }
}

fn validate_instruction(
    register_count: usize,
    instruction_count: usize,
    instruction: &Instruction,
) -> Result<(), RuntimeError> {
    match instruction {
        Instruction::Load { dst, .. } => validate_register(register_count, *dst),
        Instruction::Move { dst, src } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *src)
        }
        Instruction::CheckKind { value, subject, .. } => {
            validate_register(register_count, *value)?;
            if subject.name().is_none() {
                return Err(artifact_error("kind check subject must be a named symbol"));
            }
            Ok(())
        }
        Instruction::CheckType {
            value,
            expected,
            subject,
            ..
        } => {
            validate_register(register_count, *value)?;
            if subject.name().is_none() {
                return Err(artifact_error("type check subject must be a named symbol"));
            }
            validate_type_contract(expected)
        }
        Instruction::Unary { dst, src, .. } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *src)
        }
        Instruction::Binary {
            dst, left, right, ..
        } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *left)?;
            validate_register(register_count, *right)
        }
        Instruction::BuildList { dst, items } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, items.iter().map(ListItem::operand))
        }
        Instruction::BuildRelation {
            dst,
            heading,
            cells,
            row_count,
        } => {
            validate_register(register_count, *dst)?;
            let expected = heading
                .len()
                .checked_mul(usize::from(*row_count))
                .ok_or_else(|| artifact_error("relation literal cell count overflow"))?;
            if cells.len() != expected {
                return Err(artifact_error(format!(
                    "relation literal has {} cells; expected {expected}",
                    cells.len()
                )));
            }
            let mut columns = heading.clone();
            columns.sort_unstable();
            if columns.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(artifact_error(
                    "relation literal heading contains duplicate columns",
                ));
            }
            validate_operands(register_count, cells.iter())
        }
        Instruction::BuildMap { dst, entries } => {
            validate_register(register_count, *dst)?;
            validate_operands(
                register_count,
                entries
                    .iter()
                    .flat_map(|(key, value)| [key, value].into_iter()),
            )
        }
        Instruction::BuildMapDynamic { dst, items } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, items.iter().flat_map(MapItem::operands))
        }
        Instruction::BuildRange { dst, start, end } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, start)?;
            validate_operands(register_count, end.iter())
        }
        Instruction::Index {
            dst,
            collection,
            index,
        } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *collection)?;
            validate_operand(register_count, index)
        }
        Instruction::SetIndex {
            dst,
            collection,
            index,
            value,
        } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *collection)?;
            validate_operand(register_count, index)?;
            validate_operand(register_count, value)
        }
        Instruction::ErrorField { dst, error, .. } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *error)
        }
        Instruction::One { dst, src } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *src)
        }
        Instruction::CollectionLen { dst, collection }
        | Instruction::CollectionKeyAt {
            dst, collection, ..
        }
        | Instruction::CollectionValueAt {
            dst, collection, ..
        } => {
            validate_register(register_count, *dst)?;
            validate_register(register_count, *collection)?;
            match instruction {
                Instruction::CollectionKeyAt { index, .. }
                | Instruction::CollectionValueAt { index, .. } => {
                    validate_register(register_count, *index)
                }
                _ => Ok(()),
            }
        }
        Instruction::ScanExists { dst, bindings, .. }
        | Instruction::ScanBindings { dst, bindings, .. } => {
            validate_register(register_count, *dst)?;
            validate_bindings(register_count, bindings)
        }
        Instruction::ScanValue { dst, key, .. } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, key)
        }
        Instruction::Assert { values, .. }
        | Instruction::Retract { values, .. }
        | Instruction::ReplaceFunctional { values, .. } => {
            validate_operands(register_count, values.iter())
        }
        Instruction::RetractWhere { bindings, .. } => validate_bindings(register_count, bindings),
        Instruction::ScanDynamic { args, .. }
        | Instruction::AssertDynamic { args, .. }
        | Instruction::RetractDynamic { args, .. } => validate_relation_args(register_count, args),
        Instruction::Branch {
            condition,
            if_true,
            if_false,
        } => {
            validate_register(register_count, *condition)?;
            validate_target(instruction_count, *if_true)?;
            validate_target(instruction_count, *if_false)
        }
        Instruction::Jump { target } => validate_target(instruction_count, *target),
        Instruction::EnterTry {
            catches,
            finally,
            end,
        } => {
            validate_target(instruction_count, *end)?;
            if let Some(finally) = finally {
                validate_target(instruction_count, *finally)?;
            }
            for catch in catches {
                validate_target(instruction_count, catch.target)?;
                if let Some(binding) = catch.binding {
                    validate_register(register_count, binding)?;
                }
            }
            Ok(())
        }
        Instruction::ExitTry | Instruction::EndFinally => Ok(()),
        Instruction::Emit { target, value } => {
            validate_operand(register_count, target)?;
            validate_operand(register_count, value)
        }
        Instruction::Return { value } | Instruction::Abort { error: value } => {
            validate_operand(register_count, value)
        }
        Instruction::Raise {
            error,
            message,
            value,
        } => {
            validate_operand(register_count, error)?;
            validate_operands(register_count, message.iter())?;
            validate_operands(register_count, value.iter())
        }
        Instruction::LoadFunction { dst, captures, .. } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, captures.iter())
        }
        Instruction::CallValue { dst, callee, args } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, callee)?;
            validate_operands(register_count, args.iter())
        }
        Instruction::CallValueDynamic { dst, callee, args } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, callee)?;
            validate_operands(register_count, args.iter().map(ListItem::operand))
        }
        Instruction::Call { dst, program, args } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, args.iter())?;
            if args.len() > program.register_count() {
                return Err(RuntimeError::InvalidCallArity {
                    expected_min: 0,
                    expected_max: program.register_count(),
                    actual: args.len(),
                });
            }
            Ok(())
        }
        Instruction::BuiltinCall { dst, args, .. } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, args.iter())
        }
        Instruction::BuiltinCallDynamic { dst, args, .. } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, args.iter().map(ListItem::operand))
        }
        Instruction::Dispatch {
            dst,
            selector,
            roles,
            ..
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operands(register_count, roles.iter().map(|(_, operand)| operand))
        }
        Instruction::DynamicDispatch {
            dst,
            selector,
            roles,
            ..
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operand(register_count, roles)
        }
        Instruction::PositionalDispatch {
            dst,
            selector,
            args,
            ..
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operands(register_count, args.iter())
        }
        Instruction::PositionalDispatchDynamic {
            dst,
            selector,
            args,
            ..
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operands(register_count, args.iter().map(ListItem::operand))
        }
        Instruction::SpawnDispatch {
            dst,
            selector,
            roles,
            delay,
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operands(register_count, roles.iter().map(|(_, operand)| operand))?;
            validate_operands(register_count, delay.iter())
        }
        Instruction::SpawnDispatchDynamic {
            dst,
            selector,
            roles,
            delay,
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operand(register_count, roles)?;
            validate_operands(register_count, delay.iter())
        }
        Instruction::SpawnPositionalDispatch {
            dst,
            selector,
            args,
            delay,
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operands(register_count, args.iter())?;
            validate_operands(register_count, delay.iter())
        }
        Instruction::SpawnPositionalDispatchDynamic {
            dst,
            selector,
            args,
            delay,
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, selector)?;
            validate_operands(register_count, args.iter().map(ListItem::operand))?;
            validate_operands(register_count, delay.iter())
        }
        Instruction::Commit | Instruction::Suspend { .. } | Instruction::RollbackRetry => Ok(()),
        Instruction::SuspendValue { dst, duration }
        | Instruction::Read {
            dst,
            metadata: duration,
        } => {
            validate_register(register_count, *dst)?;
            validate_operands(register_count, duration.iter())
        }
        Instruction::MailboxRecv {
            dst,
            receivers,
            timeout,
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, receivers)?;
            validate_operands(register_count, timeout.iter())
        }
        Instruction::ExternalRequest {
            dst,
            service,
            payload,
            timeout,
        } => {
            validate_register(register_count, *dst)?;
            validate_operand(register_count, service)?;
            validate_operand(register_count, payload)?;
            validate_operands(register_count, timeout.iter())
        }
        Instruction::CommitValue { dst } => validate_register(register_count, *dst),
    }
}

fn validate_type_contract(contract: &TypeContract) -> Result<(), RuntimeError> {
    match contract {
        TypeContract::Dynamic | TypeContract::Never | TypeContract::Kind(_) => Ok(()),
        TypeContract::Literal(TypeLiteralContract::Symbol(symbol))
        | TypeContract::Literal(TypeLiteralContract::ErrorCode(symbol)) => {
            if symbol.name().is_none() {
                return Err(artifact_error("type literal must be a named symbol"));
            }
            Ok(())
        }
        TypeContract::Literal(_) => Ok(()),
        TypeContract::Union(types) => {
            if types.is_empty() {
                return Err(artifact_error("type contract union has no members"));
            }
            for ty in types {
                validate_type_contract(ty)?;
            }
            Ok(())
        }
        TypeContract::Relation(relation) => {
            if relation.alternatives.is_empty() {
                return Err(artifact_error("relation type has no row alternatives"));
            }
            if relation
                .maximum_rows
                .is_some_and(|maximum| relation.minimum_rows > maximum)
            {
                return Err(artifact_error("invalid relation type cardinality"));
            }
            let heading = relation.alternatives[0]
                .columns
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>();
            for alternative in &relation.alternatives {
                if !alternative
                    .columns
                    .iter()
                    .map(|(name, _)| *name)
                    .eq(heading.iter().copied())
                {
                    return Err(artifact_error(
                        "relation type alternatives have different headings",
                    ));
                }
                let mut previous = None;
                for (name, ty) in &alternative.columns {
                    if name.name().is_none() {
                        return Err(artifact_error("relation type column must be named"));
                    }
                    if previous.is_some_and(|previous| previous >= *name) {
                        return Err(artifact_error(
                            "relation type columns are not uniquely sorted",
                        ));
                    }
                    previous = Some(*name);
                    validate_type_contract(ty)?;
                }
            }
            Ok(())
        }
    }
}

fn validate_bindings(
    register_count: usize,
    bindings: &[Option<Operand>],
) -> Result<(), RuntimeError> {
    validate_operands(register_count, bindings.iter().filter_map(Option::as_ref))
}

fn validate_relation_args(register_count: usize, args: &[RelationArg]) -> Result<(), RuntimeError> {
    validate_operands(
        register_count,
        args.iter().filter_map(|arg| match arg {
            RelationArg::Value(operand) | RelationArg::Splice(operand) => Some(operand),
            RelationArg::Query(_) | RelationArg::Hole => None,
        }),
    )
}

fn validate_operands<'a>(
    register_count: usize,
    operands: impl IntoIterator<Item = &'a Operand>,
) -> Result<(), RuntimeError> {
    for operand in operands {
        validate_operand(register_count, operand)?;
    }
    Ok(())
}

fn validate_operand(register_count: usize, operand: &Operand) -> Result<(), RuntimeError> {
    match operand {
        Operand::Register(register) => validate_register(register_count, *register),
        Operand::Value(_) => Ok(()),
    }
}

fn validate_register(register_count: usize, register: Register) -> Result<(), RuntimeError> {
    if register.0 as usize >= register_count {
        return Err(RuntimeError::RegisterOutOfBounds {
            register: register.0,
            register_count,
        });
    }
    Ok(())
}

fn validate_target(instruction_count: usize, target: usize) -> Result<(), RuntimeError> {
    if target >= instruction_count {
        return Err(RuntimeError::InvalidBranchTarget {
            target,
            instruction_count,
        });
    }
    Ok(())
}

const INST_LOAD: u8 = 0;
const INST_MOVE: u8 = 1;
const INST_UNARY: u8 = 2;
const INST_BINARY: u8 = 3;
const INST_SCAN_EXISTS: u8 = 4;
const INST_ASSERT: u8 = 5;
const INST_RETRACT: u8 = 6;
const INST_RETRACT_WHERE: u8 = 7;
const INST_REPLACE_FUNCTIONAL: u8 = 8;
const INST_BRANCH: u8 = 9;
const INST_JUMP: u8 = 10;
const INST_EMIT: u8 = 11;
const INST_COMMIT: u8 = 12;
const INST_SUSPEND_COMMIT: u8 = 13;
const INST_ROLLBACK_RETRY: u8 = 14;
const INST_RETURN: u8 = 15;
const INST_ABORT: u8 = 16;
const INST_DISPATCH: u8 = 17;
const INST_BUILD_LIST: u8 = 18;
const INST_BUILD_MAP: u8 = 19;
const INST_INDEX: u8 = 20;
const INST_COLLECTION_LEN: u8 = 21;
const INST_COLLECTION_KEY_AT: u8 = 22;
const INST_COLLECTION_VALUE_AT: u8 = 23;
const INST_SET_INDEX: u8 = 24;
const INST_SCAN_VALUE: u8 = 25;
const INST_CALL: u8 = 26;
const INST_BUILD_RANGE: u8 = 27;
const INST_ENTER_TRY: u8 = 28;
const INST_EXIT_TRY: u8 = 29;
const INST_END_FINALLY: u8 = 30;
const INST_RAISE: u8 = 31;
const INST_ERROR_FIELD: u8 = 32;
const INST_BUILTIN_CALL: u8 = 33;
const INST_SCAN_BINDINGS: u8 = 34;
const INST_ONE: u8 = 35;
const INST_SUSPEND_VALUE: u8 = 36;
const INST_READ: u8 = 37;
const INST_COMMIT_VALUE: u8 = 38;
const INST_POSITIONAL_DISPATCH: u8 = 39;
const INST_DYNAMIC_DISPATCH: u8 = 40;
const INST_SPAWN_DISPATCH: u8 = 41;
const INST_MAILBOX_RECV: u8 = 42;
const INST_SCAN_DYNAMIC: u8 = 43;
const INST_ASSERT_DYNAMIC: u8 = 44;
const INST_RETRACT_DYNAMIC: u8 = 45;
const INST_BUILTIN_CALL_DYNAMIC: u8 = 46;
const INST_SPAWN_POSITIONAL_DISPATCH: u8 = 47;
const INST_LOAD_FUNCTION: u8 = 48;
const INST_CALL_VALUE: u8 = 49;
const INST_CALL_VALUE_DYNAMIC: u8 = 50;
const INST_POSITIONAL_DISPATCH_DYNAMIC: u8 = 51;
const INST_SPAWN_POSITIONAL_DISPATCH_DYNAMIC: u8 = 52;
const INST_BUILD_MAP_DYNAMIC: u8 = 53;
const INST_SPAWN_DISPATCH_DYNAMIC: u8 = 54;
const INST_EXTERNAL_REQUEST: u8 = 55;
const INST_BUILD_RELATION: u8 = 56;
const INST_CHECK_KIND: u8 = 57;
const INST_CHECK_TYPE: u8 = 58;

const UNARY_NOT: u8 = 0;
const UNARY_NEG: u8 = 1;

const ERROR_FIELD_CODE: u8 = 0;
const ERROR_FIELD_MESSAGE: u8 = 1;
const ERROR_FIELD_VALUE: u8 = 2;

const KIND_BOOL: u8 = 0;
const KIND_INT: u8 = 1;
const KIND_FLOAT: u8 = 2;
const KIND_IDENTITY: u8 = 3;
const KIND_STRING: u8 = 4;
const KIND_BYTES: u8 = 5;
const KIND_SYMBOL: u8 = 6;
const KIND_ERROR_CODE: u8 = 7;
const KIND_ERROR: u8 = 8;
const KIND_CAPABILITY: u8 = 9;
const KIND_FROB: u8 = 10;
const KIND_FUNCTION: u8 = 11;
const KIND_LIST: u8 = 12;
const KIND_MAP: u8 = 13;
const KIND_RANGE: u8 = 14;
const KIND_RELATION: u8 = 15;
const KIND_FACT_UNKNOWN: u8 = u8::MAX;

const KIND_CHECK_BINDING: u8 = 0;
const KIND_CHECK_PARAMETER: u8 = 1;
const KIND_CHECK_BUILTIN: u8 = 2;

const BINARY_EQ: u8 = 0;
const BINARY_NE: u8 = 1;
const BINARY_LT: u8 = 2;
const BINARY_LE: u8 = 3;
const BINARY_GT: u8 = 4;
const BINARY_GE: u8 = 5;
const BINARY_ADD: u8 = 6;
const BINARY_SUB: u8 = 7;
const BINARY_MUL: u8 = 8;
const BINARY_DIV: u8 = 9;
const BINARY_REM: u8 = 10;

const OPERAND_REGISTER: u8 = 0;
const OPERAND_VALUE: u8 = 1;

const LIST_ITEM_VALUE: u8 = 0;
const LIST_ITEM_SPLICE: u8 = 1;
const MAP_ITEM_ENTRY: u8 = 0;
const MAP_ITEM_SPLICE: u8 = 1;

const VALUE_EMPTY_RELATION: u8 = 0;
const VALUE_BOOL: u8 = 1;
const VALUE_INT: u8 = 2;
const VALUE_FLOAT: u8 = 3;
const VALUE_IDENTITY: u8 = 4;
const VALUE_SYMBOL: u8 = 5;
const VALUE_ERROR_CODE: u8 = 6;
const VALUE_STRING: u8 = 7;
const VALUE_BYTES: u8 = 8;
const VALUE_ERROR: u8 = 9;

const TYPE_KIND: u8 = 0;
const TYPE_LITERAL: u8 = 1;
const TYPE_RELATION: u8 = 2;
const TYPE_DYNAMIC: u8 = 3;
const TYPE_NEVER: u8 = 4;
const TYPE_UNION: u8 = 5;
const TYPE_LITERAL_BOOL: u8 = 0;
const TYPE_LITERAL_SYMBOL: u8 = 1;
const TYPE_LITERAL_ERROR_CODE: u8 = 2;
const TYPE_LITERAL_UNIT: u8 = 3;
const TYPE_LITERAL_EMPTY_RELATION: u8 = 4;

fn write_type_contract(out: &mut Vec<u8>, contract: &TypeContract) -> Result<(), RuntimeError> {
    match contract {
        TypeContract::Dynamic => out.push(TYPE_DYNAMIC),
        TypeContract::Never => out.push(TYPE_NEVER),
        TypeContract::Kind(kind) => {
            out.push(TYPE_KIND);
            write_value_kind(out, *kind);
        }
        TypeContract::Literal(literal) => {
            out.push(TYPE_LITERAL);
            match literal {
                TypeLiteralContract::Bool(value) => {
                    out.push(TYPE_LITERAL_BOOL);
                    out.push(u8::from(*value));
                }
                TypeLiteralContract::Symbol(symbol) => {
                    out.push(TYPE_LITERAL_SYMBOL);
                    write_named_symbol(out, *symbol, "type literal symbol")?;
                }
                TypeLiteralContract::ErrorCode(symbol) => {
                    out.push(TYPE_LITERAL_ERROR_CODE);
                    write_named_symbol(out, *symbol, "type literal error code")?;
                }
                TypeLiteralContract::Unit => out.push(TYPE_LITERAL_UNIT),
                TypeLiteralContract::EmptyRelation => out.push(TYPE_LITERAL_EMPTY_RELATION),
            }
        }
        TypeContract::Union(types) => {
            out.push(TYPE_UNION);
            write_u32(out, types.len() as u32);
            for ty in types {
                write_type_contract(out, ty)?;
            }
        }
        TypeContract::Relation(relation) => {
            out.push(TYPE_RELATION);
            write_u64(out, relation.minimum_rows as u64);
            match relation.maximum_rows {
                Some(maximum) => {
                    out.push(1);
                    write_u64(out, maximum as u64);
                }
                None => out.push(0),
            }
            write_u32(out, relation.alternatives.len() as u32);
            for alternative in &relation.alternatives {
                write_u32(out, alternative.columns.len() as u32);
                for (name, cell) in &alternative.columns {
                    write_named_symbol(out, *name, "relation type column")?;
                    write_type_contract(out, cell)?;
                }
            }
        }
    }
    Ok(())
}

fn write_named_symbol(
    out: &mut Vec<u8>,
    symbol: Symbol,
    context: &str,
) -> Result<(), RuntimeError> {
    let Some(name) = symbol.name() else {
        return Err(artifact_error(format!(
            "cannot serialize unnamed {context}"
        )));
    };
    write_str(out, name);
    Ok(())
}

fn write_instruction(out: &mut Vec<u8>, instruction: &Instruction) -> Result<(), RuntimeError> {
    match instruction {
        Instruction::Load { dst, value } => {
            out.push(INST_LOAD);
            write_register(out, *dst);
            write_value(out, value)
        }
        Instruction::Move { dst, src } => {
            out.push(INST_MOVE);
            write_register(out, *dst);
            write_register(out, *src);
            Ok(())
        }
        Instruction::CheckKind {
            value,
            expected,
            site,
            subject,
        } => {
            let Some(subject) = subject.name() else {
                return Err(artifact_error(
                    "cannot serialize unnamed kind check subject",
                ));
            };
            out.push(INST_CHECK_KIND);
            write_register(out, *value);
            write_value_kind(out, *expected);
            write_kind_check_site(out, *site);
            write_str(out, subject);
            Ok(())
        }
        Instruction::CheckType {
            value,
            expected,
            site,
            subject,
        } => {
            let Some(subject) = subject.name() else {
                return Err(artifact_error(
                    "cannot serialize unnamed type check subject",
                ));
            };
            out.push(INST_CHECK_TYPE);
            write_register(out, *value);
            write_type_contract(out, expected)?;
            write_kind_check_site(out, *site);
            write_str(out, subject);
            Ok(())
        }
        Instruction::Unary { dst, op, src } => {
            out.push(INST_UNARY);
            write_register(out, *dst);
            write_unary_op(out, *op);
            write_register(out, *src);
            Ok(())
        }
        Instruction::Binary {
            dst,
            op,
            left,
            right,
        } => {
            out.push(INST_BINARY);
            write_register(out, *dst);
            write_binary_op(out, *op);
            write_register(out, *left);
            write_register(out, *right);
            Ok(())
        }
        Instruction::BuildList { dst, items } => {
            out.push(INST_BUILD_LIST);
            write_register(out, *dst);
            write_list_items(out, items)
        }
        Instruction::BuildRelation {
            dst,
            heading,
            cells,
            row_count,
        } => {
            out.push(INST_BUILD_RELATION);
            write_register(out, *dst);
            write_u32(out, heading.len() as u32);
            for column in heading {
                write_value(out, &Value::symbol(*column))?;
            }
            write_u16(out, *row_count);
            write_u32(out, cells.len() as u32);
            for cell in cells {
                write_operand(out, cell)?;
            }
            Ok(())
        }
        Instruction::BuildMap { dst, entries } => {
            out.push(INST_BUILD_MAP);
            write_register(out, *dst);
            write_u32(out, entries.len() as u32);
            for (key, value) in entries {
                write_operand(out, key)?;
                write_operand(out, value)?;
            }
            Ok(())
        }
        Instruction::BuildMapDynamic { dst, items } => {
            out.push(INST_BUILD_MAP_DYNAMIC);
            write_register(out, *dst);
            write_map_items(out, items)
        }
        Instruction::BuildRange { dst, start, end } => {
            out.push(INST_BUILD_RANGE);
            write_register(out, *dst);
            write_operand(out, start)?;
            write_optional_operand(out, end.as_ref())
        }
        Instruction::Index {
            dst,
            collection,
            index,
        } => {
            out.push(INST_INDEX);
            write_register(out, *dst);
            write_register(out, *collection);
            write_operand(out, index)
        }
        Instruction::SetIndex {
            dst,
            collection,
            index,
            value,
        } => {
            out.push(INST_SET_INDEX);
            write_register(out, *dst);
            write_register(out, *collection);
            write_operand(out, index)?;
            write_operand(out, value)
        }
        Instruction::ErrorField { dst, error, field } => {
            out.push(INST_ERROR_FIELD);
            write_register(out, *dst);
            write_register(out, *error);
            write_error_field(out, *field);
            Ok(())
        }
        Instruction::One { dst, src } => {
            out.push(INST_ONE);
            write_register(out, *dst);
            write_register(out, *src);
            Ok(())
        }
        Instruction::CollectionLen { dst, collection } => {
            out.push(INST_COLLECTION_LEN);
            write_register(out, *dst);
            write_register(out, *collection);
            Ok(())
        }
        Instruction::CollectionKeyAt {
            dst,
            collection,
            index,
        } => {
            out.push(INST_COLLECTION_KEY_AT);
            write_register(out, *dst);
            write_register(out, *collection);
            write_register(out, *index);
            Ok(())
        }
        Instruction::CollectionValueAt {
            dst,
            collection,
            index,
        } => {
            out.push(INST_COLLECTION_VALUE_AT);
            write_register(out, *dst);
            write_register(out, *collection);
            write_register(out, *index);
            Ok(())
        }
        Instruction::ScanExists {
            dst,
            relation,
            bindings,
        } => {
            out.push(INST_SCAN_EXISTS);
            write_register(out, *dst);
            write_identity(out, *relation);
            write_optional_operands(out, bindings)
        }
        Instruction::ScanBindings {
            dst,
            relation,
            bindings,
            outputs,
        } => {
            out.push(INST_SCAN_BINDINGS);
            write_register(out, *dst);
            write_identity(out, *relation);
            write_optional_operands(out, bindings)?;
            write_query_bindings(out, outputs)
        }
        Instruction::ScanValue { dst, relation, key } => {
            out.push(INST_SCAN_VALUE);
            write_register(out, *dst);
            write_identity(out, *relation);
            write_operand(out, key)
        }
        Instruction::Assert { relation, values } => {
            out.push(INST_ASSERT);
            write_identity(out, *relation);
            write_operands(out, values)
        }
        Instruction::Retract { relation, values } => {
            out.push(INST_RETRACT);
            write_identity(out, *relation);
            write_operands(out, values)
        }
        Instruction::RetractWhere { relation, bindings } => {
            out.push(INST_RETRACT_WHERE);
            write_identity(out, *relation);
            write_optional_operands(out, bindings)
        }
        Instruction::ScanDynamic {
            dst,
            relation,
            args,
        } => {
            out.push(INST_SCAN_DYNAMIC);
            write_register(out, *dst);
            write_identity(out, *relation);
            write_relation_args(out, args)
        }
        Instruction::AssertDynamic { relation, args } => {
            out.push(INST_ASSERT_DYNAMIC);
            write_identity(out, *relation);
            write_relation_args(out, args)
        }
        Instruction::RetractDynamic { relation, args } => {
            out.push(INST_RETRACT_DYNAMIC);
            write_identity(out, *relation);
            write_relation_args(out, args)
        }
        Instruction::ReplaceFunctional { relation, values } => {
            out.push(INST_REPLACE_FUNCTIONAL);
            write_identity(out, *relation);
            write_operands(out, values)
        }
        Instruction::Branch {
            condition,
            if_true,
            if_false,
        } => {
            out.push(INST_BRANCH);
            write_register(out, *condition);
            write_u32(out, *if_true as u32);
            write_u32(out, *if_false as u32);
            Ok(())
        }
        Instruction::Jump { target } => {
            out.push(INST_JUMP);
            write_u32(out, *target as u32);
            Ok(())
        }
        Instruction::EnterTry {
            catches,
            finally,
            end,
        } => {
            out.push(INST_ENTER_TRY);
            write_u32(out, *end as u32);
            write_optional_target(out, *finally);
            write_catch_handlers(out, catches)
        }
        Instruction::ExitTry => {
            out.push(INST_EXIT_TRY);
            Ok(())
        }
        Instruction::EndFinally => {
            out.push(INST_END_FINALLY);
            Ok(())
        }
        Instruction::Emit { target, value } => {
            out.push(INST_EMIT);
            write_operand(out, target)?;
            write_operand(out, value)
        }
        Instruction::LoadFunction {
            dst,
            program,
            captures,
            min_arity,
            max_arity,
        } => {
            out.push(INST_LOAD_FUNCTION);
            write_register(out, *dst);
            write_u16(out, *min_arity);
            write_u16(out, *max_arity);
            write_bytes(out, &program.to_bytes()?);
            write_operands(out, captures)?;
            Ok(())
        }
        Instruction::CallValue { dst, callee, args } => {
            out.push(INST_CALL_VALUE);
            write_register(out, *dst);
            write_operand(out, callee)?;
            write_operands(out, args)
        }
        Instruction::CallValueDynamic { dst, callee, args } => {
            out.push(INST_CALL_VALUE_DYNAMIC);
            write_register(out, *dst);
            write_operand(out, callee)?;
            write_list_items(out, args)
        }
        Instruction::Commit => {
            out.push(INST_COMMIT);
            Ok(())
        }
        Instruction::Suspend { kind } => match kind {
            SuspendKind::Commit => {
                out.push(INST_SUSPEND_COMMIT);
                Ok(())
            }
            SuspendKind::Never
            | SuspendKind::TimedMillis(_)
            | SuspendKind::WaitingForInput(_)
            | SuspendKind::MailboxRecv(_)
            | SuspendKind::Spawn(_)
            | SuspendKind::ExternalRequest(_) => Err(artifact_error(
                "only commit suspension is serializable in program artifacts",
            )),
        },
        Instruction::SuspendValue { dst, duration } => {
            out.push(INST_SUSPEND_VALUE);
            write_register(out, *dst);
            write_optional_operand(out, duration.as_ref())
        }
        Instruction::CommitValue { dst } => {
            out.push(INST_COMMIT_VALUE);
            write_register(out, *dst);
            Ok(())
        }
        Instruction::Read { dst, metadata } => {
            out.push(INST_READ);
            write_register(out, *dst);
            write_optional_operand(out, metadata.as_ref())
        }
        Instruction::MailboxRecv {
            dst,
            receivers,
            timeout,
        } => {
            out.push(INST_MAILBOX_RECV);
            write_register(out, *dst);
            write_operand(out, receivers)?;
            write_optional_operand(out, timeout.as_ref())
        }
        Instruction::ExternalRequest {
            dst,
            service,
            payload,
            timeout,
        } => {
            out.push(INST_EXTERNAL_REQUEST);
            write_register(out, *dst);
            write_operand(out, service)?;
            write_operand(out, payload)?;
            write_optional_operand(out, timeout.as_ref())
        }
        Instruction::RollbackRetry => {
            out.push(INST_ROLLBACK_RETRY);
            Ok(())
        }
        Instruction::Return { value } => {
            out.push(INST_RETURN);
            write_operand(out, value)
        }
        Instruction::Abort { error } => {
            out.push(INST_ABORT);
            write_operand(out, error)
        }
        Instruction::Raise {
            error,
            message,
            value,
        } => {
            out.push(INST_RAISE);
            write_operand(out, error)?;
            write_optional_operand(out, message.as_ref())?;
            write_optional_operand(out, value.as_ref())
        }
        Instruction::Dispatch {
            dst,
            relations,
            program_relation,
            program_bytes,
            selector,
            roles,
        } => {
            out.push(INST_DISPATCH);
            write_register(out, *dst);
            write_identity(out, relations.method_selector);
            write_identity(out, relations.param);
            write_identity(out, relations.delegates);
            write_identity(out, *program_relation);
            write_identity(out, *program_bytes);
            write_operand(out, selector)?;
            write_u32(out, roles.len() as u32);
            for (role, operand) in roles {
                write_value(out, role)?;
                write_operand(out, operand)?;
            }
            Ok(())
        }
        Instruction::DynamicDispatch {
            dst,
            relations,
            program_relation,
            program_bytes,
            selector,
            roles,
        } => {
            out.push(INST_DYNAMIC_DISPATCH);
            write_register(out, *dst);
            write_identity(out, relations.method_selector);
            write_identity(out, relations.param);
            write_identity(out, relations.delegates);
            write_identity(out, *program_relation);
            write_identity(out, *program_bytes);
            write_operand(out, selector)?;
            write_operand(out, roles)
        }
        Instruction::PositionalDispatch {
            dst,
            relations,
            program_relation,
            program_bytes,
            selector,
            args,
        } => {
            out.push(INST_POSITIONAL_DISPATCH);
            write_register(out, *dst);
            write_identity(out, relations.method_selector);
            write_identity(out, relations.param);
            write_identity(out, relations.delegates);
            write_identity(out, *program_relation);
            write_identity(out, *program_bytes);
            write_operand(out, selector)?;
            write_operands(out, args)
        }
        Instruction::PositionalDispatchDynamic {
            dst,
            relations,
            program_relation,
            program_bytes,
            selector,
            args,
        } => {
            out.push(INST_POSITIONAL_DISPATCH_DYNAMIC);
            write_register(out, *dst);
            write_identity(out, relations.method_selector);
            write_identity(out, relations.param);
            write_identity(out, relations.delegates);
            write_identity(out, *program_relation);
            write_identity(out, *program_bytes);
            write_operand(out, selector)?;
            write_list_items(out, args)
        }
        Instruction::SpawnDispatch {
            dst,
            selector,
            roles,
            delay,
        } => {
            out.push(INST_SPAWN_DISPATCH);
            write_register(out, *dst);
            write_operand(out, selector)?;
            write_u32(out, roles.len() as u32);
            for (role, operand) in roles {
                write_value(out, role)?;
                write_operand(out, operand)?;
            }
            write_optional_operand(out, delay.as_ref())
        }
        Instruction::SpawnDispatchDynamic {
            dst,
            selector,
            roles,
            delay,
        } => {
            out.push(INST_SPAWN_DISPATCH_DYNAMIC);
            write_register(out, *dst);
            write_operand(out, selector)?;
            write_operand(out, roles)?;
            write_optional_operand(out, delay.as_ref())
        }
        Instruction::SpawnPositionalDispatch {
            dst,
            selector,
            args,
            delay,
        } => {
            out.push(INST_SPAWN_POSITIONAL_DISPATCH);
            write_register(out, *dst);
            write_operand(out, selector)?;
            write_operands(out, args)?;
            write_optional_operand(out, delay.as_ref())
        }
        Instruction::SpawnPositionalDispatchDynamic {
            dst,
            selector,
            args,
            delay,
        } => {
            out.push(INST_SPAWN_POSITIONAL_DISPATCH_DYNAMIC);
            write_register(out, *dst);
            write_operand(out, selector)?;
            write_list_items(out, args)?;
            write_optional_operand(out, delay.as_ref())
        }
        Instruction::Call { dst, program, args } => {
            out.push(INST_CALL);
            write_register(out, *dst);
            write_bytes(out, &program.to_bytes()?);
            write_operands(out, args)
        }
        Instruction::BuiltinCall {
            dst,
            name,
            result_kind,
            args,
        } => {
            let Some(name) = name.name() else {
                return Err(artifact_error("cannot serialize unnamed builtin symbol"));
            };
            out.push(INST_BUILTIN_CALL);
            write_register(out, *dst);
            write_str(out, name);
            write_optional_value_kind(out, *result_kind);
            write_operands(out, args)
        }
        Instruction::BuiltinCallDynamic {
            dst,
            name,
            result_kind,
            args,
        } => {
            let Some(name) = name.name() else {
                return Err(artifact_error("cannot serialize unnamed builtin symbol"));
            };
            out.push(INST_BUILTIN_CALL_DYNAMIC);
            write_register(out, *dst);
            write_str(out, name);
            write_optional_value_kind(out, *result_kind);
            write_list_items(out, args)
        }
    }
}

fn write_register(out: &mut Vec<u8>, register: Register) {
    write_u16(out, register.0);
}

fn write_unary_op(out: &mut Vec<u8>, op: RuntimeUnaryOp) {
    out.push(match op {
        RuntimeUnaryOp::Not => UNARY_NOT,
        RuntimeUnaryOp::Neg => UNARY_NEG,
    });
}

fn write_binary_op(out: &mut Vec<u8>, op: RuntimeBinaryOp) {
    out.push(match op {
        RuntimeBinaryOp::Eq => BINARY_EQ,
        RuntimeBinaryOp::Ne => BINARY_NE,
        RuntimeBinaryOp::Lt => BINARY_LT,
        RuntimeBinaryOp::Le => BINARY_LE,
        RuntimeBinaryOp::Gt => BINARY_GT,
        RuntimeBinaryOp::Ge => BINARY_GE,
        RuntimeBinaryOp::Add => BINARY_ADD,
        RuntimeBinaryOp::Sub => BINARY_SUB,
        RuntimeBinaryOp::Mul => BINARY_MUL,
        RuntimeBinaryOp::Div => BINARY_DIV,
        RuntimeBinaryOp::Rem => BINARY_REM,
    });
}

fn write_error_field(out: &mut Vec<u8>, field: ErrorField) {
    out.push(match field {
        ErrorField::Code => ERROR_FIELD_CODE,
        ErrorField::Message => ERROR_FIELD_MESSAGE,
        ErrorField::Value => ERROR_FIELD_VALUE,
    });
}

fn write_value_kind(out: &mut Vec<u8>, kind: ValueKind) {
    out.push(value_kind_tag(kind));
}

fn write_optional_value_kind(out: &mut Vec<u8>, kind: Option<ValueKind>) {
    out.push(kind.map_or(KIND_FACT_UNKNOWN, value_kind_tag));
}

const fn value_kind_tag(kind: ValueKind) -> u8 {
    match kind {
        ValueKind::Bool => KIND_BOOL,
        ValueKind::Int => KIND_INT,
        ValueKind::Float => KIND_FLOAT,
        ValueKind::Identity => KIND_IDENTITY,
        ValueKind::String => KIND_STRING,
        ValueKind::Bytes => KIND_BYTES,
        ValueKind::Symbol => KIND_SYMBOL,
        ValueKind::ErrorCode => KIND_ERROR_CODE,
        ValueKind::Error => KIND_ERROR,
        ValueKind::Capability => KIND_CAPABILITY,
        ValueKind::Frob => KIND_FROB,
        ValueKind::Function => KIND_FUNCTION,
        ValueKind::List => KIND_LIST,
        ValueKind::Map => KIND_MAP,
        ValueKind::Range => KIND_RANGE,
        ValueKind::Relation => KIND_RELATION,
    }
}

fn write_kind_check_site(out: &mut Vec<u8>, site: KindCheckSite) {
    out.push(match site {
        KindCheckSite::Binding => KIND_CHECK_BINDING,
        KindCheckSite::Parameter => KIND_CHECK_PARAMETER,
        KindCheckSite::Builtin => KIND_CHECK_BUILTIN,
    });
}

fn write_operands(out: &mut Vec<u8>, operands: &[Operand]) -> Result<(), RuntimeError> {
    write_u32(out, operands.len() as u32);
    for operand in operands {
        write_operand(out, operand)?;
    }
    Ok(())
}

fn write_list_items(out: &mut Vec<u8>, items: &[ListItem]) -> Result<(), RuntimeError> {
    write_u32(out, items.len() as u32);
    for item in items {
        match item {
            ListItem::Value(operand) => {
                out.push(LIST_ITEM_VALUE);
                write_operand(out, operand)?;
            }
            ListItem::Splice(operand) => {
                out.push(LIST_ITEM_SPLICE);
                write_operand(out, operand)?;
            }
        }
    }
    Ok(())
}

fn write_map_items(out: &mut Vec<u8>, items: &[MapItem]) -> Result<(), RuntimeError> {
    write_u32(out, items.len() as u32);
    for item in items {
        match item {
            MapItem::Entry(key, value) => {
                out.push(MAP_ITEM_ENTRY);
                write_operand(out, key)?;
                write_operand(out, value)?;
            }
            MapItem::Splice(operand) => {
                out.push(MAP_ITEM_SPLICE);
                write_operand(out, operand)?;
            }
        }
    }
    Ok(())
}

fn write_catch_handlers(out: &mut Vec<u8>, catches: &[CatchHandler]) -> Result<(), RuntimeError> {
    write_u32(out, catches.len() as u32);
    for catch in catches {
        write_optional_value(out, catch.code.as_ref())?;
        match catch.binding {
            Some(binding) => {
                out.push(1);
                write_register(out, binding);
            }
            None => out.push(0),
        }
        write_u32(out, catch.target as u32);
    }
    Ok(())
}

fn write_optional_operands(
    out: &mut Vec<u8>,
    operands: &[Option<Operand>],
) -> Result<(), RuntimeError> {
    write_u32(out, operands.len() as u32);
    for operand in operands {
        match operand {
            Some(operand) => {
                out.push(1);
                write_operand(out, operand)?;
            }
            None => out.push(0),
        }
    }
    Ok(())
}

fn write_relation_args(out: &mut Vec<u8>, args: &[RelationArg]) -> Result<(), RuntimeError> {
    write_u32(out, args.len() as u32);
    for arg in args {
        match arg {
            RelationArg::Value(operand) => {
                out.push(0);
                write_operand(out, operand)?;
            }
            RelationArg::Splice(operand) => {
                out.push(1);
                write_operand(out, operand)?;
            }
            RelationArg::Query(name) => {
                out.push(2);
                let Some(name) = name.name() else {
                    return Err(artifact_error("cannot serialize unnamed query symbol"));
                };
                write_str(out, name);
            }
            RelationArg::Hole => out.push(3),
        }
    }
    Ok(())
}

fn write_query_bindings(out: &mut Vec<u8>, bindings: &[QueryBinding]) -> Result<(), RuntimeError> {
    write_u32(out, bindings.len() as u32);
    for binding in bindings {
        let Some(name) = binding.name.name() else {
            return Err(artifact_error(
                "cannot serialize unnamed query binding symbol",
            ));
        };
        write_str(out, name);
        write_u16(out, binding.position);
    }
    Ok(())
}

fn write_optional_operand(
    out: &mut Vec<u8>,
    operand: Option<&Operand>,
) -> Result<(), RuntimeError> {
    match operand {
        Some(operand) => {
            out.push(1);
            write_operand(out, operand)
        }
        None => {
            out.push(0);
            Ok(())
        }
    }
}

fn write_optional_target(out: &mut Vec<u8>, target: Option<usize>) {
    match target {
        Some(target) => {
            out.push(1);
            write_u32(out, target as u32);
        }
        None => out.push(0),
    }
}

fn write_optional_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            write_str(out, value);
        }
        None => out.push(0),
    }
}

fn write_optional_value(out: &mut Vec<u8>, value: Option<&Value>) -> Result<(), RuntimeError> {
    match value {
        Some(value) => {
            out.push(1);
            write_value(out, value)
        }
        None => {
            out.push(0);
            Ok(())
        }
    }
}

fn write_operand(out: &mut Vec<u8>, operand: &Operand) -> Result<(), RuntimeError> {
    match operand {
        Operand::Register(register) => {
            out.push(OPERAND_REGISTER);
            write_register(out, *register);
            Ok(())
        }
        Operand::Value(value) => {
            out.push(OPERAND_VALUE);
            write_value(out, value)
        }
    }
}

fn write_value(out: &mut Vec<u8>, value: &Value) -> Result<(), RuntimeError> {
    if let Some(value) = value.as_bool() {
        out.push(VALUE_BOOL);
        out.push(value as u8);
    } else if let Some(value) = value.as_int() {
        out.push(VALUE_INT);
        write_i64(out, value);
    } else if let Some(value) = value.as_float() {
        out.push(VALUE_FLOAT);
        write_f32(out, value);
    } else if let Some(value) = value.as_identity() {
        out.push(VALUE_IDENTITY);
        write_identity(out, value);
    } else if let Some(value) = value.as_symbol() {
        let Some(name) = value.name() else {
            return Err(artifact_error("cannot serialize unnamed symbol"));
        };
        out.push(VALUE_SYMBOL);
        write_str(out, name);
    } else if let Some(value) = value.as_error_code() {
        let Some(name) = value.name() else {
            return Err(artifact_error("cannot serialize unnamed error code"));
        };
        out.push(VALUE_ERROR_CODE);
        write_str(out, name);
    } else if let Some(result) = value.with_error(|error| {
        let Some(name) = error.code().name() else {
            return Err(artifact_error("cannot serialize unnamed error code"));
        };
        out.push(VALUE_ERROR);
        write_str(out, name);
        write_optional_str(out, error.message());
        write_optional_value(out, error.value())
    }) {
        result?;
    } else if value.is_empty_relation() {
        out.push(VALUE_EMPTY_RELATION);
    } else if let Some(()) = value.with_str(|text| {
        out.push(VALUE_STRING);
        write_str(out, text);
    }) {
    } else if let Some(()) = value.with_bytes(|bytes| {
        out.push(VALUE_BYTES);
        write_bytes(out, bytes);
    }) {
    } else if value.as_capability().is_some() {
        return Err(artifact_error("capability values are not serializable"));
    } else {
        return Err(artifact_error(
            "collection values are not serializable in program artifacts yet",
        ));
    }
    Ok(())
}

fn write_identity(out: &mut Vec<u8>, identity: Identity) {
    write_u64(out, identity.raw());
}

fn write_str(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    write_u32(out, value.len() as u32);
    out.extend_from_slice(value);
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn expect_magic(&mut self, magic: &[u8]) -> Result<(), RuntimeError> {
        let bytes = self.read_exact(magic.len())?;
        if bytes != magic {
            return Err(artifact_error("invalid program artifact magic"));
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn read_instruction(&mut self) -> Result<Instruction, RuntimeError> {
        Ok(match self.read_u8()? {
            INST_LOAD => Instruction::Load {
                dst: self.read_register()?,
                value: self.read_value()?,
            },
            INST_MOVE => Instruction::Move {
                dst: self.read_register()?,
                src: self.read_register()?,
            },
            INST_CHECK_KIND => Instruction::CheckKind {
                value: self.read_register()?,
                expected: self.read_value_kind()?,
                site: self.read_kind_check_site()?,
                subject: Symbol::intern(&self.read_string()?),
            },
            INST_CHECK_TYPE => Instruction::CheckType {
                value: self.read_register()?,
                expected: self.read_type_contract()?,
                site: self.read_kind_check_site()?,
                subject: Symbol::intern(&self.read_string()?),
            },
            INST_UNARY => Instruction::Unary {
                dst: self.read_register()?,
                op: self.read_unary_op()?,
                src: self.read_register()?,
            },
            INST_BINARY => Instruction::Binary {
                dst: self.read_register()?,
                op: self.read_binary_op()?,
                left: self.read_register()?,
                right: self.read_register()?,
            },
            INST_BUILD_LIST => Instruction::BuildList {
                dst: self.read_register()?,
                items: self.read_list_items()?,
            },
            INST_BUILD_RELATION => {
                let dst = self.read_register()?;
                let heading_len = self.read_u32()? as usize;
                let mut heading = Vec::with_capacity(heading_len);
                for _ in 0..heading_len {
                    let column = self.read_value()?.as_symbol().ok_or_else(|| {
                        artifact_error("relation literal heading contains a non-symbol")
                    })?;
                    heading.push(column);
                }
                let row_count = self.read_u16()?;
                let cell_count = self.read_u32()? as usize;
                let mut cells = Vec::with_capacity(cell_count);
                for _ in 0..cell_count {
                    cells.push(self.read_operand()?);
                }
                Instruction::BuildRelation {
                    dst,
                    heading,
                    cells,
                    row_count,
                }
            }
            INST_BUILD_MAP => Instruction::BuildMap {
                dst: self.read_register()?,
                entries: self.read_map_entries()?,
            },
            INST_BUILD_MAP_DYNAMIC => Instruction::BuildMapDynamic {
                dst: self.read_register()?,
                items: self.read_map_items()?,
            },
            INST_BUILD_RANGE => Instruction::BuildRange {
                dst: self.read_register()?,
                start: self.read_operand()?,
                end: self.read_optional_operand()?,
            },
            INST_INDEX => Instruction::Index {
                dst: self.read_register()?,
                collection: self.read_register()?,
                index: self.read_operand()?,
            },
            INST_SET_INDEX => Instruction::SetIndex {
                dst: self.read_register()?,
                collection: self.read_register()?,
                index: self.read_operand()?,
                value: self.read_operand()?,
            },
            INST_ERROR_FIELD => Instruction::ErrorField {
                dst: self.read_register()?,
                error: self.read_register()?,
                field: self.read_error_field()?,
            },
            INST_ONE => Instruction::One {
                dst: self.read_register()?,
                src: self.read_register()?,
            },
            INST_COLLECTION_LEN => Instruction::CollectionLen {
                dst: self.read_register()?,
                collection: self.read_register()?,
            },
            INST_COLLECTION_KEY_AT => Instruction::CollectionKeyAt {
                dst: self.read_register()?,
                collection: self.read_register()?,
                index: self.read_register()?,
            },
            INST_COLLECTION_VALUE_AT => Instruction::CollectionValueAt {
                dst: self.read_register()?,
                collection: self.read_register()?,
                index: self.read_register()?,
            },
            INST_SCAN_EXISTS => Instruction::ScanExists {
                dst: self.read_register()?,
                relation: self.read_identity()?,
                bindings: self.read_optional_operands()?,
            },
            INST_SCAN_BINDINGS => Instruction::ScanBindings {
                dst: self.read_register()?,
                relation: self.read_identity()?,
                bindings: self.read_optional_operands()?,
                outputs: self.read_query_bindings()?,
            },
            INST_SCAN_VALUE => Instruction::ScanValue {
                dst: self.read_register()?,
                relation: self.read_identity()?,
                key: self.read_operand()?,
            },
            INST_CALL => Instruction::Call {
                dst: self.read_register()?,
                program: Arc::new(Program::from_bytes(&self.read_bytes()?)?),
                args: self.read_operands()?,
            },
            INST_LOAD_FUNCTION => Instruction::LoadFunction {
                dst: self.read_register()?,
                min_arity: self.read_u16()?,
                max_arity: self.read_u16()?,
                program: Arc::new(Program::from_bytes(&self.read_bytes()?)?),
                captures: self.read_operands()?,
            },
            INST_CALL_VALUE => Instruction::CallValue {
                dst: self.read_register()?,
                callee: self.read_operand()?,
                args: self.read_operands()?,
            },
            INST_CALL_VALUE_DYNAMIC => Instruction::CallValueDynamic {
                dst: self.read_register()?,
                callee: self.read_operand()?,
                args: self.read_list_items()?,
            },
            INST_BUILTIN_CALL => Instruction::BuiltinCall {
                dst: self.read_register()?,
                name: Symbol::intern(&self.read_string()?),
                result_kind: self.read_optional_value_kind()?,
                args: self.read_operands()?,
            },
            INST_BUILTIN_CALL_DYNAMIC => Instruction::BuiltinCallDynamic {
                dst: self.read_register()?,
                name: Symbol::intern(&self.read_string()?),
                result_kind: self.read_optional_value_kind()?,
                args: self.read_list_items()?,
            },
            INST_ASSERT => Instruction::Assert {
                relation: self.read_identity()?,
                values: self.read_operands()?,
            },
            INST_RETRACT => Instruction::Retract {
                relation: self.read_identity()?,
                values: self.read_operands()?,
            },
            INST_RETRACT_WHERE => Instruction::RetractWhere {
                relation: self.read_identity()?,
                bindings: self.read_optional_operands()?,
            },
            INST_SCAN_DYNAMIC => Instruction::ScanDynamic {
                dst: self.read_register()?,
                relation: self.read_identity()?,
                args: self.read_relation_args()?,
            },
            INST_ASSERT_DYNAMIC => Instruction::AssertDynamic {
                relation: self.read_identity()?,
                args: self.read_relation_args()?,
            },
            INST_RETRACT_DYNAMIC => Instruction::RetractDynamic {
                relation: self.read_identity()?,
                args: self.read_relation_args()?,
            },
            INST_REPLACE_FUNCTIONAL => Instruction::ReplaceFunctional {
                relation: self.read_identity()?,
                values: self.read_operands()?,
            },
            INST_BRANCH => Instruction::Branch {
                condition: self.read_register()?,
                if_true: self.read_u32()? as usize,
                if_false: self.read_u32()? as usize,
            },
            INST_JUMP => Instruction::Jump {
                target: self.read_u32()? as usize,
            },
            INST_ENTER_TRY => Instruction::EnterTry {
                end: self.read_u32()? as usize,
                finally: self.read_optional_target()?,
                catches: self.read_catch_handlers()?,
            },
            INST_EXIT_TRY => Instruction::ExitTry,
            INST_END_FINALLY => Instruction::EndFinally,
            INST_EMIT => Instruction::Emit {
                target: self.read_operand()?,
                value: self.read_operand()?,
            },
            INST_COMMIT => Instruction::Commit,
            INST_SUSPEND_COMMIT => Instruction::Suspend {
                kind: SuspendKind::Commit,
            },
            INST_SUSPEND_VALUE => Instruction::SuspendValue {
                dst: self.read_register()?,
                duration: self.read_optional_operand()?,
            },
            INST_COMMIT_VALUE => Instruction::CommitValue {
                dst: self.read_register()?,
            },
            INST_READ => Instruction::Read {
                dst: self.read_register()?,
                metadata: self.read_optional_operand()?,
            },
            INST_MAILBOX_RECV => Instruction::MailboxRecv {
                dst: self.read_register()?,
                receivers: self.read_operand()?,
                timeout: self.read_optional_operand()?,
            },
            INST_EXTERNAL_REQUEST => Instruction::ExternalRequest {
                dst: self.read_register()?,
                service: self.read_operand()?,
                payload: self.read_operand()?,
                timeout: self.read_optional_operand()?,
            },
            INST_ROLLBACK_RETRY => Instruction::RollbackRetry,
            INST_RETURN => Instruction::Return {
                value: self.read_operand()?,
            },
            INST_ABORT => Instruction::Abort {
                error: self.read_operand()?,
            },
            INST_RAISE => Instruction::Raise {
                error: self.read_operand()?,
                message: self.read_optional_operand()?,
                value: self.read_optional_operand()?,
            },
            INST_DISPATCH => Instruction::Dispatch {
                dst: self.read_register()?,
                relations: DispatchRelations {
                    method_selector: self.read_identity()?,
                    param: self.read_identity()?,
                    delegates: self.read_identity()?,
                },
                program_relation: self.read_identity()?,
                program_bytes: self.read_identity()?,
                selector: self.read_operand()?,
                roles: self.read_dispatch_roles()?,
            },
            INST_DYNAMIC_DISPATCH => Instruction::DynamicDispatch {
                dst: self.read_register()?,
                relations: DispatchRelations {
                    method_selector: self.read_identity()?,
                    param: self.read_identity()?,
                    delegates: self.read_identity()?,
                },
                program_relation: self.read_identity()?,
                program_bytes: self.read_identity()?,
                selector: self.read_operand()?,
                roles: self.read_operand()?,
            },
            INST_POSITIONAL_DISPATCH => Instruction::PositionalDispatch {
                dst: self.read_register()?,
                relations: DispatchRelations {
                    method_selector: self.read_identity()?,
                    param: self.read_identity()?,
                    delegates: self.read_identity()?,
                },
                program_relation: self.read_identity()?,
                program_bytes: self.read_identity()?,
                selector: self.read_operand()?,
                args: self.read_operands()?,
            },
            INST_POSITIONAL_DISPATCH_DYNAMIC => Instruction::PositionalDispatchDynamic {
                dst: self.read_register()?,
                relations: DispatchRelations {
                    method_selector: self.read_identity()?,
                    param: self.read_identity()?,
                    delegates: self.read_identity()?,
                },
                program_relation: self.read_identity()?,
                program_bytes: self.read_identity()?,
                selector: self.read_operand()?,
                args: self.read_list_items()?,
            },
            INST_SPAWN_DISPATCH => Instruction::SpawnDispatch {
                dst: self.read_register()?,
                selector: self.read_operand()?,
                roles: self.read_dispatch_roles()?,
                delay: self.read_optional_operand()?,
            },
            INST_SPAWN_DISPATCH_DYNAMIC => Instruction::SpawnDispatchDynamic {
                dst: self.read_register()?,
                selector: self.read_operand()?,
                roles: self.read_operand()?,
                delay: self.read_optional_operand()?,
            },
            INST_SPAWN_POSITIONAL_DISPATCH => Instruction::SpawnPositionalDispatch {
                dst: self.read_register()?,
                selector: self.read_operand()?,
                args: self.read_operands()?,
                delay: self.read_optional_operand()?,
            },
            INST_SPAWN_POSITIONAL_DISPATCH_DYNAMIC => Instruction::SpawnPositionalDispatchDynamic {
                dst: self.read_register()?,
                selector: self.read_operand()?,
                args: self.read_list_items()?,
                delay: self.read_optional_operand()?,
            },
            _ => return Err(artifact_error("unknown program artifact instruction tag")),
        })
    }

    fn read_type_contract(&mut self) -> Result<TypeContract, RuntimeError> {
        match self.read_u8()? {
            TYPE_DYNAMIC => Ok(TypeContract::Dynamic),
            TYPE_NEVER => Ok(TypeContract::Never),
            TYPE_KIND => Ok(TypeContract::Kind(self.read_value_kind()?)),
            TYPE_LITERAL => Ok(TypeContract::Literal(match self.read_u8()? {
                TYPE_LITERAL_BOOL => TypeLiteralContract::Bool(self.read_u8()? != 0),
                TYPE_LITERAL_SYMBOL => {
                    TypeLiteralContract::Symbol(Symbol::intern(&self.read_string()?))
                }
                TYPE_LITERAL_ERROR_CODE => {
                    TypeLiteralContract::ErrorCode(Symbol::intern(&self.read_string()?))
                }
                TYPE_LITERAL_UNIT => TypeLiteralContract::Unit,
                TYPE_LITERAL_EMPTY_RELATION => TypeLiteralContract::EmptyRelation,
                _ => return Err(artifact_error("invalid type literal contract tag")),
            })),
            TYPE_RELATION => {
                let minimum_rows = usize::try_from(self.read_u64()?)
                    .map_err(|_| artifact_error("relation type minimum exceeds platform size"))?;
                let maximum_rows = match self.read_u8()? {
                    0 => None,
                    1 => Some(usize::try_from(self.read_u64()?).map_err(|_| {
                        artifact_error("relation type maximum exceeds platform size")
                    })?),
                    _ => return Err(artifact_error("invalid optional cardinality tag")),
                };
                if maximum_rows.is_some_and(|maximum| minimum_rows > maximum) {
                    return Err(artifact_error("invalid relation type cardinality"));
                }
                let alternative_count = self.read_u32()? as usize;
                if alternative_count == 0 {
                    return Err(artifact_error("relation type has no row alternatives"));
                }
                let mut alternatives = Vec::with_capacity(alternative_count);
                for _ in 0..alternative_count {
                    let column_count = self.read_u32()? as usize;
                    let mut columns = Vec::with_capacity(column_count);
                    for _ in 0..column_count {
                        columns.push((
                            Symbol::intern(&self.read_string()?),
                            self.read_type_contract()?,
                        ));
                    }
                    alternatives.push(RelationRowContract { columns });
                }
                Ok(TypeContract::Relation(RelationTypeContract {
                    alternatives,
                    minimum_rows,
                    maximum_rows,
                }))
            }
            TYPE_UNION => {
                let count = self.read_u32()? as usize;
                if count == 0 {
                    return Err(artifact_error("type contract union has no members"));
                }
                let mut types = Vec::with_capacity(count);
                for _ in 0..count {
                    types.push(self.read_type_contract()?);
                }
                Ok(TypeContract::Union(types))
            }
            _ => Err(artifact_error("invalid type contract tag")),
        }
    }

    fn read_operands(&mut self) -> Result<Vec<Operand>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count).map(|_| self.read_operand()).collect()
    }

    fn read_list_items(&mut self) -> Result<Vec<ListItem>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| match self.read_u8()? {
                LIST_ITEM_VALUE => self.read_operand().map(ListItem::Value),
                LIST_ITEM_SPLICE => self.read_operand().map(ListItem::Splice),
                _ => Err(artifact_error("unknown list item tag")),
            })
            .collect()
    }

    fn read_map_items(&mut self) -> Result<Vec<MapItem>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| match self.read_u8()? {
                MAP_ITEM_ENTRY => Ok(MapItem::Entry(self.read_operand()?, self.read_operand()?)),
                MAP_ITEM_SPLICE => self.read_operand().map(MapItem::Splice),
                _ => Err(artifact_error("unknown map item tag")),
            })
            .collect()
    }

    fn read_catch_handlers(&mut self) -> Result<Vec<CatchHandler>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| {
                Ok(CatchHandler {
                    code: self.read_optional_value()?,
                    binding: match self.read_u8()? {
                        0 => None,
                        1 => Some(self.read_register()?),
                        _ => return Err(artifact_error("invalid optional catch binding tag")),
                    },
                    target: self.read_u32()? as usize,
                })
            })
            .collect()
    }

    fn read_optional_operands(&mut self) -> Result<Vec<Option<Operand>>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count).map(|_| self.read_optional_operand()).collect()
    }

    fn read_relation_args(&mut self) -> Result<Vec<RelationArg>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| {
                Ok(match self.read_u8()? {
                    0 => RelationArg::Value(self.read_operand()?),
                    1 => RelationArg::Splice(self.read_operand()?),
                    2 => RelationArg::Query(Symbol::intern(&self.read_string()?)),
                    3 => RelationArg::Hole,
                    _ => return Err(artifact_error("invalid relation argument tag")),
                })
            })
            .collect()
    }

    fn read_optional_operand(&mut self) -> Result<Option<Operand>, RuntimeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => self.read_operand().map(Some),
            _ => Err(artifact_error("invalid optional operand tag")),
        }
    }

    fn read_query_bindings(&mut self) -> Result<Vec<QueryBinding>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| {
                Ok(QueryBinding {
                    name: Symbol::intern(&self.read_string()?),
                    position: self.read_u16()?,
                })
            })
            .collect()
    }

    fn read_optional_target(&mut self) -> Result<Option<usize>, RuntimeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => self.read_u32().map(|target| Some(target as usize)),
            _ => Err(artifact_error("invalid optional target tag")),
        }
    }

    fn read_operand(&mut self) -> Result<Operand, RuntimeError> {
        match self.read_u8()? {
            OPERAND_REGISTER => self.read_register().map(Operand::Register),
            OPERAND_VALUE => self.read_value().map(Operand::Value),
            _ => Err(artifact_error("unknown operand tag")),
        }
    }

    fn read_dispatch_roles(&mut self) -> Result<Vec<(Value, Operand)>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| Ok((self.read_value()?, self.read_operand()?)))
            .collect()
    }

    fn read_map_entries(&mut self) -> Result<Vec<(Operand, Operand)>, RuntimeError> {
        let count = self.read_u32()? as usize;
        (0..count)
            .map(|_| Ok((self.read_operand()?, self.read_operand()?)))
            .collect()
    }

    fn read_value(&mut self) -> Result<Value, RuntimeError> {
        Ok(match self.read_u8()? {
            VALUE_EMPTY_RELATION => Value::nothing(),
            VALUE_BOOL => Value::bool(self.read_u8()? != 0),
            VALUE_INT => Value::int(self.read_i64()?).map_err(|error| {
                artifact_error(format!("invalid serialized integer value: {error:?}"))
            })?,
            VALUE_FLOAT => Value::float(self.read_f32()?).map_err(|error| {
                artifact_error(format!("invalid serialized float value: {error:?}"))
            })?,
            VALUE_IDENTITY => Value::identity(self.read_identity()?),
            VALUE_SYMBOL => Value::symbol(Symbol::intern(&self.read_string()?)),
            VALUE_ERROR_CODE => Value::error_code(Symbol::intern(&self.read_string()?)),
            VALUE_STRING => Value::string(self.read_string()?),
            VALUE_BYTES => Value::bytes(self.read_bytes()?),
            VALUE_ERROR => {
                let code = Symbol::intern(&self.read_string()?);
                let message = self.read_optional_string()?;
                let value = self.read_optional_value()?;
                Value::error(code, message, value)
            }
            _ => return Err(artifact_error("unknown value tag")),
        })
    }

    fn read_optional_string(&mut self) -> Result<Option<String>, RuntimeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => self.read_string().map(Some),
            _ => Err(artifact_error("invalid optional string tag")),
        }
    }

    fn read_optional_value(&mut self) -> Result<Option<Value>, RuntimeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => self.read_value().map(Some),
            _ => Err(artifact_error("invalid optional value tag")),
        }
    }

    fn read_register(&mut self) -> Result<Register, RuntimeError> {
        self.read_u16().map(Register)
    }

    fn read_unary_op(&mut self) -> Result<RuntimeUnaryOp, RuntimeError> {
        match self.read_u8()? {
            UNARY_NOT => Ok(RuntimeUnaryOp::Not),
            UNARY_NEG => Ok(RuntimeUnaryOp::Neg),
            _ => Err(artifact_error("unknown unary operator tag")),
        }
    }

    fn read_binary_op(&mut self) -> Result<RuntimeBinaryOp, RuntimeError> {
        match self.read_u8()? {
            BINARY_EQ => Ok(RuntimeBinaryOp::Eq),
            BINARY_NE => Ok(RuntimeBinaryOp::Ne),
            BINARY_LT => Ok(RuntimeBinaryOp::Lt),
            BINARY_LE => Ok(RuntimeBinaryOp::Le),
            BINARY_GT => Ok(RuntimeBinaryOp::Gt),
            BINARY_GE => Ok(RuntimeBinaryOp::Ge),
            BINARY_ADD => Ok(RuntimeBinaryOp::Add),
            BINARY_SUB => Ok(RuntimeBinaryOp::Sub),
            BINARY_MUL => Ok(RuntimeBinaryOp::Mul),
            BINARY_DIV => Ok(RuntimeBinaryOp::Div),
            BINARY_REM => Ok(RuntimeBinaryOp::Rem),
            _ => Err(artifact_error("unknown binary operator tag")),
        }
    }

    fn read_error_field(&mut self) -> Result<ErrorField, RuntimeError> {
        match self.read_u8()? {
            ERROR_FIELD_CODE => Ok(ErrorField::Code),
            ERROR_FIELD_MESSAGE => Ok(ErrorField::Message),
            ERROR_FIELD_VALUE => Ok(ErrorField::Value),
            _ => Err(artifact_error("unknown error field tag")),
        }
    }

    fn read_value_kind(&mut self) -> Result<ValueKind, RuntimeError> {
        decode_value_kind(self.read_u8()?)
    }

    fn read_optional_value_kind(&mut self) -> Result<Option<ValueKind>, RuntimeError> {
        let tag = self.read_u8()?;
        if tag == KIND_FACT_UNKNOWN {
            return Ok(None);
        }
        decode_value_kind(tag).map(Some)
    }

    fn read_kind_check_site(&mut self) -> Result<KindCheckSite, RuntimeError> {
        match self.read_u8()? {
            KIND_CHECK_BINDING => Ok(KindCheckSite::Binding),
            KIND_CHECK_PARAMETER => Ok(KindCheckSite::Parameter),
            KIND_CHECK_BUILTIN => Ok(KindCheckSite::Builtin),
            _ => Err(artifact_error("unknown kind check site tag")),
        }
    }

    fn read_identity(&mut self) -> Result<Identity, RuntimeError> {
        let raw = self.read_u64()?;
        Identity::new(raw).ok_or_else(|| artifact_error("identity payload out of range"))
    }

    fn read_string(&mut self) -> Result<String, RuntimeError> {
        String::from_utf8(self.read_bytes()?)
            .map_err(|error| artifact_error(format!("invalid utf8 in program artifact: {error}")))
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, RuntimeError> {
        let len = self.read_u32()? as usize;
        self.read_exact(len).map(<[u8]>::to_vec)
    }

    fn read_u8(&mut self) -> Result<u8, RuntimeError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, RuntimeError> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, RuntimeError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, RuntimeError> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_i64(&mut self) -> Result<i64, RuntimeError> {
        let bytes = self.read_exact(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_f32(&mut self) -> Result<f32, RuntimeError> {
        let bytes = self.read_exact(4)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], RuntimeError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| artifact_error("program artifact offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| artifact_error("truncated program artifact"))?;
        self.offset = end;
        Ok(bytes)
    }
}

fn decode_value_kind(tag: u8) -> Result<ValueKind, RuntimeError> {
    match tag {
        KIND_BOOL => Ok(ValueKind::Bool),
        KIND_INT => Ok(ValueKind::Int),
        KIND_FLOAT => Ok(ValueKind::Float),
        KIND_IDENTITY => Ok(ValueKind::Identity),
        KIND_STRING => Ok(ValueKind::String),
        KIND_BYTES => Ok(ValueKind::Bytes),
        KIND_SYMBOL => Ok(ValueKind::Symbol),
        KIND_ERROR_CODE => Ok(ValueKind::ErrorCode),
        KIND_ERROR => Ok(ValueKind::Error),
        KIND_CAPABILITY => Ok(ValueKind::Capability),
        KIND_FROB => Ok(ValueKind::Frob),
        KIND_FUNCTION => Ok(ValueKind::Function),
        KIND_LIST => Ok(ValueKind::List),
        KIND_MAP => Ok(ValueKind::Map),
        KIND_RANGE => Ok(ValueKind::Range),
        KIND_RELATION => Ok(ValueKind::Relation),
        _ => Err(artifact_error("unknown value kind tag")),
    }
}

fn artifact_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::ProgramArtifact(message.into())
}
