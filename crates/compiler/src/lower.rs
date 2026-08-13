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
    Arg, Ast, BinaryOp, BindingKind, BindingPattern, CardinalityRef, CatchClause, CollectionItem,
    CstElement, CstNode, CstToken, DispatchRestriction, EffectKind, Expr, FunctionBody, Item,
    Literal, LoopBinding, MethodKind, MethodParam, NodeId, Param, ParamMode, ParseError,
    RecoveryClause, ScatterBinding, Span, SyntaxKind, TypeLiteralRef, TypeRef, TypeRefKind,
    TypeRowRef, UnaryOp, parse,
};
use base64::{Engine, engine::general_purpose};

pub fn parse_ast(source: &str) -> Ast {
    let parse = parse(source);
    let mut lower = Lower::new(source, parse.errors);
    let items = lower.lower_program(&parse.root);
    Ast {
        items,
        errors: lower.errors,
        node_count: lower.next_id,
    }
}

struct Lower<'a> {
    source: &'a str,
    errors: Vec<ParseError>,
    next_id: u32,
}

impl<'a> Lower<'a> {
    fn new(source: &'a str, errors: Vec<ParseError>) -> Self {
        Self {
            source,
            errors,
            next_id: 0,
        }
    }

    fn node_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    fn lower_program(&mut self, root: &CstNode) -> Vec<Item> {
        self.node_children(root)
            .find(|node| node.kind == SyntaxKind::ItemList)
            .map(|node| self.lower_items(node))
            .unwrap_or_default()
    }

    fn lower_items(&mut self, node: &CstNode) -> Vec<Item> {
        self.node_children(node)
            .filter_map(|child| self.lower_item(child))
            .collect()
    }

    fn lower_item(&mut self, node: &CstNode) -> Option<Item> {
        match node.kind {
            SyntaxKind::ExprStmt => self
                .node_children(node)
                .next()
                .map(|child| match child.kind {
                    SyntaxKind::RelationRule => self.lower_relation_rule(child),
                    _ => Item::Expr {
                        id: self.node_id(),
                        expr: self.lower_expr(child),
                    },
                }),
            SyntaxKind::MethodItem => Some(self.lower_method_item(node, MethodKind::Method)),
            SyntaxKind::VerbItem => Some(self.lower_method_item(node, MethodKind::Verb)),
            SyntaxKind::TypeAliasItem => Some(self.lower_type_alias_item(node)),
            _ => {
                self.error(node, "expected item");
                None
            }
        }
    }

    fn lower_type_alias_item(&mut self, node: &CstNode) -> Item {
        let mut identifiers = self
            .token_children(node)
            .filter(|token| token.kind == SyntaxKind::Ident)
            .map(|token| self.text(token.span.clone()).to_owned())
            .skip(1);
        let name = identifiers.next().unwrap_or_default();
        let parameters = identifiers.collect();
        let body = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::TypeRef)
            .map(|child| self.lower_type_ref(child))
            .unwrap_or(TypeRef {
                kind: TypeRefKind::Named {
                    name: "dynamic".to_owned(),
                    arguments: Vec::new(),
                },
                span: node.span.clone(),
            });
        Item::TypeAlias {
            id: self.node_id(),
            span: node.span.clone(),
            name,
            parameters,
            body,
            source: self.source[node.span.clone()].trim().to_owned(),
        }
    }

    fn lower_relation_rule(&mut self, node: &CstNode) -> Item {
        let exprs = self
            .node_children(node)
            .map(|child| self.lower_expr(child))
            .collect::<Vec<_>>();
        let mut iter = exprs.into_iter();
        let head = iter.next().unwrap_or_else(|| self.error_expr(node));
        Item::RelationRule {
            id: self.node_id(),
            span: node.span.clone(),
            head,
            body: iter.collect(),
        }
    }

    fn lower_method_item(&mut self, node: &CstNode, kind: MethodKind) -> Item {
        let header = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::MethodHeader);
        let verb_header = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::VerbHeader);
        let clause_nodes = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::MethodClause)
            .collect::<Vec<_>>();
        let clauses = clause_nodes
            .iter()
            .map(|child| self.text(child.span.clone()).trim().to_owned())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        if matches!(kind, MethodKind::Method) && method_clauses_use_colon_params(&clauses) {
            self.error(
                node,
                "value-kind annotations are only supported on verb parameters; method dispatch restrictions use `name @ #prototype`",
            );
        }
        let (identity, selector, params, result_type) = match kind {
            MethodKind::Method => {
                let (identity, selector) = header
                    .map(|header| self.lower_method_header(header))
                    .unwrap_or((None, None));
                let params = clause_nodes
                    .iter()
                    .flat_map(|clause| self.lower_method_clause_params(clause))
                    .collect();
                (identity, selector, params, None)
            }
            MethodKind::Verb => verb_header
                .map(|header| self.lower_verb_header(header))
                .unwrap_or((None, None, Vec::new(), None)),
        };
        let body = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
            .map(|body| self.lower_items(body))
            .unwrap_or_default();
        Item::Method {
            id: self.node_id(),
            span: node.span.clone(),
            kind,
            identity,
            selector,
            clauses,
            params,
            result_type,
            body,
        }
    }

    fn lower_method_header(&self, node: &CstNode) -> (Option<String>, Option<String>) {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let identity = identity_after_hash(self.source, &tokens, 0);
        let selector = tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::Colon)
            .and_then(|idx| qualified_name_from_tokens(self.source, &tokens, idx + 1));
        (identity, selector)
    }

    fn lower_verb_header(
        &mut self,
        node: &CstNode,
    ) -> (
        Option<String>,
        Option<String>,
        Vec<MethodParam>,
        Option<TypeRef>,
    ) {
        let selector = qualified_name_from_tokens(
            self.source,
            &self.token_children(node).collect::<Vec<_>>(),
            0,
        );
        let params = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::VerbParamList)
            .map(|params| {
                self.node_children(params)
                    .filter(|child| child.kind == SyntaxKind::VerbParam)
                    .map(|param| self.lower_verb_param(param))
                    .collect()
            })
            .unwrap_or_default();
        let result_type = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::TypeRef)
            .map(|kind| self.lower_type_ref(kind));
        (None, selector, params, result_type)
    }

    fn lower_verb_param(&mut self, node: &CstNode) -> MethodParam {
        MethodParam {
            id: self.node_id(),
            name: self.first_text(node, SyntaxKind::Ident).unwrap_or_default(),
            restriction: self
                .node_children(node)
                .find(|child| child.kind == SyntaxKind::DispatchRestriction)
                .map(|restriction| self.lower_dispatch_restriction(restriction)),
            annotation: self
                .node_children(node)
                .find(|child| child.kind == SyntaxKind::TypeRef)
                .map(|kind| self.lower_type_ref(kind)),
            span: node.span.clone(),
        }
    }

    fn lower_dispatch_restriction(&self, node: &CstNode) -> DispatchRestriction {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        DispatchRestriction {
            prototype: tokens
                .iter()
                .position(|token| token.kind == SyntaxKind::Hash)
                .and_then(|hash| qualified_name_from_tokens(self.source, &tokens, hash + 1))
                .unwrap_or_default(),
            frob_only: tokens.iter().any(|token| token.kind == SyntaxKind::Lt),
            span: node.span.clone(),
        }
    }

    fn lower_method_clause_params(&mut self, node: &CstNode) -> Vec<MethodParam> {
        let raw = self.text(node.span.clone());
        let leading = raw.len() - raw.trim_start().len();
        let mut text = raw.trim_start();
        let mut offset = node.span.start + leading;
        if let Some(rest) = text.strip_prefix("roles") {
            let whitespace = rest.len() - rest.trim_start().len();
            text = rest.trim_start();
            offset += "roles".len() + whitespace;
        }
        parse_method_param_list(text, offset)
            .into_iter()
            .map(|param| MethodParam {
                id: self.node_id(),
                name: param.name,
                restriction: param.restriction,
                annotation: None,
                span: param.span,
            })
            .collect()
    }

    fn lower_expr(&mut self, node: &CstNode) -> Expr {
        match node.kind {
            SyntaxKind::LiteralExpr => self.lower_literal(node),
            SyntaxKind::NameExpr => self.lower_name(node),
            SyntaxKind::QueryVarExpr => self.lower_query_var(node),
            SyntaxKind::IdentityExpr => self.lower_identity(node),
            SyntaxKind::FrobExpr => self.lower_frob(node),
            SyntaxKind::SymbolExpr => self.lower_symbol(node),
            SyntaxKind::DomExpr => self.lower_dom_expr(node),
            SyntaxKind::HoleExpr => Expr::Hole {
                id: self.node_id(),
                span: node.span.clone(),
            },
            SyntaxKind::ListExpr => self.lower_list(node),
            SyntaxKind::RelationExpr => self.lower_relation(node),
            SyntaxKind::MapExpr => self.lower_map(node),
            SyntaxKind::UnaryExpr => self.lower_unary(node),
            SyntaxKind::BinaryExpr => self.lower_binary(node),
            SyntaxKind::AssignExpr => self.lower_assign(node),
            SyntaxKind::CallExpr => self.lower_call(node),
            SyntaxKind::RoleCallExpr => self.lower_role_call(node),
            SyntaxKind::ReceiverCallExpr => self.lower_receiver_call(node),
            SyntaxKind::SpawnExpr => self.lower_spawn(node),
            SyntaxKind::IndexExpr => self.lower_index(node),
            SyntaxKind::FieldExpr => self.lower_field(node),
            SyntaxKind::LetExpr => self.lower_binding(node, BindingKind::Let),
            SyntaxKind::ConstExpr => self.lower_binding(node, BindingKind::Const),
            SyntaxKind::IfExpr => self.lower_if(node),
            SyntaxKind::BeginExpr => Expr::Block {
                id: self.node_id(),
                span: node.span.clone(),
                items: self
                    .node_children(node)
                    .find(|child| child.kind == SyntaxKind::Block)
                    .map(|block| self.lower_items(block))
                    .unwrap_or_default(),
            },
            SyntaxKind::ForExpr => self.lower_for(node),
            SyntaxKind::WhileExpr => self.lower_while(node),
            SyntaxKind::ReturnExpr => self.lower_return(node),
            SyntaxKind::RaiseExpr => self.lower_raise(node),
            SyntaxKind::RecoverExpr => self.lower_recover(node),
            SyntaxKind::OneExpr => self.lower_one(node),
            SyntaxKind::BreakExpr => Expr::Break {
                id: self.node_id(),
                span: node.span.clone(),
            },
            SyntaxKind::ContinueExpr => Expr::Continue {
                id: self.node_id(),
                span: node.span.clone(),
            },
            SyntaxKind::TryExpr => self.lower_try(node),
            SyntaxKind::FnExpr => self.lower_fn(node),
            SyntaxKind::LambdaExpr => self.lower_lambda(node),
            SyntaxKind::AssertExpr => self.lower_effect(node, EffectKind::Assert),
            SyntaxKind::RetractExpr => self.lower_effect(node, EffectKind::Retract),
            SyntaxKind::RequireExpr => self.lower_effect(node, EffectKind::Require),
            SyntaxKind::GroupExpr => match self.node_children(node).next() {
                Some(child) => self.lower_expr(child),
                None => Expr::Literal {
                    id: self.node_id(),
                    span: node.span.clone(),
                    value: Literal::Unit,
                },
            },
            SyntaxKind::AtomExpr => self.error_expr(node),
            _ => {
                self.error(node, "expected expression node");
                self.error_expr(node)
            }
        }
    }

    fn lower_literal(&mut self, node: &CstNode) -> Expr {
        let Some(token) = self.token_children(node).next() else {
            return Expr::Error {
                id: self.node_id(),
                span: node.span.clone(),
            };
        };
        let value = match token.kind {
            SyntaxKind::Int => Literal::Int(self.text(token.span.clone()).to_owned()),
            SyntaxKind::Float => Literal::Float(self.text(token.span.clone()).to_owned()),
            SyntaxKind::String => Literal::String(unquote(self.text(token.span.clone()))),
            SyntaxKind::Bytes => match decode_bytes_literal(self.text(token.span.clone())) {
                Ok(bytes) => Literal::Bytes(bytes),
                Err(message) => {
                    self.errors
                        .push(ParseError::new(message, token.span.clone()));
                    Literal::Bytes(Vec::new())
                }
            },
            SyntaxKind::TrueKw => Literal::Bool(true),
            SyntaxKind::FalseKw => Literal::Bool(false),
            SyntaxKind::ErrorCode => Literal::ErrorCode(self.text(token.span.clone()).to_owned()),
            SyntaxKind::LParen => Literal::Unit,
            SyntaxKind::NothingKw => Literal::Nothing,
            _ => Literal::Nothing,
        };
        Expr::Literal {
            id: self.node_id(),
            span: node.span.clone(),
            value,
        }
    }

    fn lower_name(&mut self, node: &CstNode) -> Expr {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        Expr::Name {
            id: self.node_id(),
            span: node.span.clone(),
            name: qualified_name_from_tokens(self.source, &tokens, 0).unwrap_or_default(),
        }
    }

    fn lower_query_var(&mut self, node: &CstNode) -> Expr {
        Expr::QueryVar {
            id: self.node_id(),
            span: node.span.clone(),
            name: self.first_text(node, SyntaxKind::Ident).unwrap_or_default(),
        }
    }

    fn lower_identity(&mut self, node: &CstNode) -> Expr {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let name = identity_after_hash(self.source, &tokens, 0).unwrap_or_else(|| {
            self.error(node, "expected identity name");
            String::new()
        });
        Expr::Identity {
            id: self.node_id(),
            span: node.span.clone(),
            name,
        }
    }

    fn lower_frob(&mut self, node: &CstNode) -> Expr {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let delegate = identity_after_hash(self.source, &tokens, 0).unwrap_or_else(|| {
            self.error(node, "expected frob delegate identity");
            String::new()
        });
        let value = self
            .node_children(node)
            .next()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::Frob {
            id: self.node_id(),
            span: node.span.clone(),
            delegate,
            value: Box::new(value),
        }
    }

    fn lower_symbol(&mut self, node: &CstNode) -> Expr {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        if let Some(colon) = tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::Colon)
            && let Some(name) = qualified_name_from_tokens(self.source, &tokens, colon + 1)
        {
            Expr::Symbol {
                id: self.node_id(),
                span: node.span.clone(),
                name,
            }
        } else {
            self.node_children(node)
                .next()
                .map(|child| self.lower_expr(child))
                .unwrap_or_else(|| self.error_expr(node))
        }
    }

    fn lower_dom_expr(&mut self, node: &CstNode) -> Expr {
        self.node_children(node)
            .find(|child| child.kind == SyntaxKind::DomElement)
            .map(|child| self.lower_dom_element(child))
            .unwrap_or_else(|| self.error_expr(node))
    }

    fn lower_dom_element(&mut self, node: &CstNode) -> Expr {
        let tag = self
            .token_children(node)
            .find(|token| token.kind == SyntaxKind::Ident)
            .map(|token| self.text(token.span.clone()).to_owned())
            .unwrap_or_default();

        let attrs = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::DomAttr)
            .filter_map(|child| self.lower_dom_attr(child))
            .collect::<Vec<_>>();

        let children = self
            .node_children(node)
            .filter_map(|child| match child.kind {
                SyntaxKind::DomElement => Some(CollectionItem::Expr(self.lower_dom_element(child))),
                SyntaxKind::DomChildExpr => Some(self.lower_dom_child_expr(child)),
                SyntaxKind::DomText => self.lower_dom_text(child).map(CollectionItem::Expr),
                _ => None,
            })
            .collect::<Vec<_>>();

        let tag = self.literal_string(node.span.clone(), tag);
        let attrs = Expr::Map {
            id: self.node_id(),
            span: node.span.clone(),
            entries: attrs,
        };
        let children = Expr::List {
            id: self.node_id(),
            span: node.span.clone(),
            items: children,
        };
        self.call_expr(node.span.clone(), "dom_element", vec![tag, attrs, children])
    }

    fn lower_dom_attr(&mut self, node: &CstNode) -> Option<(Expr, Expr)> {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let first = tokens
            .first()
            .filter(|token| token.kind.is_dom_name_atom())?;
        let name_end = tokens
            .iter()
            .take_while(|token| {
                token.kind.is_dom_name_atom()
                    || matches!(token.kind, SyntaxKind::Minus | SyntaxKind::Colon)
            })
            .last()
            .map(|token| token.span.end)
            .unwrap_or(first.span.end);
        let name = self.text(first.span.start..name_end).to_owned();
        let key = self.literal_string(first.span.start..name_end, name);

        let value = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .or_else(|| {
                tokens
                    .iter()
                    .find(|token| token.kind == SyntaxKind::String)
                    .map(|token| {
                        self.literal_string(
                            token.span.clone(),
                            unquote(self.text(token.span.clone())),
                        )
                    })
            })
            .unwrap_or_else(|| Expr::Literal {
                id: self.node_id(),
                span: node.span.clone(),
                value: Literal::Bool(true),
            });

        Some((key, value))
    }

    fn lower_dom_child_expr(&mut self, node: &CstNode) -> CollectionItem {
        let value = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        if self
            .token_children(node)
            .any(|token| token.kind == SyntaxKind::At)
        {
            CollectionItem::Splice(value)
        } else {
            CollectionItem::Expr(value)
        }
    }

    fn lower_dom_text(&mut self, node: &CstNode) -> Option<Expr> {
        let text = self.text(node.span.clone());
        if text.trim().is_empty() {
            return None;
        }
        let text = self.literal_string(node.span.clone(), text.to_owned());
        Some(self.call_expr(node.span.clone(), "dom_text", vec![text]))
    }

    fn lower_list(&mut self, node: &CstNode) -> Expr {
        let items = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::ListItem)
            .filter_map(|item| {
                let expr = self.node_children(item).next()?;
                let expr = self.lower_expr(expr);
                if self
                    .token_children(item)
                    .any(|token| token.kind == SyntaxKind::At)
                {
                    Some(CollectionItem::Splice(expr))
                } else {
                    Some(CollectionItem::Expr(expr))
                }
            })
            .collect();
        Expr::List {
            id: self.node_id(),
            span: node.span.clone(),
            items,
        }
    }

    fn lower_relation(&mut self, node: &CstNode) -> Expr {
        let heading = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::RelationHeading)
            .into_iter()
            .flat_map(|heading| self.node_children(heading))
            .filter(|child| child.kind == SyntaxKind::SymbolExpr)
            .filter_map(|symbol| {
                let tokens = self.token_children(symbol).collect::<Vec<_>>();
                let colon = tokens
                    .iter()
                    .position(|token| token.kind == SyntaxKind::Colon)?;
                qualified_name_from_tokens(self.source, &tokens, colon + 1)
            })
            .collect::<Vec<_>>();
        let rows = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::RelationRow)
            .map(|row| {
                let items = self
                    .node_children(row)
                    .filter(|child| child.kind == SyntaxKind::ListItem)
                    .collect::<Vec<_>>();
                for item in &items {
                    if self
                        .token_children(item)
                        .any(|token| token.kind == SyntaxKind::At)
                    {
                        self.error(item, "relation rows do not support splices");
                    }
                }
                let exprs = items
                    .into_iter()
                    .filter_map(|item| self.node_children(item).next())
                    .collect::<Vec<_>>();
                exprs
                    .into_iter()
                    .map(|expr| self.lower_expr(expr))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        for (index, column) in heading.iter().enumerate() {
            if heading[..index].contains(column) {
                self.error(node, &format!("duplicate relation column :{column}"));
            }
        }
        for row in &rows {
            if row.len() != heading.len() {
                self.error(
                    node,
                    &format!(
                        "relation row arity mismatch: expected {}, got {}",
                        heading.len(),
                        row.len()
                    ),
                );
            }
        }

        Expr::Relation {
            id: self.node_id(),
            span: node.span.clone(),
            heading,
            rows,
        }
    }

    fn lower_map(&mut self, node: &CstNode) -> Expr {
        let entries = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::MapEntry)
            .filter_map(|entry| {
                let mut exprs = self.node_children(entry).map(|expr| self.lower_expr(expr));
                Some((exprs.next()?, exprs.next()?))
            })
            .collect();
        Expr::Map {
            id: self.node_id(),
            span: node.span.clone(),
            entries,
        }
    }

    fn lower_unary(&mut self, node: &CstNode) -> Expr {
        let op = self
            .token_children(node)
            .find_map(|token| match token.kind {
                SyntaxKind::Minus => Some(UnaryOp::Neg),
                SyntaxKind::Bang | SyntaxKind::NotKw => Some(UnaryOp::Not),
                _ => None,
            })
            .unwrap_or(UnaryOp::Not);
        let expr = self
            .node_children(node)
            .next()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::Unary {
            id: self.node_id(),
            span: node.span.clone(),
            op,
            expr: Box::new(expr),
        }
    }

    fn lower_binary(&mut self, node: &CstNode) -> Expr {
        let op = self
            .token_children(node)
            .find_map(binary_op)
            .unwrap_or(BinaryOp::Add);
        let exprs = self.node_children(node).collect::<Vec<_>>();
        let left = exprs
            .first()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let right = exprs
            .get(1)
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::Binary {
            id: self.node_id(),
            span: node.span.clone(),
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn lower_assign(&mut self, node: &CstNode) -> Expr {
        let exprs = self.node_children(node).collect::<Vec<_>>();
        let target = exprs
            .first()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let value = exprs
            .get(1)
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::Assign {
            id: self.node_id(),
            span: node.span.clone(),
            target: Box::new(target),
            value: Box::new(value),
        }
    }

    fn lower_call(&mut self, node: &CstNode) -> Expr {
        let mut children = self.node_children(node);
        let callee = children
            .next()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let args = children
            .find(|child| child.kind == SyntaxKind::ArgList)
            .map(|args| self.lower_args(args))
            .unwrap_or_default();
        Expr::Call {
            id: self.node_id(),
            span: node.span.clone(),
            callee: Box::new(callee),
            args,
        }
    }

    fn lower_role_call(&mut self, node: &CstNode) -> Expr {
        let selector = self.lower_selector_after_colon(node);
        let args = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::ArgList)
            .map(|args| self.lower_args(args))
            .unwrap_or_default();
        Expr::RoleCall {
            id: self.node_id(),
            span: node.span.clone(),
            selector: Box::new(selector),
            args,
        }
    }

    fn lower_receiver_call(&mut self, node: &CstNode) -> Expr {
        let mut exprs = self.node_children(node);
        let receiver = exprs
            .next()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let selector = self.lower_selector_after_colon(node);
        let args = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::ArgList)
            .map(|args| self.lower_args(args))
            .unwrap_or_default();
        Expr::ReceiverCall {
            id: self.node_id(),
            span: node.span.clone(),
            receiver: Box::new(receiver),
            selector: Box::new(selector),
            args,
        }
    }

    fn lower_spawn(&mut self, node: &CstNode) -> Expr {
        let exprs = self.node_children(node).collect::<Vec<_>>();
        let target = exprs
            .first()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let delay = exprs.get(1).map(|child| Box::new(self.lower_expr(child)));
        Expr::Spawn {
            id: self.node_id(),
            span: node.span.clone(),
            target: Box::new(target),
            delay,
        }
    }

    fn lower_selector_after_colon(&mut self, node: &CstNode) -> Expr {
        if let Some(group) = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::GroupExpr)
        {
            return self.lower_expr(group);
        }
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let name = tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::Colon)
            .and_then(|idx| qualified_name_from_tokens(self.source, &tokens, idx + 1))
            .unwrap_or_default();
        Expr::Symbol {
            id: self.node_id(),
            span: node.span.clone(),
            name,
        }
    }

    fn lower_index(&mut self, node: &CstNode) -> Expr {
        let exprs = self.node_children(node).collect::<Vec<_>>();
        let collection = exprs
            .first()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let index = exprs.get(1).map(|child| Box::new(self.lower_expr(child)));
        Expr::Index {
            id: self.node_id(),
            span: node.span.clone(),
            collection: Box::new(collection),
            index,
        }
    }

    fn lower_field(&mut self, node: &CstNode) -> Expr {
        let base = self
            .node_children(node)
            .next()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let name = tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::Dot)
            .and_then(|idx| qualified_name_from_tokens(self.source, &tokens, idx + 1))
            .unwrap_or_default();
        Expr::Field {
            id: self.node_id(),
            span: node.span.clone(),
            base: Box::new(base),
            name,
        }
    }

    fn lower_binding(&mut self, node: &CstNode, kind: BindingKind) -> Expr {
        let pattern = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::ScatterPattern)
            .map(|child| BindingPattern::Scatter(self.lower_scatter_bindings(child)))
            .unwrap_or_else(|| {
                BindingPattern::Name(self.first_text(node, SyntaxKind::Ident).unwrap_or_default())
            });
        let annotation = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::TypeRef)
            .map(|child| self.lower_type_ref(child));
        let value = self
            .node_children(node)
            .find(|child| !matches!(child.kind, SyntaxKind::ScatterPattern | SyntaxKind::TypeRef))
            .map(|child| Box::new(self.lower_expr(child)));
        Expr::Binding {
            id: self.node_id(),
            span: node.span.clone(),
            kind,
            pattern,
            annotation,
            value,
        }
    }

    fn lower_if(&mut self, node: &CstNode) -> Expr {
        let mut exprs = self
            .node_children(node)
            .filter(|child| is_expr_node(child.kind));
        let condition = exprs
            .next()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let then_items = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        let elseif = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::ElseIfClause)
            .map(|clause| {
                let condition = self
                    .node_children(clause)
                    .find(|child| is_expr_node(child.kind))
                    .map(|child| self.lower_expr(child))
                    .unwrap_or_else(|| self.error_expr(clause));
                let body = self
                    .node_children(clause)
                    .find(|child| child.kind == SyntaxKind::Block)
                    .map(|block| self.lower_items(block))
                    .unwrap_or_default();
                (condition, body)
            })
            .collect();
        let else_items = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::ElseClause)
            .and_then(|clause| {
                self.node_children(clause)
                    .find(|child| child.kind == SyntaxKind::Block)
            })
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        Expr::If {
            id: self.node_id(),
            span: node.span.clone(),
            condition: Box::new(condition),
            then_items,
            elseif,
            else_items,
        }
    }

    fn lower_for(&mut self, node: &CstNode) -> Expr {
        let mut bindings = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::LoopBinding)
            .map(|binding| self.lower_loop_binding(binding))
            .collect::<Vec<_>>()
            .into_iter();
        let key = bindings.next().unwrap_or_else(|| LoopBinding {
            id: self.node_id(),
            name: String::new(),
            annotation: None,
            span: node.span.clone(),
        });
        let value = bindings.next();
        let iter = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let body = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        Expr::For {
            id: self.node_id(),
            span: node.span.clone(),
            key,
            value,
            iter: Box::new(iter),
            body,
        }
    }

    fn lower_while(&mut self, node: &CstNode) -> Expr {
        let condition = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let body = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        Expr::While {
            id: self.node_id(),
            span: node.span.clone(),
            condition: Box::new(condition),
            body,
        }
    }

    fn lower_return(&mut self, node: &CstNode) -> Expr {
        let value = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| Box::new(self.lower_expr(child)));
        Expr::Return {
            id: self.node_id(),
            span: node.span.clone(),
            value,
        }
    }

    fn lower_raise(&mut self, node: &CstNode) -> Expr {
        let expr_nodes = self
            .node_children(node)
            .filter(|child| is_expr_node(child.kind))
            .collect::<Vec<_>>();
        let error = expr_nodes
            .first()
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let message = expr_nodes
            .get(1)
            .map(|child| Box::new(self.lower_expr(child)));
        let value = expr_nodes
            .get(2)
            .map(|child| Box::new(self.lower_expr(child)));
        Expr::Raise {
            id: self.node_id(),
            span: node.span.clone(),
            error: Box::new(error),
            message,
            value,
        }
    }

    fn lower_recover(&mut self, node: &CstNode) -> Expr {
        let expr = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        let catches = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::RecoverClause)
            .map(|catch| self.lower_recovery_clause(catch))
            .collect();
        Expr::Recover {
            id: self.node_id(),
            span: node.span.clone(),
            expr: Box::new(expr),
            catches,
        }
    }

    fn lower_one(&mut self, node: &CstNode) -> Expr {
        let expr = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::One {
            id: self.node_id(),
            span: node.span.clone(),
            expr: Box::new(expr),
        }
    }

    fn lower_recovery_clause(&mut self, node: &CstNode) -> RecoveryClause {
        let name = self.first_text(node, SyntaxKind::Ident);
        let exprs = self
            .node_children(node)
            .filter(|child| is_expr_node(child.kind))
            .collect::<Vec<_>>();
        let (condition, value) = match exprs.as_slice() {
            [value] => (None, self.lower_expr(value)),
            [condition, value, ..] => (Some(self.lower_expr(condition)), self.lower_expr(value)),
            [] => (None, self.error_expr(node)),
        };
        RecoveryClause {
            id: self.node_id(),
            name,
            condition,
            value,
        }
    }

    fn lower_try(&mut self, node: &CstNode) -> Expr {
        let body = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        let catches = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::CatchClause)
            .map(|catch| self.lower_catch(catch))
            .collect();
        let finally = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::FinallyClause)
            .and_then(|finally| {
                self.node_children(finally)
                    .find(|child| child.kind == SyntaxKind::Block)
            })
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        Expr::Try {
            id: self.node_id(),
            span: node.span.clone(),
            body,
            catches,
            finally,
        }
    }

    fn lower_catch(&mut self, node: &CstNode) -> CatchClause {
        let name = self.first_text(node, SyntaxKind::Ident);
        let condition = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child));
        let body = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
            .map(|block| self.lower_items(block))
            .unwrap_or_default();
        CatchClause {
            id: self.node_id(),
            name,
            condition,
            body,
        }
    }

    fn lower_fn(&mut self, node: &CstNode) -> Expr {
        let name = if self
            .token_children(node)
            .any(|token| token.kind == SyntaxKind::FnKw)
        {
            self.first_text(node, SyntaxKind::Ident)
        } else {
            None
        };
        let params = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::ParamList)
            .map(|params| self.lower_params(params))
            .unwrap_or_default();
        let result_type = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::TypeRef)
            .map(|child| self.lower_type_ref(child));
        let body = if let Some(block) = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Block)
        {
            FunctionBody::Block(self.lower_items(block))
        } else {
            let expr = self
                .node_children(node)
                .filter(|child| child.kind != SyntaxKind::ParamList)
                .last()
                .map(|child| self.lower_expr(child))
                .unwrap_or_else(|| self.error_expr(node));
            FunctionBody::Expr(Box::new(expr))
        };
        Expr::Function {
            id: self.node_id(),
            span: node.span.clone(),
            name,
            params,
            result_type,
            body,
        }
    }

    fn lower_lambda(&mut self, node: &CstNode) -> Expr {
        let params = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::ParamList)
            .map(|params| self.lower_params(params))
            .unwrap_or_default();
        let body = self
            .node_children(node)
            .find(|child| child.kind != SyntaxKind::ParamList)
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::Function {
            id: self.node_id(),
            span: node.span.clone(),
            name: None,
            params,
            result_type: None,
            body: FunctionBody::Expr(Box::new(body)),
        }
    }

    fn lower_effect(&mut self, node: &CstNode, kind: EffectKind) -> Expr {
        let expr = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child))
            .unwrap_or_else(|| self.error_expr(node));
        Expr::Effect {
            id: self.node_id(),
            span: node.span.clone(),
            kind,
            expr: Box::new(expr),
        }
    }

    fn lower_args(&mut self, node: &CstNode) -> Vec<Arg> {
        self.node_children(node)
            .filter(|child| child.kind == SyntaxKind::Arg)
            .map(|arg| {
                let role = self
                    .token_children(arg)
                    .find(|token| token.kind == SyntaxKind::Ident)
                    .filter(|_| {
                        self.token_children(arg)
                            .any(|token| token.kind == SyntaxKind::Colon)
                    })
                    .map(|token| self.text(token.span.clone()).to_owned());
                let value = self
                    .node_children(arg)
                    .next()
                    .map(|expr| self.lower_expr(expr))
                    .unwrap_or_else(|| self.error_expr(arg));
                let splice = self
                    .token_children(arg)
                    .any(|token| token.kind == SyntaxKind::At);
                Arg {
                    id: self.node_id(),
                    role,
                    splice,
                    value,
                }
            })
            .collect()
    }

    fn lower_params(&mut self, node: &CstNode) -> Vec<Param> {
        let params = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::Param)
            .map(|param| self.lower_param(param))
            .collect::<Vec<_>>();
        if !params.is_empty() {
            return params;
        }

        let mut mode = ParamMode::Required;
        self.token_children(node)
            .filter_map(|token| match token.kind {
                SyntaxKind::Question => {
                    mode = ParamMode::Optional;
                    None
                }
                SyntaxKind::At => {
                    mode = ParamMode::Rest;
                    None
                }
                SyntaxKind::Ident => {
                    let param = Param {
                        id: self.node_id(),
                        name: self.text(token.span.clone()).to_owned(),
                        mode: mode.clone(),
                        annotation: None,
                        default: None,
                    };
                    mode = ParamMode::Required;
                    Some(param)
                }
                _ => None,
            })
            .collect()
    }

    fn lower_loop_binding(&mut self, node: &CstNode) -> LoopBinding {
        LoopBinding {
            id: self.node_id(),
            name: self.first_text(node, SyntaxKind::Ident).unwrap_or_default(),
            annotation: self
                .node_children(node)
                .find(|child| child.kind == SyntaxKind::TypeRef)
                .map(|child| self.lower_type_ref(child)),
            span: node.span.clone(),
        }
    }

    fn lower_scatter_bindings(&mut self, node: &CstNode) -> Vec<ScatterBinding> {
        self.node_children(node)
            .filter(|child| child.kind == SyntaxKind::ScatterBinding)
            .map(|binding| self.lower_scatter_binding(binding))
            .collect()
    }

    fn lower_scatter_binding(&mut self, node: &CstNode) -> ScatterBinding {
        let mode = if self
            .token_children(node)
            .any(|token| token.kind == SyntaxKind::At)
        {
            ParamMode::Rest
        } else if self
            .token_children(node)
            .any(|token| token.kind == SyntaxKind::Question)
        {
            ParamMode::Optional
        } else {
            ParamMode::Required
        };
        ScatterBinding {
            id: self.node_id(),
            name: self.first_text(node, SyntaxKind::Ident).unwrap_or_default(),
            mode,
            annotation: self
                .node_children(node)
                .find(|child| child.kind == SyntaxKind::TypeRef)
                .map(|child| self.lower_type_ref(child)),
            default: self
                .node_children(node)
                .find(|child| is_expr_node(child.kind))
                .map(|child| self.lower_expr(child)),
            span: node.span.clone(),
        }
    }

    fn lower_param(&mut self, node: &CstNode) -> Param {
        let mode = if self
            .token_children(node)
            .any(|token| token.kind == SyntaxKind::At)
        {
            ParamMode::Rest
        } else if self
            .token_children(node)
            .any(|token| token.kind == SyntaxKind::Question)
        {
            ParamMode::Optional
        } else {
            ParamMode::Required
        };
        let name = self.first_text(node, SyntaxKind::Ident).unwrap_or_default();
        let annotation = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::TypeRef)
            .map(|child| self.lower_type_ref(child));
        let default = self
            .node_children(node)
            .find(|child| is_expr_node(child.kind))
            .map(|child| self.lower_expr(child));
        Param {
            id: self.node_id(),
            name,
            mode,
            annotation,
            default,
        }
    }

    fn lower_type_ref(&self, node: &CstNode) -> TypeRef {
        if let Some(relation) = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::RelationType)
        {
            return self.lower_relation_type(relation);
        }

        let tokens = self.token_children(node).collect::<Vec<_>>();
        let kind = match tokens.first().map(|token| token.kind) {
            Some(SyntaxKind::Ident) => {
                let arguments = self
                    .node_children(node)
                    .find(|child| child.kind == SyntaxKind::TypeArguments)
                    .map(|arguments| {
                        self.node_children(arguments)
                            .filter(|child| child.kind == SyntaxKind::TypeRef)
                            .map(|argument| self.lower_type_ref(argument))
                            .collect()
                    })
                    .unwrap_or_default();
                TypeRefKind::Named {
                    name: self.text(tokens[0].span.clone()).to_owned(),
                    arguments,
                }
            }
            Some(SyntaxKind::Colon) => TypeRefKind::Literal(TypeLiteralRef::Symbol(
                qualified_name_from_tokens(self.source, &tokens, 1).unwrap_or_default(),
            )),
            Some(SyntaxKind::TrueKw) => TypeRefKind::Literal(TypeLiteralRef::Bool(true)),
            Some(SyntaxKind::FalseKw) => TypeRefKind::Literal(TypeLiteralRef::Bool(false)),
            Some(SyntaxKind::ErrorCode) => TypeRefKind::Literal(TypeLiteralRef::ErrorCode(
                self.text(tokens[0].span.clone()).to_owned(),
            )),
            Some(SyntaxKind::LParen) => TypeRefKind::Literal(TypeLiteralRef::Unit),
            Some(SyntaxKind::LBracket) => TypeRefKind::Literal(TypeLiteralRef::EmptyRelation),
            _ => TypeRefKind::Named {
                name: String::new(),
                arguments: Vec::new(),
            },
        };
        TypeRef {
            kind,
            span: node.span.clone(),
        }
    }

    fn lower_relation_type(&self, node: &CstNode) -> TypeRef {
        let alternatives = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::TypeRow)
            .map(|row| self.lower_type_row(row))
            .collect();
        let cardinality = self
            .node_children(node)
            .find(|child| child.kind == SyntaxKind::Cardinality)
            .map(|cardinality| self.lower_cardinality(cardinality))
            .unwrap_or(CardinalityRef { min: 0, max: None });
        TypeRef {
            kind: TypeRefKind::Relation {
                alternatives,
                cardinality,
            },
            span: node.span.clone(),
        }
    }

    fn lower_type_row(&self, node: &CstNode) -> TypeRowRef {
        let columns = self
            .node_children(node)
            .filter(|child| child.kind == SyntaxKind::TypeColumn)
            .map(|column| {
                let tokens = self.token_children(column).collect::<Vec<_>>();
                let name = tokens
                    .iter()
                    .position(|token| token.kind == SyntaxKind::Colon)
                    .and_then(|colon| qualified_name_from_tokens(self.source, &tokens, colon + 1))
                    .unwrap_or_default();
                let ty = self
                    .node_children(column)
                    .find(|child| child.kind == SyntaxKind::TypeRef)
                    .map(|ty| self.lower_type_ref(ty))
                    .unwrap_or(TypeRef {
                        kind: TypeRefKind::Named {
                            name: String::new(),
                            arguments: Vec::new(),
                        },
                        span: column.span.clone(),
                    });
                (name, ty)
            })
            .collect();
        TypeRowRef {
            columns,
            span: node.span.clone(),
        }
    }

    fn lower_cardinality(&self, node: &CstNode) -> CardinalityRef {
        let tokens = self.token_children(node).collect::<Vec<_>>();
        let minimum = tokens
            .iter()
            .find(|token| token.kind == SyntaxKind::Int)
            .and_then(|token| self.text(token.span.clone()).parse().ok())
            .unwrap_or(0);
        let has_range = tokens.iter().any(|token| token.kind == SyntaxKind::DotDot);
        let maximum = if !has_range {
            Some(minimum)
        } else if tokens.iter().any(|token| token.kind == SyntaxKind::Star) {
            None
        } else {
            tokens
                .iter()
                .filter(|token| token.kind == SyntaxKind::Int)
                .nth(1)
                .and_then(|token| self.text(token.span.clone()).parse().ok())
        };
        CardinalityRef {
            min: minimum,
            max: maximum,
        }
    }

    fn error_expr(&mut self, node: &CstNode) -> Expr {
        Expr::Error {
            id: self.node_id(),
            span: node.span.clone(),
        }
    }

    fn literal_string(&mut self, span: std::ops::Range<usize>, value: String) -> Expr {
        Expr::Literal {
            id: self.node_id(),
            span,
            value: Literal::String(value),
        }
    }

    fn name_expr(&mut self, span: std::ops::Range<usize>, name: &str) -> Expr {
        Expr::Name {
            id: self.node_id(),
            span,
            name: name.to_owned(),
        }
    }

    fn call_expr(&mut self, span: std::ops::Range<usize>, callee: &str, values: Vec<Expr>) -> Expr {
        let args = values
            .into_iter()
            .map(|value| Arg {
                id: self.node_id(),
                role: None,
                splice: false,
                value,
            })
            .collect();
        Expr::Call {
            id: self.node_id(),
            span: span.clone(),
            callee: Box::new(self.name_expr(span, callee)),
            args,
        }
    }

    fn node_children<'n>(&self, node: &'n CstNode) -> impl Iterator<Item = &'n CstNode> + use<'n> {
        node.children.iter().filter_map(|child| match child {
            CstElement::Node(node) => Some(node),
            CstElement::Token(_) => None,
        })
    }

    fn token_children<'n>(
        &self,
        node: &'n CstNode,
    ) -> impl Iterator<Item = &'n CstToken> + use<'n> {
        node.children.iter().filter_map(|child| match child {
            CstElement::Node(_) => None,
            CstElement::Token(token) => Some(token),
        })
    }

    fn first_text(&self, node: &CstNode, kind: SyntaxKind) -> Option<String> {
        self.token_children(node)
            .find(|token| token.kind == kind)
            .map(|token| self.text(token.span.clone()).to_owned())
    }

    fn text(&self, span: std::ops::Range<usize>) -> &str {
        &self.source[span]
    }

    fn error(&mut self, node: &CstNode, message: &str) {
        self.errors
            .push(ParseError::new(message, node.span.clone()));
    }
}

fn identity_after_hash(source: &str, tokens: &[&CstToken], start: usize) -> Option<String> {
    let idx = tokens
        .iter()
        .skip(start)
        .position(|token| token.kind == SyntaxKind::Hash)
        .map(|relative| start + relative + 1)?;
    if tokens
        .get(idx)
        .is_some_and(|token| token.kind == SyntaxKind::Int)
    {
        return tokens
            .get(idx)
            .map(|token| source[token.span.clone()].to_owned());
    }
    qualified_name_from_tokens(source, tokens, idx)
}

fn qualified_name_from_tokens(source: &str, tokens: &[&CstToken], start: usize) -> Option<String> {
    let first = tokens
        .get(start)
        .filter(|token| token.kind == SyntaxKind::Ident)?;
    let mut end = first.span.end;
    let mut idx = start + 1;
    while let (Some(slash), Some(ident)) = (tokens.get(idx), tokens.get(idx + 1)) {
        if slash.kind != SyntaxKind::Slash
            || ident.kind != SyntaxKind::Ident
            || slash.span.start != end
            || slash.span.end != ident.span.start
        {
            break;
        }
        end = ident.span.end;
        idx += 2;
    }
    Some(source[first.span.start..end].to_owned())
}

fn decode_bytes_literal(text: &str) -> Result<Vec<u8>, String> {
    let Some(content) = text
        .strip_prefix("b\"")
        .and_then(|text| text.strip_suffix('"'))
    else {
        return Err("invalid bytes literal".to_owned());
    };
    general_purpose::URL_SAFE
        .decode(content)
        .map_err(|error| format!("invalid bytes literal: invalid base64: {error}"))
}

fn method_clauses_use_colon_params(clauses: &[String]) -> bool {
    clauses
        .iter()
        .map(|clause| clause.trim())
        .filter(|clause| clause.starts_with("roles"))
        .any(|clause| clause.contains(':'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedMethodParam {
    name: String,
    restriction: Option<DispatchRestriction>,
    span: Span,
}

fn parse_method_param_list(text: &str, base_offset: usize) -> Vec<ParsedMethodParam> {
    let mut params = Vec::new();
    let mut start = 0;
    for part in text.split(',') {
        if let Some(param) = parse_method_param(part, base_offset + start) {
            params.push(param);
        }
        start += part.len() + 1;
    }
    params
}

fn parse_method_param(part: &str, base_offset: usize) -> Option<ParsedMethodParam> {
    let leading_trim = part.len() - part.trim_start().len();
    let part = part.trim();
    let part_offset = base_offset + leading_trim;
    if part.is_empty() || part.contains(':') {
        return None;
    }
    let (name, restriction) = match part.split_once('@') {
        Some((name, restriction)) => {
            let restriction_start = part_offset + name.len() + 1;
            let restriction_leading_trim = restriction.len() - restriction.trim_start().len();
            let restriction = restriction.trim();
            let restriction_span_start = restriction_start + restriction_leading_trim;
            let restriction = restriction.strip_prefix('#')?.trim();
            if restriction.is_empty() {
                return None;
            }
            let (prototype, frob_only) = restriction
                .strip_suffix("<_>")
                .map_or((restriction, false), |prototype| (prototype.trim(), true));
            if prototype.is_empty() {
                return None;
            }
            (
                name,
                Some(DispatchRestriction {
                    prototype: prototype.to_owned(),
                    frob_only,
                    span: restriction_span_start..restriction_span_start + 1 + restriction.len(),
                }),
            )
        }
        None => (part, None),
    };
    let name = name.split_whitespace().last().unwrap_or_default().trim();
    if name.is_empty() {
        return None;
    }
    Some(ParsedMethodParam {
        name: name.to_owned(),
        restriction,
        span: part_offset..part_offset + part.len(),
    })
}

fn unquote(text: &str) -> String {
    let Some(text) = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
    else {
        return text.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn binary_op(token: &CstToken) -> Option<BinaryOp> {
    Some(match token.kind {
        SyntaxKind::EqEq => BinaryOp::Eq,
        SyntaxKind::BangEq => BinaryOp::Ne,
        SyntaxKind::Lt => BinaryOp::Lt,
        SyntaxKind::LtEq => BinaryOp::Le,
        SyntaxKind::Gt => BinaryOp::Gt,
        SyntaxKind::GtEq => BinaryOp::Ge,
        SyntaxKind::Plus => BinaryOp::Add,
        SyntaxKind::Minus => BinaryOp::Sub,
        SyntaxKind::Star => BinaryOp::Mul,
        SyntaxKind::Slash => BinaryOp::Div,
        SyntaxKind::Percent => BinaryOp::Rem,
        SyntaxKind::AmpAmp => BinaryOp::And,
        SyntaxKind::PipePipe => BinaryOp::Or,
        SyntaxKind::DotDot => BinaryOp::Range,
        _ => return None,
    })
}

fn is_expr_node(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LetExpr
            | SyntaxKind::ConstExpr
            | SyntaxKind::IfExpr
            | SyntaxKind::BeginExpr
            | SyntaxKind::ForExpr
            | SyntaxKind::WhileExpr
            | SyntaxKind::ReturnExpr
            | SyntaxKind::RaiseExpr
            | SyntaxKind::RecoverExpr
            | SyntaxKind::OneExpr
            | SyntaxKind::BreakExpr
            | SyntaxKind::ContinueExpr
            | SyntaxKind::TryExpr
            | SyntaxKind::FnExpr
            | SyntaxKind::LambdaExpr
            | SyntaxKind::AssertExpr
            | SyntaxKind::RetractExpr
            | SyntaxKind::RequireExpr
            | SyntaxKind::AssignExpr
            | SyntaxKind::BinaryExpr
            | SyntaxKind::UnaryExpr
            | SyntaxKind::CallExpr
            | SyntaxKind::ReceiverCallExpr
            | SyntaxKind::RoleCallExpr
            | SyntaxKind::SpawnExpr
            | SyntaxKind::IndexExpr
            | SyntaxKind::FieldExpr
            | SyntaxKind::ListExpr
            | SyntaxKind::RelationExpr
            | SyntaxKind::MapExpr
            | SyntaxKind::GroupExpr
            | SyntaxKind::LiteralExpr
            | SyntaxKind::NameExpr
            | SyntaxKind::QueryVarExpr
            | SyntaxKind::IdentityExpr
            | SyntaxKind::FrobExpr
            | SyntaxKind::SymbolExpr
            | SyntaxKind::DomExpr
            | SyntaxKind::HoleExpr
            | SyntaxKind::AtomExpr
    )
}

#[cfg(test)]
mod tests {
    use super::parse_ast;
    use crate::{
        BinaryOp, BindingKind, BindingPattern, CardinalityRef, CollectionItem, EffectKind, Expr,
        FunctionBody, Item, Literal, MethodKind, NodeId, Param, ParamMode, TypeLiteralRef, TypeRef,
        TypeRefKind,
    };
    use std::collections::BTreeSet;

    fn named_type(ty: &TypeRef) -> &str {
        let TypeRefKind::Named { name, .. } = &ty.kind else {
            panic!("expected named type, got {ty:?}");
        };
        name
    }

    #[test]
    fn lowers_value_kind_references_with_exact_spans() {
        let source = "let count: int = 1\nfn convert(value: float) -> string => value";
        let ast = parse_ast(source);

        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr:
                Expr::Binding {
                    annotation: Some(annotation),
                    ..
                },
            ..
        } = &ast.items[0]
        else {
            panic!("expected annotated binding");
        };
        assert_eq!(named_type(annotation), "int");
        let annotation_start = source.find("int").unwrap();
        assert_eq!(annotation.span, annotation_start..annotation_start + 3);

        let Item::Expr {
            expr:
                Expr::Function {
                    params,
                    result_type,
                    ..
                },
            ..
        } = &ast.items[1]
        else {
            panic!("expected annotated function");
        };
        assert_eq!(named_type(params[0].annotation.as_ref().unwrap()), "float");
        assert_eq!(named_type(result_type.as_ref().unwrap()), "string");
        let result_start = source.rfind("string").unwrap();
        assert_eq!(
            result_type.as_ref().unwrap().span,
            result_start..result_start + 6
        );
    }

    #[test]
    fn lowers_structural_relation_type_rows_and_literals() {
        let ast = parse_ast(
            "let outcome: relation<{:case -> :ok, :value -> int} | {:case -> :error, :value -> error}> where rows in 1 = value",
        );
        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr:
                Expr::Binding {
                    annotation: Some(annotation),
                    ..
                },
            ..
        } = &ast.items[0]
        else {
            panic!("expected annotated binding");
        };
        let TypeRefKind::Relation {
            alternatives,
            cardinality,
        } = &annotation.kind
        else {
            panic!("expected relation type");
        };
        assert_eq!(
            *cardinality,
            CardinalityRef {
                min: 1,
                max: Some(1)
            }
        );
        assert_eq!(alternatives.len(), 2);
        assert_eq!(alternatives[0].columns[0].0, "case");
        assert!(matches!(
            alternatives[0].columns[0].1.kind,
            TypeRefKind::Literal(TypeLiteralRef::Symbol(ref value)) if value == "ok"
        ));
    }

    #[test]
    fn lowers_structured_collection_bindings_with_kind_spans() {
        let source = "let [head: int, ?label: string = \"\", @tail: list] = values\n\
                      for index: int, row: map in rows\nend";
        let ast = parse_ast(source);

        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr:
                Expr::Binding {
                    pattern: BindingPattern::Scatter(bindings),
                    ..
                },
            ..
        } = &ast.items[0]
        else {
            panic!("expected scatter binding");
        };
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].name, "head");
        assert_eq!(named_type(bindings[0].annotation.as_ref().unwrap()), "int");
        assert_eq!(bindings[1].mode, ParamMode::Optional);
        assert_eq!(
            named_type(bindings[1].annotation.as_ref().unwrap()),
            "string"
        );
        assert_eq!(bindings[2].mode, ParamMode::Rest);
        assert_eq!(named_type(bindings[2].annotation.as_ref().unwrap()), "list");

        let Item::Expr {
            expr:
                Expr::For {
                    key,
                    value: Some(value),
                    ..
                },
            ..
        } = &ast.items[1]
        else {
            panic!("expected two-binding loop");
        };
        assert_eq!(key.name, "index");
        assert_eq!(named_type(key.annotation.as_ref().unwrap()), "int");
        assert_eq!(value.name, "row");
        assert_eq!(named_type(value.annotation.as_ref().unwrap()), "map");
        for annotation in bindings
            .iter()
            .filter_map(|binding| binding.annotation.as_ref())
            .chain(
                [key, value]
                    .into_iter()
                    .filter_map(|binding| binding.annotation.as_ref()),
            )
        {
            assert_eq!(&source[annotation.span.clone()], named_type(annotation));
        }
    }

    #[test]
    fn lowers_calls_and_collections() {
        let ast = parse_ast(
            "let xs = [1, @rest]\n\
             let opts = {:style -> :brief}\n\
             :move(actor: #alice, item: #coin)\n\
             #box:put(#coin, :into)",
        );
        assert_eq!(ast.errors, vec![]);
        assert_eq!(ast.items.len(), 4);

        let Item::Expr {
            expr:
                Expr::Binding {
                    kind: BindingKind::Let,
                    pattern: BindingPattern::Name(name),
                    value: Some(value),
                    ..
                },
            ..
        } = &ast.items[0]
        else {
            panic!("expected let binding");
        };
        assert_eq!(name, "xs");
        let Expr::List { items, .. } = &**value else {
            panic!("expected list");
        };
        assert!(matches!(items[1], CollectionItem::Splice(_)));

        let Item::Expr {
            expr: Expr::RoleCall { args, .. },
            ..
        } = &ast.items[2]
        else {
            panic!("expected role call");
        };
        assert_eq!(args[0].role.as_deref(), Some("actor"));

        let Item::Expr {
            expr: Expr::ReceiverCall { selector, .. },
            ..
        } = &ast.items[3]
        else {
            panic!("expected receiver call");
        };
        assert!(matches!(&**selector, Expr::Symbol { name, .. } if name == "put"));
    }

    #[test]
    fn lowers_dom_markup_to_dom_calls() {
        let ast = parse_ast(
            "return dom <button type=\"submit\" class={class} data-sync-key=\"send\">\n\
               Send {label}<span aria-selected={selected}>!</span>{@extra}\n\
             </button>",
        );
        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr: Expr::Return {
                value: Some(value), ..
            },
            ..
        } = &ast.items[0]
        else {
            panic!("expected return");
        };
        let Expr::Call { callee, args, .. } = &**value else {
            panic!("expected dom_element call");
        };
        assert!(matches!(&**callee, Expr::Name { name, .. } if name == "dom_element"));
        assert_eq!(args.len(), 3);
        assert!(matches!(
            &args[0].value,
            Expr::Literal {
                value: Literal::String(tag),
                ..
            } if tag == "button"
        ));
        let Expr::Map { entries, .. } = &args[1].value else {
            panic!("expected attr map");
        };
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|(key, value)| {
            matches!(
                (key, value),
                (
                    Expr::Literal {
                        value: Literal::String(key),
                        ..
                    },
                    Expr::Name { name, .. }
                ) if key == "class" && name == "class"
            )
        }));
        let Expr::List { items, .. } = &args[2].value else {
            panic!("expected child list");
        };
        assert!(
            items
                .iter()
                .any(|item| matches!(item, CollectionItem::Splice(_)))
        );
        assert!(items.iter().any(|item| matches!(
            item,
            CollectionItem::Expr(Expr::Call { callee, .. })
                if matches!(callee.as_ref(), Expr::Name { name, .. } if name == "dom_text")
        )));
    }

    #[test]
    fn lowers_dom_attribute_name_parts_that_are_keywords() {
        let ast = parse_ast(
            "return dom <option for=\"target\" data-source-symbol-end={to_literal(2)}>symbol</option>",
        );
        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr: Expr::Return {
                value: Some(value), ..
            },
            ..
        } = &ast.items[0]
        else {
            panic!("expected return");
        };
        let Expr::Call { args, .. } = &**value else {
            panic!("expected dom_element call");
        };
        let Expr::Map { entries, .. } = &args[1].value else {
            panic!("expected attr map");
        };
        assert!(entries.iter().any(|(key, _)| {
            matches!(
                key,
                Expr::Literal {
                    value: Literal::String(key),
                    ..
                } if key == "for"
            )
        }));
        assert!(entries.iter().any(|(key, _)| {
            matches!(
                key,
                Expr::Literal {
                    value: Literal::String(key),
                    ..
                } if key == "data-source-symbol-end"
            )
        }));
    }

    #[test]
    fn dom_identifier_is_not_reserved_without_markup() {
        let ast = parse_ast("let dom = 1\nreturn dom < 2");
        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr: Expr::Return {
                value: Some(value), ..
            },
            ..
        } = &ast.items[1]
        else {
            panic!("expected return");
        };
        assert!(matches!(
            &**value,
            Expr::Binary {
                op: BinaryOp::Lt,
                ..
            }
        ));
    }

    #[test]
    fn lowers_slash_qualified_names() {
        let ast = parse_ast(
            "ui/Visible(#ui/lamp)\n\
             :ui/polish(actor: #ui/alice, item: #ui/lamp)\n\
             #ui/lamp:ui/examine(actor: #ui/alice)\n\
             method #ui/examine_method :ui/examine\n\
               roles actor @ #ui/player\n\
             do\n\
               return :ui/ok\n\
             end\n\
             verb ui/look(actor @ #ui/player)\n\
               return true\n\
             end",
        );
        assert_eq!(ast.errors, vec![]);
        assert!(matches!(
            &ast.items[0],
            Item::Expr { expr: Expr::Call { callee, args, .. }, .. }
                if matches!(&**callee, Expr::Name { name, .. } if name == "ui/Visible")
                    && matches!(&args[0].value, Expr::Identity { name, .. } if name == "ui/lamp")
        ));
        assert!(matches!(
            &ast.items[1],
            Item::Expr { expr: Expr::RoleCall { selector, args, .. }, .. }
                if matches!(&**selector, Expr::Symbol { name, .. } if name == "ui/polish")
                    && matches!(&args[0].value, Expr::Identity { name, .. } if name == "ui/alice")
        ));
        assert!(matches!(
            &ast.items[2],
            Item::Expr { expr: Expr::ReceiverCall { receiver, selector, .. }, .. }
                if matches!(&**receiver, Expr::Identity { name, .. } if name == "ui/lamp")
                    && matches!(&**selector, Expr::Symbol { name, .. } if name == "ui/examine")
        ));
        assert!(matches!(
            &ast.items[3],
            Item::Method { identity, selector, params, body, .. }
                if identity.as_deref() == Some("ui/examine_method")
                    && selector.as_deref() == Some("ui/examine")
                    && params[0].restriction.as_ref().map(|restriction| restriction.prototype.as_str()) == Some("ui/player")
                    && matches!(&body[0], Item::Expr { expr: Expr::Return { value: Some(value), .. }, .. }
                        if matches!(&**value, Expr::Symbol { name, .. } if name == "ui/ok"))
        ));
        assert!(matches!(
            &ast.items[4],
            Item::Method { kind: MethodKind::Verb, selector, params, .. }
                if selector.as_deref() == Some("ui/look")
                    && params[0].restriction.as_ref().map(|restriction| restriction.prototype.as_str()) == Some("ui/player")
        ));
    }

    #[test]
    fn lowers_relation_rule_and_control_forms() {
        let ast = parse_ast(
            "VisibleTo(actor, obj) :- LocatedIn(actor, room), LocatedIn(obj, room)\n\
             if Lit(#lamp, true)\n  \"lit\"\nelse\n  \"dark\"\nend",
        );
        assert_eq!(ast.errors, vec![]);
        assert!(matches!(
            &ast.items[0],
            Item::RelationRule { body, .. } if body.len() == 2
        ));
        assert!(matches!(
            &ast.items[1],
            Item::Expr { expr: Expr::If { else_items, .. }, .. } if else_items.len() == 1
        ));
    }

    #[test]
    fn lowers_methods_and_effects() {
        let ast = parse_ast(
            "method #move_into :move\n\
               roles actor @ #player, item @ #portable\n\
             do\n\
               require CanMove(actor, item)\n\
               assert LocatedIn(item, destination)\n\
             end",
        );
        assert_eq!(ast.errors, vec![]);
        let Item::Method {
            kind,
            identity,
            selector,
            clauses,
            params,
            body,
            ..
        } = &ast.items[0]
        else {
            panic!("expected method");
        };
        assert_eq!(kind, &MethodKind::Method);
        assert_eq!(identity.as_deref(), Some("move_into"));
        assert_eq!(selector.as_deref(), Some("move"));
        assert_eq!(clauses.len(), 1);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "actor");
        assert_eq!(
            params[0]
                .restriction
                .as_ref()
                .map(|restriction| restriction.prototype.as_str()),
            Some("player")
        );
        assert!(matches!(
            &body[0],
            Item::Expr {
                expr: Expr::Effect {
                    kind: EffectKind::Require,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn lowers_verb_header_roles() {
        let source =
            "verb get(actor @ #player: identity, item @ #thing<_>) -> bool\n  return true\nend";
        let ast = parse_ast(source);
        assert_eq!(ast.errors, vec![]);
        let Item::Method {
            kind,
            identity,
            selector,
            params,
            result_type,
            body,
            ..
        } = &ast.items[0]
        else {
            panic!("expected verb");
        };
        assert_eq!(kind, &MethodKind::Verb);
        assert_eq!(identity, &None);
        assert_eq!(selector.as_deref(), Some("get"));
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "actor");
        assert_eq!(
            params[0]
                .restriction
                .as_ref()
                .map(|restriction| restriction.prototype.as_str()),
            Some("player")
        );
        assert_eq!(
            params[0].annotation.as_ref().map(named_type),
            Some("identity")
        );
        assert_eq!(params[1].name, "item");
        assert_eq!(
            params[1]
                .restriction
                .as_ref()
                .map(|restriction| restriction.prototype.as_str()),
            Some("thing")
        );
        assert!(params[1].restriction.as_ref().unwrap().frob_only);
        assert_eq!(result_type.as_ref().map(named_type), Some("bool"));
        for kind in [params[0].annotation.as_ref(), result_type.as_ref()]
            .into_iter()
            .flatten()
        {
            assert_eq!(&source[kind.span.clone()], named_type(kind));
        }
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn lowers_unrestricted_verb_params() {
        let ast = parse_ast(
            "verb say(actor @ #player, message)\n\
               emit(actor, message)\n\
             end",
        );
        assert_eq!(ast.errors, vec![]);
        let Item::Method { params, .. } = &ast.items[0] else {
            panic!("expected verb");
        };
        assert_eq!(params.len(), 2);
        assert_eq!(
            params[0]
                .restriction
                .as_ref()
                .map(|restriction| restriction.prototype.as_str()),
            Some("player")
        );
        assert_eq!(params[1].name, "message");
        assert_eq!(params[1].restriction, None);
    }

    #[test]
    fn rejects_identity_literals_as_type_annotations() {
        let verb = parse_ast(
            "verb say(actor: #player, message)\n\
               emit(actor, message)\n\
             end",
        );
        assert!(
            verb.errors
                .iter()
                .any(|error| error.message.contains("expected type"))
        );

        let method = parse_ast(
            "method #say :say\n\
               roles actor: #player, message\n\
             do\n\
               emit(actor, message)\n\
             end",
        );
        assert!(
            method
                .errors
                .iter()
                .any(|error| error.message.contains("name @ #prototype"))
        );
    }

    #[test]
    fn lowers_functions_loops_and_try() {
        let ast = parse_ast(
            "let f = {x, ?style = :short, @rest} => x + 1\n\
             begin\n\
               for key, value in properties\n\
                 render_property(key, value)\n\
               end\n\
             end\n\
             try\n\
               risky()\n\
             catch err if err == :perm\n\
               \"permission denied\"\n\
             finally\n\
               cleanup()\n\
             end",
        );
        assert_eq!(ast.errors, vec![]);

        let Item::Expr {
            expr: Expr::Binding {
                value: Some(value), ..
            },
            ..
        } = &ast.items[0]
        else {
            panic!("expected lambda binding");
        };
        let Expr::Function {
            params,
            body: FunctionBody::Expr(body),
            ..
        } = &**value
        else {
            panic!("expected lambda function");
        };
        assert_eq!(params[1].mode, ParamMode::Optional);
        assert_eq!(params[2].mode, ParamMode::Rest);
        assert!(matches!(
            &**body,
            Expr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));

        assert!(matches!(
            &ast.items[1],
            Item::Expr { expr: Expr::Block { items, .. }, .. }
                if matches!(&items[0], Item::Expr { expr: Expr::For { key, value: Some(value), .. }, .. } if key.name == "key" && value.name == "value")
        ));
        assert!(matches!(
            &ast.items[2],
            Item::Expr { expr: Expr::Try { catches, finally, .. }, .. } if catches.len() == 1 && !finally.is_empty()
        ));
    }

    #[test]
    fn lowers_literals_and_field_assignment() {
        let ast = parse_ast(
            "#lamp.name = \"golden lamp\"\nendpoint.session/actor = #alice\ntrue\nE_NOT_PORTABLE\nnothing\n\"Alice says, \\\"hello\\\"\"\nb\"3q2-7w==\"",
        );
        assert_eq!(ast.errors, vec![]);
        assert!(matches!(
            &ast.items[0],
            Item::Expr { expr: Expr::Assign { target, value, .. }, .. }
                if matches!(&**target, Expr::Field { name, .. } if name == "name")
                    && matches!(&**value, Expr::Literal { value: Literal::String(text), .. } if text == "golden lamp")
        ));
        assert!(matches!(
            &ast.items[1],
            Item::Expr { expr: Expr::Assign { target, .. }, .. }
                if matches!(&**target, Expr::Field { name, .. } if name == "session/actor")
        ));
        assert!(matches!(
            &ast.items[2],
            Item::Expr {
                expr: Expr::Literal {
                    value: Literal::Bool(true),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            &ast.items[5],
            Item::Expr { expr: Expr::Literal { value: Literal::String(text), .. }, .. }
                if text == "Alice says, \"hello\""
        ));
        assert!(matches!(
            &ast.items[3],
            Item::Expr {
                expr: Expr::Literal {
                    value: Literal::ErrorCode(text),
                    ..
                },
                ..
            } if text == "E_NOT_PORTABLE"
        ));
        assert!(matches!(
            &ast.items[4],
            Item::Expr {
                expr: Expr::Literal {
                    value: Literal::Nothing,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            &ast.items[6],
            Item::Expr {
                expr: Expr::Literal {
                    value: Literal::Bytes(bytes),
                    ..
                },
                ..
            } if bytes == &[0xde, 0xad, 0xbe, 0xef]
        ));
    }

    #[test]
    fn lowers_relation_literal_heading_and_rows() {
        let ast = parse_ast("return [:thing, :count] { [#coin, 1], [#lamp, 2] }");
        assert_eq!(ast.errors, vec![]);
        let Item::Expr {
            expr: Expr::Return {
                value: Some(value), ..
            },
            ..
        } = &ast.items[0]
        else {
            panic!("expected return expression");
        };
        let Expr::Relation { heading, rows, .. } = &**value else {
            panic!("expected relation literal");
        };
        assert_eq!(heading, &["thing", "count"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
    }

    #[test]
    fn rejects_malformed_relation_literal_shapes() {
        let duplicate = parse_ast("return [:thing, :thing] {}");
        assert!(
            duplicate
                .errors
                .iter()
                .any(|error| error.message == "duplicate relation column :thing")
        );

        let wrong_arity = parse_ast("return [:thing, :count] { [#coin] }");
        assert!(
            wrong_arity
                .errors
                .iter()
                .any(|error| { error.message == "relation row arity mismatch: expected 2, got 1" })
        );
    }

    #[test]
    fn rejects_invalid_bytes_literal_base64() {
        let ast = parse_ast("b\"SGVsbG8\"");
        assert!(
            ast.errors
                .iter()
                .any(|error| error.message.contains("invalid bytes literal"))
        );
    }

    #[test]
    fn assigns_unique_dense_node_ids() {
        let ast = parse_ast(
            "let f = {x, ?style = :short, @rest} => x + 1\n\
             :move(actor: #alice, item: #coin)\n\
             try\n\
               risky()\n\
             catch err if err == :perm\n\
               \"permission denied\"\n\
             finally\n\
               cleanup()\n\
             end",
        );
        assert_eq!(ast.errors, vec![]);

        let mut ids = Vec::new();
        for item in &ast.items {
            collect_item_ids(item, &mut ids);
        }
        let unique = ids.iter().copied().collect::<BTreeSet<_>>();

        assert_eq!(ids.len(), unique.len());
        assert_eq!(ids.len(), ast.node_count as usize);
        assert_eq!(
            unique
                .iter()
                .copied()
                .map(NodeId::as_u32)
                .collect::<Vec<_>>(),
            (0..ast.node_count).collect::<Vec<_>>()
        );
    }

    fn collect_item_ids(item: &Item, ids: &mut Vec<NodeId>) {
        ids.push(item.id());
        match item {
            Item::TypeAlias { .. } => {}
            Item::Expr { expr, .. } => collect_expr_ids(expr, ids),
            Item::RelationRule { head, body, .. } => {
                collect_expr_ids(head, ids);
                for expr in body {
                    collect_expr_ids(expr, ids);
                }
            }
            Item::Method { params, body, .. } => {
                ids.extend(params.iter().map(|param| param.id));
                for item in body {
                    collect_item_ids(item, ids);
                }
            }
        }
    }

    fn collect_expr_ids(expr: &Expr, ids: &mut Vec<NodeId>) {
        ids.push(expr.id());
        match expr {
            Expr::List { items, .. } => {
                for item in items {
                    match item {
                        CollectionItem::Expr(expr) | CollectionItem::Splice(expr) => {
                            collect_expr_ids(expr, ids);
                        }
                    }
                }
            }
            Expr::Relation { rows, .. } => {
                for expr in rows.iter().flatten() {
                    collect_expr_ids(expr, ids);
                }
            }
            Expr::Map { entries, .. } => {
                for (key, value) in entries {
                    collect_expr_ids(key, ids);
                    collect_expr_ids(value, ids);
                }
            }
            Expr::Unary { expr, .. } => collect_expr_ids(expr, ids),
            Expr::Binary { left, right, .. } => {
                collect_expr_ids(left, ids);
                collect_expr_ids(right, ids);
            }
            Expr::Assign { target, value, .. } => {
                collect_expr_ids(target, ids);
                collect_expr_ids(value, ids);
            }
            Expr::Call { callee, args, .. } => {
                collect_expr_ids(callee, ids);
                collect_arg_ids(args, ids);
            }
            Expr::RoleCall { selector, args, .. } => {
                collect_expr_ids(selector, ids);
                collect_arg_ids(args, ids);
            }
            Expr::ReceiverCall {
                receiver,
                selector,
                args,
                ..
            } => {
                collect_expr_ids(receiver, ids);
                collect_expr_ids(selector, ids);
                collect_arg_ids(args, ids);
            }
            Expr::Spawn { target, delay, .. } => {
                collect_expr_ids(target, ids);
                if let Some(delay) = delay {
                    collect_expr_ids(delay, ids);
                }
            }
            Expr::Index {
                collection, index, ..
            } => {
                collect_expr_ids(collection, ids);
                if let Some(index) = index {
                    collect_expr_ids(index, ids);
                }
            }
            Expr::Field { base, .. } => collect_expr_ids(base, ids),
            Expr::Binding { pattern, value, .. } => {
                if let BindingPattern::Scatter(bindings) = pattern {
                    for binding in bindings {
                        ids.push(binding.id);
                        if let Some(default) = &binding.default {
                            collect_expr_ids(default, ids);
                        }
                    }
                }
                if let Some(value) = value {
                    collect_expr_ids(value, ids);
                }
            }
            Expr::If {
                condition,
                then_items,
                elseif,
                else_items,
                ..
            } => {
                collect_expr_ids(condition, ids);
                for item in then_items {
                    collect_item_ids(item, ids);
                }
                for (condition, items) in elseif {
                    collect_expr_ids(condition, ids);
                    for item in items {
                        collect_item_ids(item, ids);
                    }
                }
                for item in else_items {
                    collect_item_ids(item, ids);
                }
            }
            Expr::Block { items, .. } => {
                for item in items {
                    collect_item_ids(item, ids);
                }
            }
            Expr::For {
                key,
                value,
                iter,
                body,
                ..
            } => {
                ids.push(key.id);
                if let Some(value) = value {
                    ids.push(value.id);
                }
                collect_expr_ids(iter, ids);
                for item in body {
                    collect_item_ids(item, ids);
                }
            }
            Expr::While {
                condition, body, ..
            } => {
                collect_expr_ids(condition, ids);
                for item in body {
                    collect_item_ids(item, ids);
                }
            }
            Expr::Return { value, .. } => {
                if let Some(value) = value {
                    collect_expr_ids(value, ids);
                }
            }
            Expr::Raise {
                error,
                message,
                value,
                ..
            } => {
                collect_expr_ids(error, ids);
                if let Some(message) = message {
                    collect_expr_ids(message, ids);
                }
                if let Some(value) = value {
                    collect_expr_ids(value, ids);
                }
            }
            Expr::Recover { expr, catches, .. } => {
                collect_expr_ids(expr, ids);
                for catch in catches {
                    ids.push(catch.id);
                    if let Some(condition) = &catch.condition {
                        collect_expr_ids(condition, ids);
                    }
                    collect_expr_ids(&catch.value, ids);
                }
            }
            Expr::One { expr, .. } => collect_expr_ids(expr, ids),
            Expr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                for item in body {
                    collect_item_ids(item, ids);
                }
                for catch in catches {
                    ids.push(catch.id);
                    if let Some(condition) = &catch.condition {
                        collect_expr_ids(condition, ids);
                    }
                    for item in &catch.body {
                        collect_item_ids(item, ids);
                    }
                }
                for item in finally {
                    collect_item_ids(item, ids);
                }
            }
            Expr::Function { params, body, .. } => {
                collect_param_ids(params, ids);
                match body {
                    FunctionBody::Expr(expr) => collect_expr_ids(expr, ids),
                    FunctionBody::Block(items) => {
                        for item in items {
                            collect_item_ids(item, ids);
                        }
                    }
                }
            }
            Expr::Effect { expr, .. } => collect_expr_ids(expr, ids),
            Expr::Frob { value, .. } => collect_expr_ids(value, ids),
            Expr::Literal { .. }
            | Expr::Name { .. }
            | Expr::QueryVar { .. }
            | Expr::Identity { .. }
            | Expr::Symbol { .. }
            | Expr::Hole { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. }
            | Expr::Error { .. } => {}
        }
    }

    fn collect_arg_ids(args: &[crate::Arg], ids: &mut Vec<NodeId>) {
        for arg in args {
            ids.push(arg.id);
            collect_expr_ids(&arg.value, ids);
        }
    }

    fn collect_param_ids(params: &[Param], ids: &mut Vec<NodeId>) {
        for param in params {
            ids.push(param.id);
            if let Some(default) = &param.default {
                collect_expr_ids(default, ids);
            }
        }
    }
}
