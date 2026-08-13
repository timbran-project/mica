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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SyntaxKind {
    Eof,
    Error,
    Whitespace,
    Newline,
    LineComment,
    Ident,
    ErrorCode,
    Int,
    Float,
    String,
    Bytes,
    LetKw,
    ConstKw,
    IfKw,
    MatchKw,
    CaseKw,
    ExactlyKw,
    ElseIfKw,
    ElseKw,
    EndKw,
    BeginKw,
    ForKw,
    InKw,
    WhileKw,
    ReturnKw,
    RaiseKw,
    RecoverKw,
    SpawnKw,
    AfterKw,
    NotKw,
    BreakKw,
    ContinueKw,
    TryKw,
    CatchKw,
    AsKw,
    FinallyKw,
    FnKw,
    MethodKw,
    VerbKw,
    DoKw,
    AssertKw,
    RetractKw,
    RequireKw,
    TrueKw,
    FalseKw,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Dot,
    DotDot,
    Colon,
    Hash,
    At,
    Question,
    Underscore,
    Eq,
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    AmpAmp,
    PipePipe,
    Pipe,
    Membership,
    Bang,
    Arrow,
    FatArrow,
    ColonDash,
    Program,
    ItemList,
    MethodItem,
    VerbItem,
    TypeAliasItem,
    MethodHeader,
    VerbHeader,
    VerbParamList,
    VerbParam,
    DispatchRestriction,
    MethodClause,
    Block,
    LetExpr,
    ConstExpr,
    IfExpr,
    MatchExpr,
    MatchCase,
    Pattern,
    PatternField,
    ElseIfClause,
    ElseClause,
    BeginExpr,
    ForExpr,
    LoopBinding,
    WhileExpr,
    ReturnExpr,
    RaiseExpr,
    RecoverExpr,
    SpawnExpr,
    RecoverClause,
    BreakExpr,
    ContinueExpr,
    TryExpr,
    CatchClause,
    FinallyClause,
    FnExpr,
    LambdaExpr,
    ParamList,
    Param,
    ScatterPattern,
    ScatterBinding,
    TypeRef,
    TypeArguments,
    RelationType,
    TypeRow,
    TypeColumn,
    Cardinality,
    ExprStmt,
    AssignExpr,
    BinaryExpr,
    UnaryExpr,
    CallExpr,
    ReceiverCallExpr,
    RoleCallExpr,
    ArgList,
    Arg,
    IndexExpr,
    FieldExpr,
    ListExpr,
    ListItem,
    RelationExpr,
    RelationHeading,
    RelationRow,
    MapExpr,
    MapEntry,
    GroupExpr,
    LiteralExpr,
    NameExpr,
    QueryVarExpr,
    IdentityExpr,
    FrobExpr,
    SymbolExpr,
    HoleExpr,
    DomExpr,
    DomElement,
    DomAttr,
    DomChildExpr,
    DomText,
    RelationRule,
    AtomExpr,
    AssertExpr,
    RetractExpr,
    RequireExpr,
}

impl SyntaxKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Newline | Self::LineComment)
    }

    pub fn is_dom_name_atom(self) -> bool {
        matches!(
            self,
            Self::Ident
                | Self::ErrorCode
                | Self::LetKw
                | Self::ConstKw
                | Self::IfKw
                | Self::MatchKw
                | Self::CaseKw
                | Self::ExactlyKw
                | Self::ElseIfKw
                | Self::ElseKw
                | Self::EndKw
                | Self::BeginKw
                | Self::ForKw
                | Self::InKw
                | Self::WhileKw
                | Self::ReturnKw
                | Self::RaiseKw
                | Self::RecoverKw
                | Self::SpawnKw
                | Self::AfterKw
                | Self::NotKw
                | Self::BreakKw
                | Self::ContinueKw
                | Self::TryKw
                | Self::CatchKw
                | Self::AsKw
                | Self::FinallyKw
                | Self::FnKw
                | Self::MethodKw
                | Self::VerbKw
                | Self::DoKw
                | Self::AssertKw
                | Self::RetractKw
                | Self::RequireKw
                | Self::TrueKw
                | Self::FalseKw
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: Range<usize>,
}

impl Token {
    pub(crate) fn new(kind: SyntaxKind, start: usize, end: usize) -> Self {
        Self {
            kind,
            span: start..end,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CstToken {
    pub kind: SyntaxKind,
    pub span: Range<usize>,
}

impl From<Token> for CstToken {
    fn from(value: Token) -> Self {
        Self {
            kind: value.kind,
            span: value.span,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CstElement {
    Node(CstNode),
    Token(CstToken),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CstNode {
    pub kind: SyntaxKind,
    pub span: Range<usize>,
    pub children: Vec<CstElement>,
}

impl CstNode {
    pub fn new(kind: SyntaxKind, children: Vec<CstElement>) -> Self {
        let span = children_span(&children);
        Self {
            kind,
            span,
            children,
        }
    }

    pub fn token(kind: SyntaxKind, span: Range<usize>) -> CstElement {
        CstElement::Token(CstToken { kind, span })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Range<usize>,
}

impl ParseError {
    pub(crate) fn new(message: impl Into<String>, span: Range<usize>) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parse {
    pub root: CstNode,
    pub errors: Vec<ParseError>,
}

fn children_span(children: &[CstElement]) -> Range<usize> {
    let mut start = None;
    let mut end = None;
    for child in children {
        let span = match child {
            CstElement::Node(node) => node.span.clone(),
            CstElement::Token(token) => token.span.clone(),
        };
        if start.is_none() {
            start = Some(span.start);
        }
        end = Some(span.end);
    }
    start.unwrap_or(0)..end.unwrap_or(0)
}
