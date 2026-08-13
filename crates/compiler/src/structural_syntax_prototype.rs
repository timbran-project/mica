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

//! Stage 0 grammar probe for structural relation types and row bindings.
//!
//! This module is test-only so none of the proposed forms are accepted by the
//! executable source parser before their semantic model and lowering exist.

use crate::parse;

#[derive(Clone, Debug, Eq, PartialEq)]
enum PrototypeType {
    Name {
        name: String,
        arguments: Vec<Self>,
    },
    Symbol(String),
    Unit,
    EmptyRelation,
    Relation {
        alternatives: Vec<PrototypeRow>,
        cardinality: PrototypeCardinality,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrototypeRow(Vec<(String, PrototypeType)>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PrototypeCardinality {
    min: usize,
    max: Option<usize>,
}

impl PrototypeCardinality {
    const UNRESTRICTED: Self = Self { min: 0, max: None };
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PrototypeRowField {
    Punned(String),
    Renamed { column: String, binding: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrototypeBindingKind {
    Exact,
    Optional,
    Iterate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrototypeBinding {
    kind: PrototypeBindingKind,
    fields: Vec<PrototypeRowField>,
    expression: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Name(String),
    Symbol(String),
    Integer(usize),
    Lt,
    Gt,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Pipe,
    Arrow,
    Range,
    Star,
    End,
}

fn lex_type(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = source.char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        if character.is_whitespace() {
            continue;
        }
        let token = match character {
            '<' => Token::Lt,
            '>' => Token::Gt,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ',' => Token::Comma,
            '|' => Token::Pipe,
            '*' => Token::Star,
            '∈' => Token::Name("∈".to_owned()),
            '-' if chars.peek().is_some_and(|(_, next)| *next == '>') => {
                chars.next();
                Token::Arrow
            }
            '.' if chars.peek().is_some_and(|(_, next)| *next == '.') => {
                chars.next();
                Token::Range
            }
            ':' => {
                let start = chars.peek().map_or(source.len(), |(index, _)| *index);
                while chars
                    .peek()
                    .is_some_and(|(_, next)| is_name_continue(*next))
                {
                    chars.next();
                }
                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                if start == end {
                    return Err(format!("expected symbol name at byte {offset}"));
                }
                Token::Symbol(source[start..end].to_owned())
            }
            character if character.is_ascii_digit() => {
                let start = offset;
                while chars.peek().is_some_and(|(_, next)| next.is_ascii_digit()) {
                    chars.next();
                }
                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                Token::Integer(
                    source[start..end]
                        .parse()
                        .map_err(|_| format!("invalid integer at byte {start}"))?,
                )
            }
            character if is_name_start(character) => {
                let start = offset;
                while chars
                    .peek()
                    .is_some_and(|(_, next)| is_name_continue(*next))
                {
                    chars.next();
                }
                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                Token::Name(source[start..end].to_owned())
            }
            _ => {
                return Err(format!(
                    "unexpected character {character:?} at byte {offset}"
                ));
            }
        };
        tokens.push(token);
    }
    tokens.push(Token::End);
    Ok(tokens)
}

const fn is_name_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

const fn is_name_continue(character: char) -> bool {
    is_name_start(character) || character.is_ascii_digit() || character == '/'
}

struct TypeParser {
    tokens: Vec<Token>,
    position: usize,
}

impl TypeParser {
    fn parse(source: &str) -> Result<PrototypeType, String> {
        let mut parser = Self {
            tokens: lex_type(source)?,
            position: 0,
        };
        let ty = parser.parse_type()?;
        parser.expect(Token::End)?;
        Ok(ty)
    }

    fn parse_type(&mut self) -> Result<PrototypeType, String> {
        match self.current() {
            Token::Name(name) if name == "relation" => self.parse_relation_type(),
            Token::Name(_) => self.parse_named_type(),
            Token::Symbol(name) => {
                let name = name.clone();
                self.bump();
                Ok(PrototypeType::Symbol(name))
            }
            Token::LParen => {
                self.bump();
                self.expect(Token::RParen)?;
                Ok(PrototypeType::Unit)
            }
            Token::LBracket => {
                self.bump();
                self.expect(Token::RBracket)?;
                self.expect(Token::LBrace)?;
                self.expect(Token::RBrace)?;
                Ok(PrototypeType::EmptyRelation)
            }
            token => Err(format!("expected type, found {token:?}")),
        }
    }

    fn parse_named_type(&mut self) -> Result<PrototypeType, String> {
        let Token::Name(name) = self.current().clone() else {
            return Err("expected type name".to_owned());
        };
        self.bump();
        let mut arguments = Vec::new();
        if self.current() == &Token::Lt {
            self.bump();
            loop {
                arguments.push(self.parse_type()?);
                if self.current() != &Token::Comma {
                    break;
                }
                self.bump();
            }
            self.expect(Token::Gt)?;
        }
        Ok(PrototypeType::Name { name, arguments })
    }

    fn parse_relation_type(&mut self) -> Result<PrototypeType, String> {
        self.bump();
        self.expect(Token::Lt)?;
        let mut alternatives = vec![self.parse_row()?];
        while self.current() == &Token::Pipe {
            self.bump();
            alternatives.push(self.parse_row()?);
        }
        self.expect(Token::Gt)?;

        let cardinality = if self.consume_name("where") {
            self.expect_name("rows")?;
            if !self.consume_name("in") && !self.consume_name("∈") {
                return Err("expected 'in' or '∈' after 'where rows'".to_owned());
            }
            self.parse_cardinality()?
        } else {
            PrototypeCardinality::UNRESTRICTED
        };

        Ok(PrototypeType::Relation {
            alternatives,
            cardinality,
        })
    }

    fn parse_row(&mut self) -> Result<PrototypeRow, String> {
        self.expect(Token::LBrace)?;
        let mut columns = Vec::new();
        while self.current() != &Token::RBrace {
            let Token::Symbol(column) = self.current().clone() else {
                return Err(format!(
                    "expected row column symbol, found {:?}",
                    self.current()
                ));
            };
            self.bump();
            self.expect(Token::Arrow)?;
            columns.push((column, self.parse_type()?));
            if self.current() != &Token::Comma {
                break;
            }
            self.bump();
        }
        self.expect(Token::RBrace)?;
        Ok(PrototypeRow(columns))
    }

    fn parse_cardinality(&mut self) -> Result<PrototypeCardinality, String> {
        let Token::Integer(min) = self.current().clone() else {
            return Err("expected cardinality lower bound".to_owned());
        };
        self.bump();
        if self.current() != &Token::Range {
            return Ok(PrototypeCardinality {
                min,
                max: Some(min),
            });
        }
        self.bump();
        let max = match self.current().clone() {
            Token::Integer(max) => {
                self.bump();
                Some(max)
            }
            Token::Star => {
                self.bump();
                None
            }
            _ => return Err("expected cardinality upper bound or '*'".to_owned()),
        };
        if max.is_some_and(|max| min > max) {
            return Err("cardinality lower bound exceeds upper bound".to_owned());
        }
        Ok(PrototypeCardinality { min, max })
    }

    fn consume_name(&mut self, expected: &str) -> bool {
        if matches!(self.current(), Token::Name(name) if name == expected) {
            self.bump();
            return true;
        }
        false
    }

    fn expect_name(&mut self, expected: &str) -> Result<(), String> {
        if self.consume_name(expected) {
            return Ok(());
        }
        Err(format!("expected {expected:?}"))
    }

    fn expect(&mut self, expected: Token) -> Result<(), String> {
        if self.current() == &expected {
            self.bump();
            return Ok(());
        }
        Err(format!("expected {expected:?}, found {:?}", self.current()))
    }

    fn current(&self) -> &Token {
        &self.tokens[self.position]
    }

    fn bump(&mut self) {
        self.position += 1;
    }
}

fn parse_binding(source: &str) -> Result<PrototypeBinding, String> {
    let (kind, rest) = if let Some(rest) = source.strip_prefix("let exactly ") {
        (PrototypeBindingKind::Exact, rest)
    } else if let Some(rest) = source.strip_prefix("if let ") {
        (PrototypeBindingKind::Optional, rest)
    } else if let Some(rest) = source.strip_prefix("for ") {
        (PrototypeBindingKind::Iterate, rest)
    } else {
        return Err("expected exact, optional, or iterative binding".to_owned());
    };

    let delimiter = if kind == PrototypeBindingKind::Iterate {
        " in "
    } else {
        " = "
    };
    let Some((pattern, expression)) = rest.split_once(delimiter) else {
        return Err(format!("expected {delimiter:?} after row pattern"));
    };
    if expression.trim().is_empty() {
        return Err("expected binding expression".to_owned());
    }
    Ok(PrototypeBinding {
        kind,
        fields: parse_row_pattern(pattern.trim())?,
        expression: expression.trim().to_owned(),
    })
}

fn parse_row_pattern(source: &str) -> Result<Vec<PrototypeRowField>, String> {
    let Some(inner) = source
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    else {
        return Err("row pattern must be enclosed in braces".to_owned());
    };
    let mut fields = Vec::new();
    for field in inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some((column, binding)) = field.split_once("->") {
            let Some(column) = column.trim().strip_prefix(':') else {
                return Err("renamed row field must start with a symbol".to_owned());
            };
            fields.push(PrototypeRowField::Renamed {
                column: valid_name(column)?,
                binding: valid_name(binding.trim())?,
            });
        } else {
            fields.push(PrototypeRowField::Punned(valid_name(field)?));
        }
    }
    if fields.is_empty() {
        return Err("row pattern must bind at least one field".to_owned());
    }
    Ok(fields)
}

fn valid_name(source: &str) -> Result<String, String> {
    let mut characters = source.chars();
    if !characters.next().is_some_and(is_name_start) || !characters.all(is_name_continue) {
        return Err(format!("invalid binding name {source:?}"));
    }
    Ok(source.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototypes_relation_types_cardinality_and_row_alternatives() {
        let ty = TypeParser::parse(
            "relation<\n\
               {:case -> :ok, :value -> option<string>}\n\
               | {:case -> :error, :value -> error}\n\
             > where rows in 1",
        )
        .unwrap();

        let PrototypeType::Relation {
            alternatives,
            cardinality,
        } = ty
        else {
            panic!("expected relation prototype");
        };
        assert_eq!(alternatives.len(), 2);
        assert_eq!(
            cardinality,
            PrototypeCardinality {
                min: 1,
                max: Some(1)
            }
        );
    }

    #[test]
    fn prototypes_unit_empty_relation_and_nested_alias_arguments() {
        assert_eq!(TypeParser::parse("()").unwrap(), PrototypeType::Unit);
        assert_eq!(
            TypeParser::parse("[] {}").unwrap(),
            PrototypeType::EmptyRelation
        );
        assert_eq!(
            TypeParser::parse("option<result<string>>").unwrap(),
            PrototypeType::Name {
                name: "option".to_owned(),
                arguments: vec![PrototypeType::Name {
                    name: "result".to_owned(),
                    arguments: vec![PrototypeType::Name {
                        name: "string".to_owned(),
                        arguments: vec![],
                    }],
                }],
            }
        );
        assert!(matches!(
            TypeParser::parse("relation<{:value -> string}> where rows ∈ 0..1").unwrap(),
            PrototypeType::Relation {
                cardinality: PrototypeCardinality {
                    min: 0,
                    max: Some(1)
                },
                ..
            }
        ));
    }

    #[test]
    fn prototypes_exact_optional_and_iterative_row_bindings() {
        let exact = parse_binding("let exactly {label} = Label(#sensor, ?label)").unwrap();
        assert_eq!(exact.kind, PrototypeBindingKind::Exact);
        assert_eq!(
            exact.fields,
            vec![PrototypeRowField::Punned("label".to_owned())]
        );

        let optional =
            parse_binding("if let {:label -> sensor_label} = Label(#sensor, ?label)").unwrap();
        assert_eq!(optional.kind, PrototypeBindingKind::Optional);
        assert_eq!(
            optional.fields,
            vec![PrototypeRowField::Renamed {
                column: "label".to_owned(),
                binding: "sensor_label".to_owned(),
            }]
        );

        let iterate = parse_binding("for {work} in AssignedTo(?work, assignee)").unwrap();
        assert_eq!(iterate.kind, PrototypeBindingKind::Iterate);
    }

    #[test]
    fn rejects_invalid_cardinality_and_mismatched_binding_delimiters() {
        assert!(TypeParser::parse("relation<{:value -> string}> where rows in 2..1").is_err());
        assert!(parse_binding("let exactly {value} in Values(?value)").is_err());
        assert!(parse_binding("for {value} = Values(?value)").is_err());
    }

    #[test]
    fn current_parser_still_rejects_every_proposed_executable_surface() {
        for source in [
            "let value: relation<{:value -> string}> where rows in 0..1 = [] {}",
            "let exactly {label} = Label(#sensor, ?label)",
            "if let {next} = NextView(current, ?next)\nend",
            "for {work} in AssignedTo(?work, assignee)\nend",
            "return ()",
            "type option<T> = relation<{:value -> T}> where rows in 0..1",
        ] {
            assert!(
                !parse(source).errors.is_empty(),
                "proposed syntax unexpectedly accepted: {source}"
            );
        }
    }

    #[test]
    fn existing_colliding_forms_remain_accepted_by_the_current_parser() {
        for source in [
            "let opts = {:style -> :brief}",
            "return [:value] { [42] }",
            "return values[0..1]",
            "fn f(?value: string, @rest: list) -> relation => [] {}",
            "Location(#thing, ?room)",
            "verb inspect(value @ #relation)\nend",
        ] {
            assert_eq!(
                parse(source).errors,
                vec![],
                "existing syntax destabilized: {source}"
            );
        }
    }
}
