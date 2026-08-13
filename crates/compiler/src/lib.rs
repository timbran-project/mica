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

//! Frontend syntax support for Mica.
//!
//! This crate intentionally starts with parsing rather than bytecode emission.
//! The surface language still has open semantic questions around dispatch,
//! relation metadata, and filein/fileout expansion, so the first artifact is a
//! concrete syntax tree with source spans and recoverable parse errors.

mod ast;
mod backend;
mod diagnostics;
mod hir;
mod kinds;
mod lexer;
mod lower;
mod parser;
mod semantics;
mod static_type;
#[cfg(test)]
mod structural_syntax_prototype;
mod syntax;

pub use ast::{
    Arg, Ast, BinaryOp, BindingKind, BindingPattern, CardinalityRef, CatchClause, CollectionItem,
    DispatchRestriction, EffectKind, Expr, FunctionBody, Item, Literal, LoopBinding, MethodKind,
    MethodParam, NodeId, Param, ParamMode, RecoveryClause, ScatterBinding, Span, TypeLiteralRef,
    TypeRef, TypeRefKind, TypeRowRef, UnaryOp,
};
pub use backend::{
    CompileContext, CompileError, CompiledProgram, HostRequestFunction, InstalledMethod,
    InstalledParam, MethodInstallation, MethodRelations, RuleInstallation, compile_semantic,
    compile_source, install_methods, install_methods_from_source, install_rules,
    install_rules_from_source,
};
pub use diagnostics::{
    CompileDiagnostic, DiagnosticRenderOptions, DiagnosticSource, DiagnosticVerbosity,
    compile_error_diagnostics, format_compile_error,
};
pub use hir::{
    HirArg, HirCatch, HirCollectionItem, HirExpr, HirFunctionBody, HirItem, HirLoopBinding,
    HirMethodParam, HirParam, HirPlace, HirProgram, HirRecovery, HirRelationAtom, HirRuleBodyItem,
    HirRuleGuard, HirScatterBinding,
};
pub use lexer::lex;
pub use lower::parse_ast;
pub use parser::parse;
pub use semantics::{
    Binding, BindingId, Diagnostic, DiagnosticCode, LocalKind, Reference, ResolvedName, Scope,
    ScopeId, SemanticProgram, analyze_ast, parse_semantic,
};
pub use static_type::{
    Cardinality, RelationType, RowShape, StaticLiteral, StaticType, StaticTypeError, TypeAliasId,
    TypeParameterId,
};
pub use syntax::{CstElement, CstNode, CstToken, Parse, ParseError, SyntaxKind, Token};
