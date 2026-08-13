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

use std::ops::Range;

pub type Span = Range<usize>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ast {
    pub items: Vec<Item>,
    pub errors: Vec<crate::ParseError>,
    pub node_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Item {
    Expr {
        id: NodeId,
        expr: Expr,
    },
    RelationRule {
        id: NodeId,
        span: Span,
        head: Expr,
        body: Vec<Expr>,
    },
    Method {
        id: NodeId,
        span: Span,
        kind: MethodKind,
        identity: Option<String>,
        selector: Option<String>,
        clauses: Vec<String>,
        params: Vec<MethodParam>,
        result_type: Option<TypeRef>,
        body: Vec<Item>,
    },
}

impl Item {
    pub fn id(&self) -> NodeId {
        match self {
            Self::Expr { id, .. } | Self::RelationRule { id, .. } | Self::Method { id, .. } => *id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MethodKind {
    Method,
    Verb,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodParam {
    pub id: NodeId,
    pub name: String,
    pub restriction: Option<DispatchRestriction>,
    pub annotation: Option<TypeRef>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchRestriction {
    pub prototype: String,
    pub frob_only: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    Literal {
        id: NodeId,
        span: Span,
        value: Literal,
    },
    Name {
        id: NodeId,
        span: Span,
        name: String,
    },
    QueryVar {
        id: NodeId,
        span: Span,
        name: String,
    },
    Identity {
        id: NodeId,
        span: Span,
        name: String,
    },
    Frob {
        id: NodeId,
        span: Span,
        delegate: String,
        value: Box<Expr>,
    },
    Symbol {
        id: NodeId,
        span: Span,
        name: String,
    },
    Hole {
        id: NodeId,
        span: Span,
    },
    List {
        id: NodeId,
        span: Span,
        items: Vec<CollectionItem>,
    },
    Relation {
        id: NodeId,
        span: Span,
        heading: Vec<String>,
        rows: Vec<Vec<Expr>>,
    },
    Map {
        id: NodeId,
        span: Span,
        entries: Vec<(Expr, Expr)>,
    },
    Unary {
        id: NodeId,
        span: Span,
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        id: NodeId,
        span: Span,
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Assign {
        id: NodeId,
        span: Span,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Call {
        id: NodeId,
        span: Span,
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    RoleCall {
        id: NodeId,
        span: Span,
        selector: Box<Expr>,
        args: Vec<Arg>,
    },
    ReceiverCall {
        id: NodeId,
        span: Span,
        receiver: Box<Expr>,
        selector: Box<Expr>,
        args: Vec<Arg>,
    },
    Spawn {
        id: NodeId,
        span: Span,
        target: Box<Expr>,
        delay: Option<Box<Expr>>,
    },
    Index {
        id: NodeId,
        span: Span,
        collection: Box<Expr>,
        index: Option<Box<Expr>>,
    },
    Field {
        id: NodeId,
        span: Span,
        base: Box<Expr>,
        name: String,
    },
    Binding {
        id: NodeId,
        span: Span,
        kind: BindingKind,
        pattern: BindingPattern,
        annotation: Option<TypeRef>,
        value: Option<Box<Expr>>,
    },
    If {
        id: NodeId,
        span: Span,
        condition: Box<Expr>,
        then_items: Vec<Item>,
        elseif: Vec<(Expr, Vec<Item>)>,
        else_items: Vec<Item>,
    },
    Block {
        id: NodeId,
        span: Span,
        items: Vec<Item>,
    },
    For {
        id: NodeId,
        span: Span,
        key: LoopBinding,
        value: Option<LoopBinding>,
        iter: Box<Expr>,
        body: Vec<Item>,
    },
    While {
        id: NodeId,
        span: Span,
        condition: Box<Expr>,
        body: Vec<Item>,
    },
    Return {
        id: NodeId,
        span: Span,
        value: Option<Box<Expr>>,
    },
    Raise {
        id: NodeId,
        span: Span,
        error: Box<Expr>,
        message: Option<Box<Expr>>,
        value: Option<Box<Expr>>,
    },
    Recover {
        id: NodeId,
        span: Span,
        expr: Box<Expr>,
        catches: Vec<RecoveryClause>,
    },
    One {
        id: NodeId,
        span: Span,
        expr: Box<Expr>,
    },
    Break {
        id: NodeId,
        span: Span,
    },
    Continue {
        id: NodeId,
        span: Span,
    },
    Try {
        id: NodeId,
        span: Span,
        body: Vec<Item>,
        catches: Vec<CatchClause>,
        finally: Vec<Item>,
    },
    Function {
        id: NodeId,
        span: Span,
        name: Option<String>,
        params: Vec<Param>,
        result_type: Option<TypeRef>,
        body: FunctionBody,
    },
    Effect {
        id: NodeId,
        span: Span,
        kind: EffectKind,
        expr: Box<Expr>,
    },
    Error {
        id: NodeId,
        span: Span,
    },
}

impl Expr {
    pub fn id(&self) -> NodeId {
        match self {
            Self::Literal { id, .. }
            | Self::Name { id, .. }
            | Self::QueryVar { id, .. }
            | Self::Identity { id, .. }
            | Self::Frob { id, .. }
            | Self::Symbol { id, .. }
            | Self::Hole { id, .. }
            | Self::List { id, .. }
            | Self::Relation { id, .. }
            | Self::Map { id, .. }
            | Self::Unary { id, .. }
            | Self::Binary { id, .. }
            | Self::Assign { id, .. }
            | Self::Call { id, .. }
            | Self::RoleCall { id, .. }
            | Self::ReceiverCall { id, .. }
            | Self::Spawn { id, .. }
            | Self::Index { id, .. }
            | Self::Field { id, .. }
            | Self::Binding { id, .. }
            | Self::If { id, .. }
            | Self::Block { id, .. }
            | Self::For { id, .. }
            | Self::While { id, .. }
            | Self::Return { id, .. }
            | Self::Raise { id, .. }
            | Self::Recover { id, .. }
            | Self::One { id, .. }
            | Self::Break { id, .. }
            | Self::Continue { id, .. }
            | Self::Try { id, .. }
            | Self::Function { id, .. }
            | Self::Effect { id, .. }
            | Self::Error { id, .. } => *id,
        }
    }

    pub fn span(&self) -> &Span {
        match self {
            Self::Literal { span, .. }
            | Self::Name { span, .. }
            | Self::QueryVar { span, .. }
            | Self::Identity { span, .. }
            | Self::Frob { span, .. }
            | Self::Symbol { span, .. }
            | Self::Hole { span, .. }
            | Self::List { span, .. }
            | Self::Relation { span, .. }
            | Self::Map { span, .. }
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Assign { span, .. }
            | Self::Call { span, .. }
            | Self::RoleCall { span, .. }
            | Self::ReceiverCall { span, .. }
            | Self::Spawn { span, .. }
            | Self::Index { span, .. }
            | Self::Field { span, .. }
            | Self::Binding { span, .. }
            | Self::If { span, .. }
            | Self::Block { span, .. }
            | Self::For { span, .. }
            | Self::While { span, .. }
            | Self::Return { span, .. }
            | Self::Raise { span, .. }
            | Self::Recover { span, .. }
            | Self::One { span, .. }
            | Self::Break { span, .. }
            | Self::Continue { span, .. }
            | Self::Try { span, .. }
            | Self::Function { span, .. }
            | Self::Effect { span, .. }
            | Self::Error { span, .. } => span,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Literal {
    Int(String),
    Float(String),
    String(String),
    Bytes(Vec<u8>),
    Bool(bool),
    ErrorCode(String),
    Nothing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollectionItem {
    Expr(Expr),
    Splice(Expr),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Arg {
    pub id: NodeId,
    pub role: Option<String>,
    pub splice: bool,
    pub value: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingKind {
    Let,
    Const,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingPattern {
    Name(String),
    Scatter(Vec<ScatterBinding>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoopBinding {
    pub id: NodeId,
    pub name: String,
    pub annotation: Option<TypeRef>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScatterBinding {
    pub id: NodeId,
    pub name: String,
    pub mode: ParamMode,
    pub annotation: Option<TypeRef>,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeRef {
    pub kind: TypeRefKind,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeRefKind {
    Named {
        name: String,
        arguments: Vec<TypeRef>,
    },
    Literal(TypeLiteralRef),
    Relation {
        alternatives: Vec<TypeRowRef>,
        cardinality: CardinalityRef,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeLiteralRef {
    Bool(bool),
    Symbol(String),
    ErrorCode(String),
    Unit,
    EmptyRelation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeRowRef {
    pub columns: Vec<(String, TypeRef)>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CardinalityRef {
    pub min: usize,
    pub max: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Param {
    pub id: NodeId,
    pub name: String,
    pub mode: ParamMode,
    pub annotation: Option<TypeRef>,
    pub default: Option<Expr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParamMode {
    Required,
    Optional,
    Rest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionBody {
    Expr(Box<Expr>),
    Block(Vec<Item>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatchClause {
    pub id: NodeId,
    pub name: Option<String>,
    pub condition: Option<Expr>,
    pub body: Vec<Item>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryClause {
    pub id: NodeId,
    pub name: Option<String>,
    pub condition: Option<Expr>,
    pub value: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectKind {
    Assert,
    Retract,
    Require,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
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
    And,
    Or,
    Range,
}
