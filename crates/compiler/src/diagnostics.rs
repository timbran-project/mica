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

use ariadne::{CharSet, Config, Label, Report, ReportKind, sources};

use crate::{CompileError, Diagnostic, ParseError, Span};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticVerbosity {
    Summary,
    SourceContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticRenderOptions {
    pub verbosity: DiagnosticVerbosity,
    pub use_graphics: bool,
    pub use_color: bool,
}

impl DiagnosticRenderOptions {
    pub const fn source_context() -> Self {
        Self {
            verbosity: DiagnosticVerbosity::SourceContext,
            use_graphics: true,
            use_color: false,
        }
    }
}

impl Default for DiagnosticRenderOptions {
    fn default() -> Self {
        Self {
            verbosity: DiagnosticVerbosity::Summary,
            use_graphics: false,
            use_color: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticSource<'a> {
    pub name: Option<&'a str>,
    pub text: &'a str,
}

impl<'a> DiagnosticSource<'a> {
    pub const fn new(name: Option<&'a str>, text: &'a str) -> Self {
        Self { name, text }
    }
}

pub fn format_compile_error(
    error: &CompileError,
    source: Option<DiagnosticSource<'_>>,
    options: DiagnosticRenderOptions,
) -> String {
    let reports = compile_error_diagnostics(error);
    if options.verbosity == DiagnosticVerbosity::SourceContext
        && options.use_graphics
        && let Some(source) = source
    {
        return reports
            .iter()
            .map(|report| render_graphical_report(report, source, options))
            .collect::<Vec<_>>()
            .join("\n");
    }

    reports
        .iter()
        .map(|report| render_summary_report(report, source))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileDiagnostic {
    pub title: String,
    pub message: String,
    pub span: Option<Span>,
}

pub fn compile_error_diagnostics(error: &CompileError) -> Vec<CompileDiagnostic> {
    match error {
        CompileError::Diagnostics { errors } => {
            errors.iter().flat_map(compile_error_diagnostics).collect()
        }
        CompileError::ParseErrors { errors } => parse_error_reports(errors),
        CompileError::SemanticDiagnostic { diagnostic } => {
            vec![semantic_diagnostic_report(diagnostic)]
        }
        CompileError::Unsupported { message, span, .. } => {
            vec![span_report("unsupported syntax", message, span.clone())]
        }
        CompileError::UnknownRelation { name, span, .. } => vec![span_report(
            "unknown relation",
            &format!("unknown relation `{name}`"),
            span.clone(),
        )],
        CompileError::UnknownIdentity { name, span, .. } => vec![span_report(
            "unknown identity",
            &format!("unknown identity `#{name}`"),
            span.clone(),
        )],
        CompileError::UnknownValue { name, span, .. } => vec![span_report(
            "unknown value",
            &format!("unknown value `{name}`"),
            span.clone(),
        )],
        CompileError::InvalidLiteral { message, span, .. } => {
            vec![span_report("invalid literal", message, span.clone())]
        }
        CompileError::UnboundLocal { binding, span, .. } => vec![span_report(
            "unbound local",
            &format!("unbound local binding {:?}", binding),
            span.clone(),
        )],
        CompileError::ValueKindMismatch {
            subject,
            expected,
            inferred,
            span,
            ..
        } => vec![span_report(
            "value-kind mismatch",
            &format!(
                "binding `{subject}` requires {}, but this expression produces {inferred}",
                expected.name()
            ),
            span.clone(),
        )],
        CompileError::StaticTypeMismatch {
            subject,
            expected,
            inferred,
            span,
            ..
        } => vec![span_report(
            "static type mismatch",
            &format!(
                "binding `{subject}` requires {expected}, but this expression produces {inferred}"
            ),
            span.clone(),
        )],
        CompileError::FunctionResultKindMismatch {
            function,
            expected,
            inferred,
            span,
            ..
        } => {
            let boundary = function.as_ref().map_or_else(
                || "function result".to_owned(),
                |function| format!("result of function `{function}`"),
            );
            vec![span_report(
                "function result kind mismatch",
                &format!(
                    "{boundary} requires {}, but its normal exits produce {inferred}",
                    expected.name()
                ),
                span.clone(),
            )]
        }
        CompileError::VerbResultKindMismatch {
            selector,
            expected,
            inferred,
            span,
            ..
        } => vec![span_report(
            "verb result kind mismatch",
            &format!(
                "result of verb `{selector}` requires {}, but its normal exits produce {inferred}",
                expected.name()
            ),
            span.clone(),
        )],
        CompileError::ParameterKindMismatch {
            parameter,
            expected,
            inferred,
            span,
            ..
        } => vec![span_report(
            "parameter kind mismatch",
            &format!(
                "parameter `{parameter}` requires {}, but this argument produces {inferred}",
                expected.name()
            ),
            span.clone(),
        )],
        CompileError::ParameterDefaultKindMismatch {
            parameter,
            expected,
            inferred,
            span,
            ..
        } => vec![span_report(
            "parameter default kind mismatch",
            &format!(
                "default for parameter `{parameter}` requires {}, but this expression produces {inferred}",
                expected.name()
            ),
            span.clone(),
        )],
        CompileError::MissingOptionalParameterDefault {
            parameter, span, ..
        } => vec![span_report(
            "missing optional parameter default",
            &format!("optional parameter `{parameter}` requires an explicit default"),
            span.clone(),
        )],
        CompileError::InvalidRestParameterKind {
            parameter,
            declared,
            span,
            ..
        } => vec![span_report(
            "invalid rest parameter kind",
            &format!(
                "rest parameter `{parameter}` is constructed as list, not {}",
                declared.name()
            ),
            span.clone(),
        )],
        CompileError::Runtime(error) => {
            vec![message_report("runtime error", &format!("{error:?}"))]
        }
        CompileError::Kernel(error) => vec![message_report("kernel error", &format!("{error:?}"))],
    }
}

fn parse_error_reports(errors: &[ParseError]) -> Vec<CompileDiagnostic> {
    if errors.is_empty() {
        return vec![message_report(
            "parse error",
            "parser reported an error without details",
        )];
    }
    errors
        .iter()
        .map(|error| span_report("parse error", &error.message, Some(error.span.clone())))
        .collect()
}

fn semantic_diagnostic_report(diagnostic: &Diagnostic) -> CompileDiagnostic {
    CompileDiagnostic {
        title: diagnostic.code.title().to_owned(),
        message: diagnostic.message.clone(),
        span: Some(diagnostic.span.clone()),
    }
}

fn span_report(title: &str, message: &str, span: Option<Span>) -> CompileDiagnostic {
    CompileDiagnostic {
        title: title.to_owned(),
        message: message.to_owned(),
        span,
    }
}

fn message_report(title: &str, message: &str) -> CompileDiagnostic {
    CompileDiagnostic {
        title: title.to_owned(),
        message: message.to_owned(),
        span: None,
    }
}

fn render_graphical_report(
    report: &CompileDiagnostic,
    source: DiagnosticSource<'_>,
    options: DiagnosticRenderOptions,
) -> String {
    let Some(span) = &report.span else {
        return render_summary_report(report, Some(source));
    };
    let source_name = source.name.unwrap_or("<source>").to_owned();
    let span = ariadne_span(span, source.text);
    let ariadne_report = Report::build(ReportKind::Error, (source_name.clone(), span.clone()))
        .with_config(
            Config::default()
                .with_color(options.use_color)
                .with_char_set(CharSet::Ascii),
        )
        .with_message(format!("{}: {}", report.title, report.message))
        .with_label(Label::new((source_name.clone(), span)).with_message(report.message.clone()))
        .finish();

    let mut buffer = Vec::new();
    if ariadne_report
        .write(sources([(source_name, source.text)]), &mut buffer)
        .is_err()
    {
        return render_summary_report(report, Some(source));
    }
    String::from_utf8(buffer).unwrap_or_else(|_| render_summary_report(report, Some(source)))
}

fn render_summary_report(
    report: &CompileDiagnostic,
    source: Option<DiagnosticSource<'_>>,
) -> String {
    let mut rendered = format!("compile error: {}: {}", report.title, report.message);
    let Some(span) = &report.span else {
        return rendered;
    };
    let Some(source) = source else {
        rendered.push_str(&format!(" at bytes {}..{}", span.start, span.end));
        return rendered;
    };
    rendered.push('\n');
    rendered.push_str(&render_plain_span(source, span, &report.message));
    rendered
}

fn render_plain_span(source: DiagnosticSource<'_>, span: &Range<usize>, message: &str) -> String {
    let source_text = source.text;
    let byte = span.start.min(source_text.len());
    let line_start = source_text[..byte]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line_end = source_text[byte..]
        .find('\n')
        .map(|offset| byte + offset)
        .unwrap_or(source_text.len());
    let line_number = source_text[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let column = source_text[line_start..byte].chars().count() + 1;
    let line = &source_text[line_start..line_end];
    let caret_width = source_text[byte..span.end.min(line_end)]
        .chars()
        .count()
        .max(1);
    let source_name = source.name.unwrap_or("<source>");
    let gutter_width = line_number.to_string().len();
    format!(
        "  --> {source_name}:{line_number}:{column}\n   |\n{line_number:>gutter_width$} | {line}\n   | {}{} {message}",
        " ".repeat(column.saturating_sub(1)),
        "^".repeat(caret_width),
    )
}

fn ariadne_span(span: &Range<usize>, source: &str) -> Range<usize> {
    let start = span.start.min(source.len());
    let end = span.end.min(source.len()).max(start);
    if start == end && start < source.len() {
        start..start + 1
    } else {
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiagnosticCode, NodeId};

    #[test]
    fn source_context_uses_ariadne_report_for_parse_errors() {
        let error = CompileError::ParseErrors {
            errors: vec![ParseError {
                message: "expected expression".to_owned(),
                span: 7..10,
            }],
        };
        let rendered = format_compile_error(
            &error,
            Some(DiagnosticSource::new(Some("sample.mica"), "return \nend")),
            DiagnosticRenderOptions::source_context(),
        );

        assert!(rendered.contains("Error: parse error: expected expression"));
        assert!(rendered.contains("sample.mica"));
        assert!(rendered.contains("expected expression"));
    }

    #[test]
    fn summary_preserves_parse_error_span_without_source() {
        let error = CompileError::ParseErrors {
            errors: vec![ParseError {
                message: "expected end".to_owned(),
                span: 12..12,
            }],
        };

        assert_eq!(
            format_compile_error(&error, None, DiagnosticRenderOptions::default()),
            "compile error: parse error: expected end at bytes 12..12"
        );
    }

    #[test]
    fn source_context_renders_value_kind_diagnostics_with_human_titles() {
        let source = "let count: integer = 1";
        let start = source.find("integer").unwrap();
        let error = CompileError::SemanticDiagnostic {
            diagnostic: Diagnostic {
                code: DiagnosticCode::UnknownValueKind,
                node: NodeId(0),
                span: start..start + "integer".len(),
                message: "unknown value kind `integer`".to_owned(),
            },
        };
        let rendered = format_compile_error(
            &error,
            Some(DiagnosticSource::new(Some("kinds.mica"), source)),
            DiagnosticRenderOptions::source_context(),
        );

        assert!(rendered.contains("Error: unknown value kind: unknown value kind `integer`"));
        assert!(rendered.contains("kinds.mica"));
        assert!(rendered.contains("let count: integer = 1"));
        assert!(!rendered.contains("UnknownValueKind"));
    }
}
