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
    Arg, Ast, BindingKind, BindingPattern, Cardinality, CatchClause, CollectionItem, EffectKind,
    Expr, FunctionBody, HirArg, HirCatch, HirCollectionItem, HirExpr, HirFunctionBody, HirItem,
    HirLoopBinding, HirMethodParam, HirParam, HirPlace, HirProgram, HirRecovery, HirRelationAtom,
    HirRuleBodyItem, HirRuleGuard, HirScatterBinding, Item, NodeId, Param, ParamMode, ParseError,
    RecoveryClause, RelationType, RowShape, Span, StaticLiteral, StaticType, TypeAliasDefinition,
    TypeLiteralRef, TypeRef, TypeRefKind, parse_ast,
};
use mica_var::ValueKind;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

pub fn parse_semantic(source: &str) -> SemanticProgram {
    let ast = parse_ast(source);
    analyze_ast(&ast)
}

pub fn analyze_ast(ast: &Ast) -> SemanticProgram {
    analyze_ast_with_aliases(ast, &[])
}

pub fn analyze_ast_with_aliases(
    ast: &Ast,
    installed_aliases: &[TypeAliasDefinition],
) -> SemanticProgram {
    Analyzer::new(ast, installed_aliases).analyze()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ScopeId(pub u32);

impl ScopeId {
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BindingId(pub u32);

impl BindingId {
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticProgram {
    pub hir: HirProgram,
    pub spans: HashMap<NodeId, Span>,
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    pub references: Vec<Reference>,
    pub captures: HashMap<NodeId, Vec<BindingId>>,
    pub diagnostics: Vec<Diagnostic>,
    pub parse_errors: Vec<ParseError>,
    pub type_aliases: Vec<TypeAliasDefinition>,
    pub type_alias_dependencies: BTreeSet<String>,
}

impl SemanticProgram {
    pub fn span(&self, node: NodeId) -> Option<&Span> {
        self.spans.get(&node)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub owner: Option<NodeId>,
    pub bindings: Vec<BindingId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    pub id: BindingId,
    pub name: String,
    pub kind: LocalKind,
    pub mutable: bool,
    pub assigned: bool,
    pub declared_type: Option<StaticType>,
    pub declared_kind: Option<ValueKind>,
    pub scope: ScopeId,
    pub declared_at: NodeId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalKind {
    Let,
    Const,
    Param,
    OptionalParam,
    RestParam,
    InstalledParam,
    Loop,
    Catch,
    Function,
}

impl LocalKind {
    fn mutable_by_default(&self) -> bool {
        matches!(self, Self::Let | Self::Loop | Self::Catch | Self::Function)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    pub node: NodeId,
    pub name: String,
    pub resolution: ResolvedName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedName {
    Local(BindingId),
    External {
        name: String,
        kind: ExternalNameKind,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalNameKind {
    Relation,
    Runtime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub node: NodeId,
    pub span: Span,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    DuplicateBinding,
    AssignToConst,
    InvalidAssignmentTarget,
    InvalidFactChange,
    InvalidRelationRule,
    MissingValueKindInitializer,
    UnknownValueKind,
    InvalidStaticType,
    UnsupportedSyntax,
}

impl DiagnosticCode {
    pub const fn title(&self) -> &'static str {
        match self {
            Self::DuplicateBinding => "duplicate binding",
            Self::AssignToConst => "assignment to const",
            Self::InvalidAssignmentTarget => "invalid assignment target",
            Self::InvalidFactChange => "invalid fact change",
            Self::InvalidRelationRule => "invalid relation rule",
            Self::MissingValueKindInitializer => "missing value-kind initializer",
            Self::UnknownValueKind => "unknown value kind",
            Self::InvalidStaticType => "invalid static type",
            Self::UnsupportedSyntax => "unsupported syntax",
        }
    }
}

struct Analyzer<'a> {
    ast: &'a Ast,
    spans: HashMap<NodeId, Span>,
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    references: Vec<Reference>,
    captures: HashMap<NodeId, BTreeSet<BindingId>>,
    diagnostics: Vec<Diagnostic>,
    function_stack: Vec<FunctionContext>,
    aliases: HashMap<String, TypeAliasDefinition>,
    local_aliases: Vec<TypeAliasDefinition>,
    type_alias_dependencies: BTreeSet<String>,
}

#[derive(Clone, Copy)]
struct FunctionContext {
    owner: NodeId,
    scope: ScopeId,
}

impl<'a> Analyzer<'a> {
    fn new(ast: &'a Ast, installed_aliases: &[TypeAliasDefinition]) -> Self {
        let mut spans = HashMap::new();
        collect_item_spans(&ast.items, &mut spans);
        let mut aliases = standard_type_aliases()
            .iter()
            .cloned()
            .map(|alias| (alias.name.clone(), alias))
            .collect::<HashMap<_, _>>();
        for alias in installed_aliases {
            aliases
                .entry(alias.name.clone())
                .or_insert_with(|| alias.clone());
        }
        Self {
            ast,
            spans,
            scopes: Vec::new(),
            bindings: Vec::new(),
            references: Vec::new(),
            captures: HashMap::new(),
            diagnostics: Vec::new(),
            function_stack: Vec::new(),
            aliases,
            local_aliases: Vec::new(),
            type_alias_dependencies: BTreeSet::new(),
        }
    }

    fn analyze(mut self) -> SemanticProgram {
        self.collect_local_aliases();
        let root_scope = self.alloc_scope(None, None);
        let items = self.lower_items(&self.ast.items, root_scope);
        self.validate_supported_surface_items(&items);
        SemanticProgram {
            hir: HirProgram { items },
            spans: self.spans,
            scopes: self.scopes,
            bindings: self.bindings,
            references: self.references,
            captures: self
                .captures
                .into_iter()
                .map(|(node, bindings)| (node, bindings.into_iter().collect()))
                .collect(),
            diagnostics: self.diagnostics,
            parse_errors: self.ast.errors.clone(),
            type_aliases: self.local_aliases,
            type_alias_dependencies: self.type_alias_dependencies,
        }
    }

    fn collect_local_aliases(&mut self) {
        for item in &self.ast.items {
            let Item::TypeAlias {
                id,
                span,
                name,
                parameters,
                body,
                source,
            } = item
            else {
                continue;
            };
            let mut seen = BTreeSet::new();
            for parameter in parameters {
                if !seen.insert(parameter.clone()) {
                    self.diagnostic(
                        DiagnosticCode::InvalidStaticType,
                        *id,
                        span.clone(),
                        format!("duplicate type parameter `{parameter}` in alias `{name}`"),
                    );
                }
            }
            if self.local_aliases.iter().any(|alias| alias.name == *name)
                || standard_type_aliases()
                    .iter()
                    .any(|standard| standard.name == *name)
            {
                self.diagnostic(
                    DiagnosticCode::InvalidStaticType,
                    *id,
                    span.clone(),
                    format!("type alias `{name}` is already defined"),
                );
                continue;
            }
            let alias = TypeAliasDefinition {
                name: name.clone(),
                parameters: parameters.clone(),
                body: body.clone(),
                source: source.clone(),
            };
            self.aliases.insert(name.clone(), alias.clone());
            self.local_aliases.push(alias);
        }
        for item in &self.ast.items {
            let Item::TypeAlias {
                id,
                name,
                parameters,
                body,
                ..
            } = item
            else {
                continue;
            };
            let parameters = parameters
                .iter()
                .cloned()
                .map(|name| (name, StaticType::Dynamic))
                .collect();
            let _ = self.resolve_type_inner(body, *id, &parameters, &mut vec![name.clone()]);
        }
    }

    fn alloc_scope(&mut self, parent: Option<ScopeId>, owner: Option<NodeId>) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope {
            id,
            parent,
            owner,
            bindings: Vec::new(),
        });
        id
    }

    fn declare(
        &mut self,
        scope: ScopeId,
        name: impl Into<String>,
        kind: LocalKind,
        declared_type: Option<StaticType>,
        declared_at: NodeId,
        span: &Span,
    ) -> BindingId {
        let name = name.into();
        if self.binding_in_scope(scope, &name).is_some() {
            self.diagnostic(
                DiagnosticCode::DuplicateBinding,
                declared_at,
                span.clone(),
                format!("duplicate binding `{name}` in this scope"),
            );
        }
        let id = BindingId(self.bindings.len() as u32);
        let declared_kind = declared_type
            .as_ref()
            .and_then(StaticType::exact_outer_kind);
        let binding = Binding {
            id,
            name,
            mutable: kind.mutable_by_default(),
            assigned: false,
            kind,
            declared_type,
            declared_kind,
            scope,
            declared_at,
        };
        self.bindings.push(binding);
        self.scopes[scope.0 as usize].bindings.push(id);
        id
    }

    fn binding_in_scope(&self, scope: ScopeId, name: &str) -> Option<BindingId> {
        self.scopes[scope.0 as usize]
            .bindings
            .iter()
            .copied()
            .find(|binding| self.bindings[binding.0 as usize].name == name)
    }

    fn resolve(&mut self, name: &str, node: NodeId, scope: ScopeId) -> ResolvedName {
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            if let Some(binding) = self.binding_in_scope(scope_id, name) {
                self.record_capture(binding, scope);
                let resolution = ResolvedName::Local(binding);
                self.references.push(Reference {
                    node,
                    name: name.to_owned(),
                    resolution: resolution.clone(),
                });
                return resolution;
            }
            current = self.scopes[scope_id.0 as usize].parent;
        }

        let resolution = ResolvedName::External {
            name: name.to_owned(),
            kind: if looks_like_relation_name(name) {
                ExternalNameKind::Relation
            } else {
                ExternalNameKind::Runtime
            },
        };
        self.references.push(Reference {
            node,
            name: name.to_owned(),
            resolution: resolution.clone(),
        });
        resolution
    }

    fn record_capture(&mut self, binding: BindingId, use_scope: ScopeId) {
        let binding_scope = self.bindings[binding.0 as usize].scope;
        for context in self.function_stack.iter().rev() {
            if context.scope == binding_scope {
                break;
            }
            if self.scope_contains(context.scope, use_scope) {
                self.captures
                    .entry(context.owner)
                    .or_default()
                    .insert(binding);
            }
        }
    }

    fn scope_contains(&self, ancestor: ScopeId, mut descendant: ScopeId) -> bool {
        loop {
            if ancestor == descendant {
                return true;
            }
            let Some(parent) = self.scopes[descendant.0 as usize].parent else {
                return false;
            };
            descendant = parent;
        }
    }

    fn validate_supported_surface_items(&mut self, items: &[HirItem]) {
        for item in items {
            match item {
                HirItem::TypeAlias { .. } => {}
                HirItem::Expr { expr, .. } => self.validate_supported_surface_expr(expr, false),
                HirItem::RelationRule { head, body, .. } => {
                    self.validate_relation_atom_support(head, true, false, false);
                    for item in body {
                        match item {
                            HirRuleBodyItem::Atom(atom) => {
                                self.validate_relation_atom_support(atom, true, false, false);
                            }
                            HirRuleBodyItem::Guard(guard) => {
                                self.validate_supported_surface_expr(&guard.left, false);
                                self.validate_supported_surface_expr(&guard.right, false);
                            }
                        }
                    }
                }
                HirItem::Method { body, .. } => self.validate_supported_surface_items(body),
            }
        }
    }

    fn validate_supported_surface_expr(&mut self, expr: &HirExpr, _direct_function_binding: bool) {
        match expr {
            HirExpr::QueryVar { id, .. } => {
                self.unsupported(*id, "query variables are only valid as relation arguments");
            }
            HirExpr::List { items, .. } => {
                for item in items {
                    match item {
                        HirCollectionItem::Expr(expr) | HirCollectionItem::Splice(expr) => {
                            self.validate_supported_surface_expr(expr, false);
                        }
                    }
                }
            }
            HirExpr::Relation { rows, .. } => {
                for expr in rows.iter().flatten() {
                    self.validate_supported_surface_expr(expr, false);
                }
            }
            HirExpr::Map { entries, .. } => {
                for (key, value) in entries {
                    self.validate_supported_surface_expr(key, false);
                    self.validate_supported_surface_expr(value, false);
                }
            }
            HirExpr::Unary { expr, .. } => self.validate_supported_surface_expr(expr, false),
            HirExpr::Binary {
                op: crate::BinaryOp::Range,
                left,
                right,
                ..
            } => {
                self.validate_supported_surface_expr(left, false);
                if !matches!(right.as_ref(), HirExpr::Hole { .. }) {
                    self.validate_supported_surface_expr(right, false);
                }
            }
            HirExpr::Binary { left, right, .. } => {
                self.validate_supported_surface_expr(left, false);
                self.validate_supported_surface_expr(right, false);
            }
            HirExpr::Assign { target, value, .. } => {
                self.validate_supported_surface_place(target);
                self.validate_supported_surface_expr(value, false);
            }
            HirExpr::Call { id, callee, args } => {
                if args.iter().any(|arg| arg.role.is_some()) {
                    self.unsupported(*id, "ordinary calls only support positional arguments");
                }
                if !matches!(
                    callee.as_ref(),
                    HirExpr::LocalRef { .. } | HirExpr::ExternalRef { .. }
                ) && let Some(arg) = args.iter().find(|arg| arg.splice)
                {
                    self.unsupported(
                        arg.id,
                        "argument splices are only supported for direct local or runtime calls",
                    );
                }
                self.validate_supported_surface_expr(callee, false);
                self.validate_args(args);
            }
            HirExpr::RoleDispatch { id, selector, args } => {
                self.validate_dispatch_args(*id, args, "dispatch");
                self.validate_supported_surface_expr(selector, false);
                self.validate_args(args);
            }
            HirExpr::ReceiverDispatch {
                id,
                receiver,
                selector,
                args,
            } => {
                self.validate_receiver_dispatch_args(*id, args);
                self.validate_supported_surface_expr(receiver, false);
                self.validate_supported_surface_expr(selector, false);
                self.validate_args(args);
            }
            HirExpr::Spawn { id, target, delay } => {
                self.validate_spawn_target(*id, target);
                if let Some(delay) = delay {
                    self.validate_supported_surface_expr(delay, false);
                }
            }
            HirExpr::RelationAtom(atom) => {
                self.validate_relation_atom_support(atom, true, true, true)
            }
            HirExpr::FactChange { kind, atom, .. } => {
                self.validate_relation_atom_support(
                    atom,
                    matches!(kind, EffectKind::Retract),
                    true,
                    true,
                );
                if matches!(kind, EffectKind::Assert)
                    && atom.args.iter().any(|arg| {
                        matches!(arg.value, HirExpr::QueryVar { .. } | HirExpr::Hole { .. })
                    })
                {
                    self.unsupported(
                        atom.id,
                        "assert facts cannot contain query variables or holes",
                    );
                }
            }
            HirExpr::Require { condition, .. }
            | HirExpr::One {
                expr: condition, ..
            } => self.validate_supported_surface_expr(condition, false),
            HirExpr::Index {
                collection, index, ..
            } => {
                self.validate_supported_surface_expr(collection, false);
                if let Some(index) = index {
                    self.validate_supported_surface_expr(index, false);
                }
            }
            HirExpr::Field { base, .. } => self.validate_supported_surface_expr(base, false),
            HirExpr::Binding { value, .. } => {
                if let Some(value) = value {
                    let is_direct_function =
                        matches!(value.as_ref(), HirExpr::Function { name: None, .. });
                    self.validate_supported_surface_expr(value, is_direct_function);
                }
            }
            HirExpr::If {
                condition,
                then_items,
                elseif,
                else_items,
                ..
            } => {
                self.validate_supported_surface_expr(condition, false);
                self.validate_supported_surface_items(then_items);
                for (condition, items) in elseif {
                    self.validate_supported_surface_expr(condition, false);
                    self.validate_supported_surface_items(items);
                }
                self.validate_supported_surface_items(else_items);
            }
            HirExpr::Block { items, .. } => self.validate_supported_surface_items(items),
            HirExpr::For { iter, body, .. } => {
                self.validate_supported_surface_expr(iter, false);
                self.validate_supported_surface_items(body);
            }
            HirExpr::While {
                condition, body, ..
            } => {
                self.validate_supported_surface_expr(condition, false);
                self.validate_supported_surface_items(body);
            }
            HirExpr::Return { value, .. } => {
                if let Some(value) = value {
                    self.validate_supported_surface_expr(value, false);
                }
            }
            HirExpr::Raise {
                error,
                message,
                value,
                ..
            } => {
                self.validate_supported_surface_expr(error, false);
                if let Some(message) = message {
                    self.validate_supported_surface_expr(message, false);
                }
                if let Some(value) = value {
                    self.validate_supported_surface_expr(value, false);
                }
            }
            HirExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                self.validate_supported_surface_items(body);
                for catch in catches {
                    self.validate_catch_condition(catch.id, catch.condition.as_ref());
                    self.validate_supported_surface_items(&catch.body);
                }
                self.validate_supported_surface_items(finally);
            }
            HirExpr::Function { body, .. } => match body {
                HirFunctionBody::Expr(expr) => {
                    self.validate_supported_surface_expr(expr, false);
                }
                HirFunctionBody::Block(items) => self.validate_supported_surface_items(items),
            },
            HirExpr::Recover { expr, catches, .. } => {
                self.validate_supported_surface_expr(expr, false);
                for catch in catches {
                    self.validate_catch_condition(catch.id, catch.condition.as_ref());
                    self.validate_supported_surface_expr(&catch.value, false);
                }
            }
            HirExpr::Frob { value, .. } => self.validate_supported_surface_expr(value, false),
            HirExpr::Hole { id } => {
                self.unsupported(*id, "holes are only valid as relation arguments")
            }
            HirExpr::LocalRef { .. }
            | HirExpr::ExternalRef { .. }
            | HirExpr::Identity { .. }
            | HirExpr::Symbol { .. }
            | HirExpr::Literal { .. }
            | HirExpr::Break { .. }
            | HirExpr::Continue { .. }
            | HirExpr::Error { .. } => {}
        }
    }

    fn validate_supported_surface_place(&mut self, place: &HirPlace) {
        match place {
            HirPlace::Index {
                collection, index, ..
            } => {
                self.validate_supported_surface_expr(collection, false);
                if let Some(index) = index {
                    self.validate_supported_surface_expr(index, false);
                }
            }
            HirPlace::Dot { base, .. } => self.validate_supported_surface_expr(base, false),
            HirPlace::Invalid { .. } => {}
            HirPlace::Local { .. } => {}
        }
    }

    fn validate_args(&mut self, args: &[HirArg]) {
        for arg in args {
            self.validate_supported_surface_expr(&arg.value, false);
        }
    }

    fn validate_dispatch_args(&mut self, id: NodeId, args: &[HirArg], context: &str) {
        if let Some(arg) = args.iter().find(|arg| arg.splice && arg.role.is_some()) {
            self.unsupported(
                arg.id,
                format!("{context} role values do not support splices; splice a role map"),
            );
        }
        if args.iter().any(|arg| arg.role.is_none() && !arg.splice) {
            self.unsupported(
                id,
                format!("{context} arguments must use explicit role names"),
            );
        }
    }

    fn validate_receiver_dispatch_args(&mut self, id: NodeId, args: &[HirArg]) {
        let has_named = args.iter().any(|arg| arg.role.is_some());
        let has_positional = args.iter().any(|arg| arg.role.is_none() && !arg.splice);
        if has_named && let Some(arg) = args.iter().find(|arg| arg.splice && arg.role.is_some()) {
            self.unsupported(
                arg.id,
                "receiver role dispatch values do not support splices; splice a role map",
            );
        }
        if has_named && has_positional {
            self.unsupported(
                id,
                "receiver dispatch arguments must be all positional or all role-named",
            );
        }
    }

    fn validate_spawn_target(&mut self, id: NodeId, target: &HirExpr) {
        match target {
            HirExpr::RoleDispatch { id, selector, args } => {
                let has_named = args.iter().any(|arg| arg.role.is_some());
                let has_positional = args.iter().any(|arg| arg.role.is_none() && !arg.splice);
                if has_named
                    && let Some(arg) = args.iter().find(|arg| arg.splice && arg.role.is_some())
                {
                    self.unsupported(
                        arg.id,
                        "spawn role values do not support splices; splice a role map",
                    );
                }
                if has_named && has_positional {
                    self.unsupported(
                        *id,
                        "spawn arguments must be all positional or all role-named",
                    );
                }
                self.validate_supported_surface_expr(selector, false);
                self.validate_args(args);
            }
            HirExpr::ReceiverDispatch {
                id,
                receiver,
                selector,
                args,
            } => {
                self.validate_receiver_dispatch_args(*id, args);
                self.validate_supported_surface_expr(receiver, false);
                self.validate_supported_surface_expr(selector, false);
                self.validate_args(args);
            }
            _ => self.unsupported(id, "spawn expects a role or receiver dispatch target"),
        }
    }

    fn validate_relation_atom_support(
        &mut self,
        atom: &HirRelationAtom,
        allow_query_vars: bool,
        allow_holes: bool,
        allow_splices: bool,
    ) {
        for arg in &atom.args {
            if arg.splice && !allow_splices {
                self.unsupported(arg.id, "relation argument splices are not valid here");
            }
            match &arg.value {
                HirExpr::QueryVar { id, .. } if !allow_query_vars => {
                    self.unsupported(*id, "query variables are not valid here");
                }
                HirExpr::Hole { id } if !allow_holes => {
                    self.unsupported(*id, "holes are not valid here");
                }
                HirExpr::QueryVar { .. } | HirExpr::Hole { .. } => {}
                expr => self.validate_supported_surface_expr(expr, false),
            }
        }
    }

    fn validate_catch_condition(&mut self, id: NodeId, condition: Option<&HirExpr>) {
        let Some(condition) = condition else {
            return;
        };
        let _ = id;
        self.validate_supported_surface_expr(condition, false);
    }

    fn lower_items(&mut self, items: &[Item], scope: ScopeId) -> Vec<HirItem> {
        items
            .iter()
            .map(|item| self.lower_item(item, scope))
            .collect()
    }

    fn lower_item(&mut self, item: &Item, scope: ScopeId) -> HirItem {
        match item {
            Item::TypeAlias {
                id,
                name,
                parameters,
                body,
                source,
                ..
            } => HirItem::TypeAlias {
                id: *id,
                name: name.clone(),
                parameters: parameters.clone(),
                body: body.clone(),
                source: source.clone(),
            },
            Item::Expr { id, expr } => HirItem::Expr {
                id: *id,
                expr: self.lower_expr(expr, scope),
            },
            Item::RelationRule {
                id,
                span,
                head,
                body,
            } => {
                let head = self.lower_relation_atom(head, scope).unwrap_or_else(|| {
                    self.diagnostic(
                        DiagnosticCode::InvalidRelationRule,
                        *id,
                        span.clone(),
                        "relation rule head must be a relation atom",
                    );
                    self.error_atom(*id)
                });
                let body = body
                    .iter()
                    .filter_map(|expr| {
                        let item = self.lower_rule_body_item(expr, scope);
                        if item.is_none() {
                            self.diagnostic(
                                DiagnosticCode::InvalidRelationRule,
                                expr.id(),
                                expr.span().clone(),
                                "relation rule body entries must be relation atoms or comparison guards",
                            );
                        }
                        item
                    })
                    .collect();
                HirItem::RelationRule {
                    id: *id,
                    head,
                    body,
                }
            }
            Item::Method {
                id,
                kind,
                identity,
                selector,
                clauses,
                params,
                result_type,
                body,
                ..
            } => {
                let method_scope = self.alloc_scope(Some(scope), Some(*id));
                let params = params
                    .iter()
                    .map(|param| {
                        let declared_type =
                            self.resolve_type_ref(param.annotation.as_ref(), param.id);
                        let declared_kind = declared_type
                            .as_ref()
                            .and_then(StaticType::exact_outer_kind);
                        HirMethodParam {
                            id: param.id,
                            binding: self.declare(
                                method_scope,
                                param.name.clone(),
                                LocalKind::InstalledParam,
                                declared_type.clone(),
                                param.id,
                                &param.span,
                            ),
                            restriction: param.restriction.clone(),
                            declared_type,
                            declared_kind,
                        }
                    })
                    .collect();
                let result_type = self.resolve_type_ref(result_type.as_ref(), *id);
                let result_kind = result_type.as_ref().and_then(StaticType::exact_outer_kind);
                self.function_stack.push(FunctionContext {
                    owner: *id,
                    scope: method_scope,
                });
                let body = self.lower_items(body, method_scope);
                self.function_stack.pop();
                HirItem::Method {
                    id: *id,
                    kind: kind.clone(),
                    identity: identity.clone(),
                    selector: selector.clone(),
                    clauses: clauses.clone(),
                    params,
                    result_type,
                    result_kind,
                    scope: method_scope,
                    body,
                }
            }
        }
    }

    fn lower_rule_body_item(&mut self, expr: &Expr, scope: ScopeId) -> Option<HirRuleBodyItem> {
        if let Some(atom) = self.lower_rule_atom(expr, scope) {
            return Some(HirRuleBodyItem::Atom(atom));
        }
        self.lower_rule_guard(expr, scope)
            .map(Box::new)
            .map(HirRuleBodyItem::Guard)
    }

    fn lower_rule_atom(&mut self, expr: &Expr, scope: ScopeId) -> Option<HirRelationAtom> {
        match expr {
            Expr::Unary {
                op: crate::UnaryOp::Not,
                expr,
                ..
            } => self.lower_relation_atom(expr, scope).map(|mut atom| {
                atom.negated = true;
                atom
            }),
            _ => self.lower_relation_atom(expr, scope),
        }
    }

    fn lower_rule_guard(&mut self, expr: &Expr, scope: ScopeId) -> Option<HirRuleGuard> {
        let Expr::Binary {
            id,
            op,
            left,
            right,
            ..
        } = expr
        else {
            return None;
        };
        if !matches!(
            op,
            crate::BinaryOp::Eq
                | crate::BinaryOp::Ne
                | crate::BinaryOp::Lt
                | crate::BinaryOp::Le
                | crate::BinaryOp::Gt
                | crate::BinaryOp::Ge
        ) {
            return None;
        }
        Some(HirRuleGuard {
            id: *id,
            op: *op,
            left: self.lower_expr(left, scope),
            right: self.lower_expr(right, scope),
        })
    }

    fn lower_expr(&mut self, expr: &Expr, scope: ScopeId) -> HirExpr {
        match expr {
            Expr::Literal { id, value, .. } => HirExpr::Literal {
                id: *id,
                value: value.clone(),
            },
            Expr::Name { id, name, .. } => match self.resolve(name, *id, scope) {
                ResolvedName::Local(binding) => HirExpr::LocalRef { id: *id, binding },
                ResolvedName::External { name, .. } => HirExpr::ExternalRef { id: *id, name },
            },
            Expr::Identity { id, name, .. } => HirExpr::Identity {
                id: *id,
                name: name.clone(),
            },
            Expr::Frob {
                id,
                delegate,
                value,
                ..
            } => HirExpr::Frob {
                id: *id,
                delegate: delegate.clone(),
                value: Box::new(self.lower_expr(value, scope)),
            },
            Expr::Symbol { id, name, .. } => HirExpr::Symbol {
                id: *id,
                name: name.clone(),
            },
            Expr::QueryVar { id, name, .. } => HirExpr::QueryVar {
                id: *id,
                name: name.clone(),
            },
            Expr::Hole { id, .. } => HirExpr::Hole { id: *id },
            Expr::List { id, items, .. } => HirExpr::List {
                id: *id,
                items: items
                    .iter()
                    .map(|item| match item {
                        CollectionItem::Expr(expr) => {
                            HirCollectionItem::Expr(self.lower_expr(expr, scope))
                        }
                        CollectionItem::Splice(expr) => {
                            HirCollectionItem::Splice(self.lower_expr(expr, scope))
                        }
                    })
                    .collect(),
            },
            Expr::Relation {
                id, heading, rows, ..
            } => HirExpr::Relation {
                id: *id,
                heading: heading.clone(),
                rows: rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|expr| self.lower_expr(expr, scope))
                            .collect()
                    })
                    .collect(),
            },
            Expr::Map { id, entries, .. } => HirExpr::Map {
                id: *id,
                entries: entries
                    .iter()
                    .map(|(key, value)| {
                        (self.lower_expr(key, scope), self.lower_expr(value, scope))
                    })
                    .collect(),
            },
            Expr::Unary { id, op, expr, .. } => HirExpr::Unary {
                id: *id,
                op: *op,
                expr: Box::new(self.lower_expr(expr, scope)),
            },
            Expr::Binary {
                id,
                op,
                left,
                right,
                ..
            } => HirExpr::Binary {
                id: *id,
                op: *op,
                left: Box::new(self.lower_expr(left, scope)),
                right: Box::new(self.lower_expr(right, scope)),
            },
            Expr::Assign {
                id, target, value, ..
            } => HirExpr::Assign {
                id: *id,
                target: self.lower_place(target, scope),
                value: Box::new(self.lower_expr(value, scope)),
            },
            Expr::Call {
                id, callee, args, ..
            } if self.expr_relation_name(callee).is_some() => {
                HirExpr::RelationAtom(self.lower_relation_atom_from_parts(*id, callee, args, scope))
            }
            Expr::Call {
                id, callee, args, ..
            } => HirExpr::Call {
                id: *id,
                callee: Box::new(self.lower_expr(callee, scope)),
                args: self.lower_args(args, scope),
            },
            Expr::RoleCall {
                id, selector, args, ..
            } => HirExpr::RoleDispatch {
                id: *id,
                selector: Box::new(self.lower_expr(selector, scope)),
                args: self.lower_args(args, scope),
            },
            Expr::ReceiverCall {
                id,
                receiver,
                selector,
                args,
                ..
            } => HirExpr::ReceiverDispatch {
                id: *id,
                receiver: Box::new(self.lower_expr(receiver, scope)),
                selector: Box::new(self.lower_expr(selector, scope)),
                args: self.lower_args(args, scope),
            },
            Expr::Spawn {
                id, target, delay, ..
            } => HirExpr::Spawn {
                id: *id,
                target: Box::new(self.lower_expr(target, scope)),
                delay: delay
                    .as_ref()
                    .map(|delay| Box::new(self.lower_expr(delay, scope))),
            },
            Expr::Index {
                id,
                collection,
                index,
                ..
            } => HirExpr::Index {
                id: *id,
                collection: Box::new(self.lower_expr(collection, scope)),
                index: index
                    .as_ref()
                    .map(|index| Box::new(self.lower_expr(index, scope))),
            },
            Expr::Field { id, base, name, .. } => HirExpr::Field {
                id: *id,
                base: Box::new(self.lower_expr(base, scope)),
                name: name.clone(),
            },
            Expr::Binding {
                id,
                span,
                kind,
                pattern,
                annotation,
                value,
            } => {
                let declared_type = self.resolve_type_ref(annotation.as_ref(), *id);
                if declared_type.is_some() && value.is_none() {
                    self.diagnostic(
                        DiagnosticCode::MissingValueKindInitializer,
                        *id,
                        annotation
                            .as_ref()
                            .map_or_else(|| span.clone(), |annotation| annotation.span.clone()),
                        "annotated bindings require an initializer",
                    );
                }
                let hir_value = value
                    .as_ref()
                    .map(|value| Box::new(self.lower_expr(value, scope)));
                let (binding, scatter) = match pattern {
                    BindingPattern::Name(name) => (
                        Some(self.declare(
                            scope,
                            name.clone(),
                            match kind {
                                BindingKind::Let => LocalKind::Let,
                                BindingKind::Const => LocalKind::Const,
                            },
                            declared_type,
                            *id,
                            span,
                        )),
                        Vec::new(),
                    ),
                    BindingPattern::Scatter(bindings) => (
                        None,
                        bindings
                            .iter()
                            .map(|param| {
                                let declared_type =
                                    self.resolve_type_ref(param.annotation.as_ref(), param.id);
                                let declared_kind = declared_type
                                    .as_ref()
                                    .and_then(StaticType::exact_outer_kind);
                                let local_kind = match kind {
                                    BindingKind::Let => LocalKind::Let,
                                    BindingKind::Const => LocalKind::Const,
                                };
                                let binding = self.declare(
                                    scope,
                                    param.name.clone(),
                                    local_kind,
                                    declared_type.clone(),
                                    param.id,
                                    &param.span,
                                );
                                let default = param
                                    .default
                                    .as_ref()
                                    .map(|default| self.lower_expr(default, scope));
                                HirScatterBinding {
                                    id: param.id,
                                    binding,
                                    mode: param.mode.clone(),
                                    declared_type,
                                    declared_kind,
                                    default,
                                }
                            })
                            .collect(),
                    ),
                };
                HirExpr::Binding {
                    id: *id,
                    binding,
                    scatter,
                    kind: kind.clone(),
                    value: hir_value,
                }
            }
            Expr::If {
                id,
                condition,
                then_items,
                elseif,
                else_items,
                ..
            } => HirExpr::If {
                id: *id,
                condition: Box::new(self.lower_expr(condition, scope)),
                then_items: self.lower_items_in_child_scope(then_items, scope, Some(*id)),
                elseif: elseif
                    .iter()
                    .map(|(condition, items)| {
                        (
                            self.lower_expr(condition, scope),
                            self.lower_items_in_child_scope(items, scope, Some(*id)),
                        )
                    })
                    .collect(),
                else_items: self.lower_items_in_child_scope(else_items, scope, Some(*id)),
            },
            Expr::Block { id, items, .. } => {
                let block_scope = self.alloc_scope(Some(scope), Some(*id));
                HirExpr::Block {
                    id: *id,
                    scope: block_scope,
                    items: self.lower_items(items, block_scope),
                }
            }
            Expr::For {
                id,
                key,
                value,
                iter,
                body,
                ..
            } => {
                let iter = self.lower_expr(iter, scope);
                let loop_scope = self.alloc_scope(Some(scope), Some(*id));
                let key_type = self.resolve_type_ref(key.annotation.as_ref(), key.id);
                let key_kind = key_type.as_ref().and_then(StaticType::exact_outer_kind);
                let key = HirLoopBinding {
                    id: key.id,
                    binding: self.declare(
                        loop_scope,
                        key.name.clone(),
                        LocalKind::Loop,
                        key_type.clone(),
                        key.id,
                        &key.span,
                    ),
                    declared_type: key_type,
                    declared_kind: key_kind,
                };
                let value = value.as_ref().map(|value| {
                    let declared_type = self.resolve_type_ref(value.annotation.as_ref(), value.id);
                    let declared_kind = declared_type
                        .as_ref()
                        .and_then(StaticType::exact_outer_kind);
                    HirLoopBinding {
                        id: value.id,
                        binding: self.declare(
                            loop_scope,
                            value.name.clone(),
                            LocalKind::Loop,
                            declared_type.clone(),
                            value.id,
                            &value.span,
                        ),
                        declared_type,
                        declared_kind,
                    }
                });
                HirExpr::For {
                    id: *id,
                    scope: loop_scope,
                    key,
                    value,
                    iter: Box::new(iter),
                    body: self.lower_items(body, loop_scope),
                }
            }
            Expr::While {
                id,
                condition,
                body,
                ..
            } => HirExpr::While {
                id: *id,
                condition: Box::new(self.lower_expr(condition, scope)),
                body: self.lower_items_in_child_scope(body, scope, Some(*id)),
            },
            Expr::Return { id, value, .. } => HirExpr::Return {
                id: *id,
                value: value
                    .as_ref()
                    .map(|value| Box::new(self.lower_expr(value, scope))),
            },
            Expr::Raise {
                id,
                error,
                message,
                value,
                ..
            } => HirExpr::Raise {
                id: *id,
                error: Box::new(self.lower_expr(error, scope)),
                message: message
                    .as_ref()
                    .map(|message| Box::new(self.lower_expr(message, scope))),
                value: value
                    .as_ref()
                    .map(|value| Box::new(self.lower_expr(value, scope))),
            },
            Expr::Recover {
                id, expr, catches, ..
            } => HirExpr::Recover {
                id: *id,
                expr: Box::new(self.lower_expr(expr, scope)),
                catches: catches
                    .iter()
                    .map(|catch| self.lower_recovery(catch, scope))
                    .collect(),
            },
            Expr::One { id, expr, .. } => HirExpr::One {
                id: *id,
                expr: Box::new(self.lower_expr(expr, scope)),
            },
            Expr::Break { id, .. } => HirExpr::Break { id: *id },
            Expr::Continue { id, .. } => HirExpr::Continue { id: *id },
            Expr::Try {
                id,
                body,
                catches,
                finally,
                ..
            } => HirExpr::Try {
                id: *id,
                body: self.lower_items_in_child_scope(body, scope, Some(*id)),
                catches: catches
                    .iter()
                    .map(|catch| self.lower_catch(catch, scope))
                    .collect(),
                finally: self.lower_items_in_child_scope(finally, scope, Some(*id)),
            },
            Expr::Function {
                id,
                span,
                name,
                params,
                result_type,
                body,
            } => {
                let result_type = self.resolve_type_ref(result_type.as_ref(), *id);
                let result_kind = result_type.as_ref().and_then(StaticType::exact_outer_kind);
                let name = name.as_ref().map(|name| {
                    self.declare(scope, name.clone(), LocalKind::Function, None, *id, span)
                });
                let function_scope = self.alloc_scope(Some(scope), Some(*id));
                self.function_stack.push(FunctionContext {
                    owner: *id,
                    scope: function_scope,
                });
                let hir_params = self.declare_params(params, function_scope, span);
                let body = match body {
                    FunctionBody::Expr(expr) => {
                        HirFunctionBody::Expr(Box::new(self.lower_expr(expr, function_scope)))
                    }
                    FunctionBody::Block(items) => {
                        HirFunctionBody::Block(self.lower_items(items, function_scope))
                    }
                };
                self.function_stack.pop();
                let captures = self
                    .captures
                    .get(id)
                    .map(|captures| captures.iter().copied().collect())
                    .unwrap_or_default();
                HirExpr::Function {
                    id: *id,
                    name,
                    scope: function_scope,
                    params: hir_params,
                    result_type,
                    result_kind,
                    captures,
                    body,
                }
            }
            Expr::Effect { id, kind, expr, .. } => match kind {
                EffectKind::Assert | EffectKind::Retract => {
                    if let Some(atom) = self.lower_relation_atom(expr, scope) {
                        HirExpr::FactChange {
                            id: *id,
                            kind: kind.clone(),
                            atom,
                        }
                    } else {
                        self.diagnostic(
                            DiagnosticCode::InvalidFactChange,
                            *id,
                            expr.span().clone(),
                            "assert and retract require a relation atom",
                        );
                        HirExpr::Error { id: *id }
                    }
                }
                EffectKind::Require => HirExpr::Require {
                    id: *id,
                    condition: Box::new(self.lower_expr(expr, scope)),
                },
            },
            Expr::Error { id, .. } => HirExpr::Error { id: *id },
        }
    }

    fn lower_items_in_child_scope(
        &mut self,
        items: &[Item],
        parent: ScopeId,
        owner: Option<NodeId>,
    ) -> Vec<HirItem> {
        let scope = self.alloc_scope(Some(parent), owner);
        self.lower_items(items, scope)
    }

    fn declare_params(&mut self, params: &[Param], scope: ScopeId, span: &Span) -> Vec<HirParam> {
        params
            .iter()
            .map(|param| {
                let kind = match param.mode {
                    ParamMode::Required => LocalKind::Param,
                    ParamMode::Optional => LocalKind::OptionalParam,
                    ParamMode::Rest => LocalKind::RestParam,
                };
                let declared_type = self.resolve_type_ref(param.annotation.as_ref(), param.id);
                let declared_kind = declared_type
                    .as_ref()
                    .and_then(StaticType::exact_outer_kind);
                let binding = self.declare(
                    scope,
                    param.name.clone(),
                    kind.clone(),
                    declared_type.clone(),
                    param.id,
                    span,
                );
                let default = param
                    .default
                    .as_ref()
                    .map(|default| self.lower_expr(default, scope));
                HirParam {
                    id: param.id,
                    binding,
                    kind,
                    declared_type,
                    declared_kind,
                    default,
                }
            })
            .collect()
    }

    fn lower_catch(&mut self, catch: &CatchClause, parent: ScopeId) -> HirCatch {
        let scope = self.alloc_scope(Some(parent), Some(catch.id));
        let binding = catch.name.as_ref().map(|name| {
            self.declare(
                scope,
                name.clone(),
                LocalKind::Catch,
                None,
                catch.id,
                &(0..0),
            )
        });
        let condition = catch
            .condition
            .as_ref()
            .map(|condition| self.lower_expr(condition, scope));
        let body = self.lower_items(&catch.body, scope);
        HirCatch {
            id: catch.id,
            binding,
            condition,
            body,
        }
    }

    fn lower_recovery(&mut self, catch: &RecoveryClause, parent: ScopeId) -> HirRecovery {
        let scope = self.alloc_scope(Some(parent), Some(catch.id));
        let binding = catch.name.as_ref().map(|name| {
            self.declare(
                scope,
                name.clone(),
                LocalKind::Catch,
                None,
                catch.id,
                &(0..0),
            )
        });
        let condition = catch
            .condition
            .as_ref()
            .map(|condition| self.lower_expr(condition, scope));
        let value = self.lower_expr(&catch.value, scope);
        HirRecovery {
            id: catch.id,
            binding,
            condition,
            value,
        }
    }

    fn lower_args(&mut self, args: &[Arg], scope: ScopeId) -> Vec<HirArg> {
        args.iter()
            .map(|arg| HirArg {
                id: arg.id,
                role: arg.role.clone(),
                splice: arg.splice,
                value: self.lower_expr(&arg.value, scope),
            })
            .collect()
    }

    fn lower_relation_atom(&mut self, expr: &Expr, scope: ScopeId) -> Option<HirRelationAtom> {
        match expr {
            Expr::Call {
                id, callee, args, ..
            } => Some(self.lower_relation_atom_from_parts(*id, callee, args, scope)),
            _ => None,
        }
    }

    fn lower_relation_atom_from_parts(
        &mut self,
        id: NodeId,
        callee: &Expr,
        args: &[Arg],
        scope: ScopeId,
    ) -> HirRelationAtom {
        let name = self
            .expr_relation_name(callee)
            .unwrap_or_else(|| match callee {
                Expr::Name { name, .. } => name.clone(),
                _ => "<invalid>".to_owned(),
            });
        self.references.push(Reference {
            node: callee.id(),
            name: name.clone(),
            resolution: ResolvedName::External {
                name: name.clone(),
                kind: ExternalNameKind::Relation,
            },
        });
        HirRelationAtom {
            id,
            name,
            args: self.lower_args(args, scope),
            negated: false,
        }
    }

    fn expr_relation_name(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name { name, .. } if looks_like_relation_name(name) => Some(name.clone()),
            _ => None,
        }
    }

    fn lower_place(&mut self, expr: &Expr, scope: ScopeId) -> HirPlace {
        match expr {
            Expr::Name { id, name, span } => match self.resolve(name, *id, scope) {
                ResolvedName::Local(binding) => {
                    if self.bindings[binding.0 as usize].mutable {
                        self.bindings[binding.0 as usize].assigned = true;
                        HirPlace::Local { id: *id, binding }
                    } else {
                        self.diagnostic(
                            DiagnosticCode::AssignToConst,
                            *id,
                            span.clone(),
                            format!("cannot assign to immutable binding `{name}`"),
                        );
                        HirPlace::Invalid {
                            id: *id,
                            span: span.clone(),
                            resolution: Some(ResolvedName::Local(binding)),
                        }
                    }
                }
                resolution => {
                    self.diagnostic(
                        DiagnosticCode::InvalidAssignmentTarget,
                        *id,
                        span.clone(),
                        format!("`{name}` is not an assignable local binding"),
                    );
                    HirPlace::Invalid {
                        id: *id,
                        span: span.clone(),
                        resolution: Some(resolution),
                    }
                }
            },
            Expr::Index {
                id,
                collection,
                index,
                ..
            } => HirPlace::Index {
                id: *id,
                collection: Box::new(self.lower_expr(collection, scope)),
                index: index
                    .as_ref()
                    .map(|index| Box::new(self.lower_expr(index, scope))),
            },
            Expr::Field { id, base, name, .. } => HirPlace::Dot {
                id: *id,
                base: Box::new(self.lower_expr(base, scope)),
                name: name.clone(),
            },
            _ => {
                self.diagnostic(
                    DiagnosticCode::InvalidAssignmentTarget,
                    expr.id(),
                    expr.span().clone(),
                    "left side of assignment is not an assignable place",
                );
                HirPlace::Invalid {
                    id: expr.id(),
                    span: expr.span().clone(),
                    resolution: None,
                }
            }
        }
    }

    fn error_atom(&self, id: NodeId) -> HirRelationAtom {
        HirRelationAtom {
            id,
            name: "<error>".to_owned(),
            args: Vec::new(),
            negated: false,
        }
    }

    fn resolve_type_ref(
        &mut self,
        annotation: Option<&TypeRef>,
        node: NodeId,
    ) -> Option<StaticType> {
        let annotation = annotation?;
        self.resolve_type(annotation, node)
    }

    fn resolve_type(&mut self, annotation: &TypeRef, node: NodeId) -> Option<StaticType> {
        self.resolve_type_inner(annotation, node, &HashMap::new(), &mut Vec::new())
    }

    fn resolve_type_inner(
        &mut self,
        annotation: &TypeRef,
        node: NodeId,
        parameters: &HashMap<String, StaticType>,
        stack: &mut Vec<String>,
    ) -> Option<StaticType> {
        match &annotation.kind {
            TypeRefKind::Named { name, arguments } => {
                if let Some(parameter) = parameters.get(name) {
                    if !arguments.is_empty() {
                        self.diagnostic(
                            DiagnosticCode::InvalidStaticType,
                            node,
                            annotation.span.clone(),
                            format!("type parameter `{name}` does not accept arguments"),
                        );
                        return None;
                    }
                    return Some(parameter.clone());
                }
                if name == "dynamic" && arguments.is_empty() {
                    return Some(StaticType::Dynamic);
                }
                if let Some(kind) = value_kind_from_name(name) {
                    if !arguments.is_empty() {
                        self.diagnostic(
                            DiagnosticCode::InvalidStaticType,
                            node,
                            annotation.span.clone(),
                            format!("value kind `{name}` does not accept type arguments"),
                        );
                        return None;
                    }
                    return Some(StaticType::Kind(kind));
                }
                let Some(alias) = self.aliases.get(name).cloned() else {
                    self.diagnostic(
                        DiagnosticCode::UnknownValueKind,
                        node,
                        annotation.span.clone(),
                        format!("unknown type `{name}`"),
                    );
                    return None;
                };
                if arguments.len() != alias.parameters.len() {
                    self.diagnostic(
                        DiagnosticCode::InvalidStaticType,
                        node,
                        annotation.span.clone(),
                        format!(
                            "type alias `{name}` expects {} argument(s), received {}",
                            alias.parameters.len(),
                            arguments.len()
                        ),
                    );
                    return None;
                }
                if stack.iter().any(|active| active == name) {
                    stack.push(name.clone());
                    self.diagnostic(
                        DiagnosticCode::InvalidStaticType,
                        node,
                        annotation.span.clone(),
                        format!("cyclic type alias: {}", stack.join(" -> ")),
                    );
                    stack.pop();
                    return None;
                }
                let resolved_arguments = arguments
                    .iter()
                    .map(|argument| self.resolve_type_inner(argument, node, parameters, stack))
                    .collect::<Option<Vec<_>>>()?;
                let substitutions = alias
                    .parameters
                    .iter()
                    .cloned()
                    .zip(resolved_arguments)
                    .collect();
                if !self.local_aliases.iter().any(|local| local.name == *name)
                    && !standard_type_aliases()
                        .iter()
                        .any(|standard| standard.name == *name)
                {
                    self.type_alias_dependencies.insert(name.clone());
                }
                stack.push(name.clone());
                let resolved = self.resolve_type_inner(&alias.body, node, &substitutions, stack);
                stack.pop();
                resolved
            }
            TypeRefKind::Literal(literal) => Some(StaticType::Literal(match literal {
                TypeLiteralRef::Bool(value) => StaticLiteral::Bool(*value),
                TypeLiteralRef::Symbol(value) => StaticLiteral::Symbol(value.clone()),
                TypeLiteralRef::ErrorCode(value) => StaticLiteral::ErrorCode(value.clone()),
                TypeLiteralRef::Unit => StaticLiteral::Unit,
                TypeLiteralRef::EmptyRelation => StaticLiteral::EmptyRelation,
            })),
            TypeRefKind::Relation {
                alternatives,
                cardinality,
            } => {
                let cardinality = match Cardinality::new(cardinality.min, cardinality.max) {
                    Ok(cardinality) => cardinality,
                    Err(error) => {
                        self.diagnostic(
                            DiagnosticCode::InvalidStaticType,
                            node,
                            annotation.span.clone(),
                            error.to_string(),
                        );
                        return None;
                    }
                };
                let mut rows = Vec::with_capacity(alternatives.len());
                for alternative in alternatives {
                    let mut columns = Vec::with_capacity(alternative.columns.len());
                    for (name, cell) in &alternative.columns {
                        let cell = self.resolve_type_inner(cell, node, parameters, stack)?;
                        columns.push((name.clone(), cell));
                    }
                    match RowShape::new(columns) {
                        Ok(row) => rows.push(row),
                        Err(error) => {
                            self.diagnostic(
                                DiagnosticCode::InvalidStaticType,
                                node,
                                alternative.span.clone(),
                                error.to_string(),
                            );
                            return None;
                        }
                    }
                }
                match RelationType::new(rows, cardinality) {
                    Ok(relation) => Some(StaticType::Relation(relation)),
                    Err(error) => {
                        self.diagnostic(
                            DiagnosticCode::InvalidStaticType,
                            node,
                            annotation.span.clone(),
                            error.to_string(),
                        );
                        None
                    }
                }
            }
        }
    }

    fn diagnostic(
        &mut self,
        code: DiagnosticCode,
        node: NodeId,
        span: Span,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic {
            code,
            node,
            span,
            message: message.into(),
        });
    }

    fn unsupported(&mut self, node: NodeId, message: impl Into<String>) {
        let span = self.spans.get(&node).cloned().unwrap_or(0..0);
        self.diagnostic(DiagnosticCode::UnsupportedSyntax, node, span, message);
    }
}

fn standard_type_aliases() -> &'static [TypeAliasDefinition] {
    static ALIASES: LazyLock<Vec<TypeAliasDefinition>> = LazyLock::new(|| {
        parse_ast(
            "type unit = relation<{}> where rows in 1\n\
             type option<T> = relation<{:value -> T}> where rows in 0..1\n\
             type result<T> = relation<\
               {:case -> :ok, :value -> T}\
               | {:case -> :error, :value -> error}\
             > where rows in 1",
        )
        .items
        .into_iter()
        .filter_map(|item| {
            let Item::TypeAlias {
                name,
                parameters,
                body,
                source,
                ..
            } = item
            else {
                return None;
            };
            Some(TypeAliasDefinition {
                name,
                parameters,
                body,
                source,
            })
        })
        .collect()
    });
    &ALIASES
}

fn value_kind_from_name(name: &str) -> Option<ValueKind> {
    match name {
        "bool" => Some(ValueKind::Bool),
        "int" => Some(ValueKind::Int),
        "float" => Some(ValueKind::Float),
        "identity" => Some(ValueKind::Identity),
        "string" => Some(ValueKind::String),
        "bytes" => Some(ValueKind::Bytes),
        "symbol" => Some(ValueKind::Symbol),
        "error_code" => Some(ValueKind::ErrorCode),
        "error" => Some(ValueKind::Error),
        "capability" => Some(ValueKind::Capability),
        "frob" => Some(ValueKind::Frob),
        "function" => Some(ValueKind::Function),
        "list" => Some(ValueKind::List),
        "map" => Some(ValueKind::Map),
        "range" => Some(ValueKind::Range),
        "relation" => Some(ValueKind::Relation),
        _ => None,
    }
}

fn looks_like_relation_name(name: &str) -> bool {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

fn collect_item_spans(items: &[Item], spans: &mut HashMap<NodeId, Span>) {
    for item in items {
        match item {
            Item::TypeAlias { id, span, .. } => {
                spans.insert(*id, span.clone());
            }
            Item::Expr { id, expr } => {
                spans.insert(*id, expr.span().clone());
                collect_expr_span(expr, spans);
            }
            Item::RelationRule {
                id,
                span,
                head,
                body,
            } => {
                spans.insert(*id, span.clone());
                collect_expr_span(head, spans);
                collect_expr_spans(body, spans);
            }
            Item::Method {
                id,
                span,
                params,
                body,
                ..
            } => {
                spans.insert(*id, span.clone());
                for param in params {
                    spans.insert(param.id, param.span.clone());
                }
                collect_item_spans(body, spans);
            }
        }
    }
}

fn collect_expr_spans(exprs: &[Expr], spans: &mut HashMap<NodeId, Span>) {
    for expr in exprs {
        collect_expr_span(expr, spans);
    }
}

fn collect_expr_span(expr: &Expr, spans: &mut HashMap<NodeId, Span>) {
    spans.insert(expr.id(), expr.span().clone());
    match expr {
        Expr::Literal { .. }
        | Expr::Name { .. }
        | Expr::QueryVar { .. }
        | Expr::Identity { .. }
        | Expr::Symbol { .. }
        | Expr::Hole { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. }
        | Expr::Error { .. } => {}
        Expr::Frob { value, .. } => collect_expr_span(value, spans),
        Expr::List { items, .. } => {
            for item in items {
                match item {
                    CollectionItem::Expr(expr) | CollectionItem::Splice(expr) => {
                        collect_expr_span(expr, spans);
                    }
                }
            }
        }
        Expr::Relation { rows, .. } => {
            for expr in rows.iter().flatten() {
                collect_expr_span(expr, spans);
            }
        }
        Expr::Map { entries, .. } => {
            for (key, value) in entries {
                collect_expr_span(key, spans);
                collect_expr_span(value, spans);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Return {
            value: Some(expr), ..
        }
        | Expr::Effect { expr, .. } => collect_expr_span(expr, spans),
        Expr::Return { value: None, .. } => {}
        Expr::Binary { left, right, .. } => {
            collect_expr_span(left, spans);
            collect_expr_span(right, spans);
        }
        Expr::Assign { target, value, .. } => {
            collect_expr_span(target, spans);
            collect_expr_span(value, spans);
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_span(callee, spans);
            collect_arg_spans(args, spans);
        }
        Expr::RoleCall { selector, args, .. } => {
            collect_expr_span(selector, spans);
            collect_arg_spans(args, spans);
        }
        Expr::ReceiverCall {
            receiver,
            selector,
            args,
            ..
        } => {
            collect_expr_span(receiver, spans);
            collect_expr_span(selector, spans);
            collect_arg_spans(args, spans);
        }
        Expr::Spawn { target, delay, .. } => {
            collect_expr_span(target, spans);
            if let Some(delay) = delay {
                collect_expr_span(delay, spans);
            }
        }
        Expr::Index {
            collection, index, ..
        } => {
            collect_expr_span(collection, spans);
            if let Some(index) = index {
                collect_expr_span(index, spans);
            }
        }
        Expr::Field { base, .. } => collect_expr_span(base, spans),
        Expr::Binding { pattern, value, .. } => {
            if let BindingPattern::Scatter(bindings) = pattern {
                for binding in bindings {
                    spans.insert(binding.id, binding.span.clone());
                    if let Some(default) = &binding.default {
                        collect_expr_span(default, spans);
                    }
                }
            }
            if let Some(value) = value {
                collect_expr_span(value, spans);
            }
        }
        Expr::If {
            condition,
            then_items,
            elseif,
            else_items,
            ..
        } => {
            collect_expr_span(condition, spans);
            collect_item_spans(then_items, spans);
            for (condition, items) in elseif {
                collect_expr_span(condition, spans);
                collect_item_spans(items, spans);
            }
            collect_item_spans(else_items, spans);
        }
        Expr::Block { items, .. } => collect_item_spans(items, spans),
        Expr::For {
            key,
            value,
            iter,
            body,
            ..
        } => {
            spans.insert(key.id, key.span.clone());
            if let Some(value) = value {
                spans.insert(value.id, value.span.clone());
            }
            collect_expr_span(iter, spans);
            collect_item_spans(body, spans);
        }
        Expr::While {
            condition, body, ..
        } => {
            collect_expr_span(condition, spans);
            collect_item_spans(body, spans);
        }
        Expr::Raise {
            error,
            message,
            value,
            ..
        } => {
            collect_expr_span(error, spans);
            if let Some(message) = message {
                collect_expr_span(message, spans);
            }
            if let Some(value) = value {
                collect_expr_span(value, spans);
            }
        }
        Expr::Recover { expr, catches, .. } => {
            collect_expr_span(expr, spans);
            for catch in catches {
                collect_recovery_span(catch, spans);
            }
        }
        Expr::One { expr, .. } => collect_expr_span(expr, spans),
        Expr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            collect_item_spans(body, spans);
            for catch in catches {
                collect_catch_span(catch, spans);
            }
            collect_item_spans(finally, spans);
        }
        Expr::Function { params, body, .. } => {
            collect_param_spans(params, spans);
            match body {
                FunctionBody::Expr(expr) => collect_expr_span(expr, spans),
                FunctionBody::Block(items) => collect_item_spans(items, spans),
            }
        }
    }
}

fn collect_arg_spans(args: &[Arg], spans: &mut HashMap<NodeId, Span>) {
    for arg in args {
        spans.insert(arg.id, arg.value.span().clone());
        collect_expr_span(&arg.value, spans);
    }
}

fn collect_param_spans(params: &[Param], spans: &mut HashMap<NodeId, Span>) {
    for param in params {
        let span = param.default.as_ref().map_or_else(
            || {
                param
                    .annotation
                    .as_ref()
                    .map_or(0..0, |annotation| annotation.span.clone())
            },
            |default| default.span().clone(),
        );
        spans.insert(param.id, span);
        if let Some(default) = &param.default {
            collect_expr_span(default, spans);
        }
    }
}

fn collect_catch_span(catch: &CatchClause, spans: &mut HashMap<NodeId, Span>) {
    let span = catch.condition.as_ref().map_or_else(
        || first_item_span(&catch.body).unwrap_or(0..0),
        |condition| condition.span().clone(),
    );
    spans.insert(catch.id, span);
    if let Some(condition) = &catch.condition {
        collect_expr_span(condition, spans);
    }
    collect_item_spans(&catch.body, spans);
}

fn collect_recovery_span(catch: &RecoveryClause, spans: &mut HashMap<NodeId, Span>) {
    spans.insert(catch.id, catch.value.span().clone());
    if let Some(condition) = &catch.condition {
        collect_expr_span(condition, spans);
    }
    collect_expr_span(&catch.value, spans);
}

fn first_item_span(items: &[Item]) -> Option<Span> {
    items.first().map(|item| match item {
        Item::Expr { expr, .. } => expr.span().clone(),
        Item::RelationRule { span, .. }
        | Item::Method { span, .. }
        | Item::TypeAlias { span, .. } => span.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(source: &str) -> SemanticProgram {
        let program = parse_semantic(source);
        assert_eq!(program.parse_errors, vec![]);
        program
    }

    #[test]
    fn resolves_structural_relation_annotations_canonically() {
        let program = parse_ok(
            "let outcome: relation<{:value -> int, :case -> :ok} | {:value -> error, :case -> :error}> where rows in 1 = source()",
        );
        assert_eq!(program.diagnostics, vec![]);
        let binding = &program.bindings[0];
        assert_eq!(binding.declared_kind, Some(ValueKind::Relation));
        assert_eq!(
            binding.declared_type.as_ref().unwrap().to_string(),
            "relation<{:case -> :error, :value -> error} | {:case -> :ok, :value -> int}> where rows in 1"
        );
    }

    #[test]
    fn enables_every_runtime_value_kind_on_ordinary_bindings() {
        let source = [
            "let v_bool: bool = nothing",
            "let v_int: int = nothing",
            "let v_float: float = nothing",
            "let v_identity: identity = nothing",
            "let v_string: string = nothing",
            "let v_bytes: bytes = nothing",
            "let v_symbol: symbol = nothing",
            "let v_error_code: error_code = nothing",
            "let v_error: error = nothing",
            "let v_capability: capability = nothing",
            "let v_frob: frob = nothing",
            "let v_function: function = nothing",
            "let v_list: list = nothing",
            "let v_map: map = nothing",
            "let v_range: range = nothing",
            "let v_relation: relation = nothing",
        ]
        .join("\n");
        let program = parse_ok(&source);

        assert_eq!(
            program
                .bindings
                .iter()
                .filter_map(|binding| binding.declared_kind)
                .collect::<Vec<_>>(),
            vec![
                ValueKind::Bool,
                ValueKind::Int,
                ValueKind::Float,
                ValueKind::Identity,
                ValueKind::String,
                ValueKind::Bytes,
                ValueKind::Symbol,
                ValueKind::ErrorCode,
                ValueKind::Error,
                ValueKind::Capability,
                ValueKind::Frob,
                ValueKind::Function,
                ValueKind::List,
                ValueKind::Map,
                ValueKind::Range,
                ValueKind::Relation,
            ]
        );
        assert_eq!(program.diagnostics, vec![]);
    }

    #[test]
    fn annotated_bindings_require_an_initializer() {
        let program = parse_ok("let count: int");

        assert_eq!(program.diagnostics.len(), 1);
        assert_eq!(
            program.diagnostics[0].code,
            DiagnosticCode::MissingValueKindInitializer
        );
        assert_eq!(
            program.diagnostics[0].message,
            "annotated bindings require an initializer"
        );
    }

    #[test]
    fn enables_parameter_and_result_kind_metadata() {
        let program = parse_ok("fn convert(value: float) -> string => value");

        let HirItem::Expr {
            expr:
                HirExpr::Function {
                    params,
                    result_kind,
                    ..
                },
            ..
        } = &program.hir.items[0]
        else {
            panic!("expected function");
        };
        assert_eq!(params[0].declared_kind, Some(ValueKind::Float));
        assert_eq!(*result_kind, Some(ValueKind::String));
        assert_eq!(program.diagnostics, vec![]);
    }

    #[test]
    fn resolves_collection_binding_kind_metadata() {
        let program = parse_ok(
            "let [head: int, ?label: string = \"\", @tail: list] = [1]\n\
             for index: int, value: int in 1..2\nend",
        );

        assert_eq!(program.diagnostics, vec![]);
        let HirItem::Expr {
            expr: HirExpr::Binding { scatter, .. },
            ..
        } = &program.hir.items[0]
        else {
            panic!("expected scatter binding");
        };
        assert_eq!(scatter[0].declared_kind, Some(ValueKind::Int));
        assert_eq!(scatter[1].declared_kind, Some(ValueKind::String));
        assert_eq!(scatter[2].declared_kind, Some(ValueKind::List));

        let HirItem::Expr {
            expr:
                HirExpr::For {
                    key,
                    value: Some(value),
                    ..
                },
            ..
        } = &program.hir.items[1]
        else {
            panic!("expected two-binding loop");
        };
        assert_eq!(key.declared_kind, Some(ValueKind::Int));
        assert_eq!(value.declared_kind, Some(ValueKind::Int));
        for (name, kind) in [
            ("head", ValueKind::Int),
            ("label", ValueKind::String),
            ("tail", ValueKind::List),
            ("index", ValueKind::Int),
            ("value", ValueKind::Int),
        ] {
            assert_eq!(
                program
                    .bindings
                    .iter()
                    .find(|binding| binding.name == name)
                    .and_then(|binding| binding.declared_kind),
                Some(kind)
            );
        }
    }

    #[test]
    fn normalizes_installed_verb_contracts_into_bindings() {
        let program = parse_ok(
            "verb echo(value @ #string: string) -> string\n\
               return value\n\
             end",
        );

        assert_eq!(program.diagnostics, vec![]);
        let HirItem::Method {
            params,
            result_kind,
            body,
            ..
        } = &program.hir.items[0]
        else {
            panic!("expected installed verb");
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].declared_kind, Some(ValueKind::String));
        assert_eq!(*result_kind, Some(ValueKind::String));
        assert_eq!(
            program.bindings[params[0].binding.as_u32() as usize].name,
            "value"
        );
        assert_eq!(
            program.bindings[params[0].binding.as_u32() as usize].kind,
            LocalKind::InstalledParam
        );
        assert!(matches!(
            &body[0],
            HirItem::Expr {
                expr: HirExpr::Return {
                    value: Some(value),
                    ..
                },
                ..
            } if matches!(&**value, HirExpr::LocalRef { binding, .. } if *binding == params[0].binding)
        ));
    }

    #[test]
    fn reports_unknown_value_kind_at_the_kind_reference() {
        let source = "let count: integer = 1";
        let program = parse_ok(source);

        assert_eq!(program.diagnostics.len(), 1);
        assert_eq!(
            program.diagnostics[0].code,
            DiagnosticCode::UnknownValueKind
        );
        let start = source.find("integer").unwrap();
        assert_eq!(program.diagnostics[0].span, start..start + "integer".len());
        assert_eq!(program.bindings[0].declared_kind, None);
    }

    #[test]
    fn resolves_every_runtime_value_kind_from_its_canonical_source_name() {
        let kinds = [
            ValueKind::Bool,
            ValueKind::Int,
            ValueKind::Float,
            ValueKind::Identity,
            ValueKind::String,
            ValueKind::Bytes,
            ValueKind::Symbol,
            ValueKind::ErrorCode,
            ValueKind::Error,
            ValueKind::Capability,
            ValueKind::Frob,
            ValueKind::Function,
            ValueKind::List,
            ValueKind::Map,
            ValueKind::Range,
            ValueKind::Relation,
        ];

        for kind in kinds {
            let source = format!("let value: {} = dynamic()", kind.name());
            let program = parse_ok(&source);
            assert_eq!(program.diagnostics, vec![], "source: {source}");
            assert_eq!(program.bindings[0].declared_kind, Some(kind));
            assert_eq!(value_kind_from_name(kind.name()), Some(kind));
        }
        assert_eq!(value_kind_from_name("integer"), None);
    }

    #[test]
    fn reports_unknown_installed_verb_kinds_at_exact_references() {
        let source = "verb typed(value: scalarish) -> answer\n  return value\nend";
        let program = parse_ok(source);

        assert_eq!(program.diagnostics.len(), 2);
        for (diagnostic, name) in program.diagnostics.iter().zip(["scalarish", "answer"]) {
            assert_eq!(diagnostic.code, DiagnosticCode::UnknownValueKind);
            let start = source.find(name).unwrap();
            assert_eq!(diagnostic.span, start..start + name.len());
        }
    }

    #[test]
    fn preserves_span_map_for_stable_node_ids() {
        let source = "let x = 1\n\
                      CanMove(#alice, #coin)\n\
                      {y} => x + y";
        let ast = parse_ast(source);
        assert_eq!(ast.errors, vec![]);
        let program = analyze_ast(&ast);
        for id in 0..ast.node_count {
            assert!(
                program.span(NodeId(id)).is_some(),
                "missing span for node {id}"
            );
        }
    }

    #[test]
    fn resolves_nested_scopes_and_captures() {
        let program = parse_ok(
            "let x = 1\n\
             let f = {y} => x + y\n\
             f(2)",
        );
        assert_eq!(program.diagnostics, vec![]);
        let x = program
            .bindings
            .iter()
            .find(|binding| binding.name == "x")
            .unwrap();
        let function = program
            .hir
            .items
            .iter()
            .find_map(|item| match item {
                HirItem::Expr {
                    expr:
                        HirExpr::Binding {
                            value: Some(value), ..
                        },
                    ..
                } => match &**value {
                    HirExpr::Function { id, .. } => Some(*id),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert_eq!(program.captures.get(&function), Some(&vec![x.id]));
    }

    #[test]
    fn distinguishes_locals_from_external_relations_and_runtime_names() {
        let program = parse_ok(
            "let actor = #alice\n\
             CanMove(actor, #coin)\n\
             format_name(actor)",
        );
        assert_eq!(program.diagnostics, vec![]);
        assert!(program.references.iter().any(|reference| matches!(
            &reference.resolution,
            ResolvedName::External { name, kind: ExternalNameKind::Relation }
                if name == "CanMove"
        )));
        assert!(program.references.iter().any(|reference| matches!(
            &reference.resolution,
            ResolvedName::External { name, kind: ExternalNameKind::Runtime }
                if name == "format_name"
        )));
        assert!(
            program
                .references
                .iter()
                .any(|reference| matches!(reference.resolution, ResolvedName::Local(_)))
        );
    }

    #[test]
    fn treats_slash_qualified_uppercase_leaf_names_as_relations() {
        let program = parse_ok(
            "ui/CanInspect(#ui/alice, #ui/lamp)\n\
             ui/render_node(#ui/lamp)",
        );
        assert_eq!(program.diagnostics, vec![]);
        assert!(program.references.iter().any(|reference| matches!(
            &reference.resolution,
            ResolvedName::External { name, kind: ExternalNameKind::Relation }
                if name == "ui/CanInspect"
        )));
        assert!(program.references.iter().any(|reference| matches!(
            &reference.resolution,
            ResolvedName::External { name, kind: ExternalNameKind::Runtime }
                if name == "ui/render_node"
        )));
    }

    #[test]
    fn validates_assignment_targets_and_const_assignments() {
        let program = parse_ok(
            "const limit = 3\n\
             limit = 4\n\
             1 = limit\n\
             #lamp.name = \"gold\"",
        );
        assert_eq!(program.diagnostics.len(), 2);
        assert!(
            program
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::AssignToConst)
        );
        assert!(
            program
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidAssignmentTarget)
        );
        assert!(matches!(
            &program.hir.items[3],
            HirItem::Expr { expr: HirExpr::Assign { target: HirPlace::Dot { name, .. }, .. }, .. }
                if name == "name"
        ));
    }

    #[test]
    fn reports_duplicate_bindings_and_invalid_fact_changes() {
        let program = parse_ok(
            "let x = 1\n\
             let x = 2\n\
             assert x",
        );
        assert!(
            program
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::DuplicateBinding)
        );
        assert!(
            program
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidFactChange)
        );
    }

    #[test]
    fn normalizes_dispatch_relation_atoms_and_fact_changes() {
        let program = parse_ok(
            ":move(actor: #alice, item: #coin)\n\
             #box:put(item: #coin, prep: :into)\n\
             CanMove(#alice, #coin)\n\
             assert LocatedIn(#coin, #box)\n\
             retract LocatedIn(#coin, _)",
        );
        assert_eq!(program.diagnostics, vec![]);
        assert!(matches!(
            &program.hir.items[0],
            HirItem::Expr {
                expr: HirExpr::RoleDispatch { .. },
                ..
            }
        ));
        assert!(matches!(
            &program.hir.items[1],
            HirItem::Expr {
                expr: HirExpr::ReceiverDispatch { .. },
                ..
            }
        ));
        assert!(matches!(
            &program.hir.items[2],
            HirItem::Expr { expr: HirExpr::RelationAtom(atom), .. } if atom.name == "CanMove"
        ));
        assert!(matches!(
            &program.hir.items[3],
            HirItem::Expr { expr: HirExpr::FactChange { kind: EffectKind::Assert, atom, .. }, .. }
                if atom.name == "LocatedIn"
        ));
        assert!(matches!(
            &program.hir.items[4],
            HirItem::Expr { expr: HirExpr::FactChange { kind: EffectKind::Retract, atom, .. }, .. }
                if atom.name == "LocatedIn"
        ));
    }

    #[test]
    fn keeps_blocks_loops_and_catches_scoped() {
        let program = parse_ok(
            "let items = [1]\n\
             for key, value in items\n\
               let seen = value\n\
             end\n\
             try\n\
               risky()\n\
             catch err\n\
               err\n\
             end",
        );
        assert_eq!(program.diagnostics, vec![]);
        assert!(
            program
                .bindings
                .iter()
                .any(|binding| binding.name == "key" && binding.kind == LocalKind::Loop)
        );
        assert!(
            program
                .bindings
                .iter()
                .any(|binding| binding.name == "value" && binding.kind == LocalKind::Loop)
        );
        assert!(
            program
                .bindings
                .iter()
                .any(|binding| binding.name == "err" && binding.kind == LocalKind::Catch)
        );
        assert!(program.scopes.len() >= 4);
    }

    #[test]
    fn reports_backend_limited_surface_forms() {
        let program = parse_ok(
            "#box:put(#alice)\n\
             try\n\
               raise E_FAIL\n\
             catch err if err.code == E_FAIL\n\
               err\n\
             end\n\
             Visible(@args) :- Source(?x)",
        );

        let messages = program
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::UnsupportedSyntax)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(messages.contains(&"relation argument splices are not valid here"));
    }

    #[test]
    fn allows_receiver_dispatch_positional_or_named_arguments() {
        let program = parse_ok(
            "#box:put(#alice, #coin)\n\
             #box:put(actor: #alice, item: #coin)",
        );

        assert_eq!(program.diagnostics, vec![]);
    }

    #[test]
    fn rejects_mixed_receiver_dispatch_argument_styles() {
        let program = parse_ok("#box:put(#alice, item: #coin)");

        assert!(program.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnsupportedSyntax
                && diagnostic.message
                    == "receiver dispatch arguments must be all positional or all role-named"
        }));
    }

    #[test]
    fn rejects_query_variables_in_non_relation_arguments() {
        let program = parse_ok("foo(?value)");

        assert!(program.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnsupportedSyntax
                && diagnostic.message == "query variables are only valid as relation arguments"
        }));
    }

    #[test]
    fn resolves_parameterized_nested_and_installed_type_aliases() {
        let program = parse_ok(
            "type Cell<T> = relation<{:value -> T}> where rows in 1\n\
             type IntCell = Cell<int>\n\
             let value: IntCell = [:value] { [1] }",
        );
        assert_eq!(program.diagnostics, vec![]);
        let declared = program
            .bindings
            .iter()
            .find(|binding| binding.name == "value")
            .and_then(|binding| binding.declared_type.as_ref());
        assert!(matches!(declared, Some(StaticType::Relation(_))));
        assert_eq!(program.type_aliases.len(), 2);

        let aliases = program.type_aliases[..1].to_vec();
        let installed = analyze_ast_with_aliases(
            &parse_ast("let value: Cell<string> = [:value] { [\"ok\"] }"),
            &aliases,
        );
        assert_eq!(installed.diagnostics, vec![]);
        assert_eq!(
            installed.type_alias_dependencies,
            BTreeSet::from(["Cell".to_owned()])
        );
    }

    #[test]
    fn diagnoses_alias_arity_parameters_unknown_names_and_cycles() {
        for (source, expected) in [
            ("type Pair<T, T> = T", "duplicate type parameter `T`"),
            (
                "type Box<T> = T\nlet value: Box = 1",
                "expects 1 argument(s), received 0",
            ),
            ("type Bad<T> = Missing<T>", "unknown type `Missing`"),
            ("type Loop = Loop", "cyclic type alias: Loop -> Loop"),
            ("type Left = Right\ntype Right = Left", "cyclic type alias"),
        ] {
            let program = parse_semantic(source);
            assert!(
                program
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "missing `{expected}` in {:?}",
                program.diagnostics
            );
        }
    }
}
