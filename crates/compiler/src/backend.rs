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

use crate::kinds::{KindInference, KindSet, iteration_binding_kinds};
use crate::static_type::StaticTypeInference;
use crate::{
    BinaryOp, BindingId, Cardinality, Diagnostic, DispatchRestriction, EffectKind, HirArg,
    HirCatch, HirCollectionItem, HirExpr, HirFunctionBody, HirItem, HirLoopBinding, HirMatchCase,
    HirMatchPattern, HirMethodParam, HirPlace, HirProgram, HirRecovery, HirRelationAtom,
    HirRuleBodyItem, HirRuleGuard, HirScatterBinding, Literal, LocalKind, NodeId, ParamMode,
    ParseError, RelationType, RowShape, SemanticProgram, Span, StaticLiteral, StaticType,
    TypeAliasDefinition, UnaryOp, analyze_ast_with_aliases, parse_ast,
};
use mica_relation_kernel::{
    Atom, ConflictPolicy, DispatchRelations, RelationId, RelationKernel, RelationMetadata, Rule,
    RuleBodyItem, RuleComparisonOp, RuleDefinition, RuleGuard, Term, Transaction, Tuple,
};
use mica_var::{Identity, Symbol, Value, ValueError, ValueKind};
use mica_vm::{
    BuiltinResultKind, CatchHandler, ErrorField, Instruction, KindCheckSite, ListItem, MapItem,
    Operand, Program, ProgramBuilder, QueryBinding, Register, RelationArg, RelationRowContract,
    RelationTypeContract, RuntimeBinaryOp, RuntimeError, RuntimeUnaryOp, TypeContract,
    TypeLiteralContract,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

pub fn compile_source(
    source: &str,
    context: &CompileContext,
) -> Result<CompiledProgram, CompileError> {
    let semantic = parse_semantic_with_context(source, context);
    compile_semantic(semantic, context)
}

pub fn parse_semantic_with_context(source: &str, context: &CompileContext) -> SemanticProgram {
    let ast = parse_ast(source);
    let aliases = context.type_aliases.values().cloned().collect::<Vec<_>>();
    analyze_ast_with_aliases(&ast, &aliases)
}

pub fn compile_semantic(
    semantic: SemanticProgram,
    context: &CompileContext,
) -> Result<CompiledProgram, CompileError> {
    if !semantic.parse_errors.is_empty() {
        return Err(CompileError::ParseErrors {
            errors: semantic.parse_errors.clone(),
        });
    }
    if let Some(diagnostic) = semantic.diagnostics.first() {
        return Err(CompileError::SemanticDiagnostic {
            diagnostic: diagnostic.clone(),
        });
    }
    return_context_errors(context_errors(&semantic, context))?;

    let compiler = ProgramCompiler::new(&semantic, context);
    let program = compiler.compile_program(&semantic.hir)?;
    Ok(CompiledProgram { semantic, program })
}

pub fn install_rules_from_source(
    source: &str,
    context: &CompileContext,
    kernel: &RelationKernel,
) -> Result<Option<RuleInstallation>, CompileError> {
    let semantic = parse_semantic_with_context(source, context);
    install_rules(semantic, context, kernel, source)
}

pub fn install_rules(
    semantic: SemanticProgram,
    context: &CompileContext,
    kernel: &RelationKernel,
    source: &str,
) -> Result<Option<RuleInstallation>, CompileError> {
    if !semantic.parse_errors.is_empty() {
        return Err(CompileError::ParseErrors {
            errors: semantic.parse_errors.clone(),
        });
    }
    if let Some(diagnostic) = semantic.diagnostics.first() {
        return Err(CompileError::SemanticDiagnostic {
            diagnostic: diagnostic.clone(),
        });
    }
    return_context_errors(context_errors(&semantic, context))?;

    if !semantic
        .hir
        .items
        .iter()
        .any(|item| matches!(item, HirItem::RelationRule { .. }))
    {
        return Ok(None);
    }

    if let Some(item) = semantic.hir.items.iter().find(|item| {
        !matches!(
            item,
            HirItem::RelationRule { .. } | HirItem::TypeAlias { .. }
        )
    }) {
        return Err(CompileError::Unsupported {
            node: item_id(item),
            span: semantic.span(item_id(item)).cloned(),
            message: "relation rule definitions cannot be mixed with executable task code yet"
                .to_owned(),
        });
    }

    let rules = semantic
        .hir
        .items
        .iter()
        .filter(|item| matches!(item, HirItem::RelationRule { .. }))
        .map(|item| compile_rule_item(&semantic, context, item))
        .collect::<Result<Vec<_>, _>>()?;
    let definitions = rules
        .into_iter()
        .map(|rule| {
            kernel
                .install_rule(rule, source.to_owned())
                .map_err(Into::into)
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    Ok(Some(RuleInstallation {
        semantic,
        rules: definitions,
    }))
}

pub fn install_methods_from_source(
    source: &str,
    context: &CompileContext,
    tx: &mut Transaction<'_>,
) -> Result<MethodInstallation, CompileError> {
    let semantic = parse_semantic_with_context(source, context);
    install_methods(semantic, context, tx)
}

pub fn install_methods(
    semantic: SemanticProgram,
    context: &CompileContext,
    tx: &mut Transaction<'_>,
) -> Result<MethodInstallation, CompileError> {
    if !semantic.parse_errors.is_empty() {
        return Err(CompileError::ParseErrors {
            errors: semantic.parse_errors.clone(),
        });
    }
    if let Some(diagnostic) = semantic.diagnostics.first() {
        return Err(CompileError::SemanticDiagnostic {
            diagnostic: diagnostic.clone(),
        });
    }
    return_context_errors(context_errors(&semantic, context))?;

    let method_relations = context
        .method_relations
        .ok_or_else(|| CompileError::Unsupported {
            node: NodeId(0),
            span: None,
            message: "method relation ids are not configured".to_owned(),
        })?;
    let mut methods = Vec::new();

    for item in &semantic.hir.items {
        if let HirItem::Method { .. } = item {
            let method = compile_installed_method(&semantic, context, item)?;
            tx.assert(
                method_relations.dispatch.method_selector,
                Tuple::from([method.method.clone(), method.selector.clone()]),
            )?;
            tx.assert(
                method_relations.method_program,
                Tuple::from([method.method.clone(), method.program.clone()]),
            )?;
            tx.assert(
                method_relations.program_bytes,
                Tuple::from([
                    method.program.clone(),
                    Value::bytes(method.compiled.program.to_bytes()?),
                ]),
            )?;
            for param in &method.params {
                tx.assert(
                    method_relations.dispatch.param,
                    Tuple::from([
                        method.method.clone(),
                        param.role.clone(),
                        param.restriction.clone(),
                        Value::int(i64::from(param.position)).unwrap(),
                    ]),
                )?;
            }
            methods.push(method);
        }
    }

    Ok(MethodInstallation { semantic, methods })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledProgram {
    pub semantic: SemanticProgram,
    pub program: Program,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodInstallation {
    pub semantic: SemanticProgram,
    pub methods: Vec<InstalledMethod>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleInstallation {
    pub semantic: SemanticProgram,
    pub rules: Vec<RuleDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledMethod {
    pub method: Value,
    pub program: Value,
    pub selector: Value,
    pub params: Vec<InstalledParam>,
    pub compiled: CompiledProgram,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledParam {
    pub name: String,
    pub role: Value,
    pub restriction: Value,
    pub position: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MethodRelations {
    pub dispatch: DispatchRelations,
    pub method_program: RelationId,
    pub program_bytes: RelationId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DotRelation {
    pub relation: RelationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRequestFunction {
    pub service: Symbol,
    pub payload_fields: Vec<Symbol>,
    pub timeout: Option<Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompileContext {
    relations: HashMap<String, RelationId>,
    relation_metadata: HashMap<RelationId, RelationMetadata>,
    dot_relations: HashMap<String, DotRelation>,
    identities: HashMap<String, Identity>,
    program_identities: HashMap<String, Identity>,
    runtime_functions: HashMap<String, BuiltinResultKind>,
    host_request_functions: HashMap<String, HostRequestFunction>,
    method_relations: Option<MethodRelations>,
    type_aliases: HashMap<String, TypeAliasDefinition>,
}

impl CompileContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_relation(mut self, name: impl Into<String>, id: RelationId) -> Self {
        self.define_relation(name, id);
        self
    }

    pub fn with_relation_metadata(mut self, metadata: RelationMetadata) -> Self {
        self.define_relation_metadata(metadata);
        self
    }

    pub fn with_dot_relation(mut self, name: impl Into<String>, relation: RelationId) -> Self {
        self.define_dot_relation(name, relation);
        self
    }

    pub fn with_identity(mut self, name: impl Into<String>, id: Identity) -> Self {
        self.define_identity(name, id);
        self
    }

    pub fn with_program_identity(mut self, method: impl Into<String>, id: Identity) -> Self {
        self.define_program_identity(method, id);
        self
    }

    pub fn with_runtime_function(mut self, name: impl Into<String>) -> Self {
        self.define_runtime_function(name, BuiltinResultKind::Dynamic);
        self
    }

    pub fn with_runtime_function_result(
        mut self,
        name: impl Into<String>,
        result: BuiltinResultKind,
    ) -> Self {
        self.define_runtime_function(name, result);
        self
    }

    pub fn with_host_request_function(
        mut self,
        name: impl Into<String>,
        function: HostRequestFunction,
    ) -> Self {
        self.define_host_request_function(name, function);
        self
    }

    pub fn with_method_relations(mut self, method_relations: MethodRelations) -> Self {
        self.method_relations = Some(method_relations);
        self
    }

    pub fn define_relation(&mut self, name: impl Into<String>, id: RelationId) {
        self.relations.insert(name.into(), id);
    }

    pub fn define_relation_metadata(&mut self, metadata: RelationMetadata) {
        if let Some(name) = metadata.name().name() {
            self.define_relation(name, metadata.id());
        }
        self.relation_metadata.insert(metadata.id(), metadata);
    }

    pub fn define_dot_relation(&mut self, name: impl Into<String>, relation: RelationId) {
        self.dot_relations
            .insert(name.into(), DotRelation { relation });
    }

    pub fn define_identity(&mut self, name: impl Into<String>, id: Identity) {
        self.identities.insert(name.into(), id);
    }

    pub fn define_program_identity(&mut self, method: impl Into<String>, id: Identity) {
        self.program_identities.insert(method.into(), id);
    }

    pub fn define_runtime_function(&mut self, name: impl Into<String>, result: BuiltinResultKind) {
        self.runtime_functions.insert(name.into(), result);
    }

    pub fn define_type_alias(&mut self, alias: TypeAliasDefinition) {
        self.type_aliases.insert(alias.name.clone(), alias);
    }

    pub fn type_alias(&self, name: &str) -> Option<&TypeAliasDefinition> {
        self.type_aliases.get(name)
    }

    pub fn define_host_request_function(
        &mut self,
        name: impl Into<String>,
        function: HostRequestFunction,
    ) {
        let name = name.into();
        self.runtime_functions
            .insert(name.clone(), BuiltinResultKind::Dynamic);
        self.host_request_functions.insert(name, function);
    }

    pub fn relation(&self, name: &str) -> Option<RelationId> {
        self.relations.get(name).copied()
    }

    pub fn relation_metadata(&self, relation: RelationId) -> Option<&RelationMetadata> {
        self.relation_metadata.get(&relation)
    }

    pub fn dot_relation(&self, name: &str) -> Option<DotRelation> {
        self.dot_relations.get(name).copied()
    }

    pub fn identity(&self, name: &str) -> Option<Identity> {
        self.identities.get(name).copied()
    }

    pub fn program_identity(&self, method: &str) -> Option<Identity> {
        self.program_identities.get(method).copied()
    }

    pub fn is_runtime_function(&self, name: &str) -> bool {
        self.runtime_functions.contains_key(name)
    }

    pub fn runtime_function_result(&self, name: &str) -> Option<BuiltinResultKind> {
        self.runtime_functions.get(name).cloned()
    }

    pub fn host_request_function(&self, name: &str) -> Option<&HostRequestFunction> {
        self.host_request_functions.get(name)
    }

    pub fn method_relations(&self) -> Option<MethodRelations> {
        self.method_relations
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    Diagnostics {
        errors: Vec<CompileError>,
    },
    ParseErrors {
        errors: Vec<ParseError>,
    },
    SemanticDiagnostic {
        diagnostic: Diagnostic,
    },
    Unsupported {
        node: NodeId,
        span: Option<Span>,
        message: String,
    },
    UnknownRelation {
        node: NodeId,
        span: Option<Span>,
        name: String,
    },
    UnknownIdentity {
        node: NodeId,
        span: Option<Span>,
        name: String,
    },
    UnknownValue {
        node: NodeId,
        span: Option<Span>,
        name: String,
    },
    InvalidLiteral {
        node: NodeId,
        span: Option<Span>,
        message: String,
    },
    UnboundLocal {
        node: NodeId,
        span: Option<Span>,
        binding: BindingId,
    },
    ValueKindMismatch {
        node: NodeId,
        span: Option<Span>,
        subject: String,
        expected: ValueKind,
        inferred: String,
    },
    StaticTypeMismatch {
        node: NodeId,
        span: Option<Span>,
        subject: String,
        expected: String,
        inferred: String,
    },
    FunctionResultKindMismatch {
        node: NodeId,
        span: Option<Span>,
        function: Option<String>,
        expected: ValueKind,
        inferred: String,
    },
    VerbResultKindMismatch {
        node: NodeId,
        span: Option<Span>,
        selector: String,
        expected: ValueKind,
        inferred: String,
    },
    ParameterKindMismatch {
        node: NodeId,
        span: Option<Span>,
        parameter: String,
        expected: ValueKind,
        inferred: String,
    },
    ParameterDefaultKindMismatch {
        node: NodeId,
        span: Option<Span>,
        parameter: String,
        expected: ValueKind,
        inferred: String,
    },
    MissingOptionalParameterDefault {
        node: NodeId,
        span: Option<Span>,
        parameter: String,
    },
    InvalidRestParameterKind {
        node: NodeId,
        span: Option<Span>,
        parameter: String,
        declared: ValueKind,
    },
    Runtime(RuntimeError),
    Kernel(mica_relation_kernel::KernelError),
}

impl From<RuntimeError> for CompileError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<mica_relation_kernel::KernelError> for CompileError {
    fn from(value: mica_relation_kernel::KernelError) -> Self {
        Self::Kernel(value)
    }
}

fn return_context_errors(errors: Vec<CompileError>) -> Result<(), CompileError> {
    match errors.len() {
        0 => Ok(()),
        1 => Err(errors.into_iter().next().expect("one error exists")),
        _ => Err(CompileError::Diagnostics { errors }),
    }
}

/// Parses a float literal string as binary32, returning a Mica `Value`.
///
/// Overflow is reported specifically as "float literal overflows binary32".
/// Underflow to canonical zero is a successful conversion.
fn parse_float_literal(text: &str) -> Result<Value, String> {
    let value = text
        .parse::<f32>()
        .map_err(|error| format!("invalid float literal: {error}"))?;
    if value.is_infinite() {
        return Err("float literal overflows binary32".to_string());
    }
    Value::float(value).map_err(|error| format!("invalid float literal: {error:?}"))
}

fn context_errors(semantic: &SemanticProgram, context: &CompileContext) -> Vec<CompileError> {
    let mut validator = ContextValidator {
        semantic,
        context,
        errors: Vec::new(),
    };
    validator.validate_items(&semantic.hir.items);
    validator.errors
}

struct ContextValidator<'a> {
    semantic: &'a SemanticProgram,
    context: &'a CompileContext,
    errors: Vec<CompileError>,
}

impl<'a> ContextValidator<'a> {
    fn validate_items(&mut self, items: &[HirItem]) {
        for item in items {
            self.validate_item(item);
        }
    }

    fn validate_item(&mut self, item: &HirItem) {
        match item {
            HirItem::TypeAlias { .. } => {}
            HirItem::Expr { expr, .. } => self.validate_expr(expr, ExprUse::Value),
            HirItem::RelationRule { head, body, .. } => {
                self.validate_rule_atom(head);
                for item in body {
                    match item {
                        HirRuleBodyItem::Atom(atom) => {
                            self.validate_rule_atom(atom);
                        }
                        HirRuleBodyItem::Guard(guard) => {
                            self.validate_rule_term(&guard.left);
                            self.validate_rule_term(&guard.right);
                        }
                    }
                }
            }
            HirItem::Method {
                id,
                identity,
                params,
                body,
                ..
            } => {
                if let Some(identity) = identity {
                    self.validate_method_identity(*id, identity);
                    self.validate_method_program_identity(*id, identity);
                }
                for param in params {
                    if let Some(restriction) = &param.restriction {
                        self.validate_param_restriction(param.id, restriction);
                    }
                }
                self.validate_items(body);
            }
        }
    }

    fn validate_expr(&mut self, expr: &HirExpr, expr_use: ExprUse) {
        match expr {
            HirExpr::Literal { .. }
            | HirExpr::LocalRef { .. }
            | HirExpr::Symbol { .. }
            | HirExpr::QueryVar { .. }
            | HirExpr::Hole { .. }
            | HirExpr::Break { .. }
            | HirExpr::Continue { .. }
            | HirExpr::Error { .. } => {}
            HirExpr::ExternalRef { id, name } => {
                if expr_use == ExprUse::Value
                    && !is_compiler_builtin(name)
                    && !self.context.is_runtime_function(name)
                {
                    self.errors.push(CompileError::UnknownValue {
                        node: *id,
                        span: self.span(*id),
                        name: name.clone(),
                    });
                }
            }
            HirExpr::Identity { id, name } => self.validate_identity(*id, name),
            HirExpr::Frob {
                id,
                delegate,
                value,
            } => {
                self.validate_identity(*id, delegate);
                self.validate_expr(value, ExprUse::Value);
            }
            HirExpr::List { items, .. } => {
                for item in items {
                    match item {
                        HirCollectionItem::Expr(expr) | HirCollectionItem::Splice(expr) => {
                            self.validate_expr(expr, ExprUse::Value);
                        }
                    }
                }
            }
            HirExpr::Relation { rows, .. } => {
                for expr in rows.iter().flatten() {
                    self.validate_expr(expr, ExprUse::Value);
                }
            }
            HirExpr::Map { entries, .. } => {
                for (key, value) in entries {
                    self.validate_expr(key, ExprUse::Value);
                    self.validate_expr(value, ExprUse::Value);
                }
            }
            HirExpr::Unary { expr, .. } => {
                self.validate_expr(expr, ExprUse::Value);
            }
            HirExpr::Binary { left, right, .. } => {
                self.validate_expr(left, ExprUse::Value);
                self.validate_expr(right, ExprUse::Value);
            }
            HirExpr::Assign { target, value, .. } => {
                self.validate_place(target);
                self.validate_expr(value, ExprUse::Value);
            }
            HirExpr::Call { callee, args, .. } => {
                self.validate_expr(callee, ExprUse::Callee);
                self.validate_args(args);
            }
            HirExpr::RoleDispatch { selector, args, .. } => {
                self.validate_expr(selector, ExprUse::Value);
                self.validate_args(args);
            }
            HirExpr::ReceiverDispatch {
                receiver,
                selector,
                args,
                ..
            } => {
                self.validate_expr(receiver, ExprUse::Value);
                self.validate_expr(selector, ExprUse::Value);
                self.validate_args(args);
            }
            HirExpr::Spawn { target, delay, .. } => {
                self.validate_expr(target, ExprUse::Value);
                if let Some(delay) = delay {
                    self.validate_expr(delay, ExprUse::Value);
                }
            }
            HirExpr::RelationAtom(atom) => self.validate_relation_atom(atom),
            HirExpr::FactChange { atom, .. } => self.validate_relation_atom(atom),
            HirExpr::Require { condition, .. }
            | HirExpr::One {
                expr: condition, ..
            } => {
                self.validate_expr(condition, ExprUse::Value);
            }
            HirExpr::Index {
                collection, index, ..
            } => {
                self.validate_expr(collection, ExprUse::Value);
                if let Some(index) = index {
                    self.validate_expr(index, ExprUse::Value);
                }
            }
            HirExpr::Field { base, .. } => {
                self.validate_expr(base, ExprUse::Value);
            }
            HirExpr::Binding { scatter, value, .. } => {
                for binding in scatter {
                    if let Some(default) = &binding.default {
                        self.validate_expr(default, ExprUse::Value);
                    }
                }
                if let Some(value) = value {
                    self.validate_expr(value, ExprUse::Value);
                }
            }
            HirExpr::If {
                condition,
                then_items,
                elseif,
                else_items,
                ..
            } => {
                self.validate_expr(condition, ExprUse::Value);
                self.validate_items(then_items);
                for (condition, items) in elseif {
                    self.validate_expr(condition, ExprUse::Value);
                    self.validate_items(items);
                }
                self.validate_items(else_items);
            }
            HirExpr::Match { value, cases, .. } => {
                self.validate_expr(value, ExprUse::Value);
                for case in cases {
                    if let Some(guard) = &case.guard {
                        self.validate_expr(guard, ExprUse::Value);
                    }
                    self.validate_items(&case.body);
                }
            }
            HirExpr::Block { items, .. } => self.validate_items(items),
            HirExpr::For { iter, body, .. } => {
                self.validate_expr(iter, ExprUse::Value);
                self.validate_items(body);
            }
            HirExpr::While {
                condition, body, ..
            } => {
                self.validate_expr(condition, ExprUse::Value);
                self.validate_items(body);
            }
            HirExpr::Return { value, .. } => {
                if let Some(value) = value {
                    self.validate_expr(value, ExprUse::Value);
                }
            }
            HirExpr::Raise {
                error,
                message,
                value,
                ..
            } => {
                self.validate_expr(error, ExprUse::Value);
                if let Some(message) = message {
                    self.validate_expr(message, ExprUse::Value);
                }
                if let Some(value) = value {
                    self.validate_expr(value, ExprUse::Value);
                }
            }
            HirExpr::Recover { expr, catches, .. } => {
                self.validate_expr(expr, ExprUse::Value);
                for catch in catches {
                    if let Some(condition) = &catch.condition {
                        self.validate_expr(condition, ExprUse::Value);
                    }
                    self.validate_expr(&catch.value, ExprUse::Value);
                }
            }
            HirExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                self.validate_items(body);
                for catch in catches {
                    if let Some(condition) = &catch.condition {
                        self.validate_expr(condition, ExprUse::Value);
                    }
                    self.validate_items(&catch.body);
                }
                self.validate_items(finally);
            }
            HirExpr::Function { params, body, .. } => {
                for param in params {
                    if let Some(default) = &param.default {
                        self.validate_expr(default, ExprUse::Value);
                    }
                }
                match body {
                    HirFunctionBody::Expr(expr) => {
                        self.validate_expr(expr, ExprUse::Value);
                    }
                    HirFunctionBody::Block(items) => self.validate_items(items),
                }
            }
        }
    }

    fn validate_args(&mut self, args: &[HirArg]) {
        for arg in args {
            self.validate_expr(&arg.value, ExprUse::Value);
        }
    }

    fn validate_place(&mut self, place: &HirPlace) {
        match place {
            HirPlace::Local { .. } | HirPlace::Invalid { .. } => {}
            HirPlace::Index {
                collection, index, ..
            } => {
                self.validate_expr(collection, ExprUse::Value);
                if let Some(index) = index {
                    self.validate_expr(index, ExprUse::Value);
                }
            }
            HirPlace::Dot { base, .. } => {
                self.validate_expr(base, ExprUse::Value);
            }
        }
    }

    fn validate_relation_atom(&mut self, atom: &HirRelationAtom) {
        if self.context.relation(&atom.name).is_none() {
            self.errors.push(CompileError::UnknownRelation {
                node: atom.id,
                span: self.span(atom.id),
                name: atom.name.clone(),
            });
        }
        self.validate_args(&atom.args);
    }

    fn validate_rule_atom(&mut self, atom: &HirRelationAtom) {
        if self.context.relation(&atom.name).is_none() {
            self.errors.push(CompileError::UnknownRelation {
                node: atom.id,
                span: self.span(atom.id),
                name: atom.name.clone(),
            });
        }
        for arg in &atom.args {
            self.validate_rule_term(&arg.value);
        }
    }

    fn validate_rule_term(&mut self, expr: &HirExpr) {
        match expr {
            HirExpr::ExternalRef { .. }
            | HirExpr::QueryVar { .. }
            | HirExpr::Symbol { .. }
            | HirExpr::Literal { .. } => {}
            HirExpr::Identity { id, name } => self.validate_identity(*id, name),
            _ => {}
        }
    }

    fn validate_identity(&mut self, id: NodeId, name: &str) {
        if self.context.identity(name).is_none() {
            self.errors.push(CompileError::UnknownIdentity {
                node: id,
                span: self.span(id),
                name: name.to_owned(),
            });
        }
    }

    fn validate_method_identity(&mut self, id: NodeId, name: &str) {
        if self.context.identity(name).is_none() {
            self.errors.push(CompileError::UnknownIdentity {
                node: id,
                span: self.span(id),
                name: name.to_owned(),
            });
        }
    }

    fn validate_method_program_identity(&mut self, id: NodeId, name: &str) {
        if self.context.program_identity(name).is_none() {
            self.errors.push(CompileError::UnknownIdentity {
                node: id,
                span: self.span(id),
                name: format!("{name} program"),
            });
        }
    }

    fn validate_param_restriction(&mut self, id: NodeId, restriction: &DispatchRestriction) {
        if self.context.identity(&restriction.prototype).is_none() {
            self.errors.push(CompileError::UnknownIdentity {
                node: id,
                span: Some(restriction.span.clone()),
                name: restriction.prototype.clone(),
            });
        }
    }

    fn span(&self, node: NodeId) -> Option<Span> {
        self.semantic.span(node).cloned()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExprUse {
    Value,
    Callee,
}

fn is_compiler_builtin(name: &str) -> bool {
    matches!(
        name,
        "commit"
            | "suspend"
            | "read"
            | "mailbox_recv"
            | "external_request"
            | "invoke"
            | "none"
            | "some"
            | "ok"
            | "err"
    )
}

fn compile_rule_item(
    semantic: &SemanticProgram,
    context: &CompileContext,
    item: &HirItem,
) -> Result<Rule, CompileError> {
    let HirItem::RelationRule { id, head, body } = item else {
        return Err(CompileError::Unsupported {
            node: item_id(item),
            span: semantic.span(item_id(item)).cloned(),
            message: "only relation rules can be installed as rules".to_owned(),
        });
    };
    let head_relation =
        context
            .relation(&head.name)
            .ok_or_else(|| CompileError::UnknownRelation {
                node: head.id,
                span: semantic.span(head.id).cloned(),
                name: head.name.clone(),
            })?;
    let head_terms = compile_rule_terms(semantic, context, &head.args)?;
    let body = body
        .iter()
        .map(|item| {
            Ok::<_, CompileError>(match item {
                HirRuleBodyItem::Atom(atom) => compile_rule_atom(semantic, context, atom)?.into(),
                HirRuleBodyItem::Guard(guard) => compile_rule_guard(semantic, context, guard)?,
            })
        })
        .collect::<Result<Vec<RuleBodyItem>, CompileError>>()?;
    if body.is_empty() {
        return Err(CompileError::Unsupported {
            node: *id,
            span: semantic.span(*id).cloned(),
            message: "relation rules require at least one body atom".to_owned(),
        });
    }
    Ok(Rule::new(head_relation, head_terms, body))
}

fn compile_rule_atom(
    semantic: &SemanticProgram,
    context: &CompileContext,
    atom: &HirRelationAtom,
) -> Result<Atom, CompileError> {
    let relation = context
        .relation(&atom.name)
        .ok_or_else(|| CompileError::UnknownRelation {
            node: atom.id,
            span: semantic.span(atom.id).cloned(),
            name: atom.name.clone(),
        })?;
    let terms = compile_rule_terms(semantic, context, &atom.args)?;
    Ok(if atom.negated {
        Atom::negated(relation, terms)
    } else {
        Atom::positive(relation, terms)
    })
}

fn compile_rule_guard(
    semantic: &SemanticProgram,
    context: &CompileContext,
    guard: &HirRuleGuard,
) -> Result<RuleBodyItem, CompileError> {
    let op = match guard.op {
        BinaryOp::Eq => RuleComparisonOp::Eq,
        BinaryOp::Ne => RuleComparisonOp::Ne,
        BinaryOp::Lt => RuleComparisonOp::Lt,
        BinaryOp::Le => RuleComparisonOp::Le,
        BinaryOp::Gt => RuleComparisonOp::Gt,
        BinaryOp::Ge => RuleComparisonOp::Ge,
        _ => {
            return Err(CompileError::Unsupported {
                node: guard.id,
                span: semantic.span(guard.id).cloned(),
                message: "relation rule guards only support comparisons".to_owned(),
            });
        }
    };
    Ok(RuleGuard::new(
        op,
        compile_rule_term(semantic, context, &guard.left)?,
        compile_rule_term(semantic, context, &guard.right)?,
    )
    .into())
}

fn compile_rule_terms(
    semantic: &SemanticProgram,
    context: &CompileContext,
    args: &[HirArg],
) -> Result<Vec<Term>, CompileError> {
    args.iter()
        .map(|arg| {
            if arg.role.is_some() || arg.splice {
                return Err(CompileError::Unsupported {
                    node: arg.id,
                    span: semantic.span(arg.id).cloned(),
                    message: "relation rule atoms do not support named or spliced arguments"
                        .to_owned(),
                });
            }
            compile_rule_term(semantic, context, &arg.value)
        })
        .collect()
}

fn compile_rule_term(
    semantic: &SemanticProgram,
    context: &CompileContext,
    expr: &HirExpr,
) -> Result<Term, CompileError> {
    match expr {
        HirExpr::ExternalRef { name, .. } | HirExpr::QueryVar { name, .. } => {
            Ok(Term::Var(Symbol::intern(name)))
        }
        HirExpr::Identity { id, name } => context
            .identity(name)
            .ok_or_else(|| CompileError::UnknownIdentity {
                node: *id,
                span: semantic.span(*id).cloned(),
                name: name.clone(),
            })
            .map(|identity| Term::Value(Value::identity(identity))),
        HirExpr::Symbol { name, .. } => Ok(Term::Value(Value::symbol(Symbol::intern(name)))),
        HirExpr::Literal { id, value } => {
            literal_value_for_rule(semantic, *id, value).map(Term::Value)
        }
        _ => Err(CompileError::Unsupported {
            node: expr_id(expr),
            span: semantic.span(expr_id(expr)).cloned(),
            message: "relation rule terms must be variables or literal values".to_owned(),
        }),
    }
}

fn literal_value_for_rule(
    semantic: &SemanticProgram,
    id: NodeId,
    literal: &Literal,
) -> Result<Value, CompileError> {
    match literal {
        Literal::Int(value) => {
            let value = value
                .parse::<i64>()
                .map_err(|error| CompileError::InvalidLiteral {
                    node: id,
                    span: semantic.span(id).cloned(),
                    message: format!("invalid integer literal: {error}"),
                })?;
            Value::int(value).map_err(|error| CompileError::InvalidLiteral {
                node: id,
                span: semantic.span(id).cloned(),
                message: format!("{error:?}"),
            })
        }
        Literal::Float(value) => {
            let value =
                parse_float_literal(value).map_err(|message| CompileError::InvalidLiteral {
                    node: id,
                    span: semantic.span(id).cloned(),
                    message,
                })?;
            Ok(value)
        }
        Literal::String(value) => Ok(Value::string(value)),
        Literal::Bytes(value) => Ok(Value::bytes(value)),
        Literal::Bool(value) => Ok(Value::bool(*value)),
        Literal::ErrorCode(value) => Ok(Value::error_code(Symbol::intern(value))),
        Literal::Nothing => Ok(Value::nothing()),
        Literal::Unit => Ok(Value::unit()),
    }
}

fn compile_installed_method(
    semantic: &SemanticProgram,
    context: &CompileContext,
    item: &HirItem,
) -> Result<InstalledMethod, CompileError> {
    let HirItem::Method {
        id,
        identity,
        selector,
        params: hir_params,
        result_type,
        result_kind,
        body,
        ..
    } = item
    else {
        return Err(CompileError::Unsupported {
            node: item_id(item),
            span: semantic.span(item_id(item)).cloned(),
            message: "only method items can be installed as methods".to_owned(),
        });
    };
    let identity_name = identity.as_ref().ok_or_else(|| CompileError::Unsupported {
        node: *id,
        span: semantic.span(*id).cloned(),
        message: "method installation requires an explicit method identity".to_owned(),
    })?;
    let selector = selector.as_ref().ok_or_else(|| CompileError::Unsupported {
        node: *id,
        span: semantic.span(*id).cloned(),
        message: "method installation requires an explicit selector".to_owned(),
    })?;
    let method = context
        .identity(identity_name)
        .ok_or_else(|| CompileError::UnknownIdentity {
            node: *id,
            span: semantic.span(*id).cloned(),
            name: identity_name.clone(),
        })
        .map(Value::identity)?;
    let program_id = context
        .program_identity(identity_name)
        .ok_or_else(|| CompileError::UnknownIdentity {
            node: *id,
            span: semantic.span(*id).cloned(),
            name: format!("{identity_name} program"),
        })
        .map(Value::identity)?;
    let params = compile_installed_params(*id, semantic, context, hir_params)?;
    let param_registers =
        u16::try_from(hir_params.len()).map_err(|_| CompileError::Unsupported {
            node: *id,
            span: semantic.span(*id).cloned(),
            message: "method parameter count exceeds supported limit".to_owned(),
        })?;

    if let Some(expected) = result_kind {
        let no_direct_result = |_| None;
        let runtime_result = |name: &str| runtime_result_kinds(context, name);
        let inferred_result =
            KindInference::new(&semantic.bindings, &no_direct_result, &runtime_result)
                .block_result(body);
        if !inferred_result.is_subset(KindSet::exact(*expected)) {
            return Err(CompileError::VerbResultKindMismatch {
                node: *id,
                span: semantic.span(*id).cloned(),
                selector: selector.clone(),
                expected: *expected,
                inferred: inferred_result.names(),
            });
        }
    }
    if let Some(expected_type) = result_type
        && !matches!(expected_type, StaticType::Kind(_))
    {
        let inferred_type = ProgramCompiler::new(semantic, context)
            .infer_simple_function_result_type(&HirFunctionBody::Block(body.clone()));
        if !inferred_type.is_subtype_of(expected_type) {
            return Err(CompileError::StaticTypeMismatch {
                node: *id,
                span: semantic.span(*id).cloned(),
                subject: format!("result of verb `{selector}`"),
                expected: expected_type.to_string(),
                inferred: inferred_type.to_string(),
            });
        }
    }

    let mut compiler = ProgramCompiler::new(semantic, context);
    compiler.next_register = param_registers;
    for (idx, param) in hir_params.iter().enumerate() {
        let register = Register(idx as u16);
        compiler.locals.insert(param.binding, register);
        compiler.emit_method_parameter_check(param, register);
    }
    let compiled_program = compiler.compile_items(body)?;
    Ok(InstalledMethod {
        method: method.clone(),
        program: program_id,
        selector: Value::symbol(Symbol::intern(selector)),
        params,
        compiled: CompiledProgram {
            semantic: semantic.clone(),
            program: compiled_program,
        },
    })
}

fn compile_installed_params(
    id: NodeId,
    semantic: &SemanticProgram,
    context: &CompileContext,
    params: &[HirMethodParam],
) -> Result<Vec<InstalledParam>, CompileError> {
    params
        .iter()
        .enumerate()
        .map(|(position, param)| {
            let binding = &semantic.bindings[param.binding.as_u32() as usize];
            let restriction = match &param.restriction {
                Some(restriction) => compile_param_restriction(param.id, context, restriction)?,
                None => Value::nothing(),
            };
            Ok(InstalledParam {
                name: binding.name.clone(),
                role: Value::symbol(Symbol::intern(&binding.name)),
                restriction,
                position: u16::try_from(position).map_err(|_| CompileError::Unsupported {
                    node: id,
                    span: semantic.span(id).cloned(),
                    message: "method parameter count exceeds supported limit".to_owned(),
                })?,
            })
        })
        .collect()
}

fn compile_param_restriction(
    id: NodeId,
    context: &CompileContext,
    restriction: &DispatchRestriction,
) -> Result<Value, CompileError> {
    let identity =
        context
            .identity(&restriction.prototype)
            .ok_or_else(|| CompileError::UnknownIdentity {
                node: id,
                span: Some(restriction.span.clone()),
                name: restriction.prototype.clone(),
            })?;
    if restriction.frob_only {
        Ok(Value::frob(identity, Value::nothing()))
    } else {
        Ok(Value::identity(identity))
    }
}

fn item_id(item: &HirItem) -> NodeId {
    match item {
        HirItem::Expr { id, .. }
        | HirItem::RelationRule { id, .. }
        | HirItem::Method { id, .. }
        | HirItem::TypeAlias { id, .. } => *id,
    }
}

fn runtime_binary_op(op: BinaryOp) -> Option<RuntimeBinaryOp> {
    Some(match op {
        BinaryOp::Eq => RuntimeBinaryOp::Eq,
        BinaryOp::Ne => RuntimeBinaryOp::Ne,
        BinaryOp::Lt => RuntimeBinaryOp::Lt,
        BinaryOp::Le => RuntimeBinaryOp::Le,
        BinaryOp::Gt => RuntimeBinaryOp::Gt,
        BinaryOp::Ge => RuntimeBinaryOp::Ge,
        BinaryOp::Add => RuntimeBinaryOp::Add,
        BinaryOp::Sub => RuntimeBinaryOp::Sub,
        BinaryOp::Mul => RuntimeBinaryOp::Mul,
        BinaryOp::Div => RuntimeBinaryOp::Div,
        BinaryOp::Rem => RuntimeBinaryOp::Rem,
        BinaryOp::And | BinaryOp::Or | BinaryOp::Range => return None,
    })
}

fn builtin_error_field(name: &str) -> Option<ErrorField> {
    Some(match name {
        "code" => ErrorField::Code,
        "message" => ErrorField::Message,
        "value" => ErrorField::Value,
        _ => return None,
    })
}

fn query_outputs(args: &[HirArg]) -> Vec<QueryBinding> {
    args.iter()
        .enumerate()
        .filter_map(|(position, arg)| match &arg.value {
            HirExpr::QueryVar { name, .. } => Some(QueryBinding {
                name: Symbol::intern(name),
                position: position as u16,
            }),
            _ => None,
        })
        .collect()
}

fn relation_name_for_dot(name: &str) -> String {
    let Some((namespace, leaf)) = name.rsplit_once('/') else {
        return uppercase_initial(name);
    };
    format!("{}/{}", namespace, uppercase_initial(leaf))
}

fn uppercase_initial(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_ascii_uppercase().to_string() + chars.as_str()
}

fn internal_bytecode_error(error: RuntimeError) -> CompileError {
    CompileError::Unsupported {
        node: NodeId(0),
        span: None,
        message: format!("internal compiler error: {error:?}"),
    }
}

fn is_static_catch_condition(condition: Option<&HirExpr>) -> bool {
    matches!(
        condition,
        None | Some(HirExpr::Literal {
            value: Literal::ErrorCode(_),
            ..
        })
    )
}

struct ProgramCompiler<'a> {
    semantic: &'a SemanticProgram,
    context: &'a CompileContext,
    instructions: ProgramBuilder,
    next_register: u16,
    locals: HashMap<BindingId, Register>,
    local_types: HashMap<BindingId, StaticType>,
    functions: HashMap<BindingId, FunctionInfo>,
    loops: Vec<LoopContext>,
    returned: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FunctionInfo {
    program: Arc<Program>,
    params: Vec<FunctionParamInfo>,
    captures: Vec<BindingId>,
    min_arity: usize,
    max_arity: Option<usize>,
    result_kinds: KindSet,
    result_type: StaticType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FunctionParamInfo {
    id: NodeId,
    binding: BindingId,
    kind: LocalKind,
    declared_type: Option<StaticType>,
    declared_kind: Option<ValueKind>,
    default: Option<HirExpr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopContext {
    continue_target: usize,
    break_jumps: Vec<usize>,
    continue_jumps: Vec<usize>,
}

struct RelationPatternSpec {
    heading: Vec<Symbol>,
    row_count: u16,
    equalities: Vec<(Symbol, Value)>,
    bindings: Vec<(Symbol, BindingId)>,
}

impl<'a> ProgramCompiler<'a> {
    fn new(semantic: &'a SemanticProgram, context: &'a CompileContext) -> Self {
        Self {
            semantic,
            context,
            instructions: ProgramBuilder::new(),
            next_register: 0,
            locals: HashMap::new(),
            local_types: HashMap::new(),
            functions: HashMap::new(),
            loops: Vec::new(),
            returned: false,
        }
    }

    fn compile_program(self, program: &HirProgram) -> Result<Program, CompileError> {
        self.compile_items(&program.items)
    }

    fn compile_items(mut self, items: &[HirItem]) -> Result<Program, CompileError> {
        let mut last_value = None;
        for item in items {
            last_value = self.compile_item(item)?;
        }
        if !self.returned {
            let value = last_value
                .map(Operand::Register)
                .unwrap_or_else(|| Operand::Value(Value::unit()));
            self.emit(Instruction::Return { value });
        }
        self.instructions
            .finish(self.next_register as usize)
            .map_err(Into::into)
    }

    fn compile_item(&mut self, item: &HirItem) -> Result<Option<Register>, CompileError> {
        if self.returned {
            return Ok(None);
        }

        match item {
            HirItem::TypeAlias { .. } => Ok(None),
            HirItem::Expr { expr, .. } => self.compile_expr_for_value(expr).map(Some),
            HirItem::RelationRule { id, .. } => Err(self.unsupported(
                *id,
                "relation rules are compile-time database definitions, not executable task code yet",
            )),
            HirItem::Method { id, .. } => Err(self.unsupported(
                *id,
                "method fileout declarations are not executable task code yet",
            )),
        }
    }

    fn compile_expr_for_value(&mut self, expr: &HirExpr) -> Result<Register, CompileError> {
        match expr {
            HirExpr::Literal { id, value } => {
                let dst = self.alloc_register();
                self.emit(Instruction::Load {
                    dst,
                    value: self.literal_value(*id, value)?,
                });
                Ok(dst)
            }
            HirExpr::Identity { id, name } => {
                let identity =
                    self.context
                        .identity(name)
                        .ok_or_else(|| CompileError::UnknownIdentity {
                            node: *id,
                            span: self.span(*id),
                            name: name.clone(),
                        })?;
                let dst = self.alloc_register();
                self.emit(Instruction::Load {
                    dst,
                    value: Value::identity(identity),
                });
                Ok(dst)
            }
            HirExpr::Frob {
                id,
                delegate,
                value,
            } => self.compile_frob(*id, delegate, value),
            HirExpr::Symbol { name, .. } => {
                let dst = self.alloc_register();
                self.emit(Instruction::Load {
                    dst,
                    value: Value::symbol(Symbol::intern(name)),
                });
                Ok(dst)
            }
            HirExpr::QueryVar { id, .. } => Err(self.unsupported(
                *id,
                "query variables are only valid inside relation queries",
            )),
            HirExpr::LocalRef { id, binding } => {
                self.locals
                    .get(binding)
                    .copied()
                    .ok_or_else(|| CompileError::UnboundLocal {
                        node: *id,
                        span: self.span(*id),
                        binding: *binding,
                    })
            }
            HirExpr::Binding {
                binding,
                scatter,
                row,
                value,
                id,
                ..
            } => {
                if let Some(row) = row {
                    return self.compile_exact_row_binding(*id, row, value.as_deref());
                }
                if !scatter.is_empty() {
                    return self.compile_scatter_binding(*id, scatter, value.as_deref());
                }
                if let (Some(binding), Some(value)) = (*binding, value.as_deref())
                    && let HirExpr::Function { name: None, .. } = value
                {
                    let function = self.compile_function(value)?;
                    if function.captures.is_empty() {
                        self.functions.insert(binding, function.clone());
                    }
                    let dst = self.emit_function_value(*id, function)?;
                    self.enforce_binding_kind(*id, binding, value, dst)?;
                    self.locals.insert(binding, dst);
                    self.record_local_type(binding, value);
                    return Ok(dst);
                }
                let dst = match value {
                    Some(value) => self.compile_expr_for_value(value)?,
                    None => {
                        let dst = self.alloc_register();
                        self.emit(Instruction::Load {
                            dst,
                            value: Value::nothing(),
                        });
                        dst
                    }
                };
                if let Some(binding) = binding {
                    if let Some(value) = value.as_deref() {
                        self.enforce_binding_kind(*id, *binding, value, dst)?;
                        self.record_local_type(*binding, value);
                    }
                    self.locals.insert(*binding, dst);
                } else {
                    return Err(self.unsupported(
                        *id,
                        "scatter assignment lowering is not implemented in the task compiler yet",
                    ));
                }
                Ok(dst)
            }
            HirExpr::Assign { id, target, value } => {
                let value_expr = value.as_ref();
                let value = self.compile_expr_for_value(value_expr)?;
                match target {
                    HirPlace::Local { binding, .. } => {
                        let dst = self.locals.get(binding).copied().ok_or_else(|| {
                            CompileError::UnboundLocal {
                                node: *id,
                                span: self.span(*id),
                                binding: *binding,
                            }
                        })?;
                        self.enforce_binding_kind(*id, *binding, value_expr, value)?;
                        self.emit(Instruction::Move { dst, src: value });
                        Ok(dst)
                    }
                    HirPlace::Index {
                        collection, index, ..
                    } => self.compile_index_assignment(*id, collection, index.as_deref(), value),
                    HirPlace::Dot { base, name, .. } => {
                        self.compile_dot_assignment(*id, base, name, value)
                    }
                    _ => Err(self.unsupported(
                        *id,
                        "only local, indexed local, and declared dot assignment are implemented in the task compiler yet",
                    )),
                }
            }
            HirExpr::List { id, items } => self.compile_list(*id, items),
            HirExpr::Relation { id, heading, rows } => self.compile_relation(*id, heading, rows),
            HirExpr::Map { entries, .. } => self.compile_map(entries),
            HirExpr::Index {
                id,
                collection,
                index,
            } => self.compile_index(*id, collection, index.as_deref()),
            HirExpr::Field { id, base, name } => self.compile_dot_read(*id, base, name),
            HirExpr::Unary { id, op, expr } => self.compile_unary(*id, *op, expr),
            HirExpr::Binary {
                id,
                op,
                left,
                right,
            } => self.compile_binary(*id, *op, left, right),
            HirExpr::RelationAtom(atom) => self.compile_relation_exists(atom),
            HirExpr::FactChange { kind, atom, .. } => {
                self.compile_fact_change(kind, atom)?;
                let dst = self.alloc_register();
                self.emit(Instruction::Load {
                    dst,
                    value: Value::unit(),
                });
                Ok(dst)
            }
            HirExpr::Require { condition, id } => {
                let condition = self.compile_expr_for_value(condition)?;
                let branch = self.instructions.len();
                self.emit(Instruction::Branch {
                    condition,
                    if_true: branch + 2,
                    if_false: branch + 1,
                });
                self.emit(Instruction::Abort {
                    error: Operand::Value(Value::string("require failed")),
                });
                let dst = self.alloc_register();
                self.emit(Instruction::Load {
                    dst,
                    value: Value::bool(true),
                });
                if self.instructions.len() <= branch + 2 {
                    return Err(self.unsupported(*id, "invalid require branch layout"));
                }
                Ok(dst)
            }
            HirExpr::Return { value, .. } => {
                let value = match value {
                    Some(value) => Operand::Register(self.compile_expr_for_value(value)?),
                    None => Operand::Value(Value::unit()),
                };
                self.emit(Instruction::Return { value });
                self.returned = true;
                let dst = self.alloc_register();
                self.emit(Instruction::Load {
                    dst,
                    value: Value::unit(),
                });
                Ok(dst)
            }
            HirExpr::Raise {
                error,
                message,
                value,
                ..
            } => self.compile_raise(error, message.as_deref(), value.as_deref()),
            HirExpr::Recover { id, expr, catches } => self.compile_recover(*id, expr, catches),
            HirExpr::One { expr, .. } => self.compile_one(expr),
            HirExpr::Block { items, .. } => {
                let saved = self.locals.clone();
                let mut last_value = None;
                for item in items {
                    last_value = self.compile_item(item)?;
                }
                self.locals = saved;
                Ok(last_value.unwrap_or_else(|| {
                    let dst = self.alloc_register();
                    self.emit(Instruction::Load {
                        dst,
                        value: Value::unit(),
                    });
                    dst
                }))
            }
            HirExpr::If {
                id,
                condition,
                then_items,
                elseif,
                else_items,
            } => self.compile_if(*id, condition, then_items, elseif, else_items),
            HirExpr::Match { id, value, cases } => self.compile_match(*id, value, cases),
            HirExpr::Call { id, callee, args } => self.compile_call(*id, callee, args),
            HirExpr::While {
                id,
                condition,
                body,
            } => self.compile_while(*id, condition, body),
            HirExpr::For {
                id,
                key,
                value,
                row,
                iter,
                body,
                ..
            } => self.compile_for(*id, key, value.as_ref(), row.as_deref(), iter, body),
            HirExpr::Break { id } => self.compile_break(*id),
            HirExpr::Continue { id } => self.compile_continue(*id),
            HirExpr::Try {
                id,
                body,
                catches,
                finally,
            } => self.compile_try(*id, body, catches, finally),
            HirExpr::Function { id, .. } => {
                let function = self.compile_function(expr)?;
                let HirExpr::Function { name, .. } = expr else {
                    unreachable!();
                };
                if let Some(binding) = name {
                    if function.captures.is_empty() {
                        self.functions.insert(*binding, function);
                        let dst = self.alloc_register();
                        self.emit(Instruction::Load {
                            dst,
                            value: Value::unit(),
                        });
                        Ok(dst)
                    } else {
                        let dst = self.emit_function_value(*id, function)?;
                        self.locals.insert(*binding, dst);
                        Ok(dst)
                    }
                } else {
                    self.emit_function_value(*id, function)
                }
            }
            HirExpr::RoleDispatch { id, selector, args } => {
                self.compile_dispatch(*id, selector, args, None)
            }
            HirExpr::ReceiverDispatch {
                id,
                receiver,
                selector,
                args,
            } => {
                if args.iter().all(|arg| arg.role.is_none()) {
                    self.compile_receiver_positional_dispatch(*id, receiver, selector, args)
                } else {
                    self.compile_dispatch(*id, selector, args, Some(receiver))
                }
            }
            HirExpr::Spawn { id, target, delay } => {
                self.compile_spawn(*id, target, delay.as_deref())
            }
            HirExpr::ExternalRef { id, name } => {
                if name == "none" {
                    return self.compile_none();
                }
                if is_compiler_builtin(name) || self.context.is_runtime_function(name) {
                    Err(CompileError::Unsupported {
                        node: *id,
                        span: self.span(*id),
                        message: format!(
                            "runtime function `{name}` is not callable from compiled tasks yet"
                        ),
                    })
                } else {
                    Err(CompileError::UnknownValue {
                        node: *id,
                        span: self.span(*id),
                        name: name.clone(),
                    })
                }
            }
            HirExpr::Error { id } => {
                Err(self.unsupported(*id, "cannot compile erroneous HIR node"))
            }
            _ => Err(self.unsupported(
                expr_id(expr),
                "HIR form is not implemented in the task compiler yet",
            )),
        }
    }

    fn compile_unary(
        &mut self,
        _id: NodeId,
        op: UnaryOp,
        expr: &HirExpr,
    ) -> Result<Register, CompileError> {
        let op = match op {
            UnaryOp::Not => RuntimeUnaryOp::Not,
            UnaryOp::Neg => RuntimeUnaryOp::Neg,
        };
        let src = self.compile_expr_for_value(expr)?;
        let dst = self.alloc_register();
        self.emit(Instruction::Unary { dst, op, src });
        Ok(dst)
    }

    fn compile_binary(
        &mut self,
        id: NodeId,
        op: BinaryOp,
        left: &HirExpr,
        right: &HirExpr,
    ) -> Result<Register, CompileError> {
        match op {
            BinaryOp::And => self.compile_and(left, right),
            BinaryOp::Or => self.compile_or(left, right),
            BinaryOp::Range => self.compile_range(left, right),
            _ => {
                let Some(op) = runtime_binary_op(op) else {
                    return Err(self.unsupported(
                        id,
                        "binary operator is not implemented in the task compiler yet",
                    ));
                };
                let left = self.compile_expr_for_value(left)?;
                let right = self.compile_expr_for_value(right)?;
                let dst = self.alloc_register();
                self.emit(Instruction::Binary {
                    dst,
                    op,
                    left,
                    right,
                });
                Ok(dst)
            }
        }
    }

    fn compile_frob(
        &mut self,
        id: NodeId,
        delegate: &str,
        value: &HirExpr,
    ) -> Result<Register, CompileError> {
        let delegate =
            self.context
                .identity(delegate)
                .ok_or_else(|| CompileError::UnknownIdentity {
                    node: id,
                    span: self.span(id),
                    name: delegate.to_owned(),
                })?;
        let value = self.compile_expr_for_operand(value)?;
        let dst = self.alloc_register();
        self.emit(Instruction::BuiltinCall {
            dst,
            name: Symbol::intern("frob"),
            result_kind: Some(ValueKind::Frob),
            args: vec![Operand::Value(Value::identity(delegate)), value],
        });
        Ok(dst)
    }

    fn compile_range(&mut self, left: &HirExpr, right: &HirExpr) -> Result<Register, CompileError> {
        let start = self.compile_expr_for_operand(left)?;
        let end = match right {
            HirExpr::Hole { .. } => None,
            _ => Some(self.compile_expr_for_operand(right)?),
        };
        let dst = self.alloc_register();
        self.emit(Instruction::BuildRange { dst, start, end });
        Ok(dst)
    }

    fn compile_list(
        &mut self,
        _id: NodeId,
        items: &[HirCollectionItem],
    ) -> Result<Register, CompileError> {
        let mut operands = Vec::with_capacity(items.len());
        for item in items {
            match item {
                HirCollectionItem::Expr(expr) => {
                    operands.push(ListItem::Value(self.compile_expr_for_operand(expr)?));
                }
                HirCollectionItem::Splice(expr) => {
                    operands.push(ListItem::Splice(self.compile_expr_for_operand(expr)?));
                }
            }
        }
        let dst = self.alloc_register();
        self.emit(Instruction::BuildList {
            dst,
            items: operands,
        });
        Ok(dst)
    }

    fn compile_relation(
        &mut self,
        id: NodeId,
        heading: &[String],
        rows: &[Vec<HirExpr>],
    ) -> Result<Register, CompileError> {
        debug_assert_eq!(
            self.infer_relation_static_type(heading, rows).outer_kinds(),
            KindSet::exact(ValueKind::Relation)
        );
        let row_count = u16::try_from(rows.len())
            .map_err(|_| self.unsupported(id, "relation literals support at most 65535 rows"))?;
        let mut cells = Vec::with_capacity(
            heading
                .len()
                .checked_mul(rows.len())
                .ok_or_else(|| self.unsupported(id, "relation literal is too large"))?,
        );
        for row in rows {
            for cell in row {
                cells.push(self.compile_expr_for_operand(cell)?);
            }
        }
        let dst = self.alloc_register();
        self.emit(Instruction::BuildRelation {
            dst,
            heading: heading
                .iter()
                .map(|column| Symbol::intern(column))
                .collect(),
            cells,
            row_count,
        });
        Ok(dst)
    }

    fn compile_empty_list(&mut self) -> Register {
        let dst = self.alloc_register();
        self.emit(Instruction::BuildList {
            dst,
            items: Vec::new(),
        });
        dst
    }

    fn compile_scatter_binding(
        &mut self,
        _id: NodeId,
        scatter: &[HirScatterBinding],
        value: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        for binding in scatter {
            self.validate_scatter_default_kind(binding)?;
        }

        let source = match value {
            Some(value) => self.compile_expr_for_value(value)?,
            None => self.compile_empty_list(),
        };
        let len = self.alloc_register();
        self.emit(Instruction::CollectionLen {
            dst: len,
            collection: source,
        });

        let mut position = 0usize;
        let mut last = None;
        let mut saw_rest = false;
        for binding in scatter {
            if matches!(binding.mode, ParamMode::Rest) {
                if saw_rest {
                    return Err(self.unsupported(
                        binding.id,
                        "scatter assignment supports only one rest binding",
                    ));
                }
                saw_rest = true;
                let dst = self.compile_collection_rest(source, len, position, binding.id)?;
                self.enforce_inferred_binding_kind(
                    binding.id,
                    binding.binding,
                    KindSet::exact(ValueKind::List),
                    dst,
                )?;
                self.locals.insert(binding.binding, dst);
                last = Some(dst);
            } else {
                let dst = if matches!(binding.mode, ParamMode::Optional) {
                    self.compile_collection_slot_with_optional_default(
                        source,
                        len,
                        position,
                        binding.id,
                        binding.default.as_ref(),
                    )?
                } else {
                    self.compile_collection_slot(source, position, binding.id)?
                };
                self.enforce_inferred_binding_kind(binding.id, binding.binding, KindSet::ALL, dst)?;
                self.locals.insert(binding.binding, dst);
                last = Some(dst);
                position += 1;
            }
        }

        Ok(last.unwrap_or_else(|| {
            let dst = self.alloc_register();
            self.emit(Instruction::Load {
                dst,
                value: Value::nothing(),
            });
            dst
        }))
    }

    fn validate_scatter_default_kind(
        &self,
        binding: &HirScatterBinding,
    ) -> Result<(), CompileError> {
        let (Some(default), Some(expected_type)) =
            (binding.default.as_ref(), binding.declared_type.as_ref())
        else {
            return Ok(());
        };
        let inferred_type = self.infer_expr_static_type(default);
        if !inferred_type.is_disjoint(expected_type) {
            return Ok(());
        }
        let subject = self.semantic.bindings[binding.binding.as_u32() as usize]
            .name
            .clone();
        if let StaticType::Kind(expected) = expected_type {
            return Err(CompileError::ValueKindMismatch {
                node: expr_id(default),
                span: self.span(expr_id(default)),
                subject,
                expected: *expected,
                inferred: inferred_type.outer_kinds().names(),
            });
        }
        Err(CompileError::StaticTypeMismatch {
            node: expr_id(default),
            span: self.span(expr_id(default)),
            subject,
            expected: expected_type.to_string(),
            inferred: inferred_type.to_string(),
        })
    }

    fn compile_collection_slot(
        &mut self,
        collection: Register,
        position: usize,
        id: NodeId,
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        self.emit(Instruction::Index {
            dst,
            collection,
            index: self.usize_operand(position, id)?,
        });
        Ok(dst)
    }

    fn compile_collection_slot_with_optional_default(
        &mut self,
        collection: Register,
        len: Register,
        position: usize,
        id: NodeId,
        default: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        let pos = self.load_usize(position, id)?;
        let condition = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: condition,
            op: RuntimeBinaryOp::Lt,
            left: pos,
            right: len,
        });
        let branch = self.emit_branch(condition, 0, 0);
        let true_target = self.instructions.len();
        self.emit(Instruction::Index {
            dst,
            collection,
            index: self.usize_operand(position, id)?,
        });
        let jump = self.emit_jump(0);
        let false_target = self.instructions.len();
        match default {
            Some(default) => {
                let default = self.compile_expr_for_value(default)?;
                self.emit(Instruction::Move { dst, src: default });
            }
            None => self.emit(Instruction::Load {
                dst,
                value: Value::nothing(),
            }),
        }
        let end = self.instructions.len();
        self.patch_branch(branch, true_target, false_target)?;
        self.patch_jump(jump, end)?;
        Ok(dst)
    }

    fn compile_collection_rest(
        &mut self,
        collection: Register,
        len: Register,
        position: usize,
        id: NodeId,
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        let start = self.load_usize(position, id)?;
        let condition = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: condition,
            op: RuntimeBinaryOp::Le,
            left: start,
            right: len,
        });
        let branch = self.emit_branch(condition, 0, 0);

        let slice_target = self.instructions.len();
        let range = self.alloc_register();
        self.emit(Instruction::BuildRange {
            dst: range,
            start: Operand::Register(start),
            end: None,
        });
        self.emit(Instruction::Index {
            dst,
            collection,
            index: Operand::Register(range),
        });
        let jump = self.emit_jump(0);

        let empty_target = self.instructions.len();
        let empty = self.compile_empty_list();
        self.emit(Instruction::Move { dst, src: empty });

        let end = self.instructions.len();
        self.patch_branch(branch, slice_target, empty_target)?;
        self.patch_jump(jump, end)?;
        Ok(dst)
    }

    fn compile_map(&mut self, entries: &[(HirExpr, HirExpr)]) -> Result<Register, CompileError> {
        let entries = entries
            .iter()
            .map(|(key, value)| {
                Ok((
                    self.compile_expr_for_operand(key)?,
                    self.compile_expr_for_operand(value)?,
                ))
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        let dst = self.alloc_register();
        self.emit(Instruction::BuildMap { dst, entries });
        Ok(dst)
    }

    fn compile_index(
        &mut self,
        id: NodeId,
        collection: &HirExpr,
        index: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        let Some(index) = index else {
            return Err(self.unsupported(id, "index expressions require an explicit index"));
        };
        let collection = self.compile_expr_for_value(collection)?;
        let index = self.compile_expr_for_operand(index)?;
        let dst = self.alloc_register();
        self.emit(Instruction::Index {
            dst,
            collection,
            index,
        });
        Ok(dst)
    }

    fn compile_index_assignment(
        &mut self,
        id: NodeId,
        collection: &HirExpr,
        index: Option<&HirExpr>,
        value: Register,
    ) -> Result<Register, CompileError> {
        let Some(index) = index else {
            return Err(self.unsupported(id, "indexed assignment requires an explicit index"));
        };
        let HirExpr::LocalRef { binding, .. } = collection else {
            return Err(self.unsupported(
                id,
                "indexed assignment currently requires a local collection target",
            ));
        };
        let binding_info = self
            .semantic
            .bindings
            .get(binding.0 as usize)
            .ok_or_else(|| CompileError::UnboundLocal {
                node: id,
                span: self.span(id),
                binding: *binding,
            })?;
        if !binding_info.mutable {
            return Err(self.unsupported(
                id,
                format!(
                    "cannot assign through immutable indexed binding `{}`",
                    binding_info.name
                ),
            ));
        }
        let collection =
            self.locals
                .get(binding)
                .copied()
                .ok_or_else(|| CompileError::UnboundLocal {
                    node: id,
                    span: self.span(id),
                    binding: *binding,
                })?;
        let index = self.compile_expr_for_operand(index)?;
        let updated = self.alloc_register();
        self.emit(Instruction::SetIndex {
            dst: updated,
            collection,
            index,
            value: Operand::Register(value),
        });
        self.emit(Instruction::Move {
            dst: collection,
            src: updated,
        });
        Ok(collection)
    }

    fn compile_dot_read(
        &mut self,
        id: NodeId,
        base: &HirExpr,
        name: &str,
    ) -> Result<Register, CompileError> {
        if let Some(field) = builtin_error_field(name)
            && self.context.dot_relation(name).is_none()
        {
            let error = self.compile_expr_for_value(base)?;
            let dst = self.alloc_register();
            self.emit(Instruction::ErrorField { dst, error, field });
            return Ok(dst);
        }
        if let Some(dot) = self.context.dot_relation(name) {
            self.ensure_dot_relation_metadata(id, name, dot.relation)?;
            let key = self.compile_expr_for_operand(base)?;
            let dst = self.alloc_register();
            self.emit(Instruction::ScanValue {
                dst,
                relation: dot.relation,
                key,
            });
            return Ok(dst);
        }
        let Some(relation) = self.conventional_dot_relation(id, name)? else {
            return Err(self.unsupported(id, format!("dot name `{name}` is not declared")));
        };
        let key = self.compile_expr_for_operand(base)?;
        let query = self.alloc_register();
        self.emit(Instruction::ScanBindings {
            dst: query,
            relation,
            bindings: vec![Some(key), None],
            outputs: vec![QueryBinding {
                name: Symbol::intern(name),
                position: 1,
            }],
        });
        let matched = self.alloc_register();
        self.emit(Instruction::RelationPattern {
            dst: matched,
            relation: query,
            heading: vec![Symbol::intern(name)],
            row_count: 1,
            equalities: Vec::new(),
        });
        let branch = self.instructions.len();
        self.emit(Instruction::Branch {
            condition: matched,
            if_true: branch + 2,
            if_false: branch + 1,
        });
        self.emit(Instruction::Raise {
            error: Operand::Value(Value::error_code(Symbol::intern("E_CARDINALITY"))),
            message: Some(Operand::Value(Value::string(
                "functional dot read requires exactly one fact",
            ))),
            value: Some(Operand::Register(query)),
        });
        let dst = self.alloc_register();
        self.emit(Instruction::RelationCell {
            dst,
            relation: query,
            column: Symbol::intern(name),
        });
        Ok(dst)
    }

    fn compile_dot_assignment(
        &mut self,
        id: NodeId,
        base: &HirExpr,
        name: &str,
        value: Register,
    ) -> Result<Register, CompileError> {
        let dot = match self.context.dot_relation(name) {
            Some(dot) => dot,
            None => {
                let Some(relation) = self.conventional_dot_relation(id, name)? else {
                    return Err(self.unsupported(id, format!("dot name `{name}` is not declared")));
                };
                DotRelation { relation }
            }
        };
        self.ensure_dot_relation_metadata(id, name, dot.relation)?;
        let key = self.compile_expr_for_operand(base)?;
        self.emit(Instruction::ReplaceFunctional {
            relation: dot.relation,
            values: vec![key, Operand::Register(value)],
        });
        Ok(value)
    }

    fn conventional_dot_relation(
        &self,
        id: NodeId,
        name: &str,
    ) -> Result<Option<RelationId>, CompileError> {
        let Some(relation) = self.context.relation(&relation_name_for_dot(name)) else {
            return Ok(None);
        };
        self.ensure_dot_relation_metadata(id, name, relation)?;
        Ok(Some(relation))
    }

    fn ensure_dot_relation_metadata(
        &self,
        id: NodeId,
        dot_name: &str,
        relation: RelationId,
    ) -> Result<(), CompileError> {
        let Some(metadata) = self.context.relation_metadata(relation) else {
            return Err(self.unsupported(
                id,
                format!("dot name `{dot_name}` has no relation metadata"),
            ));
        };
        let relation_name = metadata.name().name().unwrap_or("<unnamed relation>");
        if metadata.arity() != 2 {
            return Err(self.unsupported(
                id,
                format!(
                    "dot name `{dot_name}` requires a binary relation, but `{}` has arity {}",
                    relation_name,
                    metadata.arity()
                ),
            ));
        }
        if !matches!(
            metadata.conflict_policy(),
            ConflictPolicy::Functional { key_positions } if key_positions.as_slice() == [0]
        ) {
            return Err(self.unsupported(
                id,
                format!(
                    "dot name `{dot_name}` requires `{}` to be functional on position 0",
                    relation_name
                ),
            ));
        }
        Ok(())
    }

    fn compile_function(&self, expr: &HirExpr) -> Result<FunctionInfo, CompileError> {
        let HirExpr::Function {
            id,
            name,
            params,
            result_type,
            result_kind,
            captures,
            body,
            ..
        } = expr
        else {
            return Err(self.unsupported(expr_id(expr), "expected function expression"));
        };
        let mut saw_optional = false;
        let mut saw_rest = false;
        for param in params {
            let parameter = self.semantic.bindings[param.binding.as_u32() as usize]
                .name
                .clone();
            match param.kind {
                LocalKind::Param => {
                    if saw_optional || saw_rest {
                        return Err(self.unsupported(
                            param.id,
                            "required function parameters must precede optional and rest parameters",
                        ));
                    }
                }
                LocalKind::OptionalParam => {
                    if saw_rest {
                        return Err(self.unsupported(
                            param.id,
                            "optional function parameters must precede rest parameters",
                        ));
                    }
                    if param.default.is_none() {
                        return Err(CompileError::MissingOptionalParameterDefault {
                            node: param.id,
                            span: self.span(param.id),
                            parameter,
                        });
                    }
                    saw_optional = true;
                }
                LocalKind::RestParam => {
                    if saw_rest {
                        return Err(self.unsupported(
                            param.id,
                            "function signatures support only one rest parameter",
                        ));
                    }
                    if let Some(declared) = param.declared_kind
                        && declared != ValueKind::List
                    {
                        return Err(CompileError::InvalidRestParameterKind {
                            node: param.id,
                            span: self.span(param.id),
                            parameter,
                            declared,
                        });
                    }
                    saw_rest = true;
                }
                _ => {
                    return Err(self.unsupported(
                        param.id,
                        "unsupported function parameter kind in compiled function",
                    ));
                }
            }

            if param.kind == LocalKind::OptionalParam
                && let (Some(expected_type), Some(default)) =
                    (param.declared_type.as_ref(), param.default.as_ref())
            {
                let no_direct_result = |_| None;
                let runtime_result = |name: &str| runtime_result_kinds(self.context, name);
                let inferred_type = StaticTypeInference::new(
                    &self.semantic.bindings,
                    &no_direct_result,
                    &runtime_result,
                )
                .expr(default);
                if inferred_type.is_disjoint(expected_type) {
                    if let StaticType::Kind(expected) = expected_type {
                        return Err(CompileError::ParameterDefaultKindMismatch {
                            node: expr_id(default),
                            span: self.span(expr_id(default)),
                            parameter,
                            expected: *expected,
                            inferred: inferred_type.outer_kinds().names(),
                        });
                    }
                    return Err(CompileError::StaticTypeMismatch {
                        node: expr_id(default),
                        span: self.span(expr_id(default)),
                        subject: parameter,
                        expected: expected_type.to_string(),
                        inferred: inferred_type.to_string(),
                    });
                }
            }
        }

        let direct_result = |_| None;
        let runtime_result = |name: &str| runtime_result_kinds(self.context, name);
        let inferred = KindInference::new(&self.semantic.bindings, &direct_result, &runtime_result)
            .function_result(body);
        let inferred_type = self.infer_simple_function_result_type(body);
        if let Some(expected) = result_kind
            && !inferred.is_subset(KindSet::exact(*expected))
        {
            let function = name.and_then(|binding| {
                self.semantic
                    .bindings
                    .get(binding.as_u32() as usize)
                    .map(|binding| binding.name.clone())
            });
            return Err(CompileError::FunctionResultKindMismatch {
                node: *id,
                span: self.span(*id),
                function,
                expected: *expected,
                inferred: inferred.names(),
            });
        }
        if let Some(expected_type) = result_type
            && !matches!(expected_type, StaticType::Kind(_))
            && !inferred_type.is_subtype_of(expected_type)
        {
            let function = name
                .and_then(|binding| self.semantic.bindings.get(binding.0 as usize))
                .map_or_else(
                    || "function result".to_owned(),
                    |binding| binding.name.clone(),
                );
            return Err(CompileError::StaticTypeMismatch {
                node: *id,
                span: self.span(*id),
                subject: function,
                expected: expected_type.to_string(),
                inferred: inferred_type.to_string(),
            });
        }

        let mut compiler = ProgramCompiler::new(self.semantic, self.context);
        compiler.next_register = (captures.len() + params.len()) as u16;
        for (idx, capture) in captures.iter().enumerate() {
            compiler.locals.insert(*capture, Register(idx as u16));
            if let Some(ty) = self.local_types.get(capture) {
                compiler.local_types.insert(*capture, ty.clone());
            }
        }
        for (idx, param) in params.iter().enumerate() {
            compiler
                .locals
                .insert(param.binding, Register((captures.len() + idx) as u16));
        }
        let program = match body {
            HirFunctionBody::Expr(expr) => compiler.compile_function_expr_body(expr)?,
            HirFunctionBody::Block(items) => compiler.compile_items(items)?,
        };
        let param_info = params
            .iter()
            .map(|param| FunctionParamInfo {
                id: param.id,
                binding: param.binding,
                kind: param.kind.clone(),
                declared_type: param.declared_type.clone(),
                declared_kind: param.declared_kind,
                default: param.default.clone(),
            })
            .collect::<Vec<_>>();
        let min_arity = param_info
            .iter()
            .filter(|param| param.kind == LocalKind::Param)
            .count();
        let max_arity = if param_info
            .iter()
            .any(|param| param.kind == LocalKind::RestParam)
        {
            None
        } else {
            Some(param_info.len())
        };
        Ok(FunctionInfo {
            program: Arc::new(program),
            params: param_info,
            captures: captures.clone(),
            min_arity,
            max_arity,
            result_kinds: inferred,
            result_type: inferred_type,
        })
    }

    fn emit_function_value(
        &mut self,
        id: NodeId,
        function: FunctionInfo,
    ) -> Result<Register, CompileError> {
        let min_arity = u16::try_from(function.min_arity)
            .map_err(|_| self.unsupported(id, "function value has too many parameters"))?;
        let max_arity = match function.max_arity {
            Some(max_arity) => {
                let max_arity = u16::try_from(max_arity)
                    .map_err(|_| self.unsupported(id, "function value has too many parameters"))?;
                if max_arity == u16::MAX {
                    return Err(self.unsupported(id, "function value has too many parameters"));
                }
                max_arity
            }
            None => u16::MAX,
        };
        let captures = function
            .captures
            .iter()
            .map(|capture| {
                self.locals
                    .get(capture)
                    .copied()
                    .map(Operand::Register)
                    .ok_or_else(|| CompileError::UnboundLocal {
                        node: id,
                        span: self.span(id),
                        binding: *capture,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let wrapper = self.compile_function_value_wrapper(id, &function)?;
        let dst = self.alloc_register();
        self.emit(Instruction::LoadFunction {
            dst,
            program: Arc::new(wrapper),
            captures,
            min_arity,
            max_arity,
        });
        Ok(dst)
    }

    fn compile_function_value_wrapper(
        &self,
        id: NodeId,
        function: &FunctionInfo,
    ) -> Result<Program, CompileError> {
        let mut compiler = ProgramCompiler::new(self.semantic, self.context);
        compiler.next_register = (function.captures.len() + 1) as u16;
        let actuals = Register(function.captures.len() as u16);
        for (idx, capture) in function.captures.iter().enumerate() {
            compiler.locals.insert(*capture, Register(idx as u16));
        }

        let len = compiler.alloc_register();
        compiler.emit(Instruction::CollectionLen {
            dst: len,
            collection: actuals,
        });

        let mut operands = Vec::with_capacity(function.captures.len() + function.params.len());
        operands.extend(
            (0..function.captures.len()).map(|idx| Operand::Register(Register(idx as u16))),
        );
        let mut position = 0usize;
        for param in &function.params {
            let value = match param.kind {
                LocalKind::Param => {
                    let value = compiler.compile_collection_slot(actuals, position, param.id)?;
                    position += 1;
                    value
                }
                LocalKind::OptionalParam => {
                    let value = compiler.compile_collection_slot_with_optional_default(
                        actuals,
                        len,
                        position,
                        param.id,
                        param.default.as_ref(),
                    )?;
                    position += 1;
                    value
                }
                LocalKind::RestParam => {
                    compiler.compile_collection_rest(actuals, len, position, param.id)?
                }
                _ => {
                    return Err(self
                        .unsupported(id, "unsupported function parameter kind in function value"));
                }
            };
            if param.kind != LocalKind::RestParam {
                compiler.enforce_compiled_parameter(param, None, value)?;
            }
            compiler.locals.insert(param.binding, value);
            operands.push(Operand::Register(value));
        }

        let dst = compiler.alloc_register();
        compiler.emit(Instruction::Call {
            dst,
            program: Arc::clone(&function.program),
            args: operands,
        });
        compiler.emit(Instruction::Return {
            value: Operand::Register(dst),
        });
        let register_count = compiler.next_register as usize;
        compiler
            .instructions
            .finish(register_count)
            .map_err(Into::into)
    }

    fn compile_function_expr_body(mut self, expr: &HirExpr) -> Result<Program, CompileError> {
        let value = self.compile_expr_for_value(expr)?;
        if !self.returned {
            self.emit(Instruction::Return {
                value: Operand::Register(value),
            });
        }
        self.instructions
            .finish(self.next_register as usize)
            .map_err(Into::into)
    }

    fn compile_call(
        &mut self,
        id: NodeId,
        callee: &HirExpr,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if let HirExpr::ExternalRef { name, .. } = callee {
            return if is_compiler_builtin(name) || self.context.is_runtime_function(name) {
                self.compile_builtin_call(id, name, args)
            } else {
                self.compile_positional_dispatch(id, name, args)
            };
        }
        if let HirExpr::LocalRef { binding, .. } = callee {
            let binding_info = &self.semantic.bindings[binding.as_u32() as usize];
            if binding_info.kind == LocalKind::InstalledParam
                && (is_compiler_builtin(&binding_info.name)
                    || self.context.is_runtime_function(&binding_info.name))
            {
                let name = binding_info.name.clone();
                return self.compile_builtin_call(id, &name, args);
            }
            if let Some(function) = self.functions.get(binding).cloned() {
                return self.compile_direct_function_call(id, &function, args);
            }
        }
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(
                self.unsupported(id, "function value calls only support positional arguments")
            );
        }
        let callee = self.compile_expr_for_value(callee)?;
        let dst = self.alloc_register();
        if args.iter().any(|arg| arg.splice) {
            let call_args = self.compile_arg_items(args)?;
            self.emit(Instruction::CallValueDynamic {
                dst,
                callee: Operand::Register(callee),
                args: call_args,
            });
        } else {
            let call_args = args
                .iter()
                .map(|arg| self.compile_arg_operand(arg))
                .collect::<Result<Vec<_>, _>>()?;
            self.emit(Instruction::CallValue {
                dst,
                callee: Operand::Register(callee),
                args: call_args,
            });
        }
        Ok(dst)
    }

    fn compile_direct_function_call(
        &mut self,
        id: NodeId,
        function: &FunctionInfo,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(
                id,
                "direct function calls only support positional arguments",
            ));
        }
        let has_splice = args.iter().any(|arg| arg.splice);
        if !has_splice {
            self.validate_static_function_arity(id, function, args.len())?;
        }
        let call_args = if function
            .params
            .iter()
            .all(|param| param.kind == LocalKind::Param)
            && !has_splice
        {
            self.compile_direct_required_args(function, args)?
        } else {
            self.compile_bound_function_args(id, function, args)?
        };
        let dst = self.alloc_register();
        self.emit(Instruction::Call {
            dst,
            program: Arc::clone(&function.program),
            args: call_args,
        });
        Ok(dst)
    }

    fn compile_builtin_call(
        &mut self,
        id: NodeId,
        name: &str,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if let Some(function) = self.context.host_request_function(name).cloned() {
            return self.compile_host_request_function_call(id, name, &function, args);
        }
        match name {
            "none" => return Err(self.unsupported(id, "none is a value, not a function")),
            "some" | "ok" | "err" => {
                return self.compile_standard_constructor(id, name, args);
            }
            "commit" => return self.compile_commit_call(id, args),
            "suspend" => return self.compile_suspend_call(id, args),
            "read" => return self.compile_read_call(id, args),
            "mailbox_recv" => return self.compile_mailbox_recv_call(id, args),
            "external_request" => return self.compile_external_request_call(id, args),
            "invoke" => return self.compile_dynamic_invoke_call(id, args),
            _ => {}
        }
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "builtin calls only support positional arguments"));
        }
        let dst = self.alloc_register();
        let result = self.context.runtime_function_result(name);
        let (result_kind, structural) = match result {
            Some(BuiltinResultKind::Exact(kind)) => (Some(kind), None),
            Some(BuiltinResultKind::Structural(contract)) => {
                (contract.exact_outer_kind(), Some(contract))
            }
            Some(BuiltinResultKind::Dynamic) | None => (None, None),
        };
        let name = Symbol::intern(name);
        if args.iter().any(|arg| arg.splice) {
            let args = self.compile_arg_items(args)?;
            self.emit(Instruction::BuiltinCallDynamic {
                dst,
                name,
                result_kind,
                args,
            });
        } else {
            let args = args
                .iter()
                .map(|arg| self.compile_arg_operand(arg))
                .collect::<Result<Vec<_>, _>>()?;
            self.emit(Instruction::BuiltinCall {
                dst,
                name,
                result_kind,
                args,
            });
        }
        if let Some(expected) = structural {
            self.emit(Instruction::CheckType {
                value: dst,
                expected,
                site: KindCheckSite::Builtin,
                subject: name,
            });
        }
        Ok(dst)
    }

    fn compile_none(&mut self) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        self.emit(Instruction::BuildRelation {
            dst,
            heading: vec![Symbol::intern("value")],
            cells: Vec::new(),
            row_count: 0,
        });
        Ok(dst)
    }

    fn compile_standard_constructor(
        &mut self,
        id: NodeId,
        name: &str,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some() || arg.splice) || args.len() != 1 {
            return Err(self.unsupported(id, format!("{name} expects one positional argument")));
        }
        let source = &args[0].value;
        let value = self.compile_expr_for_value(source)?;
        if name == "err" {
            let inferred = self.infer_expr_static_type(source);
            let expected = StaticType::Kind(ValueKind::Error);
            if inferred.is_disjoint(&expected) {
                return Err(CompileError::ValueKindMismatch {
                    node: expr_id(source),
                    span: self.span(expr_id(source)),
                    subject: "err argument".to_owned(),
                    expected: ValueKind::Error,
                    inferred: inferred.to_string(),
                });
            }
            if !inferred.is_subtype_of(&expected) {
                self.emit(Instruction::CheckKind {
                    value,
                    expected: ValueKind::Error,
                    site: KindCheckSite::Builtin,
                    subject: Symbol::intern("err"),
                });
            }
        }
        let (heading, cells) = match name {
            "some" => (
                vec![Symbol::intern("value")],
                vec![Operand::Register(value)],
            ),
            "ok" => (
                vec![Symbol::intern("case"), Symbol::intern("value")],
                vec![
                    Operand::Value(Value::symbol(Symbol::intern("ok"))),
                    Operand::Register(value),
                ],
            ),
            "err" => (
                vec![Symbol::intern("case"), Symbol::intern("value")],
                vec![
                    Operand::Value(Value::symbol(Symbol::intern("error"))),
                    Operand::Register(value),
                ],
            ),
            _ => unreachable!("caller selected a standard constructor"),
        };
        let dst = self.alloc_register();
        self.emit(Instruction::BuildRelation {
            dst,
            heading,
            cells,
            row_count: 1,
        });
        Ok(dst)
    }

    fn compile_dynamic_invoke_call(
        &mut self,
        id: NodeId,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        let method_relations = self
            .context
            .method_relations
            .ok_or_else(|| self.unsupported(id, "method relation ids are not configured"))?;
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "invoke does not accept named arguments"));
        }
        if args.iter().any(|arg| arg.splice) {
            let (actuals, len) = self.compile_dynamic_arg_list(args)?;
            self.compile_abort_if_len_lt(id, len, 2, "invoke expects selector and role map")?;
            self.compile_abort_if_len_gt(id, len, 2, "invoke expects selector and role map")?;
            let selector = Operand::Register(self.compile_collection_slot(actuals, 0, id)?);
            let roles = Operand::Register(self.compile_collection_slot(actuals, 1, id)?);
            let dst = self.alloc_register();
            self.emit(Instruction::DynamicDispatch {
                dst,
                relations: method_relations.dispatch,
                program_relation: method_relations.method_program,
                program_bytes: method_relations.program_bytes,
                selector,
                roles,
            });
            return Ok(dst);
        }
        if args.len() != 2 {
            return Err(self.unsupported(id, "invoke expects selector and role map"));
        }
        let selector = self.compile_arg_operand(&args[0])?;
        let roles = self.compile_arg_operand(&args[1])?;
        let dst = self.alloc_register();
        self.emit(Instruction::DynamicDispatch {
            dst,
            relations: method_relations.dispatch,
            program_relation: method_relations.method_program,
            program_bytes: method_relations.program_bytes,
            selector,
            roles,
        });
        Ok(dst)
    }

    fn compile_positional_dispatch(
        &mut self,
        id: NodeId,
        name: &str,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        let method_relations = self
            .context
            .method_relations
            .ok_or_else(|| self.unsupported(id, "method relation ids are not configured"))?;
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(
                id,
                "positional dispatch calls do not accept named arguments",
            ));
        }
        let has_splice = args.iter().any(|arg| arg.splice);
        let dst = self.alloc_register();
        let selector = Operand::Value(Value::symbol(Symbol::intern(name)));
        if has_splice {
            let args = self.compile_arg_items(args)?;
            self.emit(Instruction::PositionalDispatchDynamic {
                dst,
                relations: method_relations.dispatch,
                program_relation: method_relations.method_program,
                program_bytes: method_relations.program_bytes,
                selector,
                args,
            });
        } else {
            let args = args
                .iter()
                .map(|arg| self.compile_arg_operand(arg))
                .collect::<Result<Vec<_>, _>>()?;
            self.emit(Instruction::PositionalDispatch {
                dst,
                relations: method_relations.dispatch,
                program_relation: method_relations.method_program,
                program_bytes: method_relations.program_bytes,
                selector,
                args,
            });
        }
        Ok(dst)
    }

    fn compile_receiver_positional_dispatch(
        &mut self,
        id: NodeId,
        receiver: &HirExpr,
        selector: &HirExpr,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        let method_relations = self
            .context
            .method_relations
            .ok_or_else(|| self.unsupported(id, "method relation ids are not configured"))?;
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(
                id,
                "receiver positional dispatch calls do not accept named arguments",
            ));
        }
        let has_splice = args.iter().any(|arg| arg.splice);
        let selector = self.compile_expr_for_operand(selector)?;
        let receiver = self.compile_expr_for_value(receiver)?;
        let dst = self.alloc_register();
        if has_splice {
            let mut items = Vec::with_capacity(args.len() + 1);
            items.push(ListItem::Value(Operand::Register(receiver)));
            items.extend(self.compile_arg_items(args)?);
            self.emit(Instruction::PositionalDispatchDynamic {
                dst,
                relations: method_relations.dispatch,
                program_relation: method_relations.method_program,
                program_bytes: method_relations.program_bytes,
                selector,
                args: items,
            });
        } else {
            let mut operands = Vec::with_capacity(args.len() + 1);
            operands.push(Operand::Register(receiver));
            for arg in args {
                operands.push(self.compile_arg_operand(arg)?);
            }
            self.emit(Instruction::PositionalDispatch {
                dst,
                relations: method_relations.dispatch,
                program_relation: method_relations.method_program,
                program_bytes: method_relations.program_bytes,
                selector,
                args: operands,
            });
        }
        Ok(dst)
    }

    fn compile_commit_call(
        &mut self,
        id: NodeId,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "commit does not accept named arguments"));
        }
        if args.iter().any(|arg| arg.splice) {
            let (_, len) = self.compile_dynamic_arg_list(args)?;
            self.compile_abort_if_len_gt(id, len, 0, "commit expects no arguments")?;
            let dst = self.alloc_register();
            self.emit(Instruction::CommitValue { dst });
            return Ok(dst);
        }
        if !args.is_empty() {
            return Err(self.unsupported(id, "commit expects no arguments"));
        }
        let dst = self.alloc_register();
        self.emit(Instruction::CommitValue { dst });
        Ok(dst)
    }

    fn compile_suspend_call(
        &mut self,
        id: NodeId,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "suspend only supports positional arguments"));
        }
        if args.iter().any(|arg| arg.splice) {
            let (actuals, len) = self.compile_dynamic_arg_list(args)?;
            self.compile_abort_if_len_gt(id, len, 1, "suspend expects 0 or 1 arguments")?;
            return self.compile_suspend_from_dynamic_args(id, actuals, len);
        }
        if args.len() > 1 {
            return Err(self.unsupported(id, "suspend expects 0 or 1 arguments"));
        }
        let duration = args
            .first()
            .map(|arg| self.compile_arg_operand(arg))
            .transpose()?;
        let dst = self.alloc_register();
        self.emit(Instruction::SuspendValue { dst, duration });
        Ok(dst)
    }

    fn compile_read_call(&mut self, id: NodeId, args: &[HirArg]) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "read only supports positional arguments"));
        }
        if args.iter().any(|arg| arg.splice) {
            let (actuals, len) = self.compile_dynamic_arg_list(args)?;
            self.compile_abort_if_len_gt(id, len, 1, "read expects 0 or 1 arguments")?;
            return self.compile_read_from_dynamic_args(id, actuals, len);
        }
        if args.len() > 1 {
            return Err(self.unsupported(id, "read expects 0 or 1 arguments"));
        }
        let metadata = args
            .first()
            .map(|arg| self.compile_arg_operand(arg))
            .transpose()?;
        let dst = self.alloc_register();
        self.emit(Instruction::Read { dst, metadata });
        Ok(dst)
    }

    fn compile_host_request_function_call(
        &mut self,
        id: NodeId,
        name: &str,
        function: &HostRequestFunction,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, format!("{name} only supports positional arguments")));
        }
        if args.iter().any(|arg| arg.splice) {
            return Err(self.unsupported(id, format!("{name} does not support argument splices")));
        }
        if args.len() != function.payload_fields.len() {
            return Err(self.unsupported(
                id,
                format!(
                    "{name} expects {} arguments for host request payload",
                    function.payload_fields.len()
                ),
            ));
        }
        let entries = args
            .iter()
            .zip(function.payload_fields.iter())
            .map(|(arg, field)| {
                Ok((
                    Operand::Value(Value::symbol(*field)),
                    self.compile_arg_operand(arg)?,
                ))
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        let payload = self.alloc_register();
        self.emit(Instruction::BuildMap {
            dst: payload,
            entries,
        });
        let dst = self.alloc_register();
        self.emit(Instruction::ExternalRequest {
            dst,
            service: Operand::Value(Value::symbol(function.service)),
            payload: Operand::Register(payload),
            timeout: function.timeout.clone().map(Operand::Value),
        });
        Ok(dst)
    }

    fn compile_external_request_call(
        &mut self,
        id: NodeId,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "external_request only supports positional arguments"));
        }
        if args.iter().any(|arg| arg.splice) {
            return Err(self.unsupported(id, "external_request does not support argument splices"));
        }
        if !(2..=3).contains(&args.len()) {
            return Err(self.unsupported(
                id,
                "external_request expects service, payload, and optional timeout",
            ));
        }
        let service = self.compile_arg_operand(&args[0])?;
        let payload = self.compile_arg_operand(&args[1])?;
        let timeout = args
            .get(2)
            .map(|arg| self.compile_arg_operand(arg))
            .transpose()?;
        let dst = self.alloc_register();
        self.emit(Instruction::ExternalRequest {
            dst,
            service,
            payload,
            timeout,
        });
        Ok(dst)
    }

    fn compile_mailbox_recv_call(
        &mut self,
        id: NodeId,
        args: &[HirArg],
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(id, "mailbox_recv only supports positional arguments"));
        }
        if args.iter().any(|arg| arg.splice) {
            let (actuals, len) = self.compile_dynamic_arg_list(args)?;
            self.compile_abort_if_len_lt(
                id,
                len,
                1,
                "mailbox_recv expects a receive-cap list and optional timeout",
            )?;
            self.compile_abort_if_len_gt(
                id,
                len,
                2,
                "mailbox_recv expects a receive-cap list and optional timeout",
            )?;
            return self.compile_mailbox_recv_from_dynamic_args(id, actuals, len);
        }
        if args.is_empty() || args.len() > 2 {
            return Err(self.unsupported(
                id,
                "mailbox_recv expects a receive-cap list and optional timeout",
            ));
        }
        let receivers = self.compile_arg_operand(&args[0])?;
        let timeout = args
            .get(1)
            .map(|arg| self.compile_arg_operand(arg))
            .transpose()?;
        let dst = self.alloc_register();
        self.emit(Instruction::MailboxRecv {
            dst,
            receivers,
            timeout,
        });
        Ok(dst)
    }

    fn compile_dynamic_arg_list(
        &mut self,
        args: &[HirArg],
    ) -> Result<(Register, Register), CompileError> {
        let items = self.compile_arg_items(args)?;
        let actuals = self.alloc_register();
        self.emit(Instruction::BuildList {
            dst: actuals,
            items,
        });
        let len = self.alloc_register();
        self.emit(Instruction::CollectionLen {
            dst: len,
            collection: actuals,
        });
        Ok((actuals, len))
    }

    fn compile_abort_if_len_lt(
        &mut self,
        id: NodeId,
        len: Register,
        min: usize,
        message: &str,
    ) -> Result<(), CompileError> {
        let min = self.load_usize(min, id)?;
        let condition = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: condition,
            op: RuntimeBinaryOp::Lt,
            left: len,
            right: min,
        });
        self.compile_abort_if(condition, message)
    }

    fn compile_abort_if_len_gt(
        &mut self,
        id: NodeId,
        len: Register,
        max: usize,
        message: &str,
    ) -> Result<(), CompileError> {
        let max = self.load_usize(max, id)?;
        let condition = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: condition,
            op: RuntimeBinaryOp::Gt,
            left: len,
            right: max,
        });
        self.compile_abort_if(condition, message)
    }

    fn compile_abort_if(&mut self, condition: Register, message: &str) -> Result<(), CompileError> {
        let branch = self.emit_branch(condition, 0, 0);
        let abort_target = self.instructions.len();
        self.emit(Instruction::Abort {
            error: Operand::Value(Value::string(message)),
        });
        let continue_target = self.instructions.len();
        self.patch_branch(branch, abort_target, continue_target)
    }

    fn compile_len_gt_condition(
        &mut self,
        id: NodeId,
        len: Register,
        position: usize,
    ) -> Result<Register, CompileError> {
        let position = self.load_usize(position, id)?;
        let condition = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: condition,
            op: RuntimeBinaryOp::Gt,
            left: len,
            right: position,
        });
        Ok(condition)
    }

    fn compile_suspend_from_dynamic_args(
        &mut self,
        id: NodeId,
        actuals: Register,
        len: Register,
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        let condition = self.compile_len_gt_condition(id, len, 0)?;
        let branch = self.emit_branch(condition, 0, 0);

        let duration_target = self.instructions.len();
        let duration = self.compile_collection_slot(actuals, 0, id)?;
        self.emit(Instruction::SuspendValue {
            dst,
            duration: Some(Operand::Register(duration)),
        });
        let duration_jump = self.emit_jump(0);

        let no_duration_target = self.instructions.len();
        self.emit(Instruction::SuspendValue {
            dst,
            duration: None,
        });

        let end = self.instructions.len();
        self.patch_branch(branch, duration_target, no_duration_target)?;
        self.patch_jump(duration_jump, end)?;
        Ok(dst)
    }

    fn compile_read_from_dynamic_args(
        &mut self,
        id: NodeId,
        actuals: Register,
        len: Register,
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        let condition = self.compile_len_gt_condition(id, len, 0)?;
        let branch = self.emit_branch(condition, 0, 0);

        let metadata_target = self.instructions.len();
        let metadata = self.compile_collection_slot(actuals, 0, id)?;
        self.emit(Instruction::Read {
            dst,
            metadata: Some(Operand::Register(metadata)),
        });
        let metadata_jump = self.emit_jump(0);

        let no_metadata_target = self.instructions.len();
        self.emit(Instruction::Read {
            dst,
            metadata: None,
        });

        let end = self.instructions.len();
        self.patch_branch(branch, metadata_target, no_metadata_target)?;
        self.patch_jump(metadata_jump, end)?;
        Ok(dst)
    }

    fn compile_mailbox_recv_from_dynamic_args(
        &mut self,
        id: NodeId,
        actuals: Register,
        len: Register,
    ) -> Result<Register, CompileError> {
        let receivers = self.compile_collection_slot(actuals, 0, id)?;
        let dst = self.alloc_register();
        let condition = self.compile_len_gt_condition(id, len, 1)?;
        let branch = self.emit_branch(condition, 0, 0);

        let timeout_target = self.instructions.len();
        let timeout = self.compile_collection_slot(actuals, 1, id)?;
        self.emit(Instruction::MailboxRecv {
            dst,
            receivers: Operand::Register(receivers),
            timeout: Some(Operand::Register(timeout)),
        });
        let timeout_jump = self.emit_jump(0);

        let no_timeout_target = self.instructions.len();
        self.emit(Instruction::MailboxRecv {
            dst,
            receivers: Operand::Register(receivers),
            timeout: None,
        });

        let end = self.instructions.len();
        self.patch_branch(branch, timeout_target, no_timeout_target)?;
        self.patch_jump(timeout_jump, end)?;
        Ok(dst)
    }

    fn validate_static_function_arity(
        &self,
        id: NodeId,
        function: &FunctionInfo,
        actual: usize,
    ) -> Result<(), CompileError> {
        if actual < function.min_arity {
            return Err(self.unsupported(
                id,
                format!(
                    "function call expected at least {} arguments but got {}",
                    function.min_arity, actual
                ),
            ));
        }
        if let Some(max_arity) = function.max_arity
            && actual > max_arity
        {
            return Err(self.unsupported(
                id,
                format!(
                    "function call expected at most {} arguments but got {}",
                    max_arity, actual
                ),
            ));
        }
        Ok(())
    }

    fn compile_bound_function_args(
        &mut self,
        id: NodeId,
        function: &FunctionInfo,
        args: &[HirArg],
    ) -> Result<Vec<Operand>, CompileError> {
        let has_splice = args.iter().any(|arg| arg.splice);
        let items = self.compile_arg_items(args)?;
        let actuals = self.alloc_register();
        self.emit(Instruction::BuildList {
            dst: actuals,
            items,
        });
        let len = self.alloc_register();
        self.emit(Instruction::CollectionLen {
            dst: len,
            collection: actuals,
        });

        let mut operands = Vec::with_capacity(function.params.len());
        let mut position = 0usize;
        for param in &function.params {
            let value = match param.kind {
                LocalKind::Param => {
                    let value = self.compile_collection_slot(actuals, position, param.id)?;
                    let source = (!has_splice).then(|| &args[position].value);
                    position += 1;
                    self.enforce_compiled_parameter(param, source, value)?;
                    value
                }
                LocalKind::OptionalParam => {
                    let value = self.compile_collection_slot_with_optional_default(
                        actuals,
                        len,
                        position,
                        param.id,
                        param.default.as_ref(),
                    )?;
                    let source = if has_splice {
                        None
                    } else {
                        args.get(position)
                            .map(|arg| &arg.value)
                            .or(param.default.as_ref())
                    };
                    position += 1;
                    self.enforce_compiled_parameter(param, source, value)?;
                    value
                }
                LocalKind::RestParam => {
                    self.compile_collection_rest(actuals, len, position, param.id)?
                }
                _ => {
                    return Err(
                        self.unsupported(id, "unsupported function parameter kind in call binding")
                    );
                }
            };
            operands.push(Operand::Register(value));
        }
        Ok(operands)
    }

    fn compile_direct_required_args(
        &mut self,
        function: &FunctionInfo,
        args: &[HirArg],
    ) -> Result<Vec<Operand>, CompileError> {
        function
            .params
            .iter()
            .zip(args)
            .map(|(param, arg)| {
                let inferred_type = self.infer_expr_static_type(&arg.value);
                let inferred = inferred_type.outer_kinds();
                if !self.parameter_needs_check(param, &inferred_type, inferred, arg.id)? {
                    return self.compile_arg_operand(arg);
                }
                let value = self.compile_expr_for_value(&arg.value)?;
                self.emit_parameter_check(param, value);
                Ok(Operand::Register(value))
            })
            .collect()
    }

    fn compile_arg_items(&mut self, args: &[HirArg]) -> Result<Vec<ListItem>, CompileError> {
        args.iter()
            .map(|arg| {
                let operand = self.compile_arg_operand(arg)?;
                Ok(if arg.splice {
                    ListItem::Splice(operand)
                } else {
                    ListItem::Value(operand)
                })
            })
            .collect()
    }

    fn compile_role_map_items(
        &mut self,
        receiver: Option<Operand>,
        args: &[HirArg],
    ) -> Result<Vec<MapItem>, CompileError> {
        let mut items = Vec::with_capacity(args.len() + usize::from(receiver.is_some()));
        if let Some(receiver) = receiver {
            items.push(MapItem::Entry(
                Operand::Value(Value::symbol(Symbol::intern("receiver"))),
                receiver,
            ));
        }
        for arg in args {
            if let Some(role) = &arg.role {
                if arg.splice {
                    return Err(self.unsupported(
                        arg.id,
                        "role-named argument values do not support splices; splice a role map",
                    ));
                }
                items.push(MapItem::Entry(
                    Operand::Value(Value::symbol(Symbol::intern(role))),
                    self.compile_arg_operand(arg)?,
                ));
            } else if arg.splice {
                items.push(MapItem::Splice(self.compile_arg_operand(arg)?));
            } else {
                return Err(self.unsupported(
                    arg.id,
                    "role-named dispatch arguments must use explicit role names",
                ));
            }
        }
        Ok(items)
    }

    fn alloc_map(&mut self, items: Vec<MapItem>) -> Register {
        let dst = self.alloc_register();
        self.emit(Instruction::BuildMapDynamic { dst, items });
        dst
    }

    fn compile_and(&mut self, left: &HirExpr, right: &HirExpr) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::bool(false),
        });
        let left = self.compile_expr_for_value(left)?;
        let branch = self.emit_branch(left, 0, 0);
        let false_target = self.instructions.len();
        self.emit(Instruction::Jump { target: 0 });
        let true_target = self.instructions.len();
        let saved_returned = self.returned;
        let right = self.compile_expr_for_value(right)?;
        self.returned = saved_returned;
        self.emit(Instruction::Move { dst, src: right });
        let end = self.instructions.len();
        self.patch_branch(branch, true_target, false_target)?;
        self.patch_jump(false_target, end)?;
        Ok(dst)
    }

    fn compile_or(&mut self, left: &HirExpr, right: &HirExpr) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::bool(true),
        });
        let left = self.compile_expr_for_value(left)?;
        let branch = self.emit_branch(left, 0, 0);
        let true_target = self.instructions.len();
        self.emit(Instruction::Jump { target: 0 });
        let false_target = self.instructions.len();
        let saved_returned = self.returned;
        let right = self.compile_expr_for_value(right)?;
        self.returned = saved_returned;
        self.emit(Instruction::Move { dst, src: right });
        let end = self.instructions.len();
        self.patch_branch(branch, true_target, false_target)?;
        self.patch_jump(true_target, end)?;
        Ok(dst)
    }

    fn compile_if(
        &mut self,
        _id: NodeId,
        condition: &HirExpr,
        then_items: &[HirItem],
        elseif: &[(HirExpr, Vec<HirItem>)],
        else_items: &[HirItem],
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        let saved_returned = self.returned;
        self.returned = false;
        self.emit(Instruction::Load {
            dst,
            value: Value::unit(),
        });
        let condition = self.compile_expr_for_value(condition)?;
        let first_branch = self.emit_branch(condition, 0, 0);
        let mut false_jumps = Vec::new();
        let mut end_jumps = Vec::new();
        let mut branch_returns = Vec::new();
        let then_target = self.instructions.len();
        let (value, returned) = self.compile_branch_items(then_items)?;
        branch_returns.push(returned);
        if let Some(value) = value {
            self.emit(Instruction::Move { dst, src: value });
        }
        if !returned {
            end_jumps.push(self.emit_jump(0));
        }
        false_jumps.push(first_branch);

        for (condition, items) in elseif {
            let else_if_test = self.instructions.len();
            let condition = self.compile_expr_for_value(condition)?;
            let branch = self.emit_branch(condition, 0, 0);
            let body_target = self.instructions.len();
            let (value, returned) = self.compile_branch_items(items)?;
            branch_returns.push(returned);
            if let Some(value) = value {
                self.emit(Instruction::Move { dst, src: value });
            }
            if !returned {
                end_jumps.push(self.emit_jump(0));
            }
            let previous = false_jumps.pop().unwrap();
            self.patch_false_target(previous, else_if_test)?;
            self.patch_true_target(branch, body_target)?;
            false_jumps.push(branch);
        }

        let else_target = self.instructions.len();
        let (value, else_returned) = self.compile_branch_items(else_items)?;
        if !else_items.is_empty() {
            branch_returns.push(else_returned);
        }
        if let Some(value) = value {
            self.emit(Instruction::Move { dst, src: value });
        }
        let end = self.instructions.len();
        if let Some(last_false) = false_jumps.pop() {
            self.patch_false_target(last_false, else_target)?;
        }
        self.patch_true_target(first_branch, then_target)?;
        for jump in end_jumps {
            self.patch_jump(jump, end)?;
        }
        self.returned = saved_returned
            || (!else_items.is_empty()
                && !branch_returns.is_empty()
                && branch_returns.iter().all(|returned| *returned));
        Ok(dst)
    }

    fn compile_match(
        &mut self,
        id: NodeId,
        value: &HirExpr,
        cases: &[HirMatchCase],
    ) -> Result<Register, CompileError> {
        let scrutinee_type = self.infer_expr_static_type(value);
        self.validate_match_exhaustiveness(id, &scrutinee_type, cases)?;

        let scrutinee = self.compile_expr_for_value(value)?;
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::unit(),
        });
        let saved_locals = self.locals.clone();
        let saved_types = self.local_types.clone();
        let saved_returned = self.returned;
        self.returned = false;
        let mut end_jumps = Vec::new();
        let mut branch_returns = Vec::new();

        for case in cases {
            self.locals = saved_locals.clone();
            self.local_types = saved_types.clone();
            let mut failed_branches = Vec::new();
            if let Some(spec) = relation_pattern_spec(&case.pattern) {
                if spec
                    .heading
                    .windows(2)
                    .any(|columns| columns[0] == columns[1])
                {
                    return Err(
                        self.unsupported(case.id, "relation patterns cannot repeat a column")
                    );
                }
                let matched = self.alloc_register();
                self.emit(Instruction::RelationPattern {
                    dst: matched,
                    relation: scrutinee,
                    heading: spec.heading,
                    row_count: spec.row_count,
                    equalities: spec.equalities,
                });
                let branch = self.emit_branch(matched, self.instructions.len() + 1, 0);
                failed_branches.push(branch);
                for (column, binding) in spec.bindings {
                    let register = self.alloc_register();
                    self.emit(Instruction::RelationCell {
                        dst: register,
                        relation: scrutinee,
                        column,
                    });
                    self.locals.insert(binding, register);
                    self.local_types.insert(
                        binding,
                        match_binding_type(&scrutinee_type, &case.pattern, column),
                    );
                }
            }
            if let Some(guard) = &case.guard {
                let guard = self.compile_expr_for_value(guard)?;
                let branch = self.emit_branch(guard, self.instructions.len() + 1, 0);
                failed_branches.push(branch);
            }

            let (value, returned) = self.compile_branch_items(&case.body)?;
            branch_returns.push(returned);
            if let Some(value) = value {
                self.emit(Instruction::Move { dst, src: value });
            }
            if !returned {
                end_jumps.push(self.emit_jump(0));
            }
            let next_case = self.instructions.len();
            for branch in failed_branches {
                self.patch_false_target(branch, next_case)?;
            }
        }

        let exhaustive = match_is_exhaustive(&scrutinee_type, cases);
        if !exhaustive {
            self.emit(Instruction::Raise {
                error: Operand::Value(Value::error_code(Symbol::intern("E_MATCH"))),
                message: Some(Operand::Value(Value::string(
                    "non-exhaustive relation match",
                ))),
                value: Some(Operand::Register(scrutinee)),
            });
        }
        let end = self.instructions.len();
        for jump in end_jumps {
            self.patch_jump(jump, end)?;
        }
        self.locals = saved_locals;
        self.local_types = saved_types;
        self.returned = saved_returned
            || (exhaustive
                && !branch_returns.is_empty()
                && branch_returns.iter().all(|returned| *returned));
        Ok(dst)
    }

    fn validate_match_exhaustiveness(
        &self,
        id: NodeId,
        scrutinee: &StaticType,
        cases: &[HirMatchCase],
    ) -> Result<(), CompileError> {
        if cases.is_empty() {
            return Err(self.unsupported(id, "match requires at least one case"));
        }
        if let Some(position) = cases.iter().position(|case| {
            matches!(case.pattern, HirMatchPattern::Wildcard) && case.guard.is_none()
        }) && position + 1 != cases.len()
        {
            return Err(self.unsupported(cases[position].id, "wildcard match case must be last"));
        }
        if match_is_exhaustive(scrutinee, cases) {
            return Ok(());
        }
        let message = if matches!(scrutinee, StaticType::Dynamic | StaticType::Kind(_)) {
            "a match on a dynamic value requires an unguarded wildcard case"
        } else {
            "non-exhaustive structural relation match"
        };
        Err(self.unsupported(id, message))
    }

    fn compile_while(
        &mut self,
        _id: NodeId,
        condition: &HirExpr,
        body: &[HirItem],
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::unit(),
        });

        let loop_start = self.instructions.len();
        let condition = self.compile_expr_for_value(condition)?;
        let branch = self.emit_branch(condition, 0, 0);
        let body_target = self.instructions.len();

        self.loops.push(LoopContext {
            continue_target: loop_start,
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
        });
        let body_returned = self.compile_loop_body(body)?;
        let loop_context = self.loops.pop().ok_or_else(|| CompileError::Unsupported {
            node: NodeId(0),
            span: None,
            message: "internal compiler error: missing loop context".to_owned(),
        })?;

        if !body_returned {
            self.emit_jump(loop_start);
        }
        let end = self.instructions.len();
        self.patch_branch(branch, body_target, end)?;
        for jump in loop_context.break_jumps {
            self.patch_jump(jump, end)?;
        }
        for jump in loop_context.continue_jumps {
            self.patch_jump(jump, loop_context.continue_target)?;
        }
        Ok(dst)
    }

    fn compile_exact_row_binding(
        &mut self,
        id: NodeId,
        row: &[(String, BindingId)],
        value: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        let Some(value) = value else {
            return Err(self.unsupported(id, "exact row bindings require an initializer"));
        };
        let source_type = self.infer_expr_static_type(value);
        let source = self.compile_expr_for_value(value)?;
        let mut heading = row
            .iter()
            .map(|(column, _)| Symbol::intern(column))
            .collect::<Vec<_>>();
        heading.sort_unstable();
        if heading.windows(2).any(|columns| columns[0] == columns[1]) {
            return Err(self.unsupported(id, "exact row patterns cannot repeat a column"));
        }
        let matched = self.alloc_register();
        self.emit(Instruction::RelationPattern {
            dst: matched,
            relation: source,
            heading,
            row_count: 1,
            equalities: Vec::new(),
        });
        let branch = self.instructions.len();
        self.emit(Instruction::Branch {
            condition: matched,
            if_true: branch + 2,
            if_false: branch + 1,
        });
        self.emit(Instruction::Raise {
            error: Operand::Value(Value::error_code(Symbol::intern("E_CARDINALITY"))),
            message: Some(Operand::Value(Value::string(
                "exact row binding requires one row with the stated heading",
            ))),
            value: Some(Operand::Register(source)),
        });
        for (column, binding) in row {
            let column = Symbol::intern(column);
            let register = self.alloc_register();
            self.emit(Instruction::RelationCell {
                dst: register,
                relation: source,
                column,
            });
            self.locals.insert(*binding, register);
            self.local_types.insert(
                *binding,
                match_binding_type(&source_type, &HirMatchPattern::Row(row.to_vec()), column),
            );
        }
        Ok(source)
    }

    fn compile_for(
        &mut self,
        _id: NodeId,
        key: &HirLoopBinding,
        value: Option<&HirLoopBinding>,
        row: Option<&[(String, BindingId)]>,
        iter: &HirExpr,
        body: &[HirItem],
    ) -> Result<Register, CompileError> {
        let saved_locals = self.locals.clone();
        let binding_kinds = iteration_binding_kinds(self.infer_expr_kinds(iter), value.is_some());
        let result = self.alloc_register();
        self.emit(Instruction::Load {
            dst: result,
            value: Value::unit(),
        });

        let collection = self.compile_expr_for_value(iter)?;
        let len = self.alloc_register();
        self.emit(Instruction::CollectionLen {
            dst: len,
            collection,
        });
        let index = self.alloc_register();
        self.emit(Instruction::Load {
            dst: index,
            value: Value::int(0).unwrap(),
        });
        let one = self.alloc_register();
        self.emit(Instruction::Load {
            dst: one,
            value: Value::int(1).unwrap(),
        });

        let key_register = (!row.is_some()).then(|| {
            let register = self.alloc_register();
            self.emit(Instruction::Load {
                dst: register,
                value: Value::unit(),
            });
            self.locals.insert(key.binding, register);
            register
        });
        let value_register = if row.is_none()
            && let Some(value) = value
        {
            let register = self.alloc_register();
            self.emit(Instruction::Load {
                dst: register,
                value: Value::unit(),
            });
            self.locals.insert(value.binding, register);
            Some(register)
        } else {
            None
        };

        let loop_start = self.instructions.len();
        let condition = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: condition,
            op: RuntimeBinaryOp::Lt,
            left: index,
            right: len,
        });
        let branch = self.emit_branch(condition, 0, 0);
        let body_target = self.instructions.len();

        if let Some(row) = row {
            let mut heading = row
                .iter()
                .map(|(column, _)| Symbol::intern(column))
                .collect::<Vec<_>>();
            heading.sort_unstable();
            for (column, binding) in row {
                let register = self.alloc_register();
                self.emit(Instruction::RelationCellAt {
                    dst: register,
                    relation: collection,
                    index,
                    heading: heading.clone(),
                    column: Symbol::intern(column),
                });
                self.locals.insert(*binding, register);
            }
        } else if let Some(value_register) = value_register {
            let key_register = key_register.expect("ordinary loop has a key register");
            self.emit(Instruction::CollectionKeyAt {
                dst: key_register,
                collection,
                index,
            });
            self.emit(Instruction::CollectionValueAt {
                dst: value_register,
                collection,
                index,
            });
            self.enforce_inferred_binding_kind(key.id, key.binding, binding_kinds.0, key_register)?;
            let value = value.expect("value register requires a loop value binding");
            self.enforce_inferred_binding_kind(
                value.id,
                value.binding,
                binding_kinds
                    .1
                    .expect("two loop bindings require two inferred kinds"),
                value_register,
            )?;
        } else {
            let key_register = key_register.expect("ordinary loop has a value register");
            self.emit(Instruction::CollectionValueAt {
                dst: key_register,
                collection,
                index,
            });
            self.enforce_inferred_binding_kind(key.id, key.binding, binding_kinds.0, key_register)?;
        }

        self.loops.push(LoopContext {
            continue_target: 0,
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
        });
        let body_returned = self.compile_loop_body(body)?;
        let loop_context = self.loops.pop().ok_or_else(|| CompileError::Unsupported {
            node: NodeId(0),
            span: None,
            message: "internal compiler error: missing loop context".to_owned(),
        })?;

        let increment_target = self.instructions.len();
        if !body_returned {
            let next_index = self.alloc_register();
            self.emit(Instruction::Binary {
                dst: next_index,
                op: RuntimeBinaryOp::Add,
                left: index,
                right: one,
            });
            self.emit(Instruction::Move {
                dst: index,
                src: next_index,
            });
            self.emit_jump(loop_start);
        }
        let end = self.instructions.len();
        self.patch_branch(branch, body_target, end)?;
        for jump in loop_context.break_jumps {
            self.patch_jump(jump, end)?;
        }
        for jump in loop_context.continue_jumps {
            self.patch_jump(jump, increment_target)?;
        }

        self.locals = saved_locals;
        Ok(result)
    }

    fn compile_loop_body(&mut self, body: &[HirItem]) -> Result<bool, CompileError> {
        let saved_returned = self.returned;
        self.returned = false;
        for item in body {
            self.compile_item(item)?;
        }
        let body_returned = self.returned;
        self.returned = saved_returned;
        Ok(body_returned)
    }

    fn compile_raise(
        &mut self,
        error: &HirExpr,
        message: Option<&HirExpr>,
        value: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        let error = self.compile_expr_for_operand(error)?;
        let message = message
            .map(|message| self.compile_expr_for_operand(message))
            .transpose()?;
        let value = value
            .map(|value| self.compile_expr_for_operand(value))
            .transpose()?;
        self.emit(Instruction::Raise {
            error,
            message,
            value,
        });
        self.returned = true;
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::unit(),
        });
        Ok(dst)
    }

    fn compile_one(&mut self, expr: &HirExpr) -> Result<Register, CompileError> {
        let src = self.compile_expr_for_value(expr)?;
        let dst = self.alloc_register();
        self.emit(Instruction::One { dst, src });
        Ok(dst)
    }

    fn compile_try(
        &mut self,
        id: NodeId,
        body: &[HirItem],
        catches: &[HirCatch],
        finally: &[HirItem],
    ) -> Result<Register, CompileError> {
        let saved_locals = self.locals.clone();
        let saved_returned = self.returned;
        self.returned = false;

        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::unit(),
        });

        let has_dynamic_catch = catches
            .iter()
            .any(|catch| !is_static_catch_condition(catch.condition.as_ref()));
        let mut handlers = if has_dynamic_catch {
            let error = self.alloc_register();
            self.emit(Instruction::Load {
                dst: error,
                value: Value::nothing(),
            });
            vec![CatchHandler {
                code: None,
                binding: Some(error),
                target: 0,
            }]
        } else {
            catches
                .iter()
                .map(|catch| {
                    let binding = catch.binding.map(|binding| {
                        let register = self.alloc_register();
                        self.emit(Instruction::Load {
                            dst: register,
                            value: Value::nothing(),
                        });
                        self.locals.insert(binding, register);
                        register
                    });
                    let code = self.catch_code(id, catch.condition.as_ref())?;
                    Ok(CatchHandler {
                        code,
                        binding,
                        target: 0,
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?
        };

        let enter = self.instructions.len();
        self.emit(Instruction::EnterTry {
            catches: handlers.clone(),
            finally: None,
            end: 0,
        });

        let (body_value, body_returned) = self.compile_branch_items(body)?;
        if let Some(value) = body_value {
            self.emit(Instruction::Move { dst, src: value });
        }
        if !body_returned {
            self.emit(Instruction::ExitTry);
        }

        let mut end_jumps = Vec::new();
        if has_dynamic_catch {
            let error = handlers[0]
                .binding
                .expect("dynamic catch handler binds error");
            handlers[0].target = self.instructions.len();
            self.compile_try_catch_dispatcher(dst, error, catches, finally, &mut end_jumps)?;
        } else {
            for (idx, catch) in catches.iter().enumerate() {
                handlers[idx].target = self.instructions.len();
                let (value, returned) = self.compile_branch_items(&catch.body)?;
                if let Some(value) = value {
                    self.emit(Instruction::Move { dst, src: value });
                }
                if !returned {
                    if finally.is_empty() {
                        end_jumps.push(self.emit_jump(0));
                    } else {
                        self.emit(Instruction::ExitTry);
                    }
                }
            }
        }

        let finally_target = if finally.is_empty() {
            None
        } else {
            let target = self.instructions.len();
            let _ = self.compile_branch_items(finally)?;
            self.emit(Instruction::EndFinally);
            Some(target)
        };

        let end = self.instructions.len();
        for jump in end_jumps {
            self.patch_jump(jump, end)?;
        }
        self.patch_enter_try(enter, handlers, finally_target, end)?;
        self.locals = saved_locals;
        self.returned = saved_returned;
        Ok(dst)
    }

    fn compile_try_catch_dispatcher(
        &mut self,
        dst: Register,
        error: Register,
        catches: &[HirCatch],
        finally: &[HirItem],
        end_jumps: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        for catch in catches {
            if let Some(binding) = catch.binding {
                self.locals.insert(binding, error);
            }
            let branch = self
                .compile_catch_match(error, catch.condition.as_ref())?
                .map(|condition| self.emit_branch(condition, 0, 0));
            let body_target = self.instructions.len();
            let (value, returned) = self.compile_branch_items(&catch.body)?;
            if let Some(value) = value {
                self.emit(Instruction::Move { dst, src: value });
            }
            if !returned {
                if finally.is_empty() {
                    end_jumps.push(self.emit_jump(0));
                } else {
                    self.emit(Instruction::ExitTry);
                }
            }
            let next_target = self.instructions.len();
            if let Some(branch) = branch {
                self.patch_branch(branch, body_target, next_target)?;
            } else {
                return Ok(());
            }
        }
        self.emit(Instruction::Raise {
            error: Operand::Register(error),
            message: None,
            value: None,
        });
        Ok(())
    }

    fn compile_recover(
        &mut self,
        id: NodeId,
        expr: &HirExpr,
        catches: &[HirRecovery],
    ) -> Result<Register, CompileError> {
        let saved_locals = self.locals.clone();
        let saved_returned = self.returned;
        self.returned = false;

        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::nothing(),
        });

        let has_dynamic_catch = catches
            .iter()
            .any(|catch| !is_static_catch_condition(catch.condition.as_ref()));
        let mut handlers = if has_dynamic_catch {
            let error = self.alloc_register();
            self.emit(Instruction::Load {
                dst: error,
                value: Value::nothing(),
            });
            vec![CatchHandler {
                code: None,
                binding: Some(error),
                target: 0,
            }]
        } else {
            catches
                .iter()
                .map(|catch| {
                    let binding = catch.binding.map(|binding| {
                        let register = self.alloc_register();
                        self.emit(Instruction::Load {
                            dst: register,
                            value: Value::nothing(),
                        });
                        self.locals.insert(binding, register);
                        register
                    });
                    let code = self.catch_code(id, catch.condition.as_ref())?;
                    Ok(CatchHandler {
                        code,
                        binding,
                        target: 0,
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?
        };

        let enter = self.instructions.len();
        self.emit(Instruction::EnterTry {
            catches: handlers.clone(),
            finally: None,
            end: 0,
        });

        let value = self.compile_expr_for_value(expr)?;
        if !self.returned {
            self.emit(Instruction::Move { dst, src: value });
            self.emit(Instruction::ExitTry);
        }

        let mut end_jumps = Vec::new();
        if has_dynamic_catch {
            let error = handlers[0]
                .binding
                .expect("dynamic recover handler binds error");
            handlers[0].target = self.instructions.len();
            self.compile_recover_catch_dispatcher(dst, error, catches, &mut end_jumps)?;
        } else {
            for (idx, catch) in catches.iter().enumerate() {
                handlers[idx].target = self.instructions.len();
                let saved_branch_returned = self.returned;
                self.returned = false;
                let value = self.compile_expr_for_value(&catch.value)?;
                if !self.returned {
                    self.emit(Instruction::Move { dst, src: value });
                    end_jumps.push(self.emit_jump(0));
                }
                self.returned = saved_branch_returned;
            }
        }

        let end = self.instructions.len();
        for jump in end_jumps {
            self.patch_jump(jump, end)?;
        }
        self.patch_enter_try(enter, handlers, None, end)?;
        self.locals = saved_locals;
        self.returned = saved_returned;
        Ok(dst)
    }

    fn compile_recover_catch_dispatcher(
        &mut self,
        dst: Register,
        error: Register,
        catches: &[HirRecovery],
        end_jumps: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        for catch in catches {
            if let Some(binding) = catch.binding {
                self.locals.insert(binding, error);
            }
            let branch = self
                .compile_catch_match(error, catch.condition.as_ref())?
                .map(|condition| self.emit_branch(condition, 0, 0));
            let body_target = self.instructions.len();
            let saved_branch_returned = self.returned;
            self.returned = false;
            let value = self.compile_expr_for_value(&catch.value)?;
            if !self.returned {
                self.emit(Instruction::Move { dst, src: value });
                end_jumps.push(self.emit_jump(0));
            }
            self.returned = saved_branch_returned;
            let next_target = self.instructions.len();
            if let Some(branch) = branch {
                self.patch_branch(branch, body_target, next_target)?;
            } else {
                return Ok(());
            }
        }
        self.emit(Instruction::Raise {
            error: Operand::Register(error),
            message: None,
            value: None,
        });
        Ok(())
    }

    fn catch_code(
        &self,
        id: NodeId,
        condition: Option<&HirExpr>,
    ) -> Result<Option<Value>, CompileError> {
        let Some(condition) = condition else {
            return Ok(None);
        };
        match condition {
            HirExpr::Literal {
                value: Literal::ErrorCode(code),
                ..
            } => Ok(Some(Value::error_code(Symbol::intern(code)))),
            _ => Err(self.unsupported(
                id,
                "compiled catch clauses currently match an error code literal or catch all",
            )),
        }
    }

    fn compile_catch_match(
        &mut self,
        error: Register,
        condition: Option<&HirExpr>,
    ) -> Result<Option<Register>, CompileError> {
        let Some(condition) = condition else {
            return Ok(None);
        };
        match condition {
            HirExpr::Literal {
                value: Literal::ErrorCode(code),
                ..
            } => self
                .compile_error_code_match(error, Symbol::intern(code))
                .map(Some),
            _ => self.compile_expr_for_value(condition).map(Some),
        }
    }

    fn compile_error_code_match(
        &mut self,
        error: Register,
        code: Symbol,
    ) -> Result<Register, CompileError> {
        let actual = self.alloc_register();
        self.emit(Instruction::ErrorField {
            dst: actual,
            error,
            field: ErrorField::Code,
        });
        let expected = self.alloc_register();
        self.emit(Instruction::Load {
            dst: expected,
            value: Value::error_code(code),
        });
        let matched = self.alloc_register();
        self.emit(Instruction::Binary {
            dst: matched,
            op: RuntimeBinaryOp::Eq,
            left: actual,
            right: expected,
        });
        Ok(matched)
    }

    fn compile_break(&mut self, id: NodeId) -> Result<Register, CompileError> {
        if self.loops.is_empty() {
            return Err(self.unsupported(id, "break is only valid inside a loop"));
        }
        let jump = self.emit_jump(0);
        self.loops
            .last_mut()
            .expect("loop stack was checked above")
            .break_jumps
            .push(jump);
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::nothing(),
        });
        Ok(dst)
    }

    fn compile_continue(&mut self, id: NodeId) -> Result<Register, CompileError> {
        if self.loops.is_empty() {
            return Err(self.unsupported(id, "continue is only valid inside a loop"));
        }
        let jump = self.emit_jump(0);
        self.loops
            .last_mut()
            .expect("loop stack was checked above")
            .continue_jumps
            .push(jump);
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: Value::nothing(),
        });
        Ok(dst)
    }

    fn compile_branch_items(
        &mut self,
        items: &[HirItem],
    ) -> Result<(Option<Register>, bool), CompileError> {
        let saved = self.locals.clone();
        let saved_returned = self.returned;
        self.returned = false;
        let mut value = None;
        for item in items {
            value = self.compile_item(item)?;
        }
        let branch_returned = self.returned;
        self.locals = saved;
        self.returned = saved_returned;
        Ok((value, branch_returned))
    }

    fn compile_dispatch(
        &mut self,
        id: NodeId,
        selector: &HirExpr,
        args: &[HirArg],
        receiver: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        let method_relations = self
            .context
            .method_relations
            .ok_or_else(|| self.unsupported(id, "method relation ids are not configured"))?;
        let selector = self.compile_expr_for_operand(selector)?;
        if args.iter().any(|arg| arg.splice) {
            let receiver = receiver
                .map(|receiver| self.compile_expr_for_value(receiver).map(Operand::Register))
                .transpose()?;
            let roles = self.compile_role_map_items(receiver, args)?;
            let roles = self.alloc_map(roles);
            let dst = self.alloc_register();
            self.emit(Instruction::DynamicDispatch {
                dst,
                relations: method_relations.dispatch,
                program_relation: method_relations.method_program,
                program_bytes: method_relations.program_bytes,
                selector,
                roles: Operand::Register(roles),
            });
            return Ok(dst);
        }
        let mut roles = Vec::new();
        if let Some(receiver) = receiver {
            roles.push((
                Value::symbol(Symbol::intern("receiver")),
                Operand::Register(self.compile_expr_for_value(receiver)?),
            ));
        }
        for arg in args {
            let Some(role) = &arg.role else {
                return Err(
                    self.unsupported(arg.id, "dispatch arguments must use explicit role names")
                );
            };
            roles.push((
                Value::symbol(Symbol::intern(role)),
                self.compile_arg_operand(arg)?,
            ));
        }
        let dst = self.alloc_register();
        self.emit(Instruction::Dispatch {
            dst,
            relations: method_relations.dispatch,
            program_relation: method_relations.method_program,
            program_bytes: method_relations.program_bytes,
            selector,
            roles,
        });
        Ok(dst)
    }

    fn compile_spawn(
        &mut self,
        id: NodeId,
        target: &HirExpr,
        delay: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        let (selector, args, receiver) = match target {
            HirExpr::RoleDispatch { selector, args, .. } => (selector.as_ref(), args, None),
            HirExpr::ReceiverDispatch {
                receiver,
                selector,
                args,
                ..
            } => (selector.as_ref(), args, Some(receiver.as_ref())),
            _ => {
                return Err(
                    self.unsupported(id, "spawn expects a role or receiver dispatch target")
                );
            }
        };
        if let Some(receiver) = receiver
            && args.iter().all(|arg| arg.role.is_none())
        {
            return self.compile_receiver_positional_spawn(id, receiver, selector, args, delay);
        }
        if receiver.is_none() && args.iter().all(|arg| arg.role.is_none()) {
            return self.compile_positional_spawn(id, selector, args, delay);
        }
        let selector = self.compile_expr_for_operand(selector)?;
        if args.iter().any(|arg| arg.splice) {
            let receiver = receiver
                .map(|receiver| self.compile_expr_for_value(receiver).map(Operand::Register))
                .transpose()?;
            let roles = self.compile_role_map_items(receiver, args)?;
            let roles = self.alloc_map(roles);
            let delay = delay
                .map(|delay| self.compile_expr_for_operand(delay))
                .transpose()?;
            let dst = self.alloc_register();
            self.emit(Instruction::SpawnDispatchDynamic {
                dst,
                selector,
                roles: Operand::Register(roles),
                delay,
            });
            return Ok(dst);
        }
        let mut roles = Vec::new();
        if let Some(receiver) = receiver {
            roles.push((
                Value::symbol(Symbol::intern("receiver")),
                Operand::Register(self.compile_expr_for_value(receiver)?),
            ));
        }
        for arg in args {
            let Some(role) = &arg.role else {
                return Err(
                    self.unsupported(arg.id, "spawn arguments must use explicit role names")
                );
            };
            roles.push((
                Value::symbol(Symbol::intern(role)),
                self.compile_arg_operand(arg)?,
            ));
        }
        let delay = delay
            .map(|delay| self.compile_expr_for_operand(delay))
            .transpose()?;
        let dst = self.alloc_register();
        self.emit(Instruction::SpawnDispatch {
            dst,
            selector,
            roles,
            delay,
        });
        Ok(dst)
    }

    fn compile_positional_spawn(
        &mut self,
        id: NodeId,
        selector: &HirExpr,
        args: &[HirArg],
        delay: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(
                self.unsupported(id, "positional spawn calls do not accept named arguments")
            );
        }
        let selector = self.compile_expr_for_operand(selector)?;
        let delay = delay
            .map(|delay| self.compile_expr_for_operand(delay))
            .transpose()?;
        let dst = self.alloc_register();
        if args.iter().any(|arg| arg.splice) {
            let args = self.compile_arg_items(args)?;
            self.emit(Instruction::SpawnPositionalDispatchDynamic {
                dst,
                selector,
                args,
                delay,
            });
        } else {
            let args = args
                .iter()
                .map(|arg| self.compile_arg_operand(arg))
                .collect::<Result<Vec<_>, _>>()?;
            self.emit(Instruction::SpawnPositionalDispatch {
                dst,
                selector,
                args,
                delay,
            });
        }
        Ok(dst)
    }

    fn compile_receiver_positional_spawn(
        &mut self,
        id: NodeId,
        receiver: &HirExpr,
        selector: &HirExpr,
        args: &[HirArg],
        delay: Option<&HirExpr>,
    ) -> Result<Register, CompileError> {
        if args.iter().any(|arg| arg.role.is_some()) {
            return Err(self.unsupported(
                id,
                "receiver positional spawn calls do not accept named arguments",
            ));
        }
        let selector = self.compile_expr_for_operand(selector)?;
        let receiver = self.compile_expr_for_value(receiver)?;
        let delay = delay
            .map(|delay| self.compile_expr_for_operand(delay))
            .transpose()?;
        let dst = self.alloc_register();
        if args.iter().any(|arg| arg.splice) {
            let mut items = Vec::with_capacity(args.len() + 1);
            items.push(ListItem::Value(Operand::Register(receiver)));
            items.extend(self.compile_arg_items(args)?);
            self.emit(Instruction::SpawnPositionalDispatchDynamic {
                dst,
                selector,
                args: items,
                delay,
            });
        } else {
            let mut operands = Vec::with_capacity(args.len() + 1);
            operands.push(Operand::Register(receiver));
            for arg in args {
                operands.push(self.compile_arg_operand(arg)?);
            }
            self.emit(Instruction::SpawnPositionalDispatch {
                dst,
                selector,
                args: operands,
                delay,
            });
        }
        Ok(dst)
    }

    fn compile_expr_for_operand(&mut self, expr: &HirExpr) -> Result<Operand, CompileError> {
        match expr {
            HirExpr::Symbol { name, .. } => Ok(Operand::Value(Value::symbol(Symbol::intern(name)))),
            HirExpr::Identity { id, name } => {
                let identity =
                    self.context
                        .identity(name)
                        .ok_or_else(|| CompileError::UnknownIdentity {
                            node: *id,
                            span: self.span(*id),
                            name: name.clone(),
                        })?;
                Ok(Operand::Value(Value::identity(identity)))
            }
            HirExpr::Literal { id, value } => Ok(Operand::Value(self.literal_value(*id, value)?)),
            _ => Ok(Operand::Register(self.compile_expr_for_value(expr)?)),
        }
    }

    fn compile_relation_exists(
        &mut self,
        atom: &HirRelationAtom,
    ) -> Result<Register, CompileError> {
        let relation = self.relation_id(atom)?;
        if atom.args.iter().any(|arg| arg.splice) {
            let dst = self.alloc_register();
            let args = self.compile_relation_arg_items(&atom.args)?;
            self.emit(Instruction::ScanDynamic {
                dst,
                relation,
                args,
            });
            return Ok(dst);
        }
        let outputs = query_outputs(&atom.args);
        if !outputs.is_empty() {
            return self.compile_relation_query(relation, atom, outputs);
        }
        let dst = self.alloc_register();
        let bindings = atom
            .args
            .iter()
            .map(|arg| self.compile_arg_operand(arg).map(Some))
            .collect::<Result<Vec<_>, _>>()?;
        self.emit(Instruction::ScanExists {
            dst,
            relation,
            bindings,
        });
        Ok(dst)
    }

    fn compile_relation_query(
        &mut self,
        relation: RelationId,
        atom: &HirRelationAtom,
        outputs: Vec<QueryBinding>,
    ) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        let bindings = atom
            .args
            .iter()
            .map(|arg| match &arg.value {
                HirExpr::QueryVar { .. } | HirExpr::Hole { .. } => Ok(None),
                _ => self.compile_arg_operand(arg).map(Some),
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.emit(Instruction::ScanBindings {
            dst,
            relation,
            bindings,
            outputs,
        });
        Ok(dst)
    }

    fn compile_fact_change(
        &mut self,
        kind: &EffectKind,
        atom: &HirRelationAtom,
    ) -> Result<(), CompileError> {
        let relation = self.relation_id(atom)?;
        if atom.args.iter().any(|arg| arg.splice) {
            let args = self.compile_relation_arg_items(&atom.args)?;
            match kind {
                EffectKind::Assert => self.emit(Instruction::AssertDynamic { relation, args }),
                EffectKind::Retract => self.emit(Instruction::RetractDynamic { relation, args }),
                EffectKind::Require => {
                    return Err(
                        self.unsupported(atom.id, "require is not a fact change instruction")
                    );
                }
            }
            return Ok(());
        }
        match kind {
            EffectKind::Assert => {
                let values = atom
                    .args
                    .iter()
                    .map(|arg| self.compile_arg_operand(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.emit(Instruction::Assert { relation, values });
            }
            EffectKind::Retract => {
                if atom
                    .args
                    .iter()
                    .any(|arg| matches!(arg.value, HirExpr::QueryVar { .. } | HirExpr::Hole { .. }))
                {
                    let bindings = atom
                        .args
                        .iter()
                        .map(|arg| match &arg.value {
                            HirExpr::QueryVar { .. } | HirExpr::Hole { .. } => Ok(None),
                            _ => self.compile_arg_operand(arg).map(Some),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    self.emit(Instruction::RetractWhere { relation, bindings });
                } else {
                    let values = atom
                        .args
                        .iter()
                        .map(|arg| self.compile_arg_operand(arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.emit(Instruction::Retract { relation, values });
                }
            }
            EffectKind::Require => {
                return Err(self.unsupported(atom.id, "require is not a fact change instruction"));
            }
        }
        Ok(())
    }

    fn compile_arg_operand(&mut self, arg: &HirArg) -> Result<Operand, CompileError> {
        Ok(Operand::Register(self.compile_expr_for_value(&arg.value)?))
    }

    fn compile_relation_arg_items(
        &mut self,
        args: &[HirArg],
    ) -> Result<Vec<RelationArg>, CompileError> {
        args.iter()
            .map(|arg| {
                if arg.splice {
                    return self.compile_arg_operand(arg).map(RelationArg::Splice);
                }
                Ok(match &arg.value {
                    HirExpr::QueryVar { name, .. } => RelationArg::Query(Symbol::intern(name)),
                    HirExpr::Hole { .. } => RelationArg::Hole,
                    _ => RelationArg::Value(self.compile_arg_operand(arg)?),
                })
            })
            .collect()
    }

    fn relation_id(&self, atom: &HirRelationAtom) -> Result<RelationId, CompileError> {
        self.context
            .relation(&atom.name)
            .ok_or_else(|| CompileError::UnknownRelation {
                node: atom.id,
                span: self.span(atom.id),
                name: atom.name.clone(),
            })
    }

    fn load_usize(&mut self, value: usize, id: NodeId) -> Result<Register, CompileError> {
        let dst = self.alloc_register();
        self.emit(Instruction::Load {
            dst,
            value: self.usize_value(value, id)?,
        });
        Ok(dst)
    }

    fn usize_operand(&self, value: usize, id: NodeId) -> Result<Operand, CompileError> {
        Ok(Operand::Value(self.usize_value(value, id)?))
    }

    fn usize_value(&self, value: usize, id: NodeId) -> Result<Value, CompileError> {
        let value = i64::try_from(value).map_err(|error| CompileError::InvalidLiteral {
            node: id,
            span: self.span(id),
            message: format!("scatter index is too large: {error}"),
        })?;
        Value::int(value).map_err(|error| self.value_error(id, error))
    }

    fn literal_value(&self, id: NodeId, literal: &Literal) -> Result<Value, CompileError> {
        match literal {
            Literal::Int(value) => {
                let value = value
                    .parse::<i64>()
                    .map_err(|error| CompileError::InvalidLiteral {
                        node: id,
                        span: self.span(id),
                        message: format!("invalid integer literal: {error}"),
                    })?;
                Value::int(value).map_err(|error| self.value_error(id, error))
            }
            Literal::Float(value) => {
                let value =
                    parse_float_literal(value).map_err(|message| CompileError::InvalidLiteral {
                        node: id,
                        span: self.span(id),
                        message,
                    })?;
                Ok(value)
            }
            Literal::String(value) => Ok(Value::string(value)),
            Literal::Bytes(value) => Ok(Value::bytes(value)),
            Literal::Bool(value) => Ok(Value::bool(*value)),
            Literal::ErrorCode(value) => Ok(Value::error_code(Symbol::intern(value))),
            Literal::Nothing => Ok(Value::nothing()),
            Literal::Unit => Ok(Value::unit()),
        }
    }

    fn value_error(&self, id: NodeId, error: ValueError) -> CompileError {
        CompileError::InvalidLiteral {
            node: id,
            span: self.span(id),
            message: format!("{error:?}"),
        }
    }

    fn alloc_register(&mut self) -> Register {
        let register = Register(self.next_register);
        self.next_register += 1;
        register
    }

    fn emit(&mut self, instruction: Instruction) {
        self.instructions
            .emit(instruction)
            .expect("compiler emitted bytecode within compact table limits");
    }

    fn emit_branch(&mut self, condition: Register, if_true: usize, if_false: usize) -> usize {
        self.instructions
            .emit_branch(condition, if_true, if_false)
            .expect("compiler emitted bytecode within compact table limits")
    }

    fn emit_jump(&mut self, target: usize) -> usize {
        self.instructions
            .emit_jump(target)
            .expect("compiler emitted bytecode within compact table limits")
    }

    fn patch_branch(
        &mut self,
        index: usize,
        if_true: usize,
        if_false: usize,
    ) -> Result<(), CompileError> {
        self.instructions
            .patch_branch(index, if_true, if_false)
            .map_err(internal_bytecode_error)
    }

    fn patch_true_target(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        self.instructions
            .patch_true_target(index, target)
            .map_err(internal_bytecode_error)
    }

    fn patch_false_target(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        self.instructions
            .patch_false_target(index, target)
            .map_err(internal_bytecode_error)
    }

    fn patch_jump(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        self.instructions
            .patch_jump(index, target)
            .map_err(internal_bytecode_error)
    }

    fn patch_enter_try(
        &mut self,
        index: usize,
        new_catches: Vec<CatchHandler>,
        new_finally: Option<usize>,
        new_end: usize,
    ) -> Result<(), CompileError> {
        self.instructions
            .patch_enter_try(index, new_catches, new_finally, new_end)
            .map_err(internal_bytecode_error)
    }

    fn enforce_binding_kind(
        &mut self,
        node: NodeId,
        binding: BindingId,
        source: &HirExpr,
        value: Register,
    ) -> Result<(), CompileError> {
        let inferred_type = self.infer_expr_static_type(source);
        let inferred_kinds = inferred_type.outer_kinds();
        self.enforce_inferred_binding_type(node, binding, inferred_type, inferred_kinds, value)
    }

    fn enforce_inferred_binding_kind(
        &mut self,
        node: NodeId,
        binding: BindingId,
        inferred: KindSet,
        value: Register,
    ) -> Result<(), CompileError> {
        let inferred_type = static_type_from_kind_set(inferred);
        self.enforce_inferred_binding_type(node, binding, inferred_type, inferred, value)
    }

    fn enforce_inferred_binding_type(
        &mut self,
        node: NodeId,
        binding: BindingId,
        inferred_type: StaticType,
        inferred_kinds: KindSet,
        value: Register,
    ) -> Result<(), CompileError> {
        let binding = &self.semantic.bindings[binding.as_u32() as usize];
        let Some(expected_type) = binding.declared_type.as_ref() else {
            return Ok(());
        };
        if inferred_type.is_subtype_of(expected_type) {
            return Ok(());
        }
        if inferred_type.is_disjoint(expected_type) {
            if let StaticType::Kind(expected) = expected_type {
                let inferred = inferred_kinds
                    .singleton()
                    .map_or_else(|| inferred_kinds.names(), |kind| kind.name().to_owned());
                return Err(CompileError::ValueKindMismatch {
                    node,
                    span: self.span(node),
                    subject: binding.name.clone(),
                    expected: *expected,
                    inferred,
                });
            }
            return Err(CompileError::StaticTypeMismatch {
                node,
                span: self.span(node),
                subject: binding.name.clone(),
                expected: expected_type.to_string(),
                inferred: inferred_type.to_string(),
            });
        }
        self.emit_type_check(
            value,
            expected_type,
            KindCheckSite::Binding,
            Symbol::intern(&binding.name),
        );
        Ok(())
    }

    fn enforce_compiled_parameter(
        &mut self,
        param: &FunctionParamInfo,
        source: Option<&HirExpr>,
        value: Register,
    ) -> Result<(), CompileError> {
        let inferred_type = source.map_or(StaticType::Dynamic, |source| {
            self.infer_expr_static_type(source)
        });
        let inferred = inferred_type.outer_kinds();
        let node = source.map_or(param.id, expr_id);
        if self.parameter_needs_check(param, &inferred_type, inferred, node)? {
            self.emit_parameter_check(param, value);
        }
        Ok(())
    }

    fn parameter_needs_check(
        &self,
        param: &FunctionParamInfo,
        inferred_type: &StaticType,
        inferred: KindSet,
        node: NodeId,
    ) -> Result<bool, CompileError> {
        let Some(expected_type) = param.declared_type.as_ref() else {
            return Ok(false);
        };
        if inferred_type.is_subtype_of(expected_type) {
            return Ok(false);
        }
        if inferred_type.is_disjoint(expected_type) {
            let parameter = self.semantic.bindings[param.binding.as_u32() as usize]
                .name
                .clone();
            if let StaticType::Kind(expected) = expected_type {
                return Err(CompileError::ParameterKindMismatch {
                    node,
                    span: self.span(node),
                    parameter,
                    expected: *expected,
                    inferred: inferred.names(),
                });
            }
            return Err(CompileError::StaticTypeMismatch {
                node,
                span: self.span(node),
                subject: parameter,
                expected: expected_type.to_string(),
                inferred: inferred_type.to_string(),
            });
        }
        Ok(true)
    }

    fn emit_parameter_check(&mut self, param: &FunctionParamInfo, value: Register) {
        self.emit_declared_parameter_check(param.binding, param.declared_type.as_ref(), value);
    }

    fn emit_method_parameter_check(&mut self, param: &HirMethodParam, value: Register) {
        self.emit_declared_parameter_check(param.binding, param.declared_type.as_ref(), value);
    }

    fn emit_declared_parameter_check(
        &mut self,
        binding: BindingId,
        expected: Option<&StaticType>,
        value: Register,
    ) {
        let Some(expected) = expected else {
            return;
        };
        let subject = &self.semantic.bindings[binding.as_u32() as usize].name;
        self.emit_type_check(
            value,
            expected,
            KindCheckSite::Parameter,
            Symbol::intern(subject),
        );
    }

    fn emit_type_check(
        &mut self,
        value: Register,
        expected: &StaticType,
        site: KindCheckSite,
        subject: Symbol,
    ) {
        if let StaticType::Kind(expected) = expected {
            self.emit(Instruction::CheckKind {
                value,
                expected: *expected,
                site,
                subject,
            });
            return;
        }
        self.emit(Instruction::CheckType {
            value,
            expected: type_contract(expected),
            site,
            subject,
        });
    }

    fn infer_expr_kinds(&self, source: &HirExpr) -> KindSet {
        let direct_result = |binding| {
            self.functions
                .get(&binding)
                .map(|function| function.result_kinds)
        };
        let runtime_result = |name: &str| runtime_result_kinds(self.context, name);
        KindInference::new(&self.semantic.bindings, &direct_result, &runtime_result).expr(source)
    }

    fn infer_relation_static_type(
        &self,
        heading: &[String],
        rows: &[Vec<HirExpr>],
    ) -> crate::StaticType {
        let direct_result = |binding| {
            self.functions
                .get(&binding)
                .map(|function| function.result_kinds)
        };
        let runtime_result = |name: &str| runtime_result_kinds(self.context, name);
        StaticTypeInference::new(&self.semantic.bindings, &direct_result, &runtime_result)
            .with_locals(&self.local_types)
            .relation(heading, rows)
    }

    fn infer_expr_static_type(&self, source: &HirExpr) -> StaticType {
        if let HirExpr::Call { callee, .. } = source {
            match callee.as_ref() {
                HirExpr::LocalRef { binding, .. } => {
                    if let Some(function) = self.functions.get(binding) {
                        return function.result_type.clone();
                    }
                }
                HirExpr::ExternalRef { name, .. } => {
                    if let Some(BuiltinResultKind::Structural(contract)) =
                        self.context.runtime_function_result(name)
                    {
                        return static_type_from_contract(&contract);
                    }
                }
                _ => {}
            }
        }
        let direct_result = |binding| {
            self.functions
                .get(&binding)
                .map(|function| function.result_kinds)
        };
        let runtime_result = |name: &str| runtime_result_kinds(self.context, name);
        StaticTypeInference::new(&self.semantic.bindings, &direct_result, &runtime_result)
            .with_locals(&self.local_types)
            .expr(source)
    }

    fn record_local_type(&mut self, binding: BindingId, source: &HirExpr) {
        let metadata = &self.semantic.bindings[binding.0 as usize];
        if metadata.assigned || metadata.declared_type.is_some() {
            return;
        }
        self.local_types
            .insert(binding, self.infer_expr_static_type(source));
    }

    fn infer_simple_function_result_type(&self, body: &HirFunctionBody) -> StaticType {
        match body {
            HirFunctionBody::Expr(expr) => self.infer_expr_static_type(expr),
            HirFunctionBody::Block(items) => {
                let mut returns = Vec::new();
                collect_return_values(items, &mut returns);
                match items.last() {
                    Some(HirItem::Expr { expr, .. }) if !self.infer_expr_kinds(expr).is_empty() => {
                        returns.push(Some(expr));
                    }
                    None => returns.push(None),
                    _ => {}
                }
                StaticType::union(returns.into_iter().map(|value| {
                    value.map_or(StaticType::Literal(StaticLiteral::Unit), |value| {
                        self.infer_expr_static_type(value)
                    })
                }))
            }
        }
    }

    fn unsupported(&self, node: NodeId, message: impl Into<String>) -> CompileError {
        CompileError::Unsupported {
            node,
            span: self.span(node),
            message: message.into(),
        }
    }

    fn span(&self, node: NodeId) -> Option<Span> {
        self.semantic.span(node).cloned()
    }
}

fn runtime_result_kinds(context: &CompileContext, name: &str) -> Option<KindSet> {
    match context.runtime_function_result(name)? {
        BuiltinResultKind::Dynamic => None,
        BuiltinResultKind::Exact(kind) => Some(KindSet::exact(kind)),
        BuiltinResultKind::Structural(contract) => contract.exact_outer_kind().map(KindSet::exact),
    }
}

fn relation_pattern_spec(pattern: &HirMatchPattern) -> Option<RelationPatternSpec> {
    let (columns, row_count, equalities, bindings) = match pattern {
        HirMatchPattern::Wildcard => return None,
        HirMatchPattern::None => (vec!["value"], 0, Vec::new(), Vec::new()),
        HirMatchPattern::Some(binding) => (vec!["value"], 1, Vec::new(), vec![("value", *binding)]),
        HirMatchPattern::Ok(binding) => (
            vec!["case", "value"],
            1,
            vec![("case", Value::symbol(Symbol::intern("ok")))],
            vec![("value", *binding)],
        ),
        HirMatchPattern::Err(binding) => (
            vec!["case", "value"],
            1,
            vec![("case", Value::symbol(Symbol::intern("error")))],
            vec![("value", *binding)],
        ),
        HirMatchPattern::Row(fields) => (
            fields.iter().map(|(column, _)| column.as_str()).collect(),
            1,
            Vec::new(),
            fields
                .iter()
                .map(|(column, binding)| (column.as_str(), *binding))
                .collect(),
        ),
    };
    let mut heading = columns.into_iter().map(Symbol::intern).collect::<Vec<_>>();
    heading.sort_unstable();
    Some(RelationPatternSpec {
        heading,
        row_count,
        equalities: equalities
            .into_iter()
            .map(|(column, value)| (Symbol::intern(column), value))
            .collect(),
        bindings: bindings
            .into_iter()
            .map(|(column, binding)| (Symbol::intern(column), binding))
            .collect(),
    })
}

fn match_is_exhaustive(scrutinee: &StaticType, cases: &[HirMatchCase]) -> bool {
    let unguarded = |pattern: fn(&HirMatchPattern) -> bool| {
        cases
            .iter()
            .any(|case| case.guard.is_none() && pattern(&case.pattern))
    };
    if unguarded(|pattern| matches!(pattern, HirMatchPattern::Wildcard)) {
        return true;
    }
    let StaticType::Relation(relation) = scrutinee else {
        return false;
    };
    let heading = relation.heading().collect::<Vec<_>>();
    if heading == ["value"] && relation.cardinality().max == Some(1) {
        let none = relation.cardinality().min == 0;
        return (!none || unguarded(|pattern| matches!(pattern, HirMatchPattern::None)))
            && unguarded(|pattern| matches!(pattern, HirMatchPattern::Some(_)));
    }
    if heading == ["case", "value"] && relation.cardinality().min == 1 {
        let has_case = |name: &str| {
            relation.alternatives().iter().any(|row| {
                row.columns().iter().any(|(column, ty)| {
                    column == "case"
                        && *ty == StaticType::Literal(StaticLiteral::Symbol(name.to_owned()))
                })
            })
        };
        return (!has_case("ok") || unguarded(|pattern| matches!(pattern, HirMatchPattern::Ok(_))))
            && (!has_case("error")
                || unguarded(|pattern| matches!(pattern, HirMatchPattern::Err(_))));
    }
    relation.cardinality().min == 1
        && relation.cardinality().max == Some(1)
        && cases.iter().any(|case| {
            case.guard.is_none()
                && matches!(&case.pattern, HirMatchPattern::Row(fields)
                    if fields.iter().map(|(column, _)| column.as_str()).collect::<BTreeSet<_>>()
                        == heading.iter().copied().collect())
        })
}

fn match_binding_type(
    scrutinee: &StaticType,
    pattern: &HirMatchPattern,
    column: Symbol,
) -> StaticType {
    let StaticType::Relation(relation) = scrutinee else {
        return StaticType::Dynamic;
    };
    let Some(column) = column.name() else {
        return StaticType::Dynamic;
    };
    let discriminator = match pattern {
        HirMatchPattern::Ok(_) => Some("ok"),
        HirMatchPattern::Err(_) => Some("error"),
        _ => None,
    };
    StaticType::union(relation.alternatives().iter().filter_map(|row| {
        if let Some(discriminator) = discriminator
            && !row.columns().iter().any(|(name, ty)| {
                name == "case"
                    && *ty == StaticType::Literal(StaticLiteral::Symbol(discriminator.to_owned()))
            })
        {
            return None;
        }
        row.columns()
            .iter()
            .find_map(|(name, ty)| (name == column).then(|| ty.clone()))
    }))
}

fn static_type_from_kind_set(kinds: KindSet) -> StaticType {
    if kinds == KindSet::ALL {
        return StaticType::Dynamic;
    }
    StaticType::union(kinds.iter().map(StaticType::Kind))
}

fn type_contract(ty: &StaticType) -> TypeContract {
    match ty {
        StaticType::Dynamic | StaticType::Parameter(_) | StaticType::Alias(_, _) => {
            TypeContract::Dynamic
        }
        StaticType::Never => TypeContract::Never,
        StaticType::Kind(kind) => TypeContract::Kind(*kind),
        StaticType::Literal(literal) => TypeContract::Literal(match literal {
            StaticLiteral::Bool(value) => TypeLiteralContract::Bool(*value),
            StaticLiteral::Symbol(value) => TypeLiteralContract::Symbol(Symbol::intern(value)),
            StaticLiteral::ErrorCode(value) => {
                TypeLiteralContract::ErrorCode(Symbol::intern(value))
            }
            StaticLiteral::Unit => TypeLiteralContract::Unit,
            StaticLiteral::EmptyRelation => TypeLiteralContract::EmptyRelation,
        }),
        StaticType::Union(types) => TypeContract::Union(types.iter().map(type_contract).collect()),
        StaticType::Relation(relation) => TypeContract::Relation(relation_type_contract(relation)),
    }
}

fn static_type_from_contract(contract: &TypeContract) -> StaticType {
    match contract {
        TypeContract::Dynamic => StaticType::Dynamic,
        TypeContract::Never => StaticType::Never,
        TypeContract::Kind(kind) => StaticType::Kind(*kind),
        TypeContract::Literal(literal) => StaticType::Literal(match literal {
            TypeLiteralContract::Bool(value) => StaticLiteral::Bool(*value),
            TypeLiteralContract::Symbol(value) => {
                let Some(name) = value.name() else {
                    return StaticType::Dynamic;
                };
                StaticLiteral::Symbol(name.to_owned())
            }
            TypeLiteralContract::ErrorCode(value) => {
                let Some(name) = value.name() else {
                    return StaticType::Dynamic;
                };
                StaticLiteral::ErrorCode(name.to_owned())
            }
            TypeLiteralContract::Unit => StaticLiteral::Unit,
            TypeLiteralContract::EmptyRelation => StaticLiteral::EmptyRelation,
        }),
        TypeContract::Union(types) => {
            StaticType::union(types.iter().map(static_type_from_contract))
        }
        TypeContract::Relation(relation) => {
            let alternatives = relation
                .alternatives
                .iter()
                .map(|row| {
                    RowShape::new(row.columns.iter().map(|(name, ty)| {
                        (
                            name.name().unwrap_or("<unnamed>").to_owned(),
                            static_type_from_contract(ty),
                        )
                    }))
                })
                .collect::<Result<Vec<_>, _>>();
            let Ok(alternatives) = alternatives else {
                return StaticType::Dynamic;
            };
            let Ok(cardinality) = Cardinality::new(relation.minimum_rows, relation.maximum_rows)
            else {
                return StaticType::Dynamic;
            };
            RelationType::new(alternatives, cardinality)
                .map(StaticType::Relation)
                .unwrap_or(StaticType::Dynamic)
        }
    }
}

fn relation_type_contract(relation: &RelationType) -> RelationTypeContract {
    RelationTypeContract {
        alternatives: relation
            .alternatives()
            .iter()
            .map(row_type_contract)
            .collect(),
        minimum_rows: relation.cardinality().min,
        maximum_rows: relation.cardinality().max,
    }
}

fn row_type_contract(row: &RowShape) -> RelationRowContract {
    let mut columns = row
        .columns()
        .iter()
        .map(|(name, ty)| (Symbol::intern(name), type_contract(ty)))
        .collect::<Vec<_>>();
    columns.sort_by_key(|(name, _)| *name);
    RelationRowContract { columns }
}

fn collect_return_values<'a>(items: &'a [HirItem], returns: &mut Vec<Option<&'a HirExpr>>) {
    for item in items {
        if let HirItem::Expr { expr, .. } = item {
            collect_expr_return_values(expr, returns);
        }
    }
}

fn collect_expr_return_values<'a>(expr: &'a HirExpr, returns: &mut Vec<Option<&'a HirExpr>>) {
    match expr {
        HirExpr::Return { value, .. } => returns.push(value.as_deref()),
        HirExpr::If {
            then_items,
            elseif,
            else_items,
            ..
        } => {
            collect_return_values(then_items, returns);
            for (_, items) in elseif {
                collect_return_values(items, returns);
            }
            collect_return_values(else_items, returns);
        }
        HirExpr::Block { items, .. }
        | HirExpr::For { body: items, .. }
        | HirExpr::While { body: items, .. } => collect_return_values(items, returns),
        HirExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            collect_return_values(body, returns);
            for catch in catches {
                collect_return_values(&catch.body, returns);
            }
            collect_return_values(finally, returns);
        }
        HirExpr::Match { cases, .. } => {
            for case in cases {
                collect_return_values(&case.body, returns);
            }
        }
        HirExpr::Function { .. } => {}
        _ => {}
    }
}

fn expr_id(expr: &HirExpr) -> NodeId {
    match expr {
        HirExpr::Literal { id, .. }
        | HirExpr::LocalRef { id, .. }
        | HirExpr::ExternalRef { id, .. }
        | HirExpr::Identity { id, .. }
        | HirExpr::Frob { id, .. }
        | HirExpr::Symbol { id, .. }
        | HirExpr::QueryVar { id, .. }
        | HirExpr::Hole { id }
        | HirExpr::List { id, .. }
        | HirExpr::Relation { id, .. }
        | HirExpr::Map { id, .. }
        | HirExpr::Unary { id, .. }
        | HirExpr::Binary { id, .. }
        | HirExpr::Assign { id, .. }
        | HirExpr::Call { id, .. }
        | HirExpr::RoleDispatch { id, .. }
        | HirExpr::ReceiverDispatch { id, .. }
        | HirExpr::Spawn { id, .. }
        | HirExpr::FactChange { id, .. }
        | HirExpr::Require { id, .. }
        | HirExpr::Index { id, .. }
        | HirExpr::Field { id, .. }
        | HirExpr::Binding { id, .. }
        | HirExpr::If { id, .. }
        | HirExpr::Match { id, .. }
        | HirExpr::Block { id, .. }
        | HirExpr::For { id, .. }
        | HirExpr::While { id, .. }
        | HirExpr::Return { id, .. }
        | HirExpr::Raise { id, .. }
        | HirExpr::Recover { id, .. }
        | HirExpr::One { id, .. }
        | HirExpr::Break { id }
        | HirExpr::Continue { id }
        | HirExpr::Try { id, .. }
        | HirExpr::Function { id, .. }
        | HirExpr::Error { id } => *id,
        HirExpr::RelationAtom(atom) => atom.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_semantic;
    use mica_relation_kernel::{ConflictPolicy, RelationKernel, RelationMetadata, Tuple};
    use mica_runtime::{
        BuiltinContext, BuiltinRegistry, Emission, RuntimeError, SuspendKind, TaskManager,
        TaskOutcome,
    };
    use std::sync::Arc;

    fn id(raw: u64) -> Identity {
        Identity::new(raw).unwrap()
    }

    fn count_kind_checks(program: &Program) -> usize {
        program
            .instructions()
            .iter()
            .map(|instruction| match instruction {
                Instruction::CheckKind { .. } => 1,
                Instruction::LoadFunction { program, .. } | Instruction::Call { program, .. } => {
                    count_kind_checks(program)
                }
                _ => 0,
            })
            .sum()
    }

    fn collect_kind_checks(
        program: &Program,
        checks: &mut Vec<(ValueKind, KindCheckSite, Symbol)>,
    ) {
        for instruction in program.instructions() {
            match instruction {
                Instruction::CheckKind {
                    expected,
                    site,
                    subject,
                    ..
                } => checks.push((expected, site, subject)),
                Instruction::LoadFunction { program, .. } | Instruction::Call { program, .. } => {
                    collect_kind_checks(&program, checks);
                }
                _ => {}
            }
        }
    }

    fn emitted(value: Value) -> Emission {
        Emission::new(id(99), value)
    }

    #[derive(Debug)]
    struct SubmittedSourceTask {
        outcome: TaskOutcome,
    }

    fn submit_source_task(
        source: &str,
        context: &CompileContext,
        task_manager: &mut TaskManager,
    ) -> Result<SubmittedSourceTask, mica_runtime::TaskManagerError> {
        let compiled = compile_source(source, context).unwrap();
        let (_, outcome) = task_manager.submit(Arc::new(compiled.program.clone()))?;
        Ok(SubmittedSourceTask { outcome })
    }

    fn emit_first_arg(
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        let target = args[0]
            .as_identity()
            .ok_or_else(|| RuntimeError::InvalidEffectTarget(args[0].clone()))?;
        let value = args[1].clone();
        context.emit(target, value.clone())?;
        Ok(value)
    }

    fn invalid_result(
        _context: &mut BuiltinContext<'_, '_>,
        _args: &[Value],
    ) -> Result<Value, RuntimeError> {
        Ok(Value::relation(
            [Symbol::intern("case"), Symbol::intern("value")],
            [Tuple::from([
                Value::symbol(Symbol::intern("ok")),
                Value::string("wrong"),
            ])],
        )
        .expect("test relation is valid"))
    }

    fn dispatch_relations() -> MethodRelations {
        MethodRelations {
            dispatch: DispatchRelations {
                method_selector: id(40),
                param: id(41),
                delegates: id(42),
            },
            method_program: id(43),
            program_bytes: id(44),
        }
    }

    fn create_method_relations(kernel: &RelationKernel) {
        let relations = dispatch_relations();
        kernel
            .create_relation(
                RelationMetadata::new(
                    relations.dispatch.method_selector,
                    Symbol::intern("MethodSelector"),
                    2,
                )
                .with_index([1, 0]),
            )
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(relations.dispatch.param, Symbol::intern("Param"), 4)
                    .with_index([0, 1]),
            )
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(relations.dispatch.delegates, Symbol::intern("Delegates"), 3)
                    .with_index([0, 2, 1]),
            )
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                relations.method_program,
                Symbol::intern("MethodProgram"),
                2,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                relations.program_bytes,
                Symbol::intern("ProgramBytes"),
                2,
            ))
            .unwrap();
    }

    fn assign_test_method_identity(semantic: &mut SemanticProgram, name: &str) {
        let HirItem::Method { identity, .. } = &mut semantic.hir.items[0] else {
            panic!("expected method item");
        };
        *identity = Some(name.to_owned());
    }

    #[test]
    fn compiles_open_error_code_literals() {
        let context = CompileContext::new();
        let compiled = compile_source("return E_NOT_PORTABLE", &context).unwrap();
        assert_eq!(
            compiled.program.instructions(),
            &[
                Instruction::Load {
                    dst: Register(0),
                    value: Value::error_code(Symbol::intern("E_NOT_PORTABLE")),
                },
                Instruction::Return {
                    value: Operand::Register(Register(0)),
                },
                Instruction::Load {
                    dst: Register(1),
                    value: Value::unit(),
                },
            ]
        );
    }

    #[test]
    fn compiled_task_catches_raised_error_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "try\n\
               raise E_NOT_PORTABLE, \"That cannot be taken.\", :lamp\n\
             catch err if E_NOT_PORTABLE\n\
               return err\n\
             end",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::error(
                    Symbol::intern("E_NOT_PORTABLE"),
                    Some("That cannot be taken."),
                    Some(Value::symbol(Symbol::intern("lamp"))),
                ),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_supports_code_first_catch_binding_and_error_fields() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "try\n\
               raise E_NOT_PORTABLE, \"That cannot be taken.\", :lamp\n\
             catch E_NOT_PORTABLE as err\n\
               return (err.code == E_NOT_PORTABLE) and (err.message == \"That cannot be taken.\") and (err.value == :lamp)\n\
             end",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_conditional_try_catches_in_order() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let handled = try\n\
               raise E_NO_EXIT, \"No exit.\"\n\
             catch err if err.code == E_PERMISSION\n\
               false\n\
             catch err if err.code == E_NO_EXIT\n\
               err.message == \"No exit.\"\n\
             end\n\
             return handled",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_recover_reraises_when_no_conditional_catch_matches() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let handled = recover raise E_NO_EXIT\n\
             catch err if err.code == E_PERMISSION => 1\n\
             end\n\
             return handled",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Aborted { .. }));
    }

    #[test]
    fn compiled_task_runs_finally_during_return_unwind() {
        let cleaned = id(1);
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(cleaned, Symbol::intern("Cleaned"), 1))
            .unwrap();
        let context = CompileContext::new().with_relation("Cleaned", cleaned);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "try\n\
               return 7\n\
             finally\n\
               assert Cleaned(:done)\n\
             end",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(7).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(cleaned, &[Some(Value::symbol(Symbol::intern("done")))])
                .unwrap(),
            vec![Tuple::from([Value::symbol(Symbol::intern("done"))])]
        );
    }

    #[test]
    fn compiled_task_recovers_expression_errors_inline() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let fallback = recover raise E_NOT_PORTABLE\n\
             catch E_NOT_PORTABLE => 10\n\
             end\n\
             let untouched = recover 1\n\
             catch E_NOT_PORTABLE => 99\n\
             end\n\
             return fallback + untouched",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(11).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_supports_code_first_recover_binding() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let code = recover raise E_NO_EXIT, \"No exit.\"\n\
             catch E_NO_EXIT as err => err.code\n\
             end\n\
             return code == E_NO_EXIT",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiles_source_to_transactional_task_manager() {
        let located_in = id(1);
        let alice = id(10);
        let room = id(11);
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();
        let context = CompileContext::new()
            .with_relation("LocatedIn", located_in)
            .with_identity("alice", alice)
            .with_identity("room", room);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let actor = #alice\n\
             assert LocatedIn(actor, #room)\n\
             require LocatedIn(actor, #room)\n\
             return true",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        let tuples = task_manager
            .kernel()
            .snapshot()
            .scan(
                located_in,
                &[Some(Value::identity(alice)), Some(Value::identity(room))],
            )
            .unwrap();
        assert_eq!(
            tuples,
            vec![Tuple::from(
                [Value::identity(alice), Value::identity(room),]
            )]
        );
    }

    #[test]
    fn retract_with_hole_removes_matching_tuples() {
        let located_in = id(1);
        let alice = id(10);
        let room = id(11);
        let hallway = id(12);
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();
        let mut seed = kernel.begin();
        seed.assert(
            located_in,
            Tuple::from([Value::identity(alice), Value::identity(room)]),
        )
        .unwrap();
        seed.assert(
            located_in,
            Tuple::from([Value::identity(hallway), Value::identity(room)]),
        )
        .unwrap();
        seed.commit().unwrap();
        let context = CompileContext::new()
            .with_relation("LocatedIn", located_in)
            .with_identity("alice", alice)
            .with_identity("room", room)
            .with_identity("hallway", hallway);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "retract LocatedIn(#alice, _)\n\
             return true",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .kernel()
                .snapshot()
                .scan(located_in, &[Some(Value::identity(alice)), None])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(located_in, &[Some(Value::identity(hallway)), None])
                .unwrap(),
            vec![Tuple::from([
                Value::identity(hallway),
                Value::identity(room)
            ])]
        );
    }

    #[test]
    fn require_aborts_and_rolls_back_pending_asserts() {
        let located_in = id(1);
        let visible = id(2);
        let alice = id(10);
        let room = id(11);
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(visible, Symbol::intern("Visible"), 2))
            .unwrap();
        let context = CompileContext::new()
            .with_relation("LocatedIn", located_in)
            .with_relation("Visible", visible)
            .with_identity("alice", alice)
            .with_identity("room", room);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "assert LocatedIn(#alice, #room)\n\
             require Visible(#alice, #room)\n\
             return true",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Aborted { .. }));
        let tuples = task_manager
            .kernel()
            .snapshot()
            .scan(located_in, &[None, None])
            .unwrap();
        assert_eq!(tuples, vec![]);
    }

    #[test]
    fn compiled_task_builds_and_indexes_collections() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let values = [10, 20, 30]\n\
             let labels = {:answer -> values[1]}\n\
             return labels[:answer]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(20).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_slices_lists_with_ranges() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let values = [0, 1, 2, 3, 4]\n\
             let mid = values[1..3]\n\
             let tail = values[2.._]\n\
             return mid[0] + mid[1] + mid[2] + tail[0] + tail[1] + tail[2]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(15).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_list_splices() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let rest = [2, 3]\n\
             let values = [1, @rest, 4]\n\
             return values[0] + values[1] + values[2] + values[3]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(10).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_scatter_assignment_with_required_optional_and_rest_parts() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let [head, ?middle = 10, @tail] = [1, 2, 3, 4]\n\
             return head + middle + tail[0] + tail[1]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(10).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_uses_scatter_optional_defaults_and_empty_rest() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let [head, ?middle = 10, @tail] = [1]\n\
             return (head == 1) and (middle == 10) and (tail == [])",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_assigns_indexed_list_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let values = [10, 20, 30]\n\
             values[1] = 99\n\
             return values[1]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(99).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_assigns_indexed_map_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let counts = {:a -> 1}\n\
             counts[:a] = 2\n\
             counts[:b] = 3\n\
             return counts[:a] + counts[:b]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(5).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_assigns_indexed_values_inside_loop() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let values = [1, 2, 3]\n\
             for index, item in values\n\
               values[index] = item * 10\n\
             end\n\
             return values[0] + values[1] + values[2]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(60).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_reads_and_writes_declared_dot_relations() {
        let name = id(1);
        let lamp = id(10);
        let kernel = RelationKernel::new();
        let name_metadata = RelationMetadata::new(name, Symbol::intern("Name"), 2)
            .with_conflict_policy(ConflictPolicy::Functional {
                key_positions: vec![0],
            });
        kernel.create_relation(name_metadata.clone()).unwrap();
        let context = CompileContext::new()
            .with_relation_metadata(name_metadata)
            .with_dot_relation("name", name)
            .with_identity("lamp", lamp);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "#lamp.name = \"brass lamp\"\n\
             return #lamp.name",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::string("brass lamp"),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(name, &[Some(Value::identity(lamp)), None])
                .unwrap(),
            vec![Tuple::from([
                Value::identity(lamp),
                Value::string("brass lamp"),
            ])]
        );
    }

    #[test]
    fn undeclared_dot_names_are_rejected() {
        let lamp = id(10);
        let context = CompileContext::new().with_identity("lamp", lamp);
        let error = compile_source("#lamp.color", &context).unwrap_err();

        assert!(matches!(
            error,
            CompileError::Unsupported { message, .. } if message == "dot name `color` is not declared"
        ));
    }

    #[test]
    fn compiled_task_expands_relation_argument_splices() {
        let located_in = id(1);
        let coin = id(10);
        let room = id(11);
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();
        let context = CompileContext::new()
            .with_relation("LocatedIn", located_in)
            .with_identity("coin", coin)
            .with_identity("room", room);
        let mut task_manager = TaskManager::new(kernel);

        let submitted = submit_source_task(
            "let pair = [#coin, #room]\n\
             assert LocatedIn(@pair)\n\
             let prefix = [#coin]\n\
             let place = one LocatedIn(@prefix, ?place)\n\
             retract LocatedIn(@prefix, ?old_place)\n\
             return place == #room && not LocatedIn(@pair)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn dot_relations_require_binary_functional_metadata() {
        let tag = id(1);
        let name = id(2);
        let lamp = id(10);
        let context = CompileContext::new()
            .with_relation_metadata(RelationMetadata::new(tag, Symbol::intern("Tag"), 2))
            .with_relation_metadata(
                RelationMetadata::new(name, Symbol::intern("Name"), 2).with_conflict_policy(
                    ConflictPolicy::Functional {
                        key_positions: vec![1],
                    },
                ),
            )
            .with_identity("lamp", lamp);

        let error = compile_source("return #lamp.tag", &context).unwrap_err();
        assert!(matches!(
            error,
            CompileError::Unsupported { message, .. }
                if message == "dot name `tag` requires `Tag` to be functional on position 0"
        ));

        let error = compile_source("return #lamp.name", &context).unwrap_err();
        assert!(matches!(
            error,
            CompileError::Unsupported { message, .. }
                if message == "dot name `name` requires `Name` to be functional on position 0"
        ));
    }

    #[test]
    fn namespaced_dot_relations_map_to_namespaced_relation_metadata() {
        let actor = id(1);
        let endpoint = id(10);
        let alice = id(11);
        let context = CompileContext::new()
            .with_relation_metadata(
                RelationMetadata::new(actor, Symbol::intern("session/Actor"), 2)
                    .with_conflict_policy(ConflictPolicy::Functional {
                        key_positions: vec![0],
                    }),
            )
            .with_identity("endpoint", endpoint)
            .with_identity("alice", alice);

        let program = compile_source("#endpoint.session/actor = #alice", &context).unwrap();
        assert!(
            program
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    Instruction::ReplaceFunctional { relation, .. } if *relation == actor
                ))
        );
    }

    #[test]
    fn compiled_task_runs_scalar_arithmetic() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let a = 20 - 3 * 4\n\
             let b = a / 2\n\
             let c = b % 3\n\
             return -c",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(-1).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn annotated_bindings_elide_proven_checks_and_reject_mismatches() {
        let compiled = compile_source(
            "let count: int = 1\n\
             count = 2\n\
             return count",
            &CompileContext::new(),
        )
        .unwrap();
        assert_eq!(
            compiled
                .program
                .instructions()
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::CheckKind { .. }))
                .count(),
            0
        );

        assert!(matches!(
            compile_source("let count: int = 1.0", &CompileContext::new()),
            Err(CompileError::ValueKindMismatch {
                subject,
                expected: ValueKind::Int,
                inferred,
                ..
            }) if subject == "count" && inferred == "float"
        ));
    }

    #[test]
    fn annotated_bindings_check_each_dynamic_write_once() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "let count: int = opaque()\n\
             count = opaque()\n\
             return count",
            &context,
        )
        .unwrap();
        let checks = compiled
            .program
            .instructions()
            .into_iter()
            .filter_map(|instruction| match instruction {
                Instruction::CheckKind {
                    expected,
                    site,
                    subject,
                    ..
                } => Some((expected, site, subject)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            checks,
            vec![
                (
                    ValueKind::Int,
                    KindCheckSite::Binding,
                    Symbol::intern("count")
                ),
                (
                    ValueKind::Int,
                    KindCheckSite::Binding,
                    Symbol::intern("count")
                )
            ]
        );
    }

    #[test]
    fn exact_builtin_results_elide_binding_checks_and_feed_kind_facts() {
        let context = CompileContext::new().with_runtime_function_result(
            "string_slice",
            BuiltinResultKind::Exact(ValueKind::String),
        );
        let compiled = compile_source(
            "let trimmed: string = string_slice(\"abc\", 0, 1)\nreturn trimmed",
            &context,
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
        let (instruction, dst) = compiled
            .program
            .instructions()
            .iter()
            .enumerate()
            .find_map(|(instruction, opcode)| match opcode {
                Instruction::BuiltinCall {
                    dst,
                    name,
                    result_kind: Some(ValueKind::String),
                    ..
                } if *name == Symbol::intern("string_slice") => Some((instruction, *dst)),
                _ => None,
            })
            .expect("compiled exact builtin call");
        assert_eq!(
            compiled.program.kind_fact_after(instruction),
            Some((dst, ValueKind::String))
        );

        assert!(matches!(
            compile_source("let value: int = string_slice(\"abc\", 0, 1)", &context),
            Err(CompileError::ValueKindMismatch {
                expected: ValueKind::Int,
                inferred,
                ..
            }) if inferred == "string"
        ));
    }

    #[test]
    fn exact_builtin_results_survive_argument_splices() {
        let context = CompileContext::new().with_runtime_function_result(
            "string_slice",
            BuiltinResultKind::Exact(ValueKind::String),
        );
        let compiled = compile_source(
            "let trimmed: string = string_slice(@[\"abc\", 0, 1])\nreturn trimmed",
            &context,
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
        assert!(compiled.program.instructions().iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::BuiltinCallDynamic {
                    name,
                    result_kind: Some(ValueKind::String),
                    ..
                } if *name == Symbol::intern("string_slice")
            )
        }));
    }

    #[test]
    fn indexed_assignment_expressions_infer_the_updated_collection_kind() {
        let compiled = compile_source(
            "let items: list = [1]\n\
             let updated: list = items[0] = 2\n\
             return updated",
            &CompileContext::new(),
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
        assert!(matches!(
            compile_source(
                "let items: list = [1]\nlet updated: int = items[0] = 2",
                &CompileContext::new()
            ),
            Err(CompileError::ValueKindMismatch {
                expected: ValueKind::Int,
                inferred,
                ..
            }) if inferred == "list"
        ));
    }

    #[test]
    fn named_function_declaration_expressions_are_not_proven_function_values() {
        let compiled = compile_source(
            "let callback: function = fn inner() => 1\nreturn callback",
            &CompileContext::new(),
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 1);
    }

    #[test]
    fn annotated_bindings_prove_exact_constructed_value_kinds() {
        let context = CompileContext::new().with_identity("alice", id(1));
        let compiled = compile_source(
            "const flag: bool = true
             let count: int = 1
             let ratio: float = 1.0
             let actor: identity = #alice
             let text: string = \"text\"
             let data: bytes = b\"3q2-7w==\"
             let label: symbol = :label
             let code: error_code = E_TEST
             let wrapped: frob = #alice<1>
             let callback: function = fn() => 1
             let items: list = [1]
             let options: map = {:key -> 1}
             let span: range = 1..2
             let rows: relation = [:item] { [1] }
             return rows",
            &context,
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
    }

    #[test]
    fn annotations_and_inference_feed_identical_program_kind_facts() {
        let unannotated = compile_source(
            "let total = 0\n\
             total = total + 1\n\
             return total",
            &CompileContext::new(),
        )
        .unwrap();
        let annotated = compile_source(
            "let total: int = 0\n\
             total = total + 1\n\
             return total",
            &CompileContext::new(),
        )
        .unwrap();

        let instructions = unannotated.program.instructions();
        assert_eq!(annotated.program.instructions(), instructions);
        for instruction in 0..instructions.len() {
            assert_eq!(
                annotated.program.kind_fact_after(instruction),
                unannotated.program.kind_fact_after(instruction),
            );
        }

        let checked = compile_source(
            "let total: int = opaque()\n\
             return total + 1",
            &CompileContext::new().with_runtime_function("opaque"),
        )
        .unwrap();
        let checked_instructions = checked.program.instructions();
        let check = checked_instructions
            .iter()
            .position(|instruction| matches!(instruction, Instruction::CheckKind { .. }))
            .unwrap();
        let add = checked_instructions
            .iter()
            .position(|instruction| {
                matches!(
                    instruction,
                    Instruction::Binary {
                        op: RuntimeBinaryOp::Add,
                        ..
                    }
                )
            })
            .unwrap();
        assert!(matches!(
            checked.program.kind_fact_after(check),
            Some((_, ValueKind::Int))
        ));
        assert!(matches!(
            checked.program.kind_fact_after(add),
            Some((_, ValueKind::Int))
        ));
    }

    #[test]
    fn structural_literal_inference_preserves_relation_program_facts() {
        let compiled = compile_source(
            "let rows = [:value] { [1] }\nreturn rows",
            &CompileContext::new(),
        )
        .unwrap();
        let (instruction, dst) = compiled
            .program
            .instructions()
            .iter()
            .enumerate()
            .find_map(|(instruction, opcode)| match opcode {
                Instruction::BuildRelation { dst, .. } => Some((instruction, *dst)),
                _ => None,
            })
            .expect("compiled relation literal");

        assert_eq!(count_kind_checks(&compiled.program), 0);
        assert_eq!(
            compiled.program.kind_fact_after(instruction),
            Some((dst, ValueKind::Relation))
        );
    }

    #[test]
    fn structural_annotations_prove_literals_reject_mismatches_and_check_dynamic_values() {
        let contract = "relation<{:case -> :ok, :value -> int} | {:case -> :error, :value -> error}> where rows in 1";
        let proven = compile_source(
            &format!("let outcome: {contract} = [:case, :value] {{ [:ok, 42] }}\nreturn outcome"),
            &CompileContext::new(),
        )
        .unwrap();
        assert!(
            !proven
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CheckType { .. }))
        );

        assert!(matches!(
            compile_source(
                &format!(
                    "let outcome: {contract} = [:case, :value] {{ [:ok, \"wrong\"] }}"
                ),
                &CompileContext::new(),
            ),
            Err(CompileError::StaticTypeMismatch { subject, .. }) if subject == "outcome"
        ));

        let dynamic = compile_source(
            &format!("let outcome: {contract} = load_result()\nreturn outcome"),
            &CompileContext::new().with_runtime_function("load_result"),
        )
        .unwrap();
        assert_eq!(
            dynamic
                .program
                .instructions()
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::CheckType { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn structurally_equal_aliases_interoperate_without_runtime_checks() {
        let compiled = compile_source(
            "type First = relation<{:value -> int}> where rows in 1\n\
             type Second = relation<{:value -> int}> where rows in 1\n\
             let first: First = [:value] { [1] }\n\
             let second: Second = first\n\
             return second",
            &CompileContext::new(),
        )
        .unwrap();

        assert!(
            !compiled
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CheckType { .. }))
        );
    }

    #[test]
    fn standard_unit_option_and_result_forms_publish_exact_structural_facts() {
        let context = CompileContext::new();
        let compiled = compile_source(
            "fn bare() -> unit\n\
               return\n\
             end\n\
             fn absent() -> option<int> => none\n\
             fn present() -> option<int> => some(42)\n\
             fn success() -> result<int> => ok(42)\n\
             let direct_none: option<int> = none\n\
             return [bare(), absent(), present(), success(), direct_none]",
            &context,
        )
        .unwrap();

        assert!(
            !compiled
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CheckType { .. }))
        );
        assert!(compiled.program.instructions().iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::BuildRelation {
                    row_count: 0,
                    heading,
                    ..
                } if heading == &[Symbol::intern("value")]
            )
        }));
    }

    #[test]
    fn structural_check_raises_catchable_type_error_with_unchanged_relation() {
        let contract = "relation<{:case -> :ok, :value -> int} | {:case -> :error, :value -> error}> where rows in 1";
        let context = CompileContext::new().with_runtime_function("load_result");
        let builtins = BuiltinRegistry::new().with_builtin(
            "load_result",
            BuiltinResultKind::Dynamic,
            invalid_result,
        );
        let mut task_manager =
            TaskManager::new(RelationKernel::new()).with_builtins(Arc::new(builtins));
        let submitted = submit_source_task(
            &format!(
                "let offending = load_result()\n\
                 let caught = try\n\
                   let outcome: {contract} = offending\n\
                   false\n\
                 catch E_TYPE as problem\n\
                   problem.value == offending\n\
                 end\n\
                 return caught"
            ),
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(true)
        ));
    }

    #[test]
    fn inferred_relation_shapes_flow_through_unassigned_locals() {
        let context = CompileContext::new()
            .with_runtime_function_result("project", BuiltinResultKind::Exact(ValueKind::Relation))
            .with_runtime_function_result(
                "natural_join",
                BuiltinResultKind::Exact(ValueKind::Relation),
            );
        let compiled = compile_source(
            "let left = [:id, :name] { [1, \"one\"] }\n\
             let right = [:active, :id] { [true, 1] }\n\
             let joined = natural_join(left, right)\n\
             let names: relation<{:name -> string}> where rows in 0..1 = project(joined, :name)\n\
             return names",
            &context,
        )
        .unwrap();

        assert!(
            !compiled
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CheckType { .. }))
        );

        let direct = compile_source(
            "let make = fn() => [:value] { [42] }\n\
             let made: relation<{:value -> int}> where rows in 1 = make()\n\
             return made",
            &CompileContext::new(),
        )
        .unwrap();
        assert!(
            !direct
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CheckType { .. }))
        );
    }

    #[test]
    fn structural_builtin_metadata_checks_once_and_feeds_following_inference() {
        let contract = TypeContract::Relation(RelationTypeContract {
            alternatives: vec![RelationRowContract {
                columns: vec![(Symbol::intern("value"), TypeContract::Kind(ValueKind::Int))],
            }],
            minimum_rows: 0,
            maximum_rows: Some(1),
        });
        let context = CompileContext::new().with_runtime_function_result(
            "load_optional",
            BuiltinResultKind::Structural(contract.clone()),
        );
        let compiled = compile_source(
            "let loaded: relation<{:value -> int}> where rows in 0..1 = load_optional()\nreturn loaded",
            &context,
        )
        .unwrap();
        let checks = compiled
            .program
            .instructions()
            .into_iter()
            .filter_map(|instruction| match instruction {
                Instruction::CheckType { expected, site, .. } => Some((expected, site)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(checks, vec![(contract, KindCheckSite::Builtin)]);
    }

    #[test]
    fn structural_builtin_contract_failures_are_catchable_with_the_original_result() {
        let contract = TypeContract::Relation(RelationTypeContract {
            alternatives: vec![RelationRowContract {
                columns: vec![(Symbol::intern("value"), TypeContract::Kind(ValueKind::Int))],
            }],
            minimum_rows: 0,
            maximum_rows: Some(1),
        });
        let context = CompileContext::new().with_runtime_function_result(
            "load_optional",
            BuiltinResultKind::Structural(contract.clone()),
        );
        let builtins = BuiltinRegistry::new().with_builtin(
            "load_optional",
            BuiltinResultKind::Structural(contract),
            invalid_result,
        );
        let mut task_manager =
            TaskManager::new(RelationKernel::new()).with_builtins(Arc::new(builtins));
        let submitted = submit_source_task(
            "let caught = try\n\
               load_optional()\n\
             catch E_TYPE as problem\n\
               problem.value\n\
             end\n\
             return caught == [:case, :value] { [:ok, \"wrong\"] }",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(true)
        ));
    }

    #[test]
    fn annotated_bindings_check_control_flow_and_captured_writes() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "let count: int = 0
             if true
               count = opaque()
             end
             while false
               count = opaque()
             end
             let update = fn(value)
               count = value
             end
             return update",
            &context,
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 3);
    }

    #[test]
    fn caught_errors_are_exact_error_values() {
        let compiled = compile_source(
            "try
               raise E_TEST
             catch E_TEST as err
               let caught: error = err
               return caught
             end",
            &CompileContext::new(),
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
    }

    #[test]
    fn capability_annotations_check_dynamic_ingress_without_conversion() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source("let grant: capability = opaque()", &context).unwrap();

        assert!(matches!(
            compiled.program.instructions().as_slice(),
            [
                Instruction::BuiltinCall { .. },
                Instruction::CheckKind {
                    expected: ValueKind::Capability,
                    site: KindCheckSite::Binding,
                    subject,
                    ..
                },
                Instruction::Return { .. }
            ] if *subject == Symbol::intern("grant")
        ));
    }

    #[test]
    fn function_results_prove_explicit_expression_and_implicit_exits() {
        let compiled = compile_source(
            "fn explicit() -> int
               return 1
             end
             let expression = fn() -> int => 2
             let implicit = fn() -> int
               3
             end
             return explicit() + expression() + implicit()",
            &CompileContext::new(),
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
    }

    #[test]
    fn function_results_cover_branches_bare_returns_and_loop_fallthrough() {
        let compiled = compile_source(
            "fn choose(flag) -> int
               if flag
                 1
               else
                 2
               end
             end
             fn empty() -> relation
               if true
                 return
               end
               while false
               end
             end
             return choose(true)",
            &CompileContext::new(),
        )
        .unwrap();
        assert_eq!(count_kind_checks(&compiled.program), 0);

        assert!(matches!(
            compile_source(
                "fn incomplete(flag) -> int\nif flag\n1\nend\nend",
                &CompileContext::new()
            ),
            Err(CompileError::FunctionResultKindMismatch {
                expected: ValueKind::Int,
                inferred,
                ..
            }) if inferred == "int or relation"
        ));
        assert!(matches!(
            compile_source("fn bare() -> int\nreturn\nend", &CompileContext::new()),
            Err(CompileError::FunctionResultKindMismatch { inferred, .. })
                if inferred == "relation"
        ));
        assert!(matches!(
            compile_source(
                "fn loop_result() -> int\nwhile false\nend\nend",
                &CompileContext::new()
            ),
            Err(CompileError::FunctionResultKindMismatch { inferred, .. })
                if inferred == "relation"
        ));
    }

    #[test]
    fn function_results_cover_recovery_try_and_finally_paths() {
        let compiled = compile_source(
            "fn recovered() -> int => recover raise E_TEST catch E_TEST => 1 end
             fn guarded() -> int
               try
                 return 2
               catch E_TEST
                 return 3
               finally
                 true
               end
             end
             return recovered() + guarded()",
            &CompileContext::new(),
        )
        .unwrap();
        assert_eq!(count_kind_checks(&compiled.program), 0);

        assert!(matches!(
            compile_source(
                "fn overridden() -> int\ntry\nreturn 1\nfinally\nreturn 1.0\nend\nend",
                &CompileContext::new()
            ),
            Err(CompileError::FunctionResultKindMismatch {
                expected: ValueKind::Int,
                inferred,
                ..
            }) if inferred == "float"
        ));
    }

    #[test]
    fn function_results_reject_dynamic_and_mixed_numeric_exits() {
        let context = CompileContext::new().with_runtime_function("opaque");
        assert!(matches!(
            compile_source("fn dynamic() -> int => opaque()", &context),
            Err(CompileError::FunctionResultKindMismatch { inferred, .. })
                if inferred == "any value kind"
        ));
        assert!(matches!(
            compile_source("fn quotient() -> int => 3 / 2", &context),
            Err(CompileError::FunctionResultKindMismatch { inferred, .. })
                if inferred == "int or float"
        ));
        assert!(matches!(
            compile_source(
                "fn mixed(flag) -> int\nif flag\n1\nelse\n1.0\nend\nend",
                &context
            ),
            Err(CompileError::FunctionResultKindMismatch { inferred, .. })
                if inferred == "int or float"
        ));
    }

    #[test]
    fn annotated_locals_establish_dynamic_function_results_without_exit_checks() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "fn checked() -> int
               let result: int = opaque()
               return result
             end
             return checked()",
            &context,
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 1);
    }

    #[test]
    fn nested_returns_do_not_create_unreachable_binding_mismatches() {
        let compiled = compile_source(
            "fn nested() -> int
               let unreachable: string = [return 1]
               return 2
             end
             return nested()",
            &CompileContext::new(),
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
    }

    #[test]
    fn known_direct_calls_reuse_annotated_and_inferred_result_facts() {
        let compiled = compile_source(
            "fn declared() -> int => 1
             fn inferred() => 2
             let left: int = declared()
             let right: int = inferred()
             return left + right",
            &CompileContext::new(),
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 0);
    }

    #[test]
    fn function_outer_kinds_do_not_imply_callable_result_facts() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "let callback: function = opaque()
             let value: int = callback()
             return value",
            &context,
        )
        .unwrap();
        assert_eq!(count_kind_checks(&compiled.program), 2);

        assert!(matches!(
            compile_source("fn invoke(callback) -> int => callback()", &context),
            Err(CompileError::FunctionResultKindMismatch { inferred, .. })
                if inferred == "any value kind"
        ));
    }

    #[test]
    fn direct_calls_prove_reject_and_check_annotated_parameters() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "fn accept(value: int) -> int => value
             let exact = accept(1)
             let dynamic = accept(opaque())
             return exact + dynamic",
            &context,
        )
        .unwrap();
        let mut checks = Vec::new();
        collect_kind_checks(&compiled.program, &mut checks);
        assert_eq!(
            checks,
            vec![(
                ValueKind::Int,
                KindCheckSite::Parameter,
                Symbol::intern("value")
            )]
        );

        assert!(matches!(
            compile_source(
                "fn accept(value: int) -> int => value\nreturn accept(1.0)",
                &context
            ),
            Err(CompileError::ParameterKindMismatch {
                parameter,
                expected: ValueKind::Int,
                inferred,
                ..
            }) if parameter == "value" && inferred == "float"
        ));
    }

    #[test]
    fn function_value_wrappers_check_annotated_parameters_once() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "let base: int = 1
             let typed = fn(value: int) -> int => value + base
             let alias = typed
             return alias(opaque())",
            &context,
        )
        .unwrap();
        let mut checks = Vec::new();
        collect_kind_checks(&compiled.program, &mut checks);

        assert_eq!(
            checks,
            vec![(
                ValueKind::Int,
                KindCheckSite::Parameter,
                Symbol::intern("value")
            )]
        );
    }

    #[test]
    fn optional_parameter_defaults_are_explicit_and_kind_checked() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let exact = compile_source(
            "fn pick(?value: int = 1) -> int => value
             let supplied = pick(2)
             let defaulted = pick()
             return supplied + defaulted",
            &context,
        )
        .unwrap();
        assert_eq!(count_kind_checks(&exact.program), 0);

        let dynamic = compile_source(
            "fn pick(?value: int = opaque()) -> int => value
             return pick()",
            &context,
        )
        .unwrap();
        assert_eq!(count_kind_checks(&dynamic.program), 1);

        let dynamic = compile_source(
            "fn pick(?value: int = 1) -> int => value
             return pick(opaque())",
            &context,
        )
        .unwrap();
        assert_eq!(count_kind_checks(&dynamic.program), 1);

        assert!(matches!(
            compile_source("fn missing(?value: int) => value", &context),
            Err(CompileError::MissingOptionalParameterDefault { parameter, .. })
                if parameter == "value"
        ));
        assert!(matches!(
            compile_source("fn wrong(?value: int = 1.0) => value", &context),
            Err(CompileError::ParameterDefaultKindMismatch {
                parameter,
                expected: ValueKind::Int,
                inferred,
                ..
            }) if parameter == "value" && inferred == "float"
        ));
    }

    #[test]
    fn rest_parameters_are_proven_lists_without_checks() {
        let compiled = compile_source(
            "let collect = fn(@items: list) -> list => items
             let alias = collect
             return alias(1, 2, 3)",
            &CompileContext::new(),
        )
        .unwrap();
        assert_eq!(count_kind_checks(&compiled.program), 0);

        assert!(matches!(
            compile_source("fn invalid(@items: int) => items", &CompileContext::new()),
            Err(CompileError::InvalidRestParameterKind {
                parameter,
                declared: ValueKind::Int,
                ..
            }) if parameter == "items"
        ));
    }

    #[test]
    fn direct_spliced_arguments_receive_one_parameter_check() {
        let context = CompileContext::new().with_runtime_function("opaque");
        let compiled = compile_source(
            "fn accept(value: int) -> int => value
             let values = [opaque()]
             return accept(@values)",
            &context,
        )
        .unwrap();

        assert_eq!(count_kind_checks(&compiled.program), 1);
    }

    #[test]
    fn range_loop_bindings_are_proven_integers_without_checks() {
        let compiled = compile_source(
            "let total: int = 0
             for value: int in 1..3
               total = total + value
             end
             for index: int, value: int in 4..5
               total = total + index + value
             end
             return total",
            &CompileContext::new(),
        )
        .unwrap();
        assert_eq!(count_kind_checks(&compiled.program), 0);

        assert!(matches!(
            compile_source(
                "for value: string in 1..3\nend",
                &CompileContext::new()
            ),
            Err(CompileError::ValueKindMismatch {
                subject,
                expected: ValueKind::String,
                inferred,
                ..
            }) if subject == "value" && inferred == "int"
        ));
    }

    #[test]
    fn collection_loops_check_only_dynamic_yields() {
        let compiled = compile_source(
            "for item: int in [1, 2]\nend
             for index: int, item: int in [1, 2]\nend
             for key: symbol, item: int in {:entry -> 1}\nend
             for row: map in [:item] { [1] }\nend
             for index: int, row: map in [:item] { [1] }\nend",
            &CompileContext::new(),
        )
        .unwrap();
        let mut checks = Vec::new();
        collect_kind_checks(&compiled.program, &mut checks);

        assert_eq!(checks.len(), 4);
        assert!(
            checks
                .iter()
                .all(|(_, site, _)| *site == KindCheckSite::Binding)
        );
        assert_eq!(
            checks
                .iter()
                .filter(|(_, _, subject)| *subject == Symbol::intern("index"))
                .count(),
            0
        );
        assert_eq!(
            checks
                .iter()
                .filter(|(_, _, subject)| *subject == Symbol::intern("row"))
                .count(),
            0
        );

        assert!(matches!(
            compile_source(
                "for row: int in [:item] { [1] }\nend",
                &CompileContext::new()
            ),
            Err(CompileError::ValueKindMismatch {
                subject,
                expected: ValueKind::Int,
                inferred,
                ..
            }) if subject == "row" && inferred == "map"
        ));
    }

    #[test]
    fn scatter_bindings_check_elements_and_prove_rest_lists() {
        let compiled = compile_source(
            "let [head: int, ?middle: int = 2, @tail: list] = [1]
             return [head, middle, tail]",
            &CompileContext::new(),
        )
        .unwrap();
        let mut checks = Vec::new();
        collect_kind_checks(&compiled.program, &mut checks);
        assert_eq!(
            checks,
            vec![
                (
                    ValueKind::Int,
                    KindCheckSite::Binding,
                    Symbol::intern("head")
                ),
                (
                    ValueKind::Int,
                    KindCheckSite::Binding,
                    Symbol::intern("middle")
                ),
            ]
        );

        assert!(matches!(
            compile_source(
                "let [?value: int = 1.0] = []",
                &CompileContext::new()
            ),
            Err(CompileError::ValueKindMismatch {
                subject,
                expected: ValueKind::Int,
                inferred,
                ..
            }) if subject == "value" && inferred == "float"
        ));
        assert!(matches!(
            compile_source("let [@tail: map] = []", &CompileContext::new()),
            Err(CompileError::ValueKindMismatch {
                subject,
                expected: ValueKind::Map,
                inferred,
                ..
            }) if subject == "tail" && inferred == "list"
        ));
    }

    #[test]
    fn compiled_short_circuit_guards_can_return_from_one_path() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);

        let passes = submit_source_task(
            "true || return false\n\
             return true",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            passes.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(true)
        ));

        let returns = submit_source_task(
            "false || return false\n\
             return true",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            returns.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(false)
        ));
    }

    #[test]
    fn compiled_task_calls_registered_runtime_builtin() {
        let context = CompileContext::new()
            .with_identity("target", id(99))
            .with_runtime_function("emit_first_arg");
        let kernel = RelationKernel::new();
        let builtins = BuiltinRegistry::new().with_builtin(
            "emit_first_arg",
            BuiltinResultKind::Dynamic,
            emit_first_arg,
        );
        let mut task_manager = TaskManager::new(kernel).with_builtins(Arc::new(builtins));
        let submitted = submit_source_task(
            "let value = emit_first_arg(#target, \"hello\")\n\
             return value",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::string("hello"),
                effects: vec![emitted(Value::string("hello"))],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_runtime_builtin_argument_splices() {
        let context = CompileContext::new()
            .with_identity("target", id(99))
            .with_runtime_function("emit_first_arg");
        let kernel = RelationKernel::new();
        let builtins = BuiltinRegistry::new().with_builtin(
            "emit_first_arg",
            BuiltinResultKind::Dynamic,
            emit_first_arg,
        );
        let mut task_manager = TaskManager::new(kernel).with_builtins(Arc::new(builtins));
        let submitted = submit_source_task(
            "let args = [#target, \"hello\"]\n\
             let value = emit_first_arg(@args)\n\
             return value",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::string("hello"),
                effects: vec![emitted(Value::string("hello"))],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_task_control_argument_splices() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);

        let commit = submit_source_task(
            "let args = []\n\
             return commit(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            commit.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Commit,
                ..
            }
        ));

        let suspend = submit_source_task(
            "let args = [0.5]\n\
             return suspend(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            suspend.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::TimedMillis(500),
                ..
            }
        ));

        let suspend_without_duration = submit_source_task(
            "let args = []\n\
             return suspend(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            suspend_without_duration.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Never,
                ..
            }
        ));

        let read = submit_source_task(
            "let args = [:line]\n\
             return read(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            read.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::WaitingForInput(value),
                ..
            } if value == Value::symbol(Symbol::intern("line"))
        ));

        let read_without_metadata = submit_source_task(
            "let args = []\n\
             return read(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            read_without_metadata.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::WaitingForInput(value),
                ..
            } if value == Value::nothing()
        ));
    }

    #[test]
    fn task_control_argument_splices_validate_dynamic_arity() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);

        let commit = submit_source_task(
            "let args = [1]\n\
             return commit(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            commit.outcome,
            TaskOutcome::Aborted { error, .. } if error == Value::string("commit expects no arguments")
        ));

        let read = submit_source_task(
            "let args = [1, 2]\n\
             return read(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            read.outcome,
            TaskOutcome::Aborted { error, .. } if error == Value::string("read expects 0 or 1 arguments")
        ));

        let mailbox_recv = submit_source_task(
            "let args = []\n\
             return mailbox_recv(@args)",
            &context,
            &mut task_manager,
        )
        .unwrap();
        assert!(matches!(
            mailbox_recv.outcome,
            TaskOutcome::Aborted { error, .. }
                if error == Value::string("mailbox_recv expects a receive-cap list and optional timeout")
        ));
    }

    #[test]
    fn compiled_builtin_call_fails_at_runtime_when_unregistered() {
        let context = CompileContext::new().with_runtime_function("missing_builtin");
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let error = submit_source_task("return missing_builtin()", &context, &mut task_manager)
            .unwrap_err();

        assert!(matches!(
            error,
            mica_runtime::TaskManagerError::Task(
                mica_runtime::TaskError::Runtime(RuntimeError::UnknownBuiltin { name })
            ) if name == Symbol::intern("missing_builtin")
        ));
    }

    #[test]
    fn unresolved_index_receiver_reports_unknown_value_at_receiver() {
        let context = CompileContext::new();
        let source = "let item = {:end_line -> 1}\nreturn found[:end_line]";
        let error = compile_source(source, &context).unwrap_err();
        let found_start = source.find("found").unwrap();
        let found_span = found_start..found_start + "found".len();

        assert!(matches!(
            error,
            CompileError::UnknownValue { name, span: Some(span), .. }
                if name == "found" && span == found_span
        ));
    }

    #[test]
    fn compile_source_collects_multiple_context_errors() {
        let known = id(1);
        let context = CompileContext::new().with_relation("Known", known);
        let error = compile_source(
            "Known(#missing_one)\n\
             MissingRelation(#missing_two)\n\
             return missing_value",
            &context,
        )
        .unwrap_err();

        let CompileError::Diagnostics { errors } = error else {
            panic!("expected aggregate context diagnostics");
        };
        assert_eq!(errors.len(), 4);
        assert!(matches!(
            &errors[0],
            CompileError::UnknownIdentity { name, .. } if name == "missing_one"
        ));
        assert!(matches!(
            &errors[1],
            CompileError::UnknownRelation { name, .. } if name == "MissingRelation"
        ));
        assert!(matches!(
            &errors[2],
            CompileError::UnknownIdentity { name, .. } if name == "missing_two"
        ));
        assert!(matches!(
            &errors[3],
            CompileError::UnknownValue { name, .. } if name == "missing_value"
        ));
    }

    #[test]
    fn relation_rule_variables_are_not_reported_as_unknown_values() {
        let readable = id(1);
        let file_revision = id(2);
        let revision_of = id(3);
        let can_browse = id(4);
        let context = CompileContext::new()
            .with_relation("Readable", readable)
            .with_relation("FileRevision", file_revision)
            .with_relation("RevisionOf", revision_of)
            .with_relation("CanBrowse", can_browse);
        let kernel = RelationKernel::new();
        for (relation, name) in [
            (readable, "Readable"),
            (file_revision, "FileRevision"),
            (revision_of, "RevisionOf"),
            (can_browse, "CanBrowse"),
        ] {
            kernel
                .create_relation(RelationMetadata::new(relation, Symbol::intern(name), 2))
                .unwrap();
        }
        let installation = install_rules_from_source(
            "Readable(actor, file) :-\n\
               FileRevision(file, revision),\n\
               RevisionOf(revision, repository),\n\
               CanBrowse(actor, repository)",
            &context,
            &kernel,
        )
        .unwrap();

        assert!(installation.is_some());
    }

    #[test]
    fn runtime_function_value_use_keeps_runtime_function_diagnostic() {
        let context = CompileContext::new().with_runtime_function("source_find");
        let error = compile_source("return source_find[:end_line]", &context).unwrap_err();

        assert!(matches!(
            error,
            CompileError::Unsupported { message, .. }
                if message == "runtime function `source_find` is not callable from compiled tasks yet"
        ));
    }

    #[test]
    fn compiled_task_calls_named_local_functions() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "fn double(x)\n\
               return x * 2\n\
             end\n\
             fn add(left, right)\n\
               return left + right\n\
             end\n\
             return add(double(10), 1)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(21).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_calls_let_bound_function_literals() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let triple = fn(x) => x * 3\n\
             return triple(7)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(21).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_passes_function_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let apply = fn(f, x) => f(x)\n\
             return apply(fn(x) => x + 1, 41)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(42).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_returns_function_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let make = fn() => fn(x) => x + 1\n\
             let inc = make()\n\
             return inc(41)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(42).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_calls_function_values_through_aliases() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let inc = fn(x) => x + 1\n\
             let alias = inc\n\
             return alias(41)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(42).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_function_value_call_splices() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let sum = fn(a, b, c) => a + b + c\n\
             let alias = sum\n\
             let rest = [2, 3]\n\
             return alias(1, @rest)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(6).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_closure_call_splices() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let base = 10\n\
             let sum = fn(a, b, c) => base + a + b + c\n\
             let rest = [2, 3]\n\
             return sum(1, @rest)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(16).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_calls_function_value_optional_params() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let pick = fn(value, ?fallback = 10) => value + fallback\n\
             let alias = pick\n\
             return alias(1) + alias(1, 2)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(14).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_calls_function_value_rest_params() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let sum = fn(first, @rest) => first + rest[0] + rest[1]\n\
             let alias = sum\n\
             return alias(1, 2, 3)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(6).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_splices_function_value_rest_params() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let sum = fn(first, @rest) => first + rest[0] + rest[1]\n\
             let alias = sum\n\
             let rest = [2, 3]\n\
             return alias(1, @rest)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(6).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn function_value_default_params_capture_values_when_created() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let fallback = 10\n\
             let add = fn(value, ?extra = fallback) => value + extra\n\
             fallback = 20\n\
             return add(1)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(11).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn function_value_call_splices_require_lists() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let error = submit_source_task(
            "let id = fn(value) => value\n\
             let alias = id\n\
             return alias(@1)",
            &context,
            &mut task_manager,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            mica_runtime::TaskManagerError::Task(mica_runtime::TaskError::Runtime(
                RuntimeError::InvalidArgumentSplice(value)
            )) if value == Value::int(1).unwrap()
        ));
    }

    #[test]
    fn function_value_calls_validate_arity_at_runtime() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let error = submit_source_task(
            "let inc = fn(x) => x + 1\n\
             let alias = inc\n\
             return alias()",
            &context,
            &mut task_manager,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            mica_runtime::TaskManagerError::Task(mica_runtime::TaskError::Runtime(
                RuntimeError::InvalidCallArity {
                    expected_min: 1,
                    expected_max: 1,
                    actual: 0,
                }
            ))
        ));
    }

    #[test]
    fn compiled_task_calls_functions_with_optional_params() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "fn pick(value, ?fallback = 10)\n\
               return value + fallback\n\
             end\n\
             return pick(1) + pick(1, 2)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(14).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_calls_functions_with_rest_params() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "fn sum(first, @rest)\n\
               return first + rest[0] + rest[1]\n\
             end\n\
             fn empty(first, @rest)\n\
               return (first == 1) and (rest[0] == nothing)\n\
             end\n\
             return sum(1, 2, 3) + if empty(1)\n\
               10\n\
             else\n\
               0\n\
             end",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(16).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_direct_call_argument_splices() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "fn sum3(a, b, c)\n\
               return a + b + c\n\
             end\n\
             let rest = [2, 3]\n\
             return sum3(1, @rest)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(6).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_combines_optional_rest_and_call_splices() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "fn total(first, ?second = 10, @rest)\n\
               return first + second + rest[0] + rest[1]\n\
             end\n\
             let extra = [3, 4]\n\
             return total(1, 2, @extra)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(10).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn direct_function_calls_validate_arity() {
        let context = CompileContext::new();
        let error = compile_source(
            "fn add(left, right)\n\
               return left + right\n\
             end\n\
             return add(1)",
            &context,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CompileError::Unsupported { message, .. }
                if message == "function call expected at least 2 arguments but got 1"
        ));
    }

    #[test]
    fn compiled_task_calls_closures() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let factor = 2\n\
             fn scale(x)\n\
               return x * factor\n\
             end\n\
             return scale(10)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(20).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_passes_returned_closures() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let make_adder = fn(base) => fn(value) => base + value\n\
             let add10 = make_adder(10)\n\
             return add10(32)",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(42).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn closures_capture_values_when_created() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let value = 1\n\
             let read = fn() => value\n\
             value = 2\n\
             return read()",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(1).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_while_loops() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let i = 0\n\
             let total = 0\n\
             while i < 5\n\
               i = i + 1\n\
               total = total + i\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(15).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_internal_branch_loop() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let i = 0\n\
             let total = 0\n\
             let flag = true\n\
             while i < 4096\n\
               if flag\n\
                 total = total + 1\n\
               else\n\
                 total = total + 2\n\
               end\n\
               flag = flag == false\n\
               i = i + 1\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(6_144).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_break_and_continue() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let i = 0\n\
             let total = 0\n\
             while i < 10\n\
               i = i + 1\n\
               if i == 2\n\
                 continue\n\
               end\n\
               if i == 5\n\
                 break\n\
               end\n\
               total = total + i\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(8).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_for_loop_over_list_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let total = 0\n\
             for item in [1, 2, 3]\n\
               total = total + item\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(6).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_for_loop_over_list_indexes_and_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let total = 0\n\
             for index, item in [4, 5]\n\
               total = total + index + item\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(10).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_for_loop_over_integer_range_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let total = 0\n\
             for number in 2..5\n\
               total = total + number\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(14).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_for_loop_over_integer_range_indexes_and_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let total = 0\n\
             for index, number in 2..4\n\
               total = total + (index * 10) + number\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(39).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_treats_non_finite_integer_ranges_as_empty() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let reversed = 0\n\
             for number in 5..2\n\
               reversed = reversed + 1\n\
             end\n\
             let open = 0\n\
             for number in 2.._\n\
               open = open + 1\n\
             end\n\
             let typed = 0\n\
             for number in \"a\"..\"c\"\n\
               typed = typed + 1\n\
             end\n\
             let huge = 0\n\
             let start = 0 - 36028797018963967 - 1\n\
             let finish = 36028797018963967\n\
             for number in start..finish\n\
               huge = huge + 1\n\
             end\n\
             return [reversed, open, typed, huge]",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::list([
                    Value::int(0).unwrap(),
                    Value::int(0).unwrap(),
                    Value::int(0).unwrap(),
                    Value::int(0).unwrap(),
                ]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_for_loop_over_map_keys_and_values() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let total = 0\n\
             for key, value in {:a -> 10, :b -> 20}\n\
               if key == :b\n\
                 total = total + value\n\
               end\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(20).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_runs_for_loop_break_and_continue() {
        let context = CompileContext::new();
        let kernel = RelationKernel::new();
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            "let total = 0\n\
             for item in [1, 2, 3, 4, 5]\n\
               if item == 2\n\
                 continue\n\
               end\n\
               if item == 5\n\
                 break\n\
               end\n\
               total = total + item\n\
             end\n\
             return total",
            &context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(8).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn installs_method_facts_and_invokes_method_through_dispatch() {
        let located_in = id(1);
        let get_method = id(100);
        let get_program = id(101);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_relation("LocatedIn", located_in)
            .with_method_relations(method_relations)
            .with_identity("get_thing", get_method)
            .with_program_identity("get_thing", get_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        let installation = install_methods_from_source(
            "method #get_thing :get\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               assert LocatedIn(item, actor)\n\
               return true\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        assert_eq!(installation.methods.len(), 1);
        assert_eq!(
            kernel
                .snapshot()
                .scan(
                    method_relations.dispatch.method_selector,
                    &[
                        Some(Value::identity(get_method)),
                        Some(Value::symbol(Symbol::intern("get"))),
                    ],
                )
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            kernel
                .snapshot()
                .scan(
                    method_relations.method_program,
                    &[
                        Some(Value::identity(get_method)),
                        Some(Value::identity(get_program))
                    ],
                )
                .unwrap()
                .len(),
            1
        );

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            ":get(actor: #alice, item: #coin)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(
                    located_in,
                    &[Some(Value::identity(coin)), Some(Value::identity(alice))],
                )
                .unwrap(),
            vec![Tuple::from(
                [Value::identity(coin), Value::identity(alice),]
            )]
        );
    }

    #[test]
    fn installed_verb_contracts_preserve_dispatch_facts_and_check_entry_values() {
        let method = id(100);
        let program = id(101);
        let string = id(200);
        let identity = id(201);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        let context = CompileContext::new()
            .with_method_relations(dispatch_relations())
            .with_identity("typed_verb", method)
            .with_program_identity("typed_verb", program)
            .with_identity("string", string)
            .with_identity("identity", identity);
        let mut semantic = parse_semantic(
            "verb typed(value @ #string: string, target @ #identity: identity) -> string\n\
               return value + \"\"\n\
             end",
        );
        assign_test_method_identity(&mut semantic, "typed_verb");
        let mut tx = kernel.begin();
        let installation = install_methods(semantic, &context, &mut tx).unwrap();
        tx.commit().unwrap();

        let installed = &installation.methods[0];
        assert_eq!(
            installed
                .params
                .iter()
                .map(|param| (param.name.clone(), param.restriction.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("value".to_owned(), Value::identity(string)),
                ("target".to_owned(), Value::identity(identity)),
            ]
        );
        let mut checks = Vec::new();
        collect_kind_checks(&installed.compiled.program, &mut checks);
        assert_eq!(
            checks,
            vec![
                (
                    ValueKind::String,
                    KindCheckSite::Parameter,
                    Symbol::intern("value"),
                ),
                (
                    ValueKind::Identity,
                    KindCheckSite::Parameter,
                    Symbol::intern("target"),
                ),
            ]
        );
        assert!(matches!(
            installed.compiled.program.instructions().first(),
            Some(Instruction::CheckKind {
                expected: ValueKind::String,
                site: KindCheckSite::Parameter,
                ..
            })
        ));
        let param_rows = kernel
            .snapshot()
            .scan(
                dispatch_relations().dispatch.param,
                &[Some(Value::identity(method)), None, None, None],
            )
            .unwrap();
        assert_eq!(param_rows.len(), 2);
        assert!(param_rows.contains(&Tuple::from([
            Value::identity(method),
            Value::symbol(Symbol::intern("value")),
            Value::identity(string),
            Value::int(0).unwrap(),
        ])));
        assert!(param_rows.contains(&Tuple::from([
            Value::identity(method),
            Value::symbol(Symbol::intern("target")),
            Value::identity(identity),
            Value::int(1).unwrap(),
        ])));
    }

    #[test]
    fn installed_verb_result_annotations_are_proof_only() {
        let method = id(100);
        let program = id(101);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        let context = CompileContext::new()
            .with_method_relations(dispatch_relations())
            .with_identity("typed_verb", method)
            .with_program_identity("typed_verb", program);

        let mut semantic = parse_semantic(
            "verb typed(value: int) -> int\n\
               return value + 1\n\
             end",
        );
        assign_test_method_identity(&mut semantic, "typed_verb");
        let mut tx = kernel.begin();
        let installation = install_methods(semantic, &context, &mut tx).unwrap();
        let instructions = installation.methods[0].compiled.program.instructions();
        assert!(matches!(instructions[0], Instruction::CheckKind { .. }));
        let return_index = instructions
            .iter()
            .position(|instruction| matches!(instruction, Instruction::Return { .. }))
            .expect("compiled verb returns");
        assert!(!matches!(
            instructions.get(return_index.saturating_sub(1)),
            Some(Instruction::CheckKind { .. })
        ));
        assert_eq!(
            count_kind_checks(&installation.methods[0].compiled.program),
            1
        );

        let mut semantic = parse_semantic(
            "verb typed(value: int) -> string\n\
               return value\n\
             end",
        );
        assign_test_method_identity(&mut semantic, "typed_verb");
        let mut tx = kernel.begin();
        assert!(matches!(
            install_methods(semantic, &context, &mut tx),
            Err(CompileError::VerbResultKindMismatch {
                selector,
                expected: ValueKind::String,
                inferred,
                ..
            }) if selector == "typed" && inferred == "int"
        ));
    }

    #[test]
    fn installed_parameter_names_do_not_shadow_runtime_calls() {
        let method = id(100);
        let program = id(101);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        let context = CompileContext::new()
            .with_method_relations(dispatch_relations())
            .with_identity("endpoint_probe", method)
            .with_program_identity("endpoint_probe", program)
            .with_runtime_function_result(
                "endpoint",
                BuiltinResultKind::Exact(ValueKind::Identity),
            );
        let mut semantic = parse_semantic(
            "verb probe(endpoint)\n\
               let current: identity = endpoint()\n\
               return [endpoint, current]\n\
             end",
        );
        assign_test_method_identity(&mut semantic, "endpoint_probe");
        let mut tx = kernel.begin();
        let installation = install_methods(semantic, &context, &mut tx).unwrap();

        assert!(
            installation.methods[0]
                .compiled
                .program
                .instructions()
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    Instruction::BuiltinCall {
                        name,
                        result_kind: Some(ValueKind::Identity),
                        ..
                    }
                        if *name == Symbol::intern("endpoint")
                ))
        );
        assert_eq!(
            count_kind_checks(&installation.methods[0].compiled.program),
            0
        );
    }

    #[test]
    fn receiver_dispatch_accepts_positional_arguments() {
        let inspect_method = id(100);
        let inspect_program = id(101);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("inspect_thing", inspect_method)
            .with_program_identity("inspect_thing", inspect_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #inspect_thing :inspect\n\
               roles receiver @ #thing, actor @ #player\n\
             do\n\
               return [receiver, actor]\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);
        let submitted =
            submit_source_task("#coin:inspect(#alice)", &invoke_context, &mut task_manager)
                .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::list([Value::identity(coin), Value::identity(alice)]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiled_task_expands_dispatch_argument_splices() {
        let inspect_method = id(100);
        let inspect_program = id(101);
        let look_method = id(102);
        let look_program = id(103);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("inspect_thing", inspect_method)
            .with_program_identity("inspect_thing", inspect_program)
            .with_identity("look_thing", look_method)
            .with_program_identity("look_thing", look_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #inspect_thing :inspect\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               return [actor, item]\n\
             end\n\
             method #look_thing :look\n\
               roles receiver @ #thing, actor @ #player\n\
             do\n\
               return [receiver, actor]\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);

        let positional = submit_source_task(
            "let args = [#alice, #coin]\n\
             return inspect(@args)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            positional.outcome,
            TaskOutcome::Complete {
                value: Value::list([Value::identity(alice), Value::identity(coin)]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );

        let receiver_positional = submit_source_task(
            "let args = [#alice]\n\
             return #coin:look(@args)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            receiver_positional.outcome,
            TaskOutcome::Complete {
                value: Value::list([Value::identity(coin), Value::identity(alice)]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );

        let dynamic_invoke = submit_source_task(
            "let args = [:inspect, {:actor -> #alice, :item -> #coin}]\n\
             return invoke(@args)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            dynamic_invoke.outcome,
            TaskOutcome::Complete {
                value: Value::list([Value::identity(alice), Value::identity(coin)]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );

        let role_named = submit_source_task(
            "let roles = {:item -> #coin}\n\
             return :inspect(actor: #alice, @roles)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            role_named.outcome,
            TaskOutcome::Complete {
                value: Value::list([Value::identity(alice), Value::identity(coin)]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );

        let receiver_role_named = submit_source_task(
            "let roles = {}\n\
             return #coin:look(actor: #alice, @roles)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            receiver_role_named.outcome,
            TaskOutcome::Complete {
                value: Value::list([Value::identity(coin), Value::identity(alice)]),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn persisted_method_can_return_indexed_collection_values() {
        let inspect_method = id(100);
        let inspect_program = id(101);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("inspect_thing", inspect_method)
            .with_program_identity("inspect_thing", inspect_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #inspect_thing :inspect\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               let values = [actor, item]\n\
               let result = {:target -> values[1]}\n\
               return result[:target]\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            ":inspect(actor: #alice, item: #coin)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::identity(coin),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(inspect_program))
        );
    }

    #[test]
    fn persisted_method_can_run_while_loop() {
        let count_method = id(100);
        let count_program = id(101);
        let player = id(200);
        let alice = id(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("count_loop", count_method)
            .with_program_identity("count_loop", count_program)
            .with_identity("player", player);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #count_loop :count\n\
               roles actor @ #player\n\
             do\n\
               let i = 0\n\
               while i < 3\n\
                 i = i + 1\n\
               end\n\
               return i\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice);
        let mut task_manager = TaskManager::new(kernel);
        let submitted =
            submit_source_task(":count(actor: #alice)", &invoke_context, &mut task_manager)
                .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(3).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(count_program))
        );
    }

    #[test]
    fn persisted_method_can_run_for_loop() {
        let count_method = id(100);
        let count_program = id(101);
        let player = id(200);
        let alice = id(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("sum_loop", count_method)
            .with_program_identity("sum_loop", count_program)
            .with_identity("player", player);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #sum_loop :sum\n\
               roles actor @ #player\n\
             do\n\
               let total = 0\n\
               for item in [2, 3, 4]\n\
                 total = total + item\n\
               end\n\
               return total\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice);
        let mut task_manager = TaskManager::new(kernel);
        let submitted =
            submit_source_task(":sum(actor: #alice)", &invoke_context, &mut task_manager).unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(9).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(count_program))
        );
    }

    #[test]
    fn persisted_method_can_run_scalar_arithmetic() {
        let calc_method = id(100);
        let calc_program = id(101);
        let player = id(200);
        let alice = id(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("calc", calc_method)
            .with_program_identity("calc", calc_program)
            .with_identity("player", player);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #calc :calc\n\
               roles actor @ #player\n\
             do\n\
               return (10 * 3 - 4) / 2\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice);
        let mut task_manager = TaskManager::new(kernel);
        let submitted =
            submit_source_task(":calc(actor: #alice)", &invoke_context, &mut task_manager).unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(13).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(calc_program))
        );
    }

    #[test]
    fn persisted_method_can_assign_indexed_values() {
        let update_method = id(100);
        let update_program = id(101);
        let player = id(200);
        let alice = id(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("update", update_method)
            .with_program_identity("update", update_program)
            .with_identity("player", player);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #update :update\n\
               roles actor @ #player\n\
             do\n\
               let counts = {:seen -> 1}\n\
               counts[:seen] = counts[:seen] + 1\n\
               return counts[:seen]\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice);
        let mut task_manager = TaskManager::new(kernel);
        let submitted =
            submit_source_task(":update(actor: #alice)", &invoke_context, &mut task_manager)
                .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(2).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(update_program))
        );
    }

    #[test]
    fn persisted_method_can_read_and_write_declared_dot_relations() {
        let name = id(1);
        let rename_method = id(100);
        let rename_program = id(101);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        let name_metadata = RelationMetadata::new(name, Symbol::intern("Name"), 2)
            .with_conflict_policy(ConflictPolicy::Functional {
                key_positions: vec![0],
            });
        kernel.create_relation(name_metadata.clone()).unwrap();

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_relation_metadata(name_metadata)
            .with_dot_relation("name", name)
            .with_method_relations(method_relations)
            .with_identity("rename_thing", rename_method)
            .with_program_identity("rename_thing", rename_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #rename_thing :rename\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               item.name = \"bright coin\"\n\
               return item.name\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            ":rename(actor: #alice, item: #coin)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::string("bright coin"),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(name, &[Some(Value::identity(coin)), None])
                .unwrap(),
            vec![Tuple::from([
                Value::identity(coin),
                Value::string("bright coin"),
            ])]
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(rename_program))
        );
    }

    #[test]
    fn persisted_method_can_call_local_function() {
        let calc_method = id(100);
        let calc_program = id(101);
        let player = id(200);
        let alice = id(300);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("calc", calc_method)
            .with_program_identity("calc", calc_program)
            .with_identity("player", player);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #calc :calc\n\
               roles actor @ #player\n\
             do\n\
               fn double(x)\n\
                 return x * 2\n\
               end\n\
               return double(21)\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice);
        let mut task_manager = TaskManager::new(kernel);
        let submitted =
            submit_source_task(":calc(actor: #alice)", &invoke_context, &mut task_manager).unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::int(42).unwrap(),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(calc_program))
        );
    }

    #[test]
    fn persisted_method_can_dispatch_to_another_persisted_method() {
        let located_in = id(1);
        let get_method = id(100);
        let get_program = id(101);
        let mark_method = id(102);
        let mark_program = id(103);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_relation("LocatedIn", located_in)
            .with_method_relations(method_relations)
            .with_identity("get_thing", get_method)
            .with_program_identity("get_thing", get_program)
            .with_identity("mark_thing", mark_method)
            .with_program_identity("mark_thing", mark_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        let installation = install_methods_from_source(
            "method #mark_thing :mark\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               assert LocatedIn(item, actor)\n\
               return true\n\
             end\n\
             method #get_thing :get\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               :mark(actor: actor, item: item)\n\
               return true\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        assert_eq!(installation.methods.len(), 2);
        assert_eq!(
            kernel
                .snapshot()
                .scan(method_relations.program_bytes, &[None, None])
                .unwrap()
                .len(),
            2
        );

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);
        let submitted = submit_source_task(
            ":get(actor: #alice, item: #coin)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();

        assert_eq!(
            submitted.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(get_program))
        );
        assert!(
            task_manager
                .resolver()
                .contains(&Value::identity(mark_program))
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(
                    located_in,
                    &[Some(Value::identity(coin)), Some(Value::identity(alice))],
                )
                .unwrap(),
            vec![Tuple::from(
                [Value::identity(coin), Value::identity(alice),]
            )]
        );
    }

    #[test]
    fn dispatched_method_can_branch_on_relation_predicates() {
        let portable = id(1);
        let located_in = id(2);
        let take_method = id(100);
        let take_program = id(101);
        let player = id(200);
        let thing = id(201);
        let alice = id(300);
        let coin = id(301);
        let kernel = RelationKernel::new();
        create_method_relations(&kernel);
        kernel
            .create_relation(RelationMetadata::new(
                portable,
                Symbol::intern("Portable"),
                2,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                located_in,
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();

        let method_relations = dispatch_relations();
        let install_context = CompileContext::new()
            .with_relation("Portable", portable)
            .with_relation("LocatedIn", located_in)
            .with_method_relations(method_relations)
            .with_identity("take_thing", take_method)
            .with_program_identity("take_thing", take_program)
            .with_identity("player", player)
            .with_identity("thing", thing);
        let mut install_tx = kernel.begin();
        install_methods_from_source(
            "method #take_thing :take\n\
               roles actor @ #player, item @ #thing\n\
             do\n\
               if Portable(item, true) && !LocatedIn(item, actor)\n\
                 assert LocatedIn(item, actor)\n\
                 return true\n\
               else\n\
                 return false\n\
               end\n\
             end",
            &install_context,
            &mut install_tx,
        )
        .unwrap();
        install_tx
            .assert(
                portable,
                Tuple::from([Value::identity(coin), Value::bool(true)]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(alice),
                    Value::identity(player),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx
            .assert(
                method_relations.dispatch.delegates,
                Tuple::from([
                    Value::identity(coin),
                    Value::identity(thing),
                    Value::int(0).unwrap(),
                ]),
            )
            .unwrap();
        install_tx.commit().unwrap();

        let invoke_context = CompileContext::new()
            .with_method_relations(method_relations)
            .with_identity("alice", alice)
            .with_identity("coin", coin);
        let mut task_manager = TaskManager::new(kernel);
        let first = submit_source_task(
            ":take(actor: #alice, item: #coin)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            first.outcome,
            TaskOutcome::Complete {
                value: Value::bool(true),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
        assert_eq!(
            task_manager
                .kernel()
                .snapshot()
                .scan(
                    located_in,
                    &[Some(Value::identity(coin)), Some(Value::identity(alice))],
                )
                .unwrap()
                .len(),
            1
        );

        let second = submit_source_task(
            ":take(actor: #alice, item: #coin)",
            &invoke_context,
            &mut task_manager,
        )
        .unwrap();
        assert_eq!(
            second.outcome,
            TaskOutcome::Complete {
                value: Value::bool(false),
                effects: vec![],
                mailbox_sends: Vec::new(),
                retries: 0,
            }
        );
    }

    #[test]
    fn compiles_float_literal_as_binary32() {
        let context = CompileContext::new();
        let compiled = compile_source("return 1.5", &context).unwrap();
        let instructions = compiled.program.instructions();
        let load = instructions
            .iter()
            .find(|i| matches!(i, Instruction::Load { value, .. } if value.as_float().is_some()))
            .unwrap();
        match load {
            Instruction::Load { value, .. } => {
                assert_eq!(value.as_float(), Some(1.5f32));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn compiles_exponent_float_literal() {
        let context = CompileContext::new();
        let compiled = compile_source("return 1.5e2", &context).unwrap();
        let instructions = compiled.program.instructions();
        let load = instructions
            .iter()
            .find(|i| matches!(i, Instruction::Load { value, .. } if value.as_float().is_some()))
            .unwrap();
        match load {
            Instruction::Load { value, .. } => {
                assert_eq!(value.as_float(), Some(150.0f32));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn rejects_float_literal_overflow() {
        let context = CompileContext::new();
        let result = compile_source("return 3.4028236e38", &context);
        assert!(result.is_err());
        let error = result.unwrap_err();
        let message = match error {
            CompileError::InvalidLiteral { message, .. } => message,
            other => panic!("expected InvalidLiteral error, got {other:?}"),
        };
        assert!(
            message.contains("overflows binary32"),
            "expected overflow message, got: {message}"
        );
    }

    #[test]
    fn compiles_binary32_literal_boundaries() {
        let context = CompileContext::new();
        for (source, expected) in [
            ("return 3.4028235e38", f32::MAX),
            ("return 1.4e-45", f32::from_bits(1)),
            ("return 1e-50", 0.0),
        ] {
            let compiled = compile_source(source, &context).unwrap();
            let value = compiled
                .program
                .instructions()
                .iter()
                .find_map(|instruction| match instruction {
                    Instruction::Load { value, .. } => value.as_float(),
                    _ => None,
                })
                .expect("literal program should load a float");
            assert_eq!(value.to_bits(), expected.to_bits(), "{source}");
        }
    }
}
