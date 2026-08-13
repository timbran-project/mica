use crate::util::{int_value, invalid_relation};
use mica_compiler::{SyntaxKind, lex};
use mica_relation_kernel::{KernelError, RelationId};
use mica_var::{Symbol, Value};
use std::path::Path;
use tree_sitter::{Language, Node, Parser};

#[derive(Debug)]
pub(crate) struct SyntaxDocument {
    pub(crate) text: String,
    pub(crate) line_starts: Vec<usize>,
    pub(crate) highlights: Vec<HighlightSpan>,
    pub(crate) outline: Vec<OutlineItem>,
}

impl SyntaxDocument {
    pub(crate) fn parse(path: &str, text: &str) -> Self {
        let language = SourceLanguage::from_path(path);
        let line_starts = line_starts(text);
        let mut highlights = Vec::new();
        let mut outline = Vec::new();

        match language {
            SourceLanguage::Rust | SourceLanguage::JavaScript | SourceLanguage::Markdown => {
                if let Some((tree_language, tree)) = parse_tree(language, text) {
                    collect_tree_highlights(tree.root_node(), text, &mut highlights);
                    collect_tree_outline(tree.root_node(), text, language, &mut outline);
                    let _ = tree_language;
                }
            }
            SourceLanguage::Mica => {
                highlights.extend(mica_highlights(text));
                outline.extend(mica_outline(text, &line_starts));
            }
            SourceLanguage::Plain => {}
        }

        if !matches!(language, SourceLanguage::Mica) {
            highlights.extend(fallback_line_highlights(text, &line_starts));
        }
        highlights.sort_by_key(|span| (span.start, span.end));
        highlights = dedupe_highlights(highlights);
        outline.sort_by_key(|item| (item.start_byte, item.end_byte, item.name.clone()));

        Self {
            text: text.to_owned(),
            line_starts,
            highlights,
            outline,
        }
    }

    pub(crate) fn node_at(&self, byte_offset: usize) -> Option<OutlineItem> {
        self.outline
            .iter()
            .filter(|item| item.start_byte <= byte_offset && byte_offset <= item.end_byte)
            .min_by_key(|item| item.end_byte.saturating_sub(item.start_byte))
            .cloned()
            .or_else(|| {
                self.outline
                    .iter()
                    .filter(|item| item.start_byte <= byte_offset)
                    .max_by_key(|item| item.start_byte)
                    .cloned()
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceLanguage {
    Rust,
    Mica,
    Markdown,
    JavaScript,
    Plain,
}

impl SourceLanguage {
    pub(crate) fn from_path(path: &str) -> Self {
        match Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("rs") => Self::Rust,
            Some("mica") => Self::Mica,
            Some("md") | Some("markdown") => Self::Markdown,
            Some("js") | Some("mjs") | Some("cjs") => Self::JavaScript,
            _ => Self::Plain,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HighlightSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct OutlineItem {
    pub(crate) node: String,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

pub(crate) struct SyntaxLine {
    pub(crate) number: usize,
    pub(crate) segments: Vec<Value>,
}

fn parse_tree(language: SourceLanguage, text: &str) -> Option<(Language, tree_sitter::Tree)> {
    let tree_language = match language {
        SourceLanguage::Rust => Language::new(tree_sitter_rust::LANGUAGE),
        SourceLanguage::JavaScript => Language::new(tree_sitter_javascript::LANGUAGE),
        SourceLanguage::Markdown => Language::new(tree_sitter_md::LANGUAGE),
        SourceLanguage::Mica | SourceLanguage::Plain => return None,
    };
    let mut parser = Parser::new();
    parser.set_language(&tree_language).ok()?;
    let tree = parser.parse(text, None)?;
    Some((tree_language, tree))
}

fn collect_tree_highlights(node: Node<'_>, text: &str, spans: &mut Vec<HighlightSpan>) {
    collect_tree_highlights_with_parent(node, None, text, spans);
}

fn collect_tree_highlights_with_parent(
    node: Node<'_>,
    parent_kind: Option<&str>,
    text: &str,
    spans: &mut Vec<HighlightSpan>,
) {
    let kind = node.kind();
    if node.child_count() == 0 {
        if let Some(highlight) = tree_highlight_kind(kind, parent_kind) {
            spans.push(HighlightSpan {
                start: node.start_byte(),
                end: node.end_byte(),
                kind: highlight,
            });
        }
        return;
    }

    if let Some(highlight) = tree_highlight_kind(kind, parent_kind) {
        spans.push(HighlightSpan {
            start: node.start_byte(),
            end: node.end_byte(),
            kind: highlight,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.start_byte() < text.len() || child.end_byte() <= text.len() {
            collect_tree_highlights_with_parent(child, Some(kind), text, spans);
        }
    }
}

fn tree_highlight_kind(kind: &str, parent_kind: Option<&str>) -> Option<&'static str> {
    if kind.contains("comment") {
        return Some("comment");
    }
    if kind.contains("string") || kind == "char_literal" || kind == "template_string" {
        return Some("string");
    }
    if kind.contains("integer") || kind.contains("float") || kind == "number" {
        return Some("number");
    }
    if matches!(
        kind,
        "type_identifier" | "primitive_type" | "scoped_type_identifier"
    ) {
        return Some("type");
    }
    if matches!(
        kind,
        "fn" | "let"
            | "mut"
            | "pub"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "mod"
            | "use"
            | "where"
            | "for"
            | "while"
            | "loop"
            | "if"
            | "else"
            | "match"
            | "return"
            | "async"
            | "await"
            | "const"
            | "static"
            | "type"
            | "class"
            | "function"
            | "import"
            | "export"
            | "from"
            | "new"
            | "yield"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "=>"
    ) {
        return Some("keyword");
    }
    if kind == "identifier"
        && parent_kind.is_some_and(|parent| {
            matches!(
                parent,
                "function_item"
                    | "function_declaration"
                    | "method_definition"
                    | "call_expression"
                    | "macro_invocation"
            )
        })
    {
        return Some("function");
    }
    if kind == "identifier" {
        return Some("identifier");
    }
    if kind == "field_identifier" || kind == "property_identifier" {
        return Some("property");
    }
    if kind.starts_with("atx_h") || kind == "atx_heading_marker" {
        return Some("heading");
    }
    if kind == "code_fence_content" || kind == "code_span" {
        return Some("string");
    }
    None
}

fn collect_tree_outline(
    node: Node<'_>,
    text: &str,
    language: SourceLanguage,
    outline: &mut Vec<OutlineItem>,
) {
    if let Some(item) = tree_outline_item(node, text, language) {
        outline.push(item);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tree_outline(child, text, language, outline);
    }
}

fn tree_outline_item(node: Node<'_>, text: &str, language: SourceLanguage) -> Option<OutlineItem> {
    let kind = node.kind();
    let (outline_kind, name) = match language {
        SourceLanguage::Rust => rust_outline_name(node, text, kind)?,
        SourceLanguage::JavaScript => javascript_outline_name(node, text, kind)?,
        SourceLanguage::Markdown => markdown_outline_name(node, text, kind)?,
        SourceLanguage::Mica | SourceLanguage::Plain => return None,
    };
    Some(outline_item(
        outline_kind,
        name,
        node.start_byte(),
        node.end_byte(),
        node.start_position().row + 1,
        node.end_position().row + 1,
    ))
}

fn rust_outline_name(node: Node<'_>, text: &str, kind: &str) -> Option<(&'static str, String)> {
    let outline_kind = match kind {
        "function_item" => "function",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "impl_item" => "impl",
        "mod_item" => "module",
        "const_item" => "const",
        "static_item" => "static",
        "type_item" => "type",
        "macro_definition" => "macro",
        _ => return None,
    };
    let name = node
        .child_by_field_name("name")
        .and_then(|child| node_text(child, text))
        .or_else(|| {
            if kind == "impl_item" {
                first_named_child_text(node, text)
            } else {
                None
            }
        })
        .unwrap_or_else(|| outline_kind.to_owned());
    Some((outline_kind, name))
}

fn javascript_outline_name(
    node: Node<'_>,
    text: &str,
    kind: &str,
) -> Option<(&'static str, String)> {
    let outline_kind = match kind {
        "function_declaration" => "function",
        "class_declaration" => "class",
        "method_definition" => "method",
        "lexical_declaration" => "binding",
        _ => return None,
    };
    let name = node
        .child_by_field_name("name")
        .and_then(|child| node_text(child, text))
        .or_else(|| first_named_child_text(node, text))
        .unwrap_or_else(|| outline_kind.to_owned());
    Some((outline_kind, name))
}

fn markdown_outline_name(node: Node<'_>, text: &str, kind: &str) -> Option<(&'static str, String)> {
    if !matches!(kind, "atx_heading" | "setext_heading") {
        return None;
    }
    let raw = node_text(node, text)?;
    let name = raw
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_owned();
    if name.is_empty() {
        return None;
    }
    Some(("heading", name))
}

fn first_named_child_text(node: Node<'_>, text: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| node_text(child, text))
}

fn node_text(node: Node<'_>, text: &str) -> Option<String> {
    text.get(node.start_byte()..node.end_byte())
        .map(str::to_owned)
}

fn mica_highlights(text: &str) -> Vec<HighlightSpan> {
    let tokens = lex(text);
    let mut highlights = Vec::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        if token.kind == SyntaxKind::Ident {
            let mut end = token.span.end;
            let mut next_index = index + 1;
            while let (Some(slash), Some(next)) =
                (tokens.get(next_index), tokens.get(next_index + 1))
            {
                if slash.kind != SyntaxKind::Slash
                    || next.kind != SyntaxKind::Ident
                    || slash.span.start != end
                    || slash.span.end != next.span.start
                {
                    break;
                }
                end = next.span.end;
                next_index += 2;
            }
            highlights.push(HighlightSpan {
                start: token.span.start,
                end,
                kind: "identifier",
            });
            index = next_index;
            continue;
        }

        let kind = match token.kind {
            SyntaxKind::LineComment => "comment",
            SyntaxKind::String => "string",
            SyntaxKind::Int | SyntaxKind::Float => "number",
            SyntaxKind::LetKw
            | SyntaxKind::ConstKw
            | SyntaxKind::IfKw
            | SyntaxKind::ElseIfKw
            | SyntaxKind::ElseKw
            | SyntaxKind::EndKw
            | SyntaxKind::BeginKw
            | SyntaxKind::ForKw
            | SyntaxKind::InKw
            | SyntaxKind::WhileKw
            | SyntaxKind::ReturnKw
            | SyntaxKind::RaiseKw
            | SyntaxKind::RecoverKw
            | SyntaxKind::SpawnKw
            | SyntaxKind::AfterKw
            | SyntaxKind::NotKw
            | SyntaxKind::BreakKw
            | SyntaxKind::ContinueKw
            | SyntaxKind::TryKw
            | SyntaxKind::CatchKw
            | SyntaxKind::AsKw
            | SyntaxKind::FinallyKw
            | SyntaxKind::FnKw
            | SyntaxKind::MethodKw
            | SyntaxKind::VerbKw
            | SyntaxKind::DoKw
            | SyntaxKind::AssertKw
            | SyntaxKind::RetractKw
            | SyntaxKind::RequireKw
            | SyntaxKind::TrueKw
            | SyntaxKind::FalseKw => "keyword",
            SyntaxKind::ErrorCode => "error",
            _ => {
                index += 1;
                continue;
            }
        };
        highlights.push(HighlightSpan {
            start: token.span.start,
            end: token.span.end,
            kind,
        });
        index += 1;
    }
    highlights
}

fn mica_outline(text: &str, line_starts: &[usize]) -> Vec<OutlineItem> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim_start();
            let start_byte = *line_starts.get(index)?;
            let indent = line.len().saturating_sub(trimmed.len());
            let item_start = start_byte + indent;
            let (kind, name) = if let Some(rest) = trimmed.strip_prefix("verb ") {
                ("verb", rest.split('(').next().unwrap_or("verb").trim())
            } else if let Some(rest) = trimmed.strip_prefix("method ") {
                ("method", rest.split_whitespace().next().unwrap_or("method"))
            } else if let Some(rest) = trimmed.strip_prefix("make_relation(:") {
                ("relation", rest.split(',').next().unwrap_or("relation"))
            } else if let Some(rest) = trimmed.strip_prefix("make_functional_relation(:") {
                ("relation", rest.split(',').next().unwrap_or("relation"))
            } else if trimmed.contains(":-") {
                ("rule", trimmed.split(":-").next().unwrap_or("rule").trim())
            } else {
                return None;
            };
            let name = name.trim().trim_end_matches(')').to_owned();
            Some(outline_item(
                kind,
                name,
                item_start,
                start_byte + line.len(),
                index + 1,
                index + 1,
            ))
        })
        .collect()
}

fn fallback_line_highlights(text: &str, line_starts: &[usize]) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let Some(line_start) = line_starts.get(index).copied() else {
            continue;
        };
        if let Some(comment_start) = line.find("//") {
            spans.push(HighlightSpan {
                start: line_start + comment_start,
                end: line_start + line.len(),
                kind: "comment",
            });
        }
        if line.trim_start().starts_with('#') {
            spans.push(HighlightSpan {
                start: line_start,
                end: line_start + line.len(),
                kind: "heading",
            });
        }
    }
    spans
}

fn dedupe_highlights(spans: Vec<HighlightSpan>) -> Vec<HighlightSpan> {
    let mut out: Vec<HighlightSpan> = Vec::new();
    for span in spans {
        if span.start >= span.end {
            continue;
        }
        if out
            .last()
            .is_some_and(|last| last.start == span.start && last.end == span.end)
        {
            continue;
        }
        out.push(span);
    }
    out
}

pub(crate) fn syntax_lines(
    relation: RelationId,
    syntax: &SyntaxDocument,
    start_line: usize,
    line_count: usize,
) -> Result<Vec<SyntaxLine>, KernelError> {
    let mut rows = Vec::new();
    for line_index in start_line.saturating_sub(1)..start_line.saturating_sub(1) + line_count {
        let Some(line_start) = syntax.line_starts.get(line_index).copied() else {
            break;
        };
        let line_end = line_end_without_newline(&syntax.text, &syntax.line_starts, line_index);
        let line_text = syntax
            .text
            .get(line_start..line_end)
            .ok_or_else(|| invalid_relation(relation, "line boundaries are not utf-8"))?;
        let segments = line_segments(relation, syntax, line_start, line_end, line_text)?;
        rows.push(SyntaxLine {
            number: line_index + 1,
            segments,
        });
    }
    Ok(rows)
}

fn line_segments(
    relation: RelationId,
    syntax: &SyntaxDocument,
    line_start: usize,
    line_end: usize,
    line_text: &str,
) -> Result<Vec<Value>, KernelError> {
    let mut cursor = line_start;
    let mut segments = Vec::new();
    for span in syntax
        .highlights
        .iter()
        .filter(|span| span.end > line_start && span.start < line_end)
    {
        let start = span.start.max(line_start);
        let end = span.end.min(line_end);
        if start < cursor || start >= end {
            continue;
        }
        if cursor < start {
            let text = syntax
                .text
                .get(cursor..start)
                .ok_or_else(|| invalid_relation(relation, "highlight boundary is not utf-8"))?;
            segments.push(syntax_segment(
                relation, line_start, cursor, start, "plain", text,
            )?);
        }
        let text = syntax
            .text
            .get(start..end)
            .ok_or_else(|| invalid_relation(relation, "highlight boundary is not utf-8"))?;
        segments.push(syntax_segment(
            relation, line_start, start, end, span.kind, text,
        )?);
        cursor = end;
    }
    if cursor < line_end {
        let text = syntax
            .text
            .get(cursor..line_end)
            .ok_or_else(|| invalid_relation(relation, "highlight boundary is not utf-8"))?;
        segments.push(syntax_segment(
            relation, line_start, cursor, line_end, "plain", text,
        )?);
    }
    if segments.is_empty() {
        segments.push(syntax_segment(
            relation, line_start, line_start, line_end, "plain", line_text,
        )?);
    }
    Ok(segments)
}

fn syntax_segment(
    relation: RelationId,
    line_start: usize,
    start: usize,
    end: usize,
    kind: &str,
    text: &str,
) -> Result<Value, KernelError> {
    Ok(Value::map([
        (
            Value::symbol(Symbol::intern("start_col")),
            int_value(relation, byte_column(line_start, start) as i64)?,
        ),
        (
            Value::symbol(Symbol::intern("end_col")),
            int_value(relation, byte_column(line_start, end) as i64)?,
        ),
        (
            Value::symbol(Symbol::intern("start_byte")),
            int_value(relation, start as i64)?,
        ),
        (
            Value::symbol(Symbol::intern("end_byte")),
            int_value(relation, end as i64)?,
        ),
        (Value::symbol(Symbol::intern("kind")), Value::string(kind)),
        (Value::symbol(Symbol::intern("text")), Value::string(text)),
    ]))
}

fn byte_column(line_start: usize, offset: usize) -> usize {
    offset.saturating_sub(line_start)
}

pub(crate) fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' && index + 1 < text.len() {
            starts.push(index + 1);
        }
    }
    starts
}

pub(crate) fn line_end_without_newline(
    text: &str,
    line_starts: &[usize],
    line_index: usize,
) -> usize {
    let next_start = line_starts
        .get(line_index + 1)
        .copied()
        .unwrap_or(text.len());
    let mut end = next_start;
    if end > 0 && text.as_bytes().get(end - 1) == Some(&b'\n') {
        end -= 1;
    }
    if end > 0 && text.as_bytes().get(end - 1) == Some(&b'\r') {
        end -= 1;
    }
    end
}

fn outline_item(
    kind: impl Into<String>,
    name: String,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
) -> OutlineItem {
    let kind = kind.into();
    let node = format!("{start_byte:012}:{kind}:{end_byte:012}:{name}");
    OutlineItem {
        node,
        kind,
        name,
        start_line,
        end_line,
        start_byte,
        end_byte,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_document_supports_phase_three_languages() {
        let rust = SyntaxDocument::parse("lib.rs", "pub fn run() -> i32 { 1 }\n");
        assert!(
            rust.outline
                .iter()
                .any(|item| item.kind == "function" && item.name == "run")
        );
        assert!(rust.highlights.iter().any(|span| span.kind == "keyword"));

        let javascript = SyntaxDocument::parse("bootstrap.js", "export function boot() {}\n");
        assert!(
            javascript
                .outline
                .iter()
                .any(|item| item.kind == "function" && item.name == "boot")
        );
        assert!(
            javascript
                .highlights
                .iter()
                .any(|span| span.kind == "keyword")
        );

        let markdown = SyntaxDocument::parse("README.md", "# Source Viewer\n\nbody\n");
        assert!(
            markdown
                .outline
                .iter()
                .any(|item| item.kind == "heading" && item.name == "Source Viewer")
        );
        assert!(
            markdown
                .highlights
                .iter()
                .any(|span| span.kind == "heading")
        );

        let mica = SyntaxDocument::parse(
            "ui.mica",
            "verb source/app_node(view)\n  return true\nend\n",
        );
        assert!(
            mica.outline
                .iter()
                .any(|item| item.kind == "verb" && item.name == "source/app_node")
        );
        assert!(mica.highlights.iter().any(|span| span.kind == "keyword"));
        assert!(mica.highlights.iter().any(|span| {
            span.kind == "identifier"
                && mica
                    .text
                    .get(span.start..span.end)
                    .is_some_and(|text| text == "source/app_node")
        }));
    }
}
