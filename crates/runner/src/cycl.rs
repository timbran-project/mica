// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{Cli, reject_actor};
use clap::Args;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::time::Instant;

const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Args)]
pub(crate) struct Options {
    /// KB5022 dump containing one (Mt formula truth direction strength) per line.
    file: PathBuf,
    /// Stop after this many valid assertions; zero means the whole file.
    #[arg(long, default_value_t = 0)]
    limit: usize,
}

#[derive(Debug, PartialEq)]
enum Form<'a> {
    Atom(&'a str),
    List {
        count: usize,
        first: Option<&'a str>,
        second_predicate: Option<&'a str>,
    },
    Other,
}

// The census validates every nested form but retains only the outer shape
// and predicate. Slices borrow the line; no AST is allocated per assertion.
struct Parser<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Parser<'a> {
    fn skip_space(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }

    fn form(&mut self, depth: usize) -> Option<Form<'a>> {
        if depth > 256 {
            return None;
        }
        self.skip_space();
        match *self.text.as_bytes().get(self.offset)? {
            b'(' => {
                self.offset += 1;
                let mut count = 0;
                let mut first = None;
                let mut second_predicate = None;
                loop {
                    self.skip_space();
                    if self.text.as_bytes().get(self.offset) == Some(&b')') {
                        self.offset += 1;
                        return Some(Form::List {
                            count,
                            first,
                            second_predicate,
                        });
                    }
                    match (count, self.form(depth + 1)?) {
                        (0, Form::Atom(atom)) => first = Some(atom),
                        (1, Form::List { first, .. }) => second_predicate = first,
                        _ => {}
                    }
                    count += 1;
                }
            }
            b'"' => {
                self.offset += 1;
                loop {
                    match *self.text.as_bytes().get(self.offset)? {
                        b'"' => {
                            self.offset += 1;
                            return Some(Form::Other);
                        }
                        b'\\' => {
                            self.offset += 1;
                            self.text.as_bytes().get(self.offset)?;
                        }
                        0 => return None,
                        _ => {}
                    }
                    self.offset += 1;
                }
            }
            b')' | 0 => None,
            _ => {
                let start = self.offset;
                while self
                    .text
                    .as_bytes()
                    .get(self.offset)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b"()\"\0".contains(b))
                {
                    self.offset += 1;
                }
                let atom = &self.text[start..self.offset];
                let bytes = atom.as_bytes();
                if bytes[0] == b'?' {
                    return (bytes.len() > 1).then_some(Form::Other);
                }
                let numeric = bytes[0].is_ascii_digit()
                    || matches!(bytes[0], b'+' | b'-')
                        && bytes.get(1).is_some_and(u8::is_ascii_digit);
                if numeric {
                    return atom
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .map(|_| Form::Other);
                }
                Some(Form::Atom(atom))
            }
        }
    }
}

fn predicate(text: &str) -> Option<&str> {
    let mut parser = Parser { text, offset: 0 };
    let Form::List {
        count: 5,
        second_predicate: Some(predicate),
        ..
    } = parser.form(0)?
    else {
        return None;
    };
    parser.skip_space();
    (parser.offset == text.len()).then_some(predicate)
}

#[derive(Default, Debug)]
struct Census {
    lines: usize,
    assertions: usize,
    failures: usize,
    predicates: BTreeMap<String, usize>,
}

fn census(reader: &mut impl BufRead, limit: usize) -> Result<Census, String> {
    let mut result = Census::default();
    let mut line = Vec::new();
    while limit == 0 || result.assertions < limit {
        line.clear();
        let count = reader
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        result.lines += 1;
        if count > MAX_LINE_BYTES {
            return Err(format!("CycL line {} exceeds 8 MiB", result.lines));
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            result.failures += 1;
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let Some(predicate) = predicate(text) else {
            result.failures += 1;
            continue;
        };
        result.assertions += 1;
        if let Some(count) = result.predicates.get_mut(predicate) {
            *count += 1;
        } else {
            result.predicates.insert(predicate.to_owned(), 1);
        }
    }
    Ok(result)
}

pub(crate) fn run(cli: &Cli, options: &Options) -> Result<(), String> {
    reject_actor(cli)?;
    let start = Instant::now();
    let file = File::open(&options.file).map_err(|e| format!("{}: {e}", options.file.display()))?;
    let result = census(&mut BufReader::new(file), options.limit)?;
    println!(
        "{}",
        serde_json::json!({"lines": result.lines, "assertions": result.assertions, "malformed_lines": result.failures, "predicates": result.predicates, "asserted": 0, "seconds": start.elapsed().as_secs_f64()})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_terms_strings_variables_and_numbers_preserve_predicate_counts() {
        let input = br##"(#$BaseKB (#$isa #$Fido #$Dog) :TRUE :FORWARD :MONOTONIC)
(#$PeopleMt (#$comment (#$NartFn ?X 2 -3.5 1e3) "say \"hello\" (with parentheses)") :TRUE :FORWARD :DEFAULT)

(#$BaseKB (#$isa #$Alice #$Person) :TRUE :FORWARD :MONOTONIC)
"##;
        let result = census(&mut &input[..], 0).unwrap();
        assert_eq!(result.assertions, 3);
        assert_eq!(result.failures, 0);
        assert_eq!(
            result.predicates,
            BTreeMap::from([("#$comment".to_owned(), 1), ("#$isa".to_owned(), 2)])
        );
        let limited = census(&mut &input[..], 2).unwrap();
        assert_eq!(limited.assertions, 2);
        assert_eq!(limited.lines, 2);
    }

    #[test]
    fn malformed_forms_and_trailing_input_are_rejected() {
        for input in [
            "(#$BaseKB (#$isa a b) :TRUE :FORWARD :MONOTONIC) extra",
            "(#$BaseKB (#$isa a b) :TRUE :FORWARD)",
            "(#$BaseKB (?predicate a b) :TRUE :FORWARD :MONOTONIC)",
            "(#$BaseKB (42 a b) :TRUE :FORWARD :MONOTONIC)",
            "(#$BaseKB (#$isa a b) :TRUE :FORWARD :MONOTONIC",
            "(#$BaseKB (#$comment a \"unfinished) :TRUE :FORWARD :MONOTONIC)",
            "; comment line",
        ] {
            assert_eq!(predicate(input), None, "{input}");
        }
        let nested = format!("{}x{}", "(".repeat(300), ")".repeat(300));
        assert_eq!(predicate(&nested), None);
        let bytes = b"\xff\ninvalid\n";
        let result = census(&mut &bytes[..], 0).unwrap();
        assert_eq!(result.failures, 2);
    }
}
