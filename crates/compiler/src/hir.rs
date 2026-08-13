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

use crate::{
    BinaryOp, BindingId, BindingKind, DispatchRestriction, EffectKind, Literal, LocalKind,
    MethodKind, NodeId, ParamMode, ResolvedName, ScopeId, Span, StaticType, UnaryOp,
};
use mica_var::ValueKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirProgram {
    pub items: Vec<HirItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirItem {
    TypeAlias {
        id: NodeId,
        name: String,
        parameters: Vec<String>,
        body: crate::TypeRef,
        source: String,
    },
    Expr {
        id: NodeId,
        expr: HirExpr,
    },
    RelationRule {
        id: NodeId,
        head: HirRelationAtom,
        body: Vec<HirRuleBodyItem>,
    },
    Method {
        id: NodeId,
        kind: MethodKind,
        identity: Option<String>,
        selector: Option<String>,
        clauses: Vec<String>,
        params: Vec<HirMethodParam>,
        result_type: Option<StaticType>,
        result_kind: Option<ValueKind>,
        scope: ScopeId,
        body: Vec<HirItem>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirExpr {
    Literal {
        id: NodeId,
        value: Literal,
    },
    LocalRef {
        id: NodeId,
        binding: BindingId,
    },
    ExternalRef {
        id: NodeId,
        name: String,
    },
    Identity {
        id: NodeId,
        name: String,
    },
    Frob {
        id: NodeId,
        delegate: String,
        value: Box<HirExpr>,
    },
    Symbol {
        id: NodeId,
        name: String,
    },
    QueryVar {
        id: NodeId,
        name: String,
    },
    Hole {
        id: NodeId,
    },
    List {
        id: NodeId,
        items: Vec<HirCollectionItem>,
    },
    Relation {
        id: NodeId,
        heading: Vec<String>,
        rows: Vec<Vec<HirExpr>>,
    },
    Map {
        id: NodeId,
        entries: Vec<(HirExpr, HirExpr)>,
    },
    Unary {
        id: NodeId,
        op: UnaryOp,
        expr: Box<HirExpr>,
    },
    Binary {
        id: NodeId,
        op: BinaryOp,
        left: Box<HirExpr>,
        right: Box<HirExpr>,
    },
    Assign {
        id: NodeId,
        target: HirPlace,
        value: Box<HirExpr>,
    },
    Call {
        id: NodeId,
        callee: Box<HirExpr>,
        args: Vec<HirArg>,
    },
    RoleDispatch {
        id: NodeId,
        selector: Box<HirExpr>,
        args: Vec<HirArg>,
    },
    ReceiverDispatch {
        id: NodeId,
        receiver: Box<HirExpr>,
        selector: Box<HirExpr>,
        args: Vec<HirArg>,
    },
    Spawn {
        id: NodeId,
        target: Box<HirExpr>,
        delay: Option<Box<HirExpr>>,
    },
    RelationAtom(HirRelationAtom),
    FactChange {
        id: NodeId,
        kind: EffectKind,
        atom: HirRelationAtom,
    },
    Require {
        id: NodeId,
        condition: Box<HirExpr>,
    },
    Index {
        id: NodeId,
        collection: Box<HirExpr>,
        index: Option<Box<HirExpr>>,
    },
    Field {
        id: NodeId,
        base: Box<HirExpr>,
        name: String,
    },
    Binding {
        id: NodeId,
        binding: Option<BindingId>,
        scatter: Vec<HirScatterBinding>,
        row: Option<Vec<(String, BindingId)>>,
        kind: BindingKind,
        value: Option<Box<HirExpr>>,
    },
    If {
        id: NodeId,
        condition: Box<HirExpr>,
        then_items: Vec<HirItem>,
        elseif: Vec<(HirExpr, Vec<HirItem>)>,
        else_items: Vec<HirItem>,
    },
    Match {
        id: NodeId,
        value: Box<HirExpr>,
        cases: Vec<HirMatchCase>,
    },
    Block {
        id: NodeId,
        scope: ScopeId,
        items: Vec<HirItem>,
    },
    For {
        id: NodeId,
        scope: ScopeId,
        key: HirLoopBinding,
        value: Option<HirLoopBinding>,
        row: Option<Vec<(String, BindingId)>>,
        iter: Box<HirExpr>,
        body: Vec<HirItem>,
    },
    While {
        id: NodeId,
        condition: Box<HirExpr>,
        body: Vec<HirItem>,
    },
    Return {
        id: NodeId,
        value: Option<Box<HirExpr>>,
    },
    Raise {
        id: NodeId,
        error: Box<HirExpr>,
        message: Option<Box<HirExpr>>,
        value: Option<Box<HirExpr>>,
    },
    Recover {
        id: NodeId,
        expr: Box<HirExpr>,
        catches: Vec<HirRecovery>,
    },
    One {
        id: NodeId,
        expr: Box<HirExpr>,
    },
    Break {
        id: NodeId,
    },
    Continue {
        id: NodeId,
    },
    Try {
        id: NodeId,
        body: Vec<HirItem>,
        catches: Vec<HirCatch>,
        finally: Vec<HirItem>,
    },
    Function {
        id: NodeId,
        name: Option<BindingId>,
        scope: ScopeId,
        params: Vec<HirParam>,
        result_type: Option<StaticType>,
        result_kind: Option<ValueKind>,
        captures: Vec<BindingId>,
        body: HirFunctionBody,
    },
    Error {
        id: NodeId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirCollectionItem {
    Expr(HirExpr),
    Splice(HirExpr),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirRelationAtom {
    pub id: NodeId,
    pub name: String,
    pub args: Vec<HirArg>,
    pub negated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirRuleBodyItem {
    Atom(HirRelationAtom),
    Guard(Box<HirRuleGuard>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirRuleGuard {
    pub id: NodeId,
    pub op: BinaryOp,
    pub left: HirExpr,
    pub right: HirExpr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirArg {
    pub id: NodeId,
    pub role: Option<String>,
    pub splice: bool,
    pub value: HirExpr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirParam {
    pub id: NodeId,
    pub binding: BindingId,
    pub kind: LocalKind,
    pub declared_type: Option<StaticType>,
    pub declared_kind: Option<ValueKind>,
    pub default: Option<HirExpr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirMethodParam {
    pub id: NodeId,
    pub binding: BindingId,
    pub restriction: Option<DispatchRestriction>,
    pub declared_type: Option<StaticType>,
    pub declared_kind: Option<ValueKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirScatterBinding {
    pub id: NodeId,
    pub binding: BindingId,
    pub mode: ParamMode,
    pub declared_type: Option<StaticType>,
    pub declared_kind: Option<ValueKind>,
    pub default: Option<HirExpr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirLoopBinding {
    pub id: NodeId,
    pub binding: BindingId,
    pub declared_type: Option<StaticType>,
    pub declared_kind: Option<ValueKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirMatchCase {
    pub id: NodeId,
    pub scope: ScopeId,
    pub pattern: HirMatchPattern,
    pub guard: Option<HirExpr>,
    pub body: Vec<HirItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirMatchPattern {
    Wildcard,
    Row(Vec<(String, BindingId)>),
    None,
    Some(BindingId),
    Ok(BindingId),
    Err(BindingId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirCatch {
    pub id: NodeId,
    pub binding: Option<BindingId>,
    pub condition: Option<HirExpr>,
    pub body: Vec<HirItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirRecovery {
    pub id: NodeId,
    pub binding: Option<BindingId>,
    pub condition: Option<HirExpr>,
    pub value: HirExpr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirFunctionBody {
    Expr(Box<HirExpr>),
    Block(Vec<HirItem>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirPlace {
    Local {
        id: NodeId,
        binding: BindingId,
    },
    Index {
        id: NodeId,
        collection: Box<HirExpr>,
        index: Option<Box<HirExpr>>,
    },
    Dot {
        id: NodeId,
        base: Box<HirExpr>,
        name: String,
    },
    Invalid {
        id: NodeId,
        span: Span,
        resolution: Option<ResolvedName>,
    },
}
