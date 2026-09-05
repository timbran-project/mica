// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_compiler::parse_ast;
use mica_runtime::{SourceRunner, TaskOutcome};
use std::fs;
use std::path::{Path, PathBuf};

fn markdown_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("book directory is readable") {
        let path = entry.expect("book directory entry is readable").path();
        if path.is_dir() {
            markdown_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
}

#[test]
fn book_examples_parse_and_marked_examples_execute() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mdbook/src");
    let mut files = Vec::new();
    markdown_files(&root, &mut files);
    files.sort();
    let mut failures = Vec::new();
    let mut parsed = 0;
    let mut executed = 0;

    for path in files {
        let markdown = fs::read_to_string(&path).expect("book chapter is readable");
        let mut block = None;
        let mut source = String::new();
        for (line, text) in markdown.lines().enumerate() {
            if let Some(info) = text.strip_prefix("```") {
                if let Some((start, mode)) = block.take() {
                    let location = format!("{}:{start}", path.display());
                    if mode == "mica,filein" {
                        executed += 1;
                        let mut runner = SourceRunner::new_empty();
                        match runner.run_filein(&source) {
                            Ok(reports) => {
                                for report in reports {
                                    if !matches!(report.outcome, TaskOutcome::Complete { .. }) {
                                        failures.push(format!("{location}: {}", report.render()));
                                    }
                                }
                            }
                            Err(error) => failures.push(format!("{location}: {error:?}")),
                        }
                        continue;
                    }
                    let ast = parse_ast(&source);
                    parsed += 1;
                    if !ast.errors.is_empty() {
                        failures.push(format!("{location}: {:?}", ast.errors));
                    } else if mode == "mica,eval" {
                        executed += 1;
                        let mut runner = SourceRunner::new_empty();
                        match runner.run_source(&source) {
                            Ok(report)
                                if matches!(report.outcome, TaskOutcome::Complete { .. }) => {}
                            Ok(report) => failures.push(format!("{location}: {}", report.render())),
                            Err(error) => failures.push(format!("{location}: {error:?}")),
                        }
                    }
                } else if matches!(info, "mica" | "mica,eval" | "mica,filein") {
                    block = Some((line + 1, info));
                    source.clear();
                }
            } else if block.is_some() {
                source.push_str(text);
                source.push('\n');
            }
        }
        if block.is_some() {
            failures.push(format!("{}: unclosed Mica code fence", path.display()));
        }
    }

    assert!(parsed > 0, "the book must contain Mica examples");
    assert!(executed > 0, "mark self-contained examples with mica,eval");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
