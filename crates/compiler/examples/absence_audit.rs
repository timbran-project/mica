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

use mica_compiler::{
    Arg, Ast, CatchClause, CollectionItem, Expr, FunctionBody, Item, Literal, Param,
    RecoveryClause, ScatterBinding, parse_ast,
};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Parent {
    Other,
    Return,
    Binding,
    Comparison,
    Condition,
    CallArgument,
    Collection,
    Default,
    AssignmentTarget,
}

impl Parent {
    const fn nothing_category(self) -> &'static str {
        match self {
            Self::Return => "returned sentinel",
            Self::Binding => "local sentinel",
            Self::Comparison => "sentinel comparison",
            Self::Condition => "sentinel condition",
            Self::CallArgument => "call argument sentinel",
            Self::Collection => "stored or nested sentinel",
            Self::Default => "default sentinel",
            Self::Other | Self::AssignmentTarget => "other sentinel",
        }
    }
}

#[derive(Default)]
struct FileCounts {
    categories: BTreeMap<&'static str, usize>,
}

impl FileCounts {
    fn add(&mut self, category: &'static str) {
        *self.categories.entry(category).or_default() += 1;
    }
}

#[derive(Default)]
struct Audit {
    files: BTreeMap<PathBuf, FileCounts>,
    parse_errors: BTreeMap<PathBuf, usize>,
}

impl Audit {
    fn add(&mut self, file: &Path, category: &'static str) {
        self.files.entry(file.to_owned()).or_default().add(category);
    }

    fn visit_ast(&mut self, file: &Path, ast: &Ast) {
        if !ast.errors.is_empty() {
            self.parse_errors.insert(file.to_owned(), ast.errors.len());
        }
        for item in &ast.items {
            self.visit_item(file, item);
        }
    }

    fn visit_item(&mut self, file: &Path, item: &Item) {
        match item {
            Item::Expr { expr, .. } => self.visit_expr(file, expr, Parent::Other),
            Item::RelationRule { head, body, .. } => {
                self.visit_expr(file, head, Parent::Other);
                for expr in body {
                    self.visit_expr(file, expr, Parent::Other);
                }
            }
            Item::Method { params, body, .. } => {
                for param in params {
                    let _ = param;
                }
                for item in body {
                    self.visit_item(file, item);
                }
            }
        }
    }

    fn visit_expr(&mut self, file: &Path, expr: &Expr, parent: Parent) {
        match expr {
            Expr::Literal {
                value: Literal::Nothing,
                ..
            } => self.add(file, parent.nothing_category()),
            Expr::Literal { .. }
            | Expr::Name { .. }
            | Expr::QueryVar { .. }
            | Expr::Identity { .. }
            | Expr::Symbol { .. }
            | Expr::Hole { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. }
            | Expr::Error { .. } => {}
            Expr::Frob { value, .. }
            | Expr::Unary { expr: value, .. }
            | Expr::Effect { expr: value, .. }
            | Expr::Spawn { target: value, .. } => {
                self.visit_expr(file, value, Parent::Other);
                if let Expr::Spawn {
                    delay: Some(delay), ..
                } = expr
                {
                    self.visit_expr(file, delay, Parent::Other);
                }
            }
            Expr::List { items, .. } => {
                for item in items {
                    match item {
                        CollectionItem::Expr(value) | CollectionItem::Splice(value) => {
                            self.visit_expr(file, value, Parent::Collection);
                        }
                    }
                }
            }
            Expr::Relation { rows, .. } => {
                for row in rows {
                    for value in row {
                        self.visit_expr(file, value, Parent::Collection);
                    }
                }
            }
            Expr::Map { entries, .. } => {
                for (key, value) in entries {
                    self.visit_expr(file, key, Parent::Collection);
                    self.visit_expr(file, value, Parent::Collection);
                }
            }
            Expr::Binary {
                op, left, right, ..
            } => {
                let parent = if matches!(
                    op,
                    mica_compiler::BinaryOp::Eq | mica_compiler::BinaryOp::Ne
                ) {
                    Parent::Comparison
                } else {
                    Parent::Other
                };
                self.visit_expr(file, left, parent);
                self.visit_expr(file, right, parent);
            }
            Expr::Assign { target, value, .. } => {
                self.visit_expr(file, target, Parent::AssignmentTarget);
                self.visit_expr(file, value, Parent::Binding);
            }
            Expr::Call { callee, args, .. } => {
                self.visit_expr(file, callee, Parent::Other);
                self.visit_args(file, args);
            }
            Expr::RoleCall { selector, args, .. } => {
                self.visit_expr(file, selector, Parent::Other);
                self.visit_args(file, args);
            }
            Expr::ReceiverCall {
                receiver,
                selector,
                args,
                ..
            } => {
                self.visit_expr(file, receiver, Parent::Other);
                self.visit_expr(file, selector, Parent::Other);
                self.visit_args(file, args);
            }
            Expr::Index {
                collection, index, ..
            } => {
                self.add(file, "index operation");
                self.visit_expr(file, collection, Parent::Other);
                if let Some(index) = index {
                    self.visit_expr(file, index, Parent::Other);
                }
            }
            Expr::Field { base, .. } => {
                if !matches!(parent, Parent::AssignmentTarget) {
                    self.add(file, "functional dot read");
                }
                self.visit_expr(file, base, parent);
            }
            Expr::Binding { pattern, value, .. } => {
                if let mica_compiler::BindingPattern::Scatter(bindings) = pattern {
                    self.visit_scatter_bindings(file, bindings);
                }
                if let Some(value) = value {
                    self.visit_expr(file, value, Parent::Binding);
                }
            }
            Expr::If {
                condition,
                then_items,
                elseif,
                else_items,
                ..
            } => {
                self.visit_expr(file, condition, Parent::Condition);
                self.visit_items(file, then_items);
                for (condition, items) in elseif {
                    self.visit_expr(file, condition, Parent::Condition);
                    self.visit_items(file, items);
                }
                self.visit_items(file, else_items);
            }
            Expr::Block { items, .. } => self.visit_items(file, items),
            Expr::For {
                iter, body, value, ..
            } => {
                self.add(file, "for binding");
                let _ = value;
                self.visit_expr(file, iter, Parent::Other);
                self.visit_items(file, body);
            }
            Expr::While {
                condition, body, ..
            } => {
                self.visit_expr(file, condition, Parent::Condition);
                self.visit_items(file, body);
            }
            Expr::Return { value, .. } => {
                if let Some(value) = value {
                    self.visit_expr(file, value, Parent::Return);
                } else {
                    self.add(file, "bare return");
                }
            }
            Expr::Raise {
                error,
                message,
                value,
                ..
            } => {
                self.visit_expr(file, error, Parent::Other);
                if let Some(message) = message {
                    self.visit_expr(file, message, Parent::Other);
                }
                if let Some(value) = value {
                    self.visit_expr(file, value, Parent::Other);
                }
            }
            Expr::Recover { expr, catches, .. } => {
                self.visit_expr(file, expr, Parent::Other);
                for catch in catches {
                    self.visit_recovery(file, catch);
                }
            }
            Expr::One { expr, .. } => {
                let category = if contains_query_var(expr) {
                    "one query extraction"
                } else if matches!(expr.as_ref(), Expr::List { .. } | Expr::Map { .. }) {
                    "one collection extraction"
                } else {
                    "one other extraction"
                };
                self.add(file, category);
                self.visit_expr(file, expr, Parent::Other);
            }
            Expr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                self.visit_items(file, body);
                for catch in catches {
                    self.visit_catch(file, catch);
                }
                self.visit_items(file, finally);
            }
            Expr::Function { params, body, .. } => {
                self.visit_params(file, params);
                match body {
                    FunctionBody::Expr(expr) => self.visit_expr(file, expr, Parent::Return),
                    FunctionBody::Block(items) => self.visit_items(file, items),
                }
            }
        }
    }

    fn visit_items(&mut self, file: &Path, items: &[Item]) {
        for item in items {
            self.visit_item(file, item);
        }
    }

    fn visit_args(&mut self, file: &Path, args: &[Arg]) {
        for arg in args {
            self.visit_expr(file, &arg.value, Parent::CallArgument);
        }
    }

    fn visit_params(&mut self, file: &Path, params: &[Param]) {
        for param in params {
            if let Some(default) = &param.default {
                self.visit_expr(file, default, Parent::Default);
            }
        }
    }

    fn visit_scatter_bindings(&mut self, file: &Path, bindings: &[ScatterBinding]) {
        for binding in bindings {
            if let Some(default) = &binding.default {
                self.visit_expr(file, default, Parent::Default);
            }
        }
    }

    fn visit_catch(&mut self, file: &Path, catch: &CatchClause) {
        if let Some(condition) = &catch.condition {
            self.visit_expr(file, condition, Parent::Condition);
        }
        self.visit_items(file, &catch.body);
    }

    fn visit_recovery(&mut self, file: &Path, catch: &RecoveryClause) {
        if let Some(condition) = &catch.condition {
            self.visit_expr(file, condition, Parent::Condition);
        }
        self.visit_expr(file, &catch.value, Parent::Other);
    }
}

fn contains_query_var(expr: &Expr) -> bool {
    match expr {
        Expr::QueryVar { .. } => true,
        Expr::Call { callee, args, .. }
        | Expr::RoleCall {
            selector: callee,
            args,
            ..
        } => contains_query_var(callee) || args.iter().any(|arg| contains_query_var(&arg.value)),
        Expr::ReceiverCall {
            receiver,
            selector,
            args,
            ..
        } => {
            contains_query_var(receiver)
                || contains_query_var(selector)
                || args.iter().any(|arg| contains_query_var(&arg.value))
        }
        Expr::Unary { expr, .. }
        | Expr::One { expr, .. }
        | Expr::Effect { expr, .. }
        | Expr::Frob { value: expr, .. } => contains_query_var(expr),
        Expr::Binary { left, right, .. }
        | Expr::Assign {
            target: left,
            value: right,
            ..
        } => contains_query_var(left) || contains_query_var(right),
        _ => false,
    }
}

fn collect_mica_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| format!("{}: {error}", root.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_mica_files(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "mica")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn mask_filein_grants(source: &str) -> String {
    let mut masked = String::with_capacity(source.len());
    let mut in_grant = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if !in_grant && trimmed.starts_with("grant ") {
            in_grant = true;
        }
        if in_grant {
            masked.extend(line.chars().map(|character| {
                if character == '\n' || character == '\r' {
                    character
                } else {
                    ' '
                }
            }));
            if trimmed == "end" {
                in_grant = false;
            }
        } else {
            masked.push_str(line);
        }
    }
    masked
}

fn main() -> Result<(), String> {
    let root = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps"));
    let mut files = Vec::new();
    collect_mica_files(&root, &mut files)?;
    files.sort();

    let mut audit = Audit::default();
    for file in files {
        let source = fs::read_to_string(&file)
            .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
        audit.visit_ast(&file, &parse_ast(&mask_filein_grants(&source)));
    }

    println!("category\tcount");
    let mut totals = BTreeMap::<&str, usize>::new();
    for counts in audit.files.values() {
        for (category, count) in &counts.categories {
            *totals.entry(category).or_default() += count;
        }
    }
    for (category, count) in totals {
        println!("{category}\t{count}");
    }
    println!("\nfile\tcategory\tcount");
    for (file, counts) in audit.files {
        for (category, count) in counts.categories {
            println!("{}\t{category}\t{count}", file.display());
        }
    }
    if !audit.parse_errors.is_empty() {
        eprintln!("files with parse errors:");
        for (file, count) in audit.parse_errors {
            eprintln!("{}\t{count}", file.display());
        }
    }
    Ok(())
}
