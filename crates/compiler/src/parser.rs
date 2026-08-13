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

use crate::{CstElement, CstNode, CstToken, Parse, ParseError, SyntaxKind, Token, lex};

pub fn parse(source: &str) -> Parse {
    let tokens = lex(source);
    Parser::new(source, &tokens).parse()
}

struct Parser<'a> {
    source: &'a str,
    tokens: &'a [Token],
    pos: usize,
    errors: Vec<ParseError>,
    query_vars_allowed: bool,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: &'a [Token]) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
            errors: Vec::new(),
            query_vars_allowed: false,
        }
    }

    fn parse(mut self) -> Parse {
        let mut children = Vec::new();
        children.push(CstElement::Node(self.parse_item_list(&[SyntaxKind::Eof])));
        self.consume_separators();
        children.push(self.expect_token(SyntaxKind::Eof, "expected end of file"));
        Parse {
            root: CstNode::new(SyntaxKind::Program, children),
            errors: self.errors,
        }
    }

    fn parse_item_list(&mut self, stops: &[SyntaxKind]) -> CstNode {
        let mut children = Vec::new();
        self.consume_separators();
        while !stops.contains(&self.current_kind()) {
            if self.current_kind() == SyntaxKind::Eof {
                break;
            }
            children.push(CstElement::Node(self.parse_item()));
            self.consume_separators();
        }
        CstNode::new(SyntaxKind::ItemList, children)
    }

    fn parse_item(&mut self) -> CstNode {
        match self.current_kind() {
            SyntaxKind::MethodKw => self.parse_method_like(SyntaxKind::MethodItem),
            SyntaxKind::VerbKw => self.parse_verb_item(),
            _ => self.parse_expr_stmt(),
        }
    }

    fn parse_method_like(&mut self, kind: SyntaxKind) -> CstNode {
        let mut children = Vec::new();
        children.push(self.bump_element());
        children.push(CstElement::Node(self.parse_method_header()));
        self.consume_separators();

        while !matches!(
            self.current_kind(),
            SyntaxKind::DoKw | SyntaxKind::EndKw | SyntaxKind::Eof
        ) {
            children.push(CstElement::Node(self.parse_method_clause()));
            self.consume_separators();
        }

        if self.current_kind() == SyntaxKind::DoKw {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])));
        }
        children.push(self.expect_token(SyntaxKind::EndKw, "expected end after method body"));
        CstNode::new(kind, children)
    }

    fn parse_verb_item(&mut self) -> CstNode {
        let children = vec![
            self.bump_element(),
            CstElement::Node(self.parse_verb_header()),
            CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])),
            self.expect_token(SyntaxKind::EndKw, "expected end after verb body"),
        ];
        CstNode::new(SyntaxKind::VerbItem, children)
    }

    fn parse_verb_header(&mut self) -> CstNode {
        let mut children = self.parse_qualified_ident_or_missing("expected selector after verb");
        children.push(CstElement::Node(self.parse_verb_param_list()));
        if self.current_kind() == SyntaxKind::Arrow {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_type_ref()));
        }
        CstNode::new(SyntaxKind::VerbHeader, children)
    }

    fn parse_verb_param_list(&mut self) -> CstNode {
        let mut children = vec![self.expect_token(SyntaxKind::LParen, "expected '('")];
        while !matches!(self.current_kind(), SyntaxKind::RParen | SyntaxKind::Eof) {
            children.push(CstElement::Node(self.parse_verb_param()));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        children.push(self.expect_token(SyntaxKind::RParen, "expected ')'"));
        CstNode::new(SyntaxKind::VerbParamList, children)
    }

    fn parse_verb_param(&mut self) -> CstNode {
        let mut children = vec![self.expect_token(SyntaxKind::Ident, "expected verb parameter")];
        if self.current_kind() == SyntaxKind::At {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_dispatch_restriction()));
        }
        if self.current_kind() == SyntaxKind::Colon {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_type_ref()));
        }
        CstNode::new(SyntaxKind::VerbParam, children)
    }

    fn parse_dispatch_restriction(&mut self) -> CstNode {
        let mut children =
            vec![self.expect_token(SyntaxKind::Hash, "expected prototype identity after '@'")];
        children
            .extend(self.parse_qualified_ident_or_missing("expected prototype identity after '#'"));
        if self.current_kind() == SyntaxKind::Lt {
            children.push(self.bump_element());
            children.push(self.expect_token(
                SyntaxKind::Underscore,
                "expected '_' in frob dispatch restriction",
            ));
            children.push(self.expect_token(
                SyntaxKind::Gt,
                "expected '>' after frob dispatch restriction",
            ));
        }
        CstNode::new(SyntaxKind::DispatchRestriction, children)
    }

    fn parse_method_header(&mut self) -> CstNode {
        let mut children = Vec::new();
        while !matches!(
            self.current_kind(),
            SyntaxKind::Newline
                | SyntaxKind::Semi
                | SyntaxKind::DoKw
                | SyntaxKind::EndKw
                | SyntaxKind::Eof
        ) {
            children.push(self.bump_element());
        }
        CstNode::new(SyntaxKind::MethodHeader, children)
    }

    fn parse_method_clause(&mut self) -> CstNode {
        let mut children = Vec::new();
        while !matches!(
            self.current_kind(),
            SyntaxKind::Newline
                | SyntaxKind::Semi
                | SyntaxKind::DoKw
                | SyntaxKind::EndKw
                | SyntaxKind::Eof
        ) {
            children.push(self.bump_element());
        }
        CstNode::new(SyntaxKind::MethodClause, children)
    }

    fn parse_expr_stmt(&mut self) -> CstNode {
        let mut children = Vec::new();
        let current = self.current_kind();
        if Self::starts_expr(current) {
            let expr = self.parse_expr(0);
            if self.current_kind() == SyntaxKind::ColonDash {
                children.push(CstElement::Node(self.parse_relation_rule(expr)));
            } else {
                children.push(CstElement::Node(expr));
            }
        } else {
            self.error("expected expression");
            children.push(self.bump_element());
        }
        CstNode::new(SyntaxKind::ExprStmt, children)
    }

    fn parse_relation_rule(&mut self, head: CstNode) -> CstNode {
        let mut children = vec![CstElement::Node(head), self.bump_element()];
        loop {
            self.consume_separators();
            let current = self.current_kind();
            if !Self::starts_expr(current) {
                self.error("expected relation atom in rule body");
                break;
            }
            children.push(CstElement::Node(self.parse_expr(0)));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        CstNode::new(SyntaxKind::RelationRule, children)
    }

    fn parse_block(&mut self, stops: &[SyntaxKind]) -> CstNode {
        let mut children = Vec::new();
        self.consume_separators();
        while !stops.contains(&self.current_kind()) && self.current_kind() != SyntaxKind::Eof {
            children.push(CstElement::Node(self.parse_item()));
            self.consume_separators();
        }
        CstNode::new(SyntaxKind::Block, children)
    }

    fn parse_expr(&mut self, min_bp: u8) -> CstNode {
        let mut lhs = self.parse_prefix();
        loop {
            lhs = match self.current_kind() {
                SyntaxKind::LParen => {
                    if 13 < min_bp {
                        break;
                    }
                    self.parse_call(lhs)
                }
                SyntaxKind::LBracket => {
                    if 13 < min_bp {
                        break;
                    }
                    self.parse_index(lhs)
                }
                SyntaxKind::Dot => {
                    if 13 < min_bp {
                        break;
                    }
                    self.parse_field(lhs)
                }
                SyntaxKind::Colon => {
                    if 13 < min_bp {
                        break;
                    }
                    self.parse_receiver_call(lhs)
                }
                op => {
                    let Some((left_bp, right_bp, kind)) = infix_binding_power(op) else {
                        break;
                    };
                    if left_bp < min_bp {
                        break;
                    }
                    let mut children = vec![CstElement::Node(lhs), self.bump_element()];
                    children.push(CstElement::Node(self.parse_expr(right_bp)));
                    CstNode::new(kind, children)
                }
            };
        }
        lhs
    }

    fn parse_prefix(&mut self) -> CstNode {
        match self.current_kind() {
            SyntaxKind::LetKw => self.parse_binding_expr(SyntaxKind::LetExpr),
            SyntaxKind::ConstKw => self.parse_binding_expr(SyntaxKind::ConstExpr),
            SyntaxKind::IfKw => self.parse_if_expr(),
            SyntaxKind::BeginKw => self.parse_begin_expr(),
            SyntaxKind::ForKw => self.parse_for_expr(),
            SyntaxKind::WhileKw => self.parse_while_expr(),
            SyntaxKind::ReturnKw => self.parse_return_expr(),
            SyntaxKind::RaiseKw => self.parse_raise_expr(),
            SyntaxKind::RecoverKw => self.parse_recover_expr(),
            SyntaxKind::OneKw => self.parse_one_expr(),
            SyntaxKind::SpawnKw => self.parse_spawn_expr(),
            SyntaxKind::BreakKw => self.parse_simple_control_expr(SyntaxKind::BreakExpr),
            SyntaxKind::ContinueKw => self.parse_simple_control_expr(SyntaxKind::ContinueExpr),
            SyntaxKind::TryKw => self.parse_try_expr(),
            SyntaxKind::FnKw => self.parse_fn_expr(),
            SyntaxKind::AssertKw => self.parse_effect_expr(SyntaxKind::AssertExpr),
            SyntaxKind::RetractKw => self.parse_effect_expr(SyntaxKind::RetractExpr),
            SyntaxKind::RequireKw => self.parse_effect_expr(SyntaxKind::RequireExpr),
            SyntaxKind::Bang | SyntaxKind::NotKw | SyntaxKind::Minus => self.parse_unary_expr(),
            SyntaxKind::LParen => self.parse_group_expr(),
            SyntaxKind::LBracket if self.looks_like_relation_expr() => self.parse_relation_expr(),
            SyntaxKind::LBracket => self.parse_list_expr(),
            SyntaxKind::LBrace if self.looks_like_brace_lambda() => self.parse_brace_lambda_expr(),
            SyntaxKind::LBrace => self.parse_map_expr(),
            SyntaxKind::Hash if matches!(self.nth_kind(1), SyntaxKind::Ident | SyntaxKind::Int) => {
                self.parse_identity_expr()
            }
            SyntaxKind::Colon => self.parse_symbol_or_role_call(),
            SyntaxKind::Ident if self.looks_like_dom_expr() => self.parse_dom_expr(),
            SyntaxKind::Question if self.nth_kind(1) == SyntaxKind::Ident => {
                if !self.query_vars_allowed {
                    self.error("query variables are only valid as relation arguments");
                }
                self.parse_query_var_expr()
            }
            SyntaxKind::Underscore => self.single_token_node(SyntaxKind::HoleExpr),
            SyntaxKind::Ident => self.parse_name_expr(),
            SyntaxKind::ErrorCode
            | SyntaxKind::Int
            | SyntaxKind::Float
            | SyntaxKind::String
            | SyntaxKind::Bytes
            | SyntaxKind::TrueKw
            | SyntaxKind::FalseKw
            | SyntaxKind::NothingKw => self.single_token_node(SyntaxKind::LiteralExpr),
            _ => {
                self.error("expected expression");
                self.single_token_node(SyntaxKind::AtomExpr)
            }
        }
    }

    fn parse_binding_expr(&mut self, kind: SyntaxKind) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::LBracket {
            children.push(CstElement::Node(self.parse_pattern_brackets()));
        } else {
            children.push(
                self.expect_token(SyntaxKind::Ident, "expected binding name or list pattern"),
            );
            if self.current_kind() == SyntaxKind::Colon {
                children.push(self.bump_element());
                children.push(CstElement::Node(self.parse_type_ref()));
            }
        }
        if self.current_kind() == SyntaxKind::Eq {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        CstNode::new(kind, children)
    }

    fn parse_spawn_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        let target = self.parse_expr(0);
        if !matches!(
            target.kind,
            SyntaxKind::RoleCallExpr | SyntaxKind::ReceiverCallExpr
        ) {
            self.error("spawn expects a role or receiver dispatch target");
        }
        children.push(CstElement::Node(target));
        if self.current_kind() == SyntaxKind::AfterKw {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        CstNode::new(SyntaxKind::SpawnExpr, children)
    }

    fn parse_if_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(CstElement::Node(self.parse_expr(0)));
        children.push(CstElement::Node(self.parse_block(&[
            SyntaxKind::ElseIfKw,
            SyntaxKind::ElseKw,
            SyntaxKind::EndKw,
        ])));
        while self.current_kind() == SyntaxKind::ElseIfKw {
            let mut clause = vec![self.bump_element()];
            clause.push(CstElement::Node(self.parse_expr(0)));
            clause.push(CstElement::Node(self.parse_block(&[
                SyntaxKind::ElseIfKw,
                SyntaxKind::ElseKw,
                SyntaxKind::EndKw,
            ])));
            children.push(CstElement::Node(CstNode::new(
                SyntaxKind::ElseIfClause,
                clause,
            )));
        }
        if self.current_kind() == SyntaxKind::ElseKw {
            children.push(CstElement::Node(self.parse_else_clause()));
        }
        children.push(self.expect_token(SyntaxKind::EndKw, "expected end after if"));
        CstNode::new(SyntaxKind::IfExpr, children)
    }

    fn parse_else_clause(&mut self) -> CstNode {
        let children = vec![
            self.bump_element(),
            CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])),
        ];
        CstNode::new(SyntaxKind::ElseClause, children)
    }

    fn parse_begin_expr(&mut self) -> CstNode {
        let children = vec![
            self.bump_element(),
            CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])),
            self.expect_token(SyntaxKind::EndKw, "expected end after begin"),
        ];
        CstNode::new(SyntaxKind::BeginExpr, children)
    }

    fn parse_for_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(CstElement::Node(self.parse_loop_binding()));
        if self.current_kind() == SyntaxKind::Comma {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_loop_binding()));
        }
        children.push(self.expect_token(SyntaxKind::InKw, "expected in in for loop"));
        children.push(CstElement::Node(self.parse_expr(0)));
        children.push(CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])));
        children.push(self.expect_token(SyntaxKind::EndKw, "expected end after for"));
        CstNode::new(SyntaxKind::ForExpr, children)
    }

    fn parse_loop_binding(&mut self) -> CstNode {
        let mut children = vec![self.expect_token(SyntaxKind::Ident, "expected loop binding")];
        if self.current_kind() == SyntaxKind::Colon {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_type_ref()));
        }
        CstNode::new(SyntaxKind::LoopBinding, children)
    }

    fn parse_while_expr(&mut self) -> CstNode {
        let children = vec![
            self.bump_element(),
            CstElement::Node(self.parse_expr(0)),
            CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])),
            self.expect_token(SyntaxKind::EndKw, "expected end after while"),
        ];
        CstNode::new(SyntaxKind::WhileExpr, children)
    }

    fn parse_return_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        let current = self.current_kind();
        if Self::starts_expr(current) && !self.at_separator_or_stop() {
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        CstNode::new(SyntaxKind::ReturnExpr, children)
    }

    fn parse_raise_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if Self::starts_expr(self.current_kind()) && !self.at_separator_or_stop() {
            children.push(CstElement::Node(self.parse_expr(0)));
            for _ in 0..2 {
                if self.current_kind() != SyntaxKind::Comma {
                    break;
                }
                children.push(self.bump_element());
                children.push(CstElement::Node(self.parse_expr(0)));
            }
        }
        CstNode::new(SyntaxKind::RaiseExpr, children)
    }

    fn parse_recover_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(CstElement::Node(self.parse_expr(0)));
        self.consume_separators();
        while self.current_kind() == SyntaxKind::CatchKw {
            children.push(CstElement::Node(self.parse_recover_clause()));
            self.consume_separators();
        }
        children.push(self.expect_token(SyntaxKind::EndKw, "expected end after recover"));
        CstNode::new(SyntaxKind::RecoverExpr, children)
    }

    fn parse_one_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(CstElement::Node(self.parse_expr(13)));
        CstNode::new(SyntaxKind::OneExpr, children)
    }

    fn parse_recover_clause(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::Ident && self.nth_kind(1) != SyntaxKind::AsKw {
            children.push(self.bump_element());
            if self.current_kind() == SyntaxKind::IfKw {
                children.push(self.bump_element());
                children.push(CstElement::Node(self.parse_expr(0)));
            }
        } else if Self::starts_expr(self.current_kind()) {
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        if self.current_kind() == SyntaxKind::AsKw {
            children.push(self.bump_element());
            children.push(self.expect_token(SyntaxKind::Ident, "expected recovery binding"));
        }
        children.push(self.expect_token(SyntaxKind::FatArrow, "expected => in recovery clause"));
        children.push(CstElement::Node(self.parse_expr(0)));
        CstNode::new(SyntaxKind::RecoverClause, children)
    }

    fn parse_simple_control_expr(&mut self, kind: SyntaxKind) -> CstNode {
        CstNode::new(kind, vec![self.bump_element()])
    }

    fn parse_try_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(CstElement::Node(self.parse_block(&[
            SyntaxKind::CatchKw,
            SyntaxKind::FinallyKw,
            SyntaxKind::EndKw,
        ])));
        while self.current_kind() == SyntaxKind::CatchKw {
            children.push(CstElement::Node(self.parse_catch_clause()));
        }
        if self.current_kind() == SyntaxKind::FinallyKw {
            children.push(CstElement::Node(self.parse_finally_clause()));
        }
        children.push(self.expect_token(SyntaxKind::EndKw, "expected end after try"));
        CstNode::new(SyntaxKind::TryExpr, children)
    }

    fn parse_catch_clause(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::Ident && self.nth_kind(1) != SyntaxKind::AsKw {
            children.push(self.bump_element());
            if self.current_kind() == SyntaxKind::IfKw {
                children.push(self.bump_element());
                children.push(CstElement::Node(self.parse_expr(0)));
            }
        } else if Self::starts_expr(self.current_kind()) {
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        if self.current_kind() == SyntaxKind::AsKw {
            children.push(self.bump_element());
            children.push(self.expect_token(SyntaxKind::Ident, "expected catch binding"));
        }
        children.push(CstElement::Node(self.parse_block(&[
            SyntaxKind::CatchKw,
            SyntaxKind::FinallyKw,
            SyntaxKind::EndKw,
        ])));
        CstNode::new(SyntaxKind::CatchClause, children)
    }

    fn parse_finally_clause(&mut self) -> CstNode {
        let children = vec![
            self.bump_element(),
            CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])),
        ];
        CstNode::new(SyntaxKind::FinallyClause, children)
    }

    fn parse_fn_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::Ident && self.nth_kind(1) == SyntaxKind::LParen {
            children.push(self.bump_element());
        }
        children.push(CstElement::Node(self.parse_param_list()));
        if self.current_kind() == SyntaxKind::Arrow {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_type_ref()));
        }
        if self.current_kind() == SyntaxKind::FatArrow {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_expr(0)));
        } else {
            children.push(CstElement::Node(self.parse_block(&[SyntaxKind::EndKw])));
            children.push(self.expect_token(SyntaxKind::EndKw, "expected end after fn"));
        }
        CstNode::new(SyntaxKind::FnExpr, children)
    }

    fn parse_brace_lambda_expr(&mut self) -> CstNode {
        let children = vec![
            CstElement::Node(self.parse_brace_param_list()),
            self.expect_token(
                SyntaxKind::FatArrow,
                "expected '=>' after lambda parameters",
            ),
            CstElement::Node(self.parse_expr(0)),
        ];
        CstNode::new(SyntaxKind::LambdaExpr, children)
    }

    fn parse_effect_expr(&mut self, kind: SyntaxKind) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(CstElement::Node(self.parse_expr(0)));
        CstNode::new(kind, children)
    }

    fn parse_unary_expr(&mut self) -> CstNode {
        let children = vec![self.bump_element(), CstElement::Node(self.parse_expr(12))];
        CstNode::new(SyntaxKind::UnaryExpr, children)
    }

    fn parse_group_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::RParen {
            self.error("empty parentheses are not yet a value");
        } else {
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        children.push(self.expect_token(SyntaxKind::RParen, "expected ')'"));
        CstNode::new(SyntaxKind::GroupExpr, children)
    }

    fn parse_list_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        self.parse_delimited_items(SyntaxKind::RBracket, SyntaxKind::ListItem, &mut children);
        children.push(self.expect_token(SyntaxKind::RBracket, "expected ']'"));
        CstNode::new(SyntaxKind::ListExpr, children)
    }

    fn parse_relation_expr(&mut self) -> CstNode {
        let mut children = vec![CstElement::Node(self.parse_relation_heading())];
        self.consume_separators();
        children.push(self.expect_token(SyntaxKind::LBrace, "expected '{' after relation heading"));
        self.consume_separators();
        while !matches!(self.current_kind(), SyntaxKind::RBrace | SyntaxKind::Eof) {
            children.push(CstElement::Node(self.parse_relation_row()));
            self.consume_separators();
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
            self.consume_separators();
        }
        children.push(self.expect_token(SyntaxKind::RBrace, "expected '}' after relation rows"));
        CstNode::new(SyntaxKind::RelationExpr, children)
    }

    fn parse_relation_heading(&mut self) -> CstNode {
        let mut children = vec![self.expect_token(SyntaxKind::LBracket, "expected '['")];
        self.consume_separators();
        while !matches!(self.current_kind(), SyntaxKind::RBracket | SyntaxKind::Eof) {
            children.push(CstElement::Node(self.parse_symbol_or_role_call()));
            self.consume_separators();
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
            self.consume_separators();
        }
        children
            .push(self.expect_token(SyntaxKind::RBracket, "expected ']' after relation heading"));
        CstNode::new(SyntaxKind::RelationHeading, children)
    }

    fn parse_relation_row(&mut self) -> CstNode {
        let mut children =
            vec![self.expect_token(SyntaxKind::LBracket, "expected '[' to start relation row")];
        self.parse_delimited_items(SyntaxKind::RBracket, SyntaxKind::ListItem, &mut children);
        children.push(self.expect_token(SyntaxKind::RBracket, "expected ']' after relation row"));
        CstNode::new(SyntaxKind::RelationRow, children)
    }

    fn parse_map_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        while !matches!(self.current_kind(), SyntaxKind::RBrace | SyntaxKind::Eof) {
            let entry = vec![
                CstElement::Node(self.parse_expr(0)),
                self.expect_token(SyntaxKind::Arrow, "expected '->' in map entry"),
                CstElement::Node(self.parse_expr(0)),
            ];
            children.push(CstElement::Node(CstNode::new(SyntaxKind::MapEntry, entry)));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        children.push(self.expect_token(SyntaxKind::RBrace, "expected '}'"));
        CstNode::new(SyntaxKind::MapExpr, children)
    }

    fn parse_brace_param_list(&mut self) -> CstNode {
        let mut children = vec![self.expect_token(SyntaxKind::LBrace, "expected parameter list")];
        while !matches!(self.current_kind(), SyntaxKind::RBrace | SyntaxKind::Eof) {
            let mut param = Vec::new();
            if matches!(self.current_kind(), SyntaxKind::Question | SyntaxKind::At) {
                param.push(self.bump_element());
            }
            param.push(self.expect_token(SyntaxKind::Ident, "expected parameter name"));
            if self.current_kind() == SyntaxKind::Colon {
                self.error(
                    "type annotations are not supported in brace lambdas; use `fn(...)` for annotated parameters",
                );
                param.push(self.bump_element());
                param.push(self.expect_token(SyntaxKind::Ident, "expected type"));
            }
            if self.current_kind() == SyntaxKind::Eq {
                param.push(self.bump_element());
                param.push(CstElement::Node(self.parse_expr(0)));
            }
            children.push(CstElement::Node(CstNode::new(SyntaxKind::Param, param)));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        children.push(self.expect_token(SyntaxKind::RBrace, "expected end of parameter list"));
        CstNode::new(SyntaxKind::ParamList, children)
    }

    fn parse_pattern_brackets(&mut self) -> CstNode {
        let mut children =
            vec![self.expect_token(SyntaxKind::LBracket, "expected scatter pattern")];
        while !matches!(self.current_kind(), SyntaxKind::RBracket | SyntaxKind::Eof) {
            let mut binding = Vec::new();
            if matches!(self.current_kind(), SyntaxKind::Question | SyntaxKind::At) {
                binding.push(self.bump_element());
            }
            binding.push(self.expect_token(SyntaxKind::Ident, "expected scatter binding name"));
            if self.current_kind() == SyntaxKind::Colon {
                binding.push(self.bump_element());
                binding.push(CstElement::Node(self.parse_type_ref()));
            }
            if self.current_kind() == SyntaxKind::Eq {
                binding.push(self.bump_element());
                binding.push(CstElement::Node(self.parse_expr(0)));
            }
            children.push(CstElement::Node(CstNode::new(
                SyntaxKind::ScatterBinding,
                binding,
            )));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        children.push(self.expect_token(SyntaxKind::RBracket, "expected end of scatter pattern"));
        CstNode::new(SyntaxKind::ScatterPattern, children)
    }

    fn parse_identity_expr(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::Ident {
            children.extend(self.parse_qualified_ident_tokens());
        } else if self.current_kind() == SyntaxKind::Int {
            children.push(self.bump_element());
        } else {
            children.push(self.missing("expected identity name after '#'"));
        }
        if self.current_kind() == SyntaxKind::Lt {
            children.push(self.bump_element());
            children.push(CstElement::Node(self.parse_expr(10)));
            children.push(self.expect_token(SyntaxKind::Gt, "expected '>' after frob payload"));
            CstNode::new(SyntaxKind::FrobExpr, children)
        } else {
            CstNode::new(SyntaxKind::IdentityExpr, children)
        }
    }

    fn parse_dom_expr(&mut self) -> CstNode {
        let children = vec![
            self.bump_element(),
            CstElement::Node(self.parse_dom_element()),
        ];
        CstNode::new(SyntaxKind::DomExpr, children)
    }

    fn parse_dom_element(&mut self) -> CstNode {
        let mut children = Vec::new();
        children.push(self.dom_expect_token(SyntaxKind::Lt, "expected '<' to start DOM element"));
        let tag = self.dom_expect_token(SyntaxKind::Ident, "expected DOM tag name");
        let tag_name = self.token_text(&tag).unwrap_or_default();
        children.push(tag);

        while !matches!(
            self.dom_current_kind(),
            SyntaxKind::Gt | SyntaxKind::Slash | SyntaxKind::Eof
        ) {
            children.push(CstElement::Node(self.parse_dom_attr()));
        }

        if self.dom_current_kind() == SyntaxKind::Slash {
            children.push(self.dom_bump_element());
            children.push(self.dom_expect_token(
                SyntaxKind::Gt,
                "expected '>' after self-closing DOM element",
            ));
            return CstNode::new(SyntaxKind::DomElement, children);
        }

        children.push(
            self.dom_expect_token(SyntaxKind::Gt, "expected '>' after DOM element start tag"),
        );

        while !matches!(self.raw_current_kind(), SyntaxKind::Eof) {
            if self.raw_current_kind() == SyntaxKind::Lt
                && self.raw_nth_non_ws_kind(1) == SyntaxKind::Slash
            {
                break;
            }
            if self.raw_current_kind() == SyntaxKind::Lt {
                children.push(CstElement::Node(self.parse_dom_element()));
            } else if self.raw_current_kind() == SyntaxKind::LBrace {
                children.push(CstElement::Node(self.parse_dom_child_expr()));
            } else {
                children.push(CstElement::Node(self.parse_dom_text()));
            }
        }

        children.push(self.dom_expect_token(SyntaxKind::Lt, "expected DOM closing tag"));
        children.push(self.dom_expect_token(SyntaxKind::Slash, "expected '/' in DOM closing tag"));
        let close_tag = self.dom_expect_token(SyntaxKind::Ident, "expected DOM closing tag name");
        if let Some(close_tag_name) = self.token_text(&close_tag)
            && close_tag_name != tag_name
        {
            self.errors.push(ParseError::new(
                format!("expected closing tag </{tag_name}>"),
                element_span(&close_tag),
            ));
        }
        children.push(close_tag);
        children.push(self.dom_expect_token(SyntaxKind::Gt, "expected '>' after DOM closing tag"));

        CstNode::new(SyntaxKind::DomElement, children)
    }

    fn parse_dom_attr(&mut self) -> CstNode {
        self.skip_dom_trivia();
        let mut children = Vec::new();
        if self.raw_current_kind().is_dom_name_atom() {
            children.push(self.raw_bump_element());
        } else {
            children.push(self.missing("expected DOM attribute name"));
            if !matches!(
                self.raw_current_kind(),
                SyntaxKind::Eq | SyntaxKind::Gt | SyntaxKind::Slash | SyntaxKind::Eof
            ) {
                children.push(self.raw_bump_element());
            }
        }
        while matches!(
            self.raw_current_kind(),
            SyntaxKind::Minus | SyntaxKind::Colon
        ) && self.raw_nth_non_ws_kind(1).is_dom_name_atom()
        {
            children.push(self.raw_bump_element());
            children.push(self.raw_bump_element());
        }
        if matches!(
            self.raw_current_kind(),
            SyntaxKind::Minus | SyntaxKind::Colon
        ) {
            children.push(self.missing("expected DOM attribute name part"));
            children.push(self.raw_bump_element());
        }

        self.skip_dom_trivia();
        if self.raw_current_kind() == SyntaxKind::Eq {
            children.push(self.raw_bump_element());
            self.skip_dom_trivia();
            if self.raw_current_kind() == SyntaxKind::String {
                children.push(self.raw_bump_element());
            } else if self.raw_current_kind() == SyntaxKind::LBrace {
                children.push(self.raw_bump_element());
                children.push(CstElement::Node(self.parse_expr(0)));
                children.push(self.expect_token(
                    SyntaxKind::RBrace,
                    "expected '}' after DOM attribute expression",
                ));
            } else {
                children
                    .push(self.missing("expected quoted string or '{...}' DOM attribute value"));
            }
        }
        CstNode::new(SyntaxKind::DomAttr, children)
    }

    fn parse_dom_child_expr(&mut self) -> CstNode {
        let mut children = vec![self.raw_bump_element()];
        if self.current_kind() == SyntaxKind::At {
            children.push(self.bump_element());
        }
        children.push(CstElement::Node(self.parse_expr(0)));
        children.push(self.expect_token(
            SyntaxKind::RBrace,
            "expected '}' after DOM child expression",
        ));
        CstNode::new(SyntaxKind::DomChildExpr, children)
    }

    fn parse_dom_text(&mut self) -> CstNode {
        let start = self.raw_current_span().start;
        let mut end = start;
        while !matches!(
            self.raw_current_kind(),
            SyntaxKind::Lt | SyntaxKind::LBrace | SyntaxKind::Eof
        ) {
            end = self.raw_current_span().end;
            self.pos += 1;
        }
        CstNode::new(
            SyntaxKind::DomText,
            vec![CstElement::Token(CstToken {
                kind: SyntaxKind::DomText,
                span: start..end,
            })],
        )
    }

    fn parse_symbol_or_role_call(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        if self.current_kind() == SyntaxKind::LParen {
            children.push(CstElement::Node(self.parse_group_expr()));
        } else {
            children
                .extend(self.parse_qualified_ident_or_missing("expected symbol name after ':'"));
        }
        if self.current_kind() == SyntaxKind::LParen {
            children.push(CstElement::Node(self.parse_arg_list(true)));
            CstNode::new(SyntaxKind::RoleCallExpr, children)
        } else {
            CstNode::new(SyntaxKind::SymbolExpr, children)
        }
    }

    fn parse_call(&mut self, callee: CstNode) -> CstNode {
        let children = vec![
            CstElement::Node(callee),
            CstElement::Node(self.parse_arg_list(false)),
        ];
        CstNode::new(SyntaxKind::CallExpr, children)
    }

    fn parse_receiver_call(&mut self, receiver: CstNode) -> CstNode {
        let mut children = vec![CstElement::Node(receiver), self.bump_element()];
        if self.current_kind() == SyntaxKind::LParen {
            children.push(CstElement::Node(self.parse_group_expr()));
        } else {
            children.extend(self.parse_qualified_ident_or_missing("expected selector after ':'"));
        }
        children.push(CstElement::Node(self.parse_arg_list(true)));
        CstNode::new(SyntaxKind::ReceiverCallExpr, children)
    }

    fn parse_name_expr(&mut self) -> CstNode {
        if self.looks_like_qualified_callee() {
            CstNode::new(SyntaxKind::NameExpr, self.parse_qualified_ident_tokens())
        } else {
            self.single_token_node(SyntaxKind::NameExpr)
        }
    }

    fn parse_qualified_ident_or_missing(&mut self, message: &str) -> Vec<CstElement> {
        if self.current_kind() == SyntaxKind::Ident {
            self.parse_qualified_ident_tokens()
        } else {
            vec![self.missing(message)]
        }
    }

    fn parse_qualified_ident_tokens(&mut self) -> Vec<CstElement> {
        let mut children = vec![self.expect_token(SyntaxKind::Ident, "expected identifier")];
        let mut last_end = element_end(children.last().expect("identifier element exists"));
        while self.qualified_ident_continues(last_end) {
            let slash = self.bump_element();
            children.push(slash);

            let ident = self.expect_token(SyntaxKind::Ident, "expected identifier after '/'");
            last_end = element_end(&ident);
            children.push(ident);
        }
        children
    }

    fn parse_index(&mut self, collection: CstNode) -> CstNode {
        let mut children = vec![CstElement::Node(collection), self.bump_element()];
        if self.current_kind() != SyntaxKind::RBracket {
            children.push(CstElement::Node(self.parse_expr(0)));
        }
        children.push(self.expect_token(SyntaxKind::RBracket, "expected ']'"));
        CstNode::new(SyntaxKind::IndexExpr, children)
    }

    fn parse_field(&mut self, base: CstNode) -> CstNode {
        let mut children = vec![CstElement::Node(base), self.bump_element()];
        children.extend(self.parse_qualified_ident_or_missing("expected field name after '.'"));
        CstNode::new(SyntaxKind::FieldExpr, children)
    }

    fn parse_param_list(&mut self) -> CstNode {
        let mut children = Vec::new();
        children.push(self.expect_token(SyntaxKind::LParen, "expected '('"));
        while !matches!(self.current_kind(), SyntaxKind::RParen | SyntaxKind::Eof) {
            let mut param = Vec::new();
            if matches!(self.current_kind(), SyntaxKind::Question | SyntaxKind::At) {
                param.push(self.bump_element());
            }
            param.push(self.expect_token(SyntaxKind::Ident, "expected parameter name"));
            if self.current_kind() == SyntaxKind::Colon {
                param.push(self.bump_element());
                param.push(CstElement::Node(self.parse_type_ref()));
            }
            if self.current_kind() == SyntaxKind::Eq {
                param.push(self.bump_element());
                param.push(CstElement::Node(self.parse_expr(0)));
            }
            children.push(CstElement::Node(CstNode::new(SyntaxKind::Param, param)));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        children.push(self.expect_token(SyntaxKind::RParen, "expected ')'"));
        CstNode::new(SyntaxKind::ParamList, children)
    }

    fn parse_type_ref(&mut self) -> CstNode {
        self.skip_type_trivia();
        let mut children = Vec::new();
        match self.current_kind() {
            SyntaxKind::Ident
                if self.current_text_is("relation") && self.nth_kind(1) == SyntaxKind::Lt =>
            {
                children.push(CstElement::Node(self.parse_relation_type()));
            }
            SyntaxKind::Ident => {
                children.push(self.bump_element());
                if self.type_current_kind() == SyntaxKind::Lt {
                    children.push(CstElement::Node(self.parse_type_arguments()));
                }
            }
            SyntaxKind::Colon => {
                children.push(self.bump_element());
                children
                    .extend(self.parse_qualified_ident_or_missing("expected symbol type literal"));
            }
            SyntaxKind::TrueKw | SyntaxKind::FalseKw | SyntaxKind::ErrorCode => {
                children.push(self.bump_element());
            }
            SyntaxKind::LParen => {
                children.push(self.bump_element());
                children
                    .push(self.type_expect_token(SyntaxKind::RParen, "expected ')' in unit type"));
            }
            SyntaxKind::LBracket => {
                children.push(self.bump_element());
                children.push(self.type_expect_token(
                    SyntaxKind::RBracket,
                    "expected ']' in empty relation type literal",
                ));
                children.push(self.type_expect_token(
                    SyntaxKind::LBrace,
                    "expected '{' in empty relation type literal",
                ));
                children.push(self.type_expect_token(
                    SyntaxKind::RBrace,
                    "expected '}' in empty relation type literal",
                ));
            }
            _ => children.push(self.missing("expected type")),
        }
        CstNode::new(SyntaxKind::TypeRef, children)
    }

    fn parse_type_arguments(&mut self) -> CstNode {
        let mut children = vec![self.type_expect_token(SyntaxKind::Lt, "expected '<'")];
        loop {
            children.push(CstElement::Node(self.parse_type_ref()));
            if self.type_current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.type_bump_element());
        }
        children.push(self.type_expect_token(SyntaxKind::Gt, "expected '>' after type arguments"));
        CstNode::new(SyntaxKind::TypeArguments, children)
    }

    fn parse_relation_type(&mut self) -> CstNode {
        let mut children = vec![self.bump_element()];
        children.push(self.type_expect_token(SyntaxKind::Lt, "expected '<' after relation"));
        children.push(CstElement::Node(self.parse_type_row()));
        while self.type_current_kind() == SyntaxKind::Pipe {
            children.push(self.type_bump_element());
            children.push(CstElement::Node(self.parse_type_row()));
        }
        children.push(self.type_expect_token(SyntaxKind::Gt, "expected '>' after relation type"));
        if self.type_current_text_is("where") {
            children.push(self.type_bump_element());
            if self.type_current_text_is("rows") {
                children.push(self.type_bump_element());
            } else {
                children.push(self.missing("expected 'rows' after 'where'"));
            }
            if matches!(
                self.type_current_kind(),
                SyntaxKind::InKw | SyntaxKind::Membership
            ) {
                children.push(self.type_bump_element());
            } else {
                children.push(self.missing("expected 'in' or '∈' after 'where rows'"));
            }
            children.push(CstElement::Node(self.parse_cardinality()));
        }
        CstNode::new(SyntaxKind::RelationType, children)
    }

    fn parse_type_row(&mut self) -> CstNode {
        let mut children = vec![self.type_expect_token(SyntaxKind::LBrace, "expected '{'")];
        while !matches!(
            self.type_current_kind(),
            SyntaxKind::RBrace | SyntaxKind::Eof
        ) {
            let mut column = vec![
                self.type_expect_token(SyntaxKind::Colon, "expected relation type column symbol"),
            ];
            column.extend(
                self.parse_qualified_ident_or_missing("expected relation type column name"),
            );
            column.push(self.type_expect_token(
                SyntaxKind::Arrow,
                "expected '->' after relation type column",
            ));
            column.push(CstElement::Node(self.parse_type_ref()));
            children.push(CstElement::Node(CstNode::new(
                SyntaxKind::TypeColumn,
                column,
            )));
            if self.type_current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.type_bump_element());
        }
        children.push(self.type_expect_token(SyntaxKind::RBrace, "expected '}'"));
        CstNode::new(SyntaxKind::TypeRow, children)
    }

    fn parse_cardinality(&mut self) -> CstNode {
        let mut children =
            vec![self.type_expect_token(SyntaxKind::Int, "expected cardinality lower bound")];
        if self.type_current_kind() == SyntaxKind::DotDot {
            children.push(self.type_bump_element());
            if matches!(self.type_current_kind(), SyntaxKind::Int | SyntaxKind::Star) {
                children.push(self.type_bump_element());
            } else {
                children.push(self.missing("expected cardinality upper bound or '*'"));
            }
        }
        CstNode::new(SyntaxKind::Cardinality, children)
    }

    fn parse_arg_list(&mut self, allow_named_args: bool) -> CstNode {
        let mut children = Vec::new();
        children.push(self.expect_token(SyntaxKind::LParen, "expected '('"));
        while !matches!(self.current_kind(), SyntaxKind::RParen | SyntaxKind::Eof) {
            let mut arg = Vec::new();
            if self.current_kind() == SyntaxKind::Ident && self.nth_kind(1) == SyntaxKind::Colon {
                if !allow_named_args {
                    self.error("ordinary call arguments must be positional");
                }
                arg.push(self.bump_element());
                arg.push(self.bump_element());
            }
            if self.current_kind() == SyntaxKind::At {
                arg.push(self.bump_element());
            }
            let query_vars_allowed = self.query_vars_allowed;
            self.query_vars_allowed = true;
            arg.push(CstElement::Node(self.parse_expr(0)));
            self.query_vars_allowed = query_vars_allowed;
            children.push(CstElement::Node(CstNode::new(SyntaxKind::Arg, arg)));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
        children.push(self.expect_token(SyntaxKind::RParen, "expected ')'"));
        CstNode::new(SyntaxKind::ArgList, children)
    }

    fn parse_delimited_items(
        &mut self,
        stop: SyntaxKind,
        item_kind: SyntaxKind,
        children: &mut Vec<CstElement>,
    ) {
        while !matches!(self.current_kind(), SyntaxKind::Eof) && self.current_kind() != stop {
            let mut item = Vec::new();
            if self.current_kind() == SyntaxKind::At {
                item.push(self.bump_element());
            }
            item.push(CstElement::Node(self.parse_expr(0)));
            children.push(CstElement::Node(CstNode::new(item_kind, item)));
            if self.current_kind() != SyntaxKind::Comma {
                break;
            }
            children.push(self.bump_element());
        }
    }

    fn single_token_node(&mut self, kind: SyntaxKind) -> CstNode {
        CstNode::new(kind, vec![self.bump_element()])
    }

    fn parse_query_var_expr(&mut self) -> CstNode {
        CstNode::new(
            SyntaxKind::QueryVarExpr,
            vec![
                self.bump_element(),
                self.expect_token(SyntaxKind::Ident, "expected query variable name"),
            ],
        )
    }

    fn starts_expr(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::LetKw
                | SyntaxKind::ConstKw
                | SyntaxKind::IfKw
                | SyntaxKind::BeginKw
                | SyntaxKind::ForKw
                | SyntaxKind::WhileKw
                | SyntaxKind::ReturnKw
                | SyntaxKind::RaiseKw
                | SyntaxKind::RecoverKw
                | SyntaxKind::OneKw
                | SyntaxKind::SpawnKw
                | SyntaxKind::BreakKw
                | SyntaxKind::ContinueKw
                | SyntaxKind::TryKw
                | SyntaxKind::FnKw
                | SyntaxKind::AssertKw
                | SyntaxKind::RetractKw
                | SyntaxKind::RequireKw
                | SyntaxKind::Bang
                | SyntaxKind::NotKw
                | SyntaxKind::Minus
                | SyntaxKind::LParen
                | SyntaxKind::LBracket
                | SyntaxKind::LBrace
                | SyntaxKind::Hash
                | SyntaxKind::Colon
                | SyntaxKind::Question
                | SyntaxKind::Underscore
                | SyntaxKind::Ident
                | SyntaxKind::ErrorCode
                | SyntaxKind::Int
                | SyntaxKind::Float
                | SyntaxKind::String
                | SyntaxKind::Bytes
                | SyntaxKind::TrueKw
                | SyntaxKind::FalseKw
                | SyntaxKind::NothingKw
        )
    }

    fn looks_like_qualified_callee(&self) -> bool {
        let first = self.nth_non_ws_token(0);
        if first.kind != SyntaxKind::Ident {
            return false;
        }
        let mut idx = 1;
        let mut last_end = first.span.end;
        let mut saw_slash = false;
        loop {
            let slash = self.nth_non_ws_token(idx);
            let ident = self.nth_non_ws_token(idx + 1);
            if slash.kind != SyntaxKind::Slash
                || ident.kind != SyntaxKind::Ident
                || slash.span.start != last_end
                || slash.span.end != ident.span.start
            {
                break;
            }
            saw_slash = true;
            last_end = ident.span.end;
            idx += 2;
        }
        saw_slash && self.nth_non_ws_token(idx).kind == SyntaxKind::LParen
    }

    fn qualified_ident_continues(&self, last_end: usize) -> bool {
        let slash = self.nth_non_ws_token(0);
        let ident = self.nth_non_ws_token(1);
        slash.kind == SyntaxKind::Slash
            && ident.kind == SyntaxKind::Ident
            && slash.span.start == last_end
            && slash.span.end == ident.span.start
    }

    fn looks_like_brace_lambda(&self) -> bool {
        let mut idx = self.pos;
        let mut depth = 0usize;
        loop {
            let token = self
                .tokens
                .get(idx)
                .or_else(|| self.tokens.last())
                .expect("lexer always emits EOF");
            match token.kind {
                SyntaxKind::LBrace => depth += 1,
                SyntaxKind::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self.nth_non_ws_kind_from(idx + 1, 0) == SyntaxKind::FatArrow;
                    }
                }
                SyntaxKind::Eof => return false,
                _ => {}
            }
            idx += 1;
        }
    }

    fn looks_like_relation_expr(&self) -> bool {
        let mut index = self.pos;
        let next = |index: &mut usize| {
            while self
                .tokens
                .get(*index)
                .is_some_and(|token| token.kind.is_trivia())
            {
                *index += 1;
            }
            let token = self
                .tokens
                .get(*index)
                .or_else(|| self.tokens.last())
                .expect("lexer always emits EOF");
            *index += usize::from(token.kind != SyntaxKind::Eof);
            token.kind
        };

        if next(&mut index) != SyntaxKind::LBracket {
            return false;
        }
        let mut kind = next(&mut index);
        if kind == SyntaxKind::RBracket {
            return next(&mut index) == SyntaxKind::LBrace;
        }
        loop {
            if kind != SyntaxKind::Colon || next(&mut index) != SyntaxKind::Ident {
                return false;
            }
            kind = next(&mut index);
            while kind == SyntaxKind::Slash {
                if next(&mut index) != SyntaxKind::Ident {
                    return false;
                }
                kind = next(&mut index);
            }
            match kind {
                SyntaxKind::Comma => kind = next(&mut index),
                SyntaxKind::RBracket => break,
                _ => return false,
            }
        }
        next(&mut index) == SyntaxKind::LBrace
    }

    fn consume_separators(&mut self) {
        while matches!(self.current_kind(), SyntaxKind::Semi | SyntaxKind::Newline) {
            self.pos += 1;
            self.skip_ws_comments();
        }
    }

    fn at_separator_or_stop(&mut self) -> bool {
        matches!(
            self.current_kind(),
            SyntaxKind::Semi
                | SyntaxKind::Newline
                | SyntaxKind::ElseIfKw
                | SyntaxKind::ElseKw
                | SyntaxKind::CatchKw
                | SyntaxKind::FinallyKw
                | SyntaxKind::EndKw
                | SyntaxKind::Eof
        )
    }

    fn expect_token(&mut self, kind: SyntaxKind, message: &str) -> CstElement {
        if self.current_kind() == kind {
            self.bump_element()
        } else {
            self.missing(message)
        }
    }

    fn missing(&mut self, message: &str) -> CstElement {
        self.error(message);
        let span = self.current_span();
        CstElement::Token(CstToken {
            kind: SyntaxKind::Error,
            span,
        })
    }

    fn bump_element(&mut self) -> CstElement {
        self.skip_ws_comments();
        let token = self
            .tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF")
            .clone();
        if token.kind != SyntaxKind::Eof {
            self.pos += 1;
        }
        self.skip_ws_comments();
        CstElement::Token(token.into())
    }

    fn current_kind(&mut self) -> SyntaxKind {
        self.skip_ws_comments();
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF")
            .kind
    }

    fn current_text_is(&mut self, expected: &str) -> bool {
        self.skip_ws_comments();
        let token = self
            .tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF");
        self.source[token.span.clone()] == *expected
    }

    fn skip_type_trivia(&mut self) {
        while matches!(
            self.tokens
                .get(self.pos)
                .or_else(|| self.tokens.last())
                .expect("lexer always emits EOF")
                .kind,
            SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::LineComment
        ) {
            self.pos += 1;
        }
    }

    fn type_current_kind(&mut self) -> SyntaxKind {
        self.skip_type_trivia();
        self.raw_current_kind()
    }

    fn type_current_text_is(&mut self, expected: &str) -> bool {
        self.skip_type_trivia();
        let span = self.raw_current_span();
        self.source[span] == *expected
    }

    fn type_bump_element(&mut self) -> CstElement {
        self.skip_type_trivia();
        self.raw_bump_element()
    }

    fn type_expect_token(&mut self, kind: SyntaxKind, message: &str) -> CstElement {
        self.skip_type_trivia();
        if self.raw_current_kind() == kind {
            self.raw_bump_element()
        } else {
            self.missing(message)
        }
    }

    fn nth_kind(&self, n: usize) -> SyntaxKind {
        self.nth_non_ws_token_from(self.pos, n).kind
    }

    fn nth_non_ws_kind_from(&self, start: usize, n: usize) -> SyntaxKind {
        self.nth_non_ws_token_from(start, n).kind
    }

    fn nth_non_ws_token(&self, n: usize) -> &Token {
        self.nth_non_ws_token_from(self.pos, n)
    }

    fn nth_non_ws_token_from(&self, start: usize, n: usize) -> &Token {
        let mut idx = start;
        let mut seen = 0usize;
        loop {
            let token = self
                .tokens
                .get(idx)
                .or_else(|| self.tokens.last())
                .expect("lexer always emits EOF");
            if !matches!(token.kind, SyntaxKind::Whitespace | SyntaxKind::LineComment) {
                if seen == n {
                    return token;
                }
                seen += 1;
            }
            if token.kind == SyntaxKind::Eof {
                return token;
            }
            idx += 1;
        }
    }

    fn looks_like_dom_expr(&self) -> bool {
        let token = self.nth_non_ws_token(0);
        token.kind == SyntaxKind::Ident
            && self.source[token.span.clone()] == *"dom"
            && self.nth_non_ws_kind_from(self.pos, 1) == SyntaxKind::Lt
            && self.nth_non_ws_kind_from(self.pos, 2) == SyntaxKind::Ident
    }

    fn raw_current_kind(&self) -> SyntaxKind {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF")
            .kind
    }

    fn raw_current_span(&self) -> std::ops::Range<usize> {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF")
            .span
            .clone()
    }

    fn raw_nth_non_ws_kind(&self, n: usize) -> SyntaxKind {
        self.nth_non_ws_token_from(self.pos, n).kind
    }

    fn raw_bump_element(&mut self) -> CstElement {
        let token = self
            .tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF")
            .clone();
        if token.kind != SyntaxKind::Eof {
            self.pos += 1;
        }
        CstElement::Token(token.into())
    }

    fn skip_dom_trivia(&mut self) {
        while matches!(
            self.tokens
                .get(self.pos)
                .or_else(|| self.tokens.last())
                .expect("lexer always emits EOF")
                .kind,
            SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::LineComment
        ) {
            self.pos += 1;
        }
    }

    fn dom_current_kind(&mut self) -> SyntaxKind {
        self.skip_dom_trivia();
        self.raw_current_kind()
    }

    fn dom_bump_element(&mut self) -> CstElement {
        self.skip_dom_trivia();
        self.raw_bump_element()
    }

    fn dom_expect_token(&mut self, kind: SyntaxKind, message: &str) -> CstElement {
        self.skip_dom_trivia();
        if self.raw_current_kind() == kind {
            self.raw_bump_element()
        } else {
            self.missing(message)
        }
    }

    fn token_text(&self, element: &CstElement) -> Option<String> {
        let CstElement::Token(token) = element else {
            return None;
        };
        Some(self.source[token.span.clone()].to_owned())
    }

    fn current_span(&mut self) -> std::ops::Range<usize> {
        self.skip_ws_comments();
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .expect("lexer always emits EOF")
            .span
            .clone()
    }

    fn skip_ws_comments(&mut self) {
        while matches!(
            self.tokens
                .get(self.pos)
                .or_else(|| self.tokens.last())
                .expect("lexer always emits EOF")
                .kind,
            SyntaxKind::Whitespace | SyntaxKind::LineComment
        ) {
            self.pos += 1;
        }
    }

    fn error(&mut self, message: &str) {
        let span = self.current_span();
        self.errors.push(ParseError::new(message, span));
    }
}

fn element_end(element: &CstElement) -> usize {
    match element {
        CstElement::Node(node) => node.span.end,
        CstElement::Token(token) => token.span.end,
    }
}

fn element_span(element: &CstElement) -> std::ops::Range<usize> {
    match element {
        CstElement::Node(node) => node.span.clone(),
        CstElement::Token(token) => token.span.clone(),
    }
}

fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8, SyntaxKind)> {
    Some(match kind {
        SyntaxKind::Eq => (1, 1, SyntaxKind::AssignExpr),
        SyntaxKind::PipePipe => (2, 3, SyntaxKind::BinaryExpr),
        SyntaxKind::AmpAmp => (4, 5, SyntaxKind::BinaryExpr),
        SyntaxKind::EqEq | SyntaxKind::BangEq => (6, 7, SyntaxKind::BinaryExpr),
        SyntaxKind::Lt | SyntaxKind::LtEq | SyntaxKind::Gt | SyntaxKind::GtEq => {
            (8, 9, SyntaxKind::BinaryExpr)
        }
        SyntaxKind::DotDot => (10, 11, SyntaxKind::BinaryExpr),
        SyntaxKind::Plus | SyntaxKind::Minus => (12, 13, SyntaxKind::BinaryExpr),
        SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::Percent => {
            (14, 15, SyntaxKind::BinaryExpr)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{CstElement, CstNode, SyntaxKind};

    fn contains(node: &CstNode, kind: SyntaxKind) -> bool {
        node.kind == kind
            || node.children.iter().any(|child| match child {
                CstElement::Node(node) => contains(node, kind),
                CstElement::Token(token) => token.kind == kind,
            })
    }

    fn count(node: &CstNode, kind: SyntaxKind) -> usize {
        usize::from(node.kind == kind)
            + node
                .children
                .iter()
                .map(|child| match child {
                    CstElement::Node(node) => count(node, kind),
                    CstElement::Token(token) => usize::from(token.kind == kind),
                })
                .sum::<usize>()
    }

    #[test]
    fn parses_contextual_value_kind_references() {
        let parsed = parse(
            "let count: int = 1\n\
             fn convert(value: float) -> string\n\
               return to_literal(value)\n\
             end\n\
             let callback = fn(value: identity) -> bool => true\n\
             fn window(?limit: int = 100, @labels: list) -> list => labels",
        );

        assert_eq!(parsed.errors, vec![]);
        assert_eq!(count(&parsed.root, SyntaxKind::TypeRef), 8);
    }

    #[test]
    fn parses_structural_relation_types_with_alternatives_and_cardinality() {
        let parsed = parse(
            "let outcome: relation<\n\
               {:case -> :ok, :value -> int}\n\
               | {:case -> :error, :value -> error}\n\
             > where rows ∈ 1 = value",
        );

        assert_eq!(parsed.errors, vec![]);
        assert_eq!(count(&parsed.root, SyntaxKind::RelationType), 1);
        assert_eq!(count(&parsed.root, SyntaxKind::TypeRow), 2);
        assert_eq!(count(&parsed.root, SyntaxKind::TypeColumn), 4);
        assert_eq!(count(&parsed.root, SyntaxKind::Cardinality), 1);
    }

    #[test]
    fn value_kind_names_remain_ordinary_identifiers() {
        let parsed = parse("let int = 1\nlet relation = int\nreturn relation");

        assert_eq!(parsed.errors, vec![]);
        assert_eq!(count(&parsed.root, SyntaxKind::TypeRef), 0);
    }

    #[test]
    fn value_kind_syntax_does_not_steal_existing_tokens() {
        let parsed = parse(
            "let entry: map = {:value -> 1}\n\
             let callback = fn(value: symbol) -> function => {item} => item\n\
             :send(actor: #alice, value: entry)\n\
             #alice:inspect(:brief)",
        );

        assert_eq!(parsed.errors, vec![]);
        assert!(contains(&parsed.root, SyntaxKind::MapEntry));
        assert!(contains(&parsed.root, SyntaxKind::LambdaExpr));
        assert!(contains(&parsed.root, SyntaxKind::RoleCallExpr));
        assert!(contains(&parsed.root, SyntaxKind::ReceiverCallExpr));
        assert_eq!(count(&parsed.root, SyntaxKind::TypeRef), 3);
    }

    #[test]
    fn enables_kind_references_on_collection_bindings() {
        let parsed = parse(
            "let [value: int, ?label: string = \"\", @rest: list] = values\n\
             for key: int, value: map in rows\nend",
        );

        assert_eq!(parsed.errors, vec![]);
        assert_eq!(count(&parsed.root, SyntaxKind::LoopBinding), 2);
        assert_eq!(count(&parsed.root, SyntaxKind::ScatterBinding), 3);
        assert_eq!(count(&parsed.root, SyntaxKind::TypeRef), 5);
    }

    #[test]
    fn brace_lambda_parameters_remain_unannotated() {
        let parsed = parse("{value: int} => value");

        assert!(!parsed.errors.is_empty());
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| { error.message.contains("type annotations are not supported") })
        );
        assert_eq!(count(&parsed.root, SyntaxKind::TypeRef), 0);
    }

    #[test]
    fn parses_bracket_lists_and_brace_maps() {
        let parse = parse("let xs = [1, @rest]\nlet opts = {:style -> :brief}");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::ListExpr));
        assert!(contains(&parse.root, SyntaxKind::MapExpr));
        assert!(contains(&parse.root, SyntaxKind::MapEntry));
    }

    #[test]
    fn parses_relation_literals_without_reserving_a_keyword() {
        let parsed = parse("return [:thing, :player] {\n  [#coin, #alice],\n  [#lamp, #bob],\n}");
        assert_eq!(parsed.errors, vec![]);
        assert!(contains(&parsed.root, SyntaxKind::RelationExpr));
        assert!(contains(&parsed.root, SyntaxKind::RelationHeading));
        assert!(contains(&parsed.root, SyntaxKind::RelationRow));

        let zero_arity = parse("return [] {[]}");
        assert_eq!(zero_arity.errors, vec![]);
        assert!(contains(&zero_arity.root, SyntaxKind::RelationExpr));
    }

    #[test]
    fn parses_empty_brace_map() {
        let parse = parse("let empty = {}");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::MapExpr));
    }

    #[test]
    fn parses_underscore_as_range_endpoint_hole() {
        let parse = parse("items[2.._]");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::BinaryExpr));
        assert!(contains(&parse.root, SyntaxKind::HoleExpr));
    }

    #[test]
    fn parses_bracket_scatter_binding() {
        let parse = parse("let [head, ?middle = 10, @tail] = values");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::LetExpr));
        assert!(contains(&parse.root, SyntaxKind::ScatterPattern));
        assert!(contains(&parse.root, SyntaxKind::ScatterBinding));
    }

    #[test]
    fn parses_call_argument_splices() {
        let parse = parse("summarize(first, @rest)");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::CallExpr));
        assert!(contains(&parse.root, SyntaxKind::At));
    }

    #[test]
    fn rejects_named_arguments_in_ordinary_calls() {
        let parse = parse("summarize(first: value)");

        assert_eq!(parse.errors.len(), 1);
        assert_eq!(
            parse.errors[0].message,
            "ordinary call arguments must be positional"
        );
    }

    #[test]
    fn parses_query_variables_in_relation_calls() {
        let parse = parse("Location(#thing, ?room)");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::CallExpr));
        assert!(contains(&parse.root, SyntaxKind::QueryVarExpr));
    }

    #[test]
    fn rejects_query_variables_outside_relation_arguments() {
        let parse = parse("return ?value");

        assert_eq!(parse.errors.len(), 1);
        assert_eq!(
            parse.errors[0].message,
            "query variables are only valid as relation arguments"
        );
    }

    #[test]
    fn parses_one_relation_query_expression() {
        let parse = parse("one Location(#thing, ?room)");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::OneExpr));
        assert!(contains(&parse.root, SyntaxKind::QueryVarExpr));
    }

    #[test]
    fn parses_not_in_relation_rule_body() {
        let parse = parse(
            "VisibleTo(actor, obj) :-\n  LocatedIn(actor, room),\n  not HiddenFrom(obj, actor)",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::RelationRule));
        assert!(contains(&parse.root, SyntaxKind::UnaryExpr));
    }

    #[test]
    fn parses_role_and_receiver_calls() {
        let parse = parse(":move(actor: #alice, item: #coin)\n#box:put(#coin, :into)");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::RoleCallExpr));
        assert!(contains(&parse.root, SyntaxKind::ReceiverCallExpr));
    }

    #[test]
    fn parses_dom_attribute_name_parts_that_are_keywords() {
        let parse = parse(
            "return dom <option for=\"target\" data-source-symbol-end={to_literal(2)}>symbol</option>",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::DomAttr));
    }

    #[test]
    fn malformed_dom_attribute_name_parts_still_advance() {
        let parse = parse("return dom <div data-=\"x\" role:=\"button\">x</div>");
        assert_eq!(parse.errors.len(), 2);
        assert!(
            parse
                .errors
                .iter()
                .all(|error| error.message == "expected DOM attribute name part")
        );
        assert!(contains(&parse.root, SyntaxKind::DomAttr));
        assert!(contains(&parse.root, SyntaxKind::DomText));
    }

    #[test]
    fn parses_slash_qualified_names_without_losing_division() {
        let parse = parse(
            "ui/Visible(#lamp)\n\
             :ui/polish(actor: #ui/alice, item: #lamp)\n\
             #lamp:ui/examine(actor: #ui/alice)\n\
             endpoint.session/actor = #alice\n\
             let ratio = total/count",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::CallExpr));
        assert!(contains(&parse.root, SyntaxKind::RoleCallExpr));
        assert!(contains(&parse.root, SyntaxKind::ReceiverCallExpr));
        assert!(contains(&parse.root, SyntaxKind::IdentityExpr));
        assert!(contains(&parse.root, SyntaxKind::BinaryExpr));
    }

    #[test]
    fn spawn_requires_dispatch_target() {
        let role = parse("spawn :tick(actor: actor()) after 5");
        assert_eq!(role.errors, vec![]);
        assert!(contains(&role.root, SyntaxKind::SpawnExpr));
        assert!(contains(&role.root, SyntaxKind::RoleCallExpr));

        let receiver = parse("spawn #clock:tick(actor())");
        assert_eq!(receiver.errors, vec![]);
        assert!(contains(&receiver.root, SyntaxKind::SpawnExpr));
        assert!(contains(&receiver.root, SyntaxKind::ReceiverCallExpr));

        let ordinary = parse("spawn tick(actor())");
        assert_eq!(ordinary.errors.len(), 1);
        assert_eq!(
            ordinary.errors[0].message,
            "spawn expects a role or receiver dispatch target"
        );
    }

    #[test]
    fn parses_brace_lambda_without_confusing_it_for_map() {
        let parse = parse("let f = {x, ?style = :short, @rest} => x + 1");
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::LambdaExpr));
        assert!(!contains(&parse.root, SyntaxKind::MapEntry));
    }

    #[test]
    fn parses_expression_blocks_and_relation_rules() {
        let parse = parse(
            "VisibleTo(actor, obj) :- LocatedIn(actor, room), LocatedIn(obj, room)\n\
             if Lit(#lamp, true)\n  \"lit\"\nelse\n  \"dark\"\nend",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::RelationRule));
        assert!(contains(&parse.root, SyntaxKind::IfExpr));
    }

    #[test]
    fn parses_method_fileout_envelope() {
        let parse = parse(
            "method #move_into :move\n\
               roles actor @ #player, item @ #portable\n\
             do\n\
               require CanMove(actor, item)\n\
               assert LocatedIn(item, destination)\n\
             end",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::MethodItem));
        assert!(contains(&parse.root, SyntaxKind::RequireExpr));
        assert!(contains(&parse.root, SyntaxKind::AssertExpr));
    }

    #[test]
    fn parses_verb_sugar_body_without_do() {
        let parse = parse(
            "verb get(actor @ #player, item @ #thing)\n\
               if Portable(item)\n\
                 return true\n\
               else\n\
                 return false\n\
               end\n\
             end",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::VerbItem));
        assert!(contains(&parse.root, SyntaxKind::IfExpr));
    }

    #[test]
    fn parses_structured_verb_value_contracts() {
        let parse = parse(
            "verb send(actor @ #player: identity, message: string) -> bool\n\
               return true\n\
             end",
        );

        assert_eq!(parse.errors, vec![]);
        assert_eq!(count(&parse.root, SyntaxKind::VerbHeader), 1);
        assert_eq!(count(&parse.root, SyntaxKind::VerbParam), 2);
        assert_eq!(count(&parse.root, SyntaxKind::DispatchRestriction), 1);
        assert_eq!(count(&parse.root, SyntaxKind::TypeRef), 3);
        assert_eq!(count(&parse.root, SyntaxKind::MethodHeader), 0);
    }

    #[test]
    fn parses_try_with_catch_and_finally() {
        let parse = parse(
            "try\n\
               risky()\n\
             catch err if err == :perm\n\
               \"permission denied\"\n\
             finally\n\
               cleanup()\n\
             end",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::TryExpr));
        assert!(contains(&parse.root, SyntaxKind::CatchClause));
        assert!(contains(&parse.root, SyntaxKind::FinallyClause));
    }

    #[test]
    fn parses_begin_and_key_value_for_loop() {
        let parse = parse(
            "begin\n\
               for key, value in properties\n\
                 render_property(key, value)\n\
               end\n\
             end",
        );
        assert_eq!(parse.errors, vec![]);
        assert!(contains(&parse.root, SyntaxKind::BeginExpr));
        assert!(contains(&parse.root, SyntaxKind::ForExpr));
    }
}
