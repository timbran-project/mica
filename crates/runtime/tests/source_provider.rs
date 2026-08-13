#![cfg(feature = "source-provider")]

#[cfg(test)]
mod tests {
    use mica_runtime::{SourceRunner, TaskOutcome};
    use mica_source_provider::{
        SourceIndexRoot, build_source_index_file, build_source_index_file_for_roots,
        receive::{GitReceiveRecorder, ReceiveCommandLine},
        write_failed_source_index_file,
    };
    use mica_var::{Symbol, Value};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    fn load_source_relations(runner: &mut SourceRunner) {
        let root = env::current_dir().unwrap().display().to_string();
        load_source_relations_at(runner, &root);
    }

    fn load_source_relations_at(runner: &mut SourceRunner, root: &str) {
        runner
            .run_source(&format!(
                "make_identity(:repo)\n\
                 make_identity(:rev)\n\
                 make_relation(:source/RepositoryName, 2)\n\
                 make_relation(:source/RepositoryRoot, 2)\n\
                 make_relation(:source/RevisionOf, 2)\n\
                 make_relation(:source/RepositoryEntry, 6)\n\
                 make_relation(:source/FileText, 5)\n\
                 make_relation(:source/FileLines, 7)\n\
                 make_relation(:source/FileLineCount, 4)\n\
                 make_relation(:source/FileContentHash, 4)\n\
                 make_relation(:source/SyntaxLine, 8)\n\
                 make_relation(:source/SyntaxOutline, 10)\n\
                 make_relation(:source/SyntaxNodeAt, 11)\n\
                 make_relation(:source/DefinitionAt, 13)\n\
                 make_relation(:source/ReferencesOf, 10)\n\
                 make_relation(:source/SymbolSearch, 11)\n\
                 make_relation(:source/IndexedTextUnit, 9)\n\
                 make_relation(:source/TextSearch, 11)\n\
                 make_relation(:source/GitReceivedRefUpdate, 12)\n\
                 make_relation(:source/RefTarget, 3)\n\
                 make_relation(:source/GitRefTarget, 3)\n\
                 make_relation(:source/CommitExists, 2)\n\
                 make_relation(:source/SourceIndex, 1)\n\
                 make_relation(:source/IndexRepository, 2)\n\
                 make_relation(:source/IndexRevision, 2)\n\
                 make_relation(:source/IndexProvider, 2)\n\
                 make_relation(:source/IndexStatus, 2)\n\
                 make_relation(:source/IndexVersion, 2)\n\
                 make_relation(:source/IndexBuildError, 2)\n\
                 make_relation(:source/Repository, 1)\n\
                 make_relation(:source/Revision, 1)\n\
                 assert source/Repository(#repo)\n\
                 assert source/Revision(#rev)\n\
                 assert source/RepositoryName(#repo, \"default\")\n\
                 assert source/RepositoryRoot(#repo, {root:?})\n\
                 assert source/RevisionOf(#rev, #repo)"
            ))
            .unwrap();
    }

    fn with_source_provider_env<T>(f: impl FnOnce() -> T) -> T {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f()
    }

    fn new_source_runner() -> SourceRunner {
        with_source_provider_env(SourceRunner::new_empty)
    }

    fn rust_analyzer_available() -> bool {
        std::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .is_ok()
    }

    fn source_provider_root() -> PathBuf {
        env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("source-provider")
    }

    fn with_source_root_env<T>(source_root: &Path, f: impl FnOnce() -> T) -> T {
        with_source_provider_env(|| {
            let old_source_root = env::var_os("MICA_SOURCE_ROOT");
            unsafe {
                env::set_var("MICA_SOURCE_ROOT", source_root);
            }
            let result = f();
            unsafe {
                if let Some(old_source_root) = old_source_root {
                    env::set_var("MICA_SOURCE_ROOT", old_source_root);
                } else {
                    env::remove_var("MICA_SOURCE_ROOT");
                }
            }
            result
        })
    }

    fn with_source_index_env<T>(
        index_path: &Path,
        rust_analyzer: Option<&Path>,
        f: impl FnOnce() -> T,
    ) -> T {
        with_source_provider_env(|| {
            let old_index = env::var_os("MICA_SOURCE_INDEX");
            let old_rust_analyzer = env::var_os("MICA_RUST_ANALYZER");
            unsafe {
                env::set_var("MICA_SOURCE_INDEX", index_path);
                if let Some(rust_analyzer) = rust_analyzer {
                    env::set_var("MICA_RUST_ANALYZER", rust_analyzer);
                }
            }
            let result = f();
            unsafe {
                if let Some(old_index) = old_index {
                    env::set_var("MICA_SOURCE_INDEX", old_index);
                } else {
                    env::remove_var("MICA_SOURCE_INDEX");
                }
                if let Some(old_rust_analyzer) = old_rust_analyzer {
                    env::set_var("MICA_RUST_ANALYZER", old_rust_analyzer);
                } else {
                    env::remove_var("MICA_RUST_ANALYZER");
                }
            }
            result
        })
    }

    fn with_source_index_and_root_env<T>(
        index_path: &Path,
        source_root: &Path,
        rust_analyzer: Option<&Path>,
        f: impl FnOnce() -> T,
    ) -> T {
        with_source_provider_env(|| {
            let old_index = env::var_os("MICA_SOURCE_INDEX");
            let old_rust_analyzer = env::var_os("MICA_RUST_ANALYZER");
            let old_source_root = env::var_os("MICA_SOURCE_ROOT");
            unsafe {
                env::set_var("MICA_SOURCE_INDEX", index_path);
                env::set_var("MICA_SOURCE_ROOT", source_root);
                if let Some(rust_analyzer) = rust_analyzer {
                    env::set_var("MICA_RUST_ANALYZER", rust_analyzer);
                }
            }
            let result = f();
            unsafe {
                if let Some(old_index) = old_index {
                    env::set_var("MICA_SOURCE_INDEX", old_index);
                } else {
                    env::remove_var("MICA_SOURCE_INDEX");
                }
                if let Some(old_rust_analyzer) = old_rust_analyzer {
                    env::set_var("MICA_RUST_ANALYZER", old_rust_analyzer);
                } else {
                    env::remove_var("MICA_RUST_ANALYZER");
                }
                if let Some(old_source_root) = old_source_root {
                    env::set_var("MICA_SOURCE_ROOT", old_source_root);
                } else {
                    env::remove_var("MICA_SOURCE_ROOT");
                }
            }
            result
        })
    }

    fn temp_index_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "mica-source-index-{name}-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    #[test]
    fn source_provider_reads_file_text_from_allowed_root() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "let exactly {text, hash} = source/FileText(#repo, #rev, \"Cargo.toml\", ?text, ?hash)\n\
                 return string_contains(text, \"[package]\")",
            )
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(true)
        ));
    }

    #[test]
    fn source_provider_reads_line_windows() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "let exactly {lines, hash} = source/FileLines(#repo, #rev, \"Cargo.toml\", 1, 2, ?lines, ?hash)\n\
                 return lines",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|values| {
                assert_eq!(values.len(), 2);
                assert_eq!(values[0], Value::string("[package]"));
            })
            .expect("expected line list");
    }

    #[test]
    fn source_provider_counts_file_lines() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "let exactly {line_count} = source/FileLineCount(#repo, #rev, \"Cargo.toml\", ?line_count)\n\
                 return line_count",
            )
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. } if value.as_int().is_some_and(|count| count > 1)
        ));
    }

    #[test]
    fn source_provider_lists_repository_entries() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "for entry in source/RepositoryEntry(#repo, #rev, \"\", ?path, ?kind, ?name)\n\
                   if entry[:path] == \"Cargo.toml\"\n\
                     return entry[:kind]\n\
                   end\n\
                 end\n\
                 return none",
            )
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::string("file")
        ));
    }

    #[test]
    fn source_provider_rejects_escaping_paths() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        // Scan KernelErrors are now raised as Mica error values so
        // try/catch can handle them; the task aborts with an error value.
        let report = runner
            .run_source("return source/FileContentHash(#repo, #rev, \"../Cargo.toml\", ?hash)")
            .expect("task should abort, not fail");
        let TaskOutcome::Aborted { error, .. } = report.outcome else {
            panic!("expected aborted task, got {:?}", report.outcome);
        };
        assert!(format!("{error}").contains("parent components"));
    }

    #[test]
    fn source_provider_requires_bound_path() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source("return source/FileText(#repo, #rev, ?path, ?text, ?hash)")
            .expect("task should abort, not fail");
        let TaskOutcome::Aborted { error, .. } = report.outcome else {
            panic!("expected aborted task, got {:?}", report.outcome);
        };
        assert!(format!("{error}").contains("MissingRequiredBindings"));
    }

    #[test]
    fn syntax_provider_requires_constrained_queries() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "return source/SyntaxOutline(#repo, #rev, ?path, ?node, ?kind, ?name, ?start_line, ?end_line, ?start_byte, ?end_byte)",
            )
            .expect("task should abort, not fail");
        let TaskOutcome::Aborted { error, .. } = report.outcome else {
            panic!("expected aborted task, got {:?}", report.outcome);
        };
        assert!(format!("{error}").contains("MissingRequiredBindings"));

        let report = runner
            .run_source(
                "return source/SyntaxNodeAt(#repo, #rev, \"src/lib.rs\", ?offset, ?node, ?kind, ?name, ?start_line, ?end_line, ?start_byte, ?end_byte)",
            )
            .expect("task should abort, not fail");
        let TaskOutcome::Aborted { error, .. } = report.outcome else {
            panic!("expected aborted task, got {:?}", report.outcome);
        };
        assert!(format!("{error}").contains("MissingRequiredBindings"));
    }

    #[test]
    fn source_provider_relations_are_read_only() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let repo = runner.named_identity(Symbol::intern("repo")).unwrap();
        let error = runner
            .run_source("assert source/FileContentHash(#repo, #rev, \"Cargo.toml\", \"x\")")
            .unwrap_err();
        assert!(format!("{error:?}").contains("ReadOnlyRelation"));
        assert!(
            format!("{error:?}").contains(&format!("{repo:?}"))
                || format!("{error:?}").contains("ReadOnlyRelation")
        );
    }

    #[test]
    fn source_provider_exposes_git_received_ref_updates() {
        let tmp = unique_test_dir("mica-git-receive-");
        let remote = tmp.join("remote.git");
        let work = tmp.join("work");
        git(["init", "--bare", path(&remote)]);
        git(["clone", path(&remote), path(&work)]);
        git_in(&work, ["config", "user.name", "Mica Tester"]);
        git_in(&work, ["config", "user.email", "mica@example.test"]);
        git_in(&work, ["checkout", "-b", "main"]);
        fs::write(work.join("README.md"), "base\n").expect("base file should be written");
        git_in(&work, ["add", "README.md"]);
        git_in(&work, ["commit", "-m", "Initial base"]);
        git_in(&work, ["push", "origin", "HEAD:refs/heads/main"]);
        git_in(&work, ["checkout", "-b", "receive"]);
        fs::write(work.join("README.md"), "base\nchange\n").expect("change should be written");
        git_in(
            &work,
            [
                "commit",
                "-am",
                "Received change\n\nChange-Id: I1234567890abcdef1234567890abcdef12345678",
            ],
        );
        let new_id = git_output_in(&work, ["rev-parse", "HEAD"]);
        git_in(&work, ["push", "origin", "HEAD:refs/for/main"]);
        GitReceiveRecorder::new(&remote)
            .receive_command(&ReceiveCommandLine {
                old_id: "0000000000000000000000000000000000000000".to_string(),
                new_id: new_id.trim().to_string(),
                ref_name: "refs/for/main".to_string(),
            })
            .expect("receive command should be recorded");

        with_source_root_env(&tmp, || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &tmp.display().to_string());
            let git_dir = remote.canonicalize().unwrap().display().to_string();
            let report = runner
                .run_source(&format!(
                    "let exactly {{update_id, target_ref, ref_name, commit_id, first_parent_id, change_id, subject, author_name, author_email, author_time, received_at}} = source/GitReceivedRefUpdate({git_dir:?}, ?update_id, ?target_ref, ?ref_name, ?commit_id, ?first_parent_id, ?change_id, ?subject, ?author_name, ?author_email, ?author_time, ?received_at)\n\
                     return {{:target_ref -> target_ref, :ref_name -> ref_name, :first_parent_id -> first_parent_id, :change_id -> change_id, :subject -> subject, :author_email -> author_email}}"
                ))
                .expect("received ref update query should run");

            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            value
                .with_map(|entries| {
                    assert_eq!(
                        map_get(entries, "target_ref"),
                        Some(&Value::string("refs/heads/main"))
                    );
                    assert_eq!(
                        map_get(entries, "ref_name"),
                        Some(&Value::string("refs/for/main"))
                    );
                    let first_parent = map_get(entries, "first_parent_id")
                        .and_then(|value| value.with_str(str::to_string))
                        .unwrap_or_default();
                    assert!(!first_parent.is_empty());
                    assert_eq!(
                        map_get(entries, "change_id"),
                        Some(&Value::string("I1234567890abcdef1234567890abcdef12345678"))
                    );
                    assert_eq!(
                        map_get(entries, "subject"),
                        Some(&Value::string("Received change"))
                    );
                    assert_eq!(
                        map_get(entries, "author_email"),
                        Some(&Value::string("mica@example.test"))
                    );
                })
                .expect("result should be a map");
        });
        fs::remove_dir_all(&tmp).expect("temporary git receive dir should remove");
    }

    #[test]
    fn source_provider_resolves_git_ref_targets() {
        let tmp = unique_test_dir("mica-git-ref-target-");
        let remote = tmp.join("remote.git");
        let work = tmp.join("work");
        git(["init", "--bare", path(&remote)]);
        git(["clone", path(&remote), path(&work)]);
        git_in(&work, ["config", "user.name", "Mica Tester"]);
        git_in(&work, ["config", "user.email", "mica@example.test"]);
        git_in(&work, ["branch", "-M", "main"]);
        fs::write(work.join("README.md"), "base\n").expect("base file should be written");
        git_in(&work, ["add", "README.md"]);
        git_in(&work, ["commit", "-m", "Initial base"]);
        git_in(&work, ["push", "origin", "HEAD:refs/heads/main"]);
        let head = git_output_in(&work, ["rev-parse", "HEAD"]);

        with_source_root_env(&tmp, || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &work.display().to_string());
            let report = runner
                .run_source("let exactly {commit} = source/RefTarget(#repo, \"refs/heads/main\", ?commit)\nreturn commit")
                .expect("ref target query should run");

            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            assert_eq!(value, Value::string(head.trim()));

            let git_dir = remote.canonicalize().unwrap().display().to_string();
            let remote_report = runner
                .run_source(&format!(
                    "let exactly {{commit}} = source/GitRefTarget({git_dir:?}, \"refs/heads/main\", ?commit)\nreturn commit"
                ))
                .expect("git ref target query should run");
            let TaskOutcome::Complete { value, .. } = remote_report.outcome else {
                panic!("expected complete outcome, got {:?}", remote_report.outcome);
            };
            assert_eq!(value, Value::string(head.trim()));

            let missing = runner
                .run_source("if let {commit} = source/RefTarget(#repo, \"refs/heads/missing\", ?commit)\n  return some(commit)\nend\nreturn none")
                .expect("missing ref query should run");
            assert!(matches!(
                missing.outcome,
                TaskOutcome::Complete { value, .. } if value == Value::option_none()
            ));
        });
        fs::remove_dir_all(&tmp).expect("temporary git ref dir should remove");
    }

    #[test]
    fn source_provider_commit_exists_finds_review_fixture_history() {
        let mica_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

        with_source_root_env(&mica_root, || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &mica_root.display().to_string());
            for commit in [
                "696dbc78cc394c7882c3199d2bac62b38a2ed2bd",
                "fea67143608204247917088611d51f1f828f4cc3",
            ] {
                let report = runner
                    .run_source(&format!("return source/CommitExists(#repo, {commit:?})"))
                    .expect("commit exists query should run");

                assert!(
                    matches!(&report.outcome, TaskOutcome::Complete { value, .. } if *value == Value::bool(true)),
                    "expected CommitExists to find {commit}, got {:?}",
                    report.outcome
                );
            }
        });
    }

    #[test]
    fn source_provider_returns_rust_syntax_outline() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "for item in source/SyntaxOutline(#repo, #rev, \"src/lib.rs\", ?node, ?kind, ?name, ?start_line, ?end_line, ?start_byte, ?end_byte)\n\
                   return item[:kind] != none\n\
                 end\n\
                 return false",
            )
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(true)
        ));
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let mut path = env::current_dir().unwrap();
        path.push(format!(
            "{prefix}{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("test directory should be created");
        path
    }

    fn git<const N: usize>(args: [&str; N]) {
        run_git(args, None);
    }

    fn git_in<const N: usize>(work_dir: &Path, args: [&str; N]) {
        run_git(args, Some(work_dir));
    }

    fn git_output_in<const N: usize>(work_dir: &Path, args: [&str; N]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(work_dir)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output should be utf-8")
    }

    fn run_git<const N: usize>(args: [&str; N], work_dir: Option<&Path>) {
        let mut command = std::process::Command::new("git");
        if let Some(work_dir) = work_dir {
            command.current_dir(work_dir);
        }
        let output = command.args(args).output().expect("git should run");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn path(path: &Path) -> &str {
        path.to_str().expect("test path should be utf-8")
    }

    fn map_get<'a>(entries: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
        let key = Value::symbol(Symbol::intern(key));
        entries
            .iter()
            .find_map(|(candidate, value)| (candidate == &key).then_some(value))
    }

    #[test]
    fn source_provider_returns_line_level_syntax_segments() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "let exactly {segments, hash} = source/SyntaxLine(#repo, #rev, \"src/lib.rs\", 1, 8, 1, ?segments, ?hash)\n\
                 return segments",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|segments| {
                assert!(!segments.is_empty());
                assert!(segments.iter().any(|segment| {
                    segment
                        .with_map(|entries| {
                            entries
                                .iter()
                                .any(|(key, _)| key == &Value::symbol(Symbol::intern("kind")))
                        })
                        .unwrap_or(false)
                }));
            })
            .expect("expected syntax segment list");
    }

    #[test]
    fn source_provider_reports_nearest_syntax_node() {
        let mut runner = new_source_runner();
        load_source_relations(&mut runner);

        let report = runner
            .run_source(
                "let exactly {node, kind, name, start_line, end_line, start_byte, end_byte} = source/SyntaxNodeAt(#repo, #rev, \"src/lib.rs\", 2500, ?node, ?kind, ?name, ?start_line, ?end_line, ?start_byte, ?end_byte)\n\
                 return node != none",
            )
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(true)
        ));
    }

    #[test]
    fn rust_analyzer_provider_returns_definition_and_references() {
        if !rust_analyzer_available() {
            return;
        }

        with_source_provider_env(|| {
            let mut runner = SourceRunner::new_empty();
            load_source_relations(&mut runner);
            let source = fs::read_to_string("src/retrieval.rs").unwrap();
            let offset = source.find("parse_vector(").unwrap();

            let report = runner
            .run_source(&format!(
                "for def in source/DefinitionAt(#repo, #rev, \"src/retrieval.rs\", {offset}, ?symbol, ?name, ?kind, ?target_path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider)\n\
                   if def[:target_path] == \"src/retrieval.rs\"\n\
                     return some(def)\n\
                   end\n\
                 end\n\
                 return none"
            ))
            .unwrap();
            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            let Some(value) = value.option_payload().expect("expected definition option") else {
                return;
            };
            let symbol = value
                .with_map(|entries| {
                    entries
                        .iter()
                        .find(|(key, _)| key == &Value::symbol(Symbol::intern("symbol")))
                        .map(|(_, value)| value.clone())
                })
                .flatten()
                .and_then(|value| value.with_str(str::to_owned))
                .expect("expected definition symbol");

            let report = runner
            .run_source(&format!(
                "for reference in source/ReferencesOf(#repo, #rev, {symbol:?}, ?path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider, ?name)\n\
                   if reference[:path] == \"src/retrieval.rs\"\n\
                     return some(reference[:provider])\n\
                   end\n\
                 end\n\
                 return none"
            ))
            .unwrap();
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. }
                    if value.option_payload().flatten().and_then(|value| value.with_str(|provider| provider.contains("rust-analyzer"))).unwrap_or(false)
            ));
        });
    }

    #[test]
    fn persistent_source_index_answers_navigation_without_rust_analyzer() {
        let index_path = temp_index_path("navigation");
        let root = source_provider_root();
        build_source_index_file(&root, &index_path).unwrap();
        with_source_index_and_root_env(&index_path, &root, Some(Path::new("/bin/false")), || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &root.display().to_string());
            let source = fs::read_to_string(root.join("src/relations.rs")).unwrap();
            let offset = source.find("LocalSourceProvider::from_env").unwrap();

            let report = runner
                .run_source(&format!(
                    "for def in source/DefinitionAt(#repo, #rev, \"src/relations.rs\", {offset}, ?symbol, ?name, ?kind, ?target_path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider)\n\
                       if def[:name] == \"LocalSourceProvider\"\n\
                         return [def[:symbol], def[:target_path], def[:start_line], def[:provider]]\n\
                       end\n\
                     end\n\
                     return none"
                ))
                .unwrap();
            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            let symbol = value
                .with_list(|values| {
                    assert!(
                        values[0]
                            .with_str(|symbol| symbol.starts_with("idx:default:src/relations.rs:"))
                            .unwrap_or(false)
                    );
                    assert_eq!(values[1], Value::string("src/relations.rs"));
                    assert!(values[2].as_int().is_some_and(|line| line > 0));
                    assert!(
                        values[3]
                            .with_str(|provider| provider.contains("mica-source-index"))
                            .unwrap_or(false)
                    );
                    values[0].clone()
                })
                .expect("expected indexed definition tuple");

            let report = runner
                .run_source(&format!(
                    "let symbol = {symbol:?}\n\
                     let count = 0\n\
                     for reference in source/ReferencesOf(#repo, #rev, symbol, ?path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider, ?name)\n\
                       if reference[:name] == \"LocalSourceProvider\" && reference[:provider] == \"mica-source-index/static-analysis 4\"\n\
                         count = count + 1\n\
                       end\n\
                     end\n\
                     return count"
                ))
                .unwrap();
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. } if value.as_int().is_some_and(|count| count > 1)
            ));

            let report = runner
                .run_source(
                    "for result in source/SymbolSearch(#repo, #rev, \"LocalSource\", 5, ?symbol, ?name, ?kind, ?path, ?start_line, ?end_line, ?provider)\n\
                       if result[:name] == \"LocalSourceProvider\"\n\
                         return result[:provider]\n\
                       end\n\
                     end\n\
                     return none",
                )
                .unwrap();
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. } if value.with_str(|provider| provider.contains("mica-source-index")).unwrap_or(false)
            ));
        });
        let _ = fs::remove_file(index_path);
    }

    #[test]
    fn persistent_source_index_filters_symbols_by_repository_name() {
        let fixture_root = env::current_dir().unwrap().join("target").join(format!(
            "source-index-multiroot-fixture-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let alpha_root = fixture_root.join("alpha");
        let beta_root = fixture_root.join("beta");
        fs::create_dir_all(alpha_root.join("src")).unwrap();
        fs::create_dir_all(beta_root.join("src")).unwrap();
        fs::write(alpha_root.join("src/lib.rs"), "pub fn only_alpha() {}\n").unwrap();
        fs::write(beta_root.join("src/lib.rs"), "pub fn only_beta() {}\n").unwrap();

        let index_path = temp_index_path("multiroot");
        build_source_index_file_for_roots(
            &[
                SourceIndexRoot {
                    name: "alpha".to_owned(),
                    root: alpha_root.clone(),
                },
                SourceIndexRoot {
                    name: "beta".to_owned(),
                    root: beta_root.clone(),
                },
            ],
            &index_path,
        )
        .unwrap();

        with_source_index_env(&index_path, Some(Path::new("/bin/false")), || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &alpha_root.display().to_string());
            runner
                .run_source(&format!(
                    "make_identity(:repo_beta)\n\
                     make_identity(:rev_beta)\n\
                     retract source/RepositoryName(#repo, _)\n\
                     assert source/RepositoryName(#repo, \"alpha\")\n\
                     assert source/Repository(#repo_beta)\n\
                     assert source/Revision(#rev_beta)\n\
                     assert source/RepositoryName(#repo_beta, \"beta\")\n\
                     assert source/RepositoryRoot(#repo_beta, {beta_root:?})\n\
                     assert source/RevisionOf(#rev_beta, #repo_beta)",
                    beta_root = beta_root.display().to_string(),
                ))
                .unwrap();

            let report = runner
                .run_source(
                    "let alpha = []\n\
                     for result in source/SymbolSearch(#repo, #rev, \"only_\", 10, ?symbol, ?name, ?kind, ?path, ?start_line, ?end_line, ?provider)\n\
                       alpha = [@alpha, result[:name]]\n\
                     end\n\
                     let beta = []\n\
                     for result in source/SymbolSearch(#repo_beta, #rev_beta, \"only_\", 10, ?symbol, ?name, ?kind, ?path, ?start_line, ?end_line, ?provider)\n\
                       beta = [@beta, result[:name]]\n\
                     end\n\
                     return [alpha, beta]",
                )
                .unwrap();
            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            value
                .with_list(|values| {
                    assert_eq!(
                        values[0].with_list(|names| names.to_vec()),
                        Some(vec![Value::string("only_alpha")])
                    );
                    assert_eq!(
                        values[1].with_list(|names| names.to_vec()),
                        Some(vec![Value::string("only_beta")])
                    );
                })
                .expect("expected repository-filtered symbol lists");
        });

        let _ = fs::remove_file(index_path);
        let _ = fs::remove_dir_all(fixture_root);
    }

    #[test]
    fn persistent_source_index_exposes_chunked_text_units() {
        let root_path = env::current_dir().unwrap().join("target").join(format!(
            "source-index-text-unit-fixture-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(
            root_path.join("src/lib.rs"),
            "pub fn actual_corpus_search_target() {\n    let phrase = \"actual corpus retrieval phrase\";\n}\n",
        )
        .unwrap();

        let index_path = temp_index_path("text-unit");
        build_source_index_file(&root_path, &index_path).unwrap();
        with_source_index_and_root_env(
            &index_path,
            &root_path,
            Some(Path::new("/bin/false")),
            || {
                let mut runner = SourceRunner::new_empty();
                load_source_relations_at(&mut runner, &root_path.display().to_string());
                let report = runner
                    .run_source(
                        "for unit in source/IndexedTextUnit(?unit, ?ordinal, ?kind, ?title, ?path, ?start_line, ?end_line, ?model, ?text)\n\
                           if unit[:path] == \"src/lib.rs\"\n\
                             return [unit[:kind], unit[:title], unit[:start_line], unit[:end_line], unit[:model], string_contains(unit[:text], \"actual corpus retrieval phrase\")]\n\
                           end\n\
                         end\n\
                         return none",
                    )
                    .unwrap();
                let TaskOutcome::Complete { value, .. } = report.outcome else {
                    panic!("expected complete outcome, got {:?}", report.outcome);
                };
                value
                    .with_list(|values| {
                        assert_eq!(values[0], Value::string("rust"));
                        assert_eq!(values[1], Value::string("src/lib.rs:1-3"));
                        assert_eq!(values[2].as_int(), Some(1));
                        assert_eq!(values[3].as_int(), Some(3));
                        assert_eq!(values[4], Value::string("source-workspace"));
                        assert_eq!(values[5], Value::bool(true));
                    })
                    .expect("expected indexed text unit tuple");
            },
        );
        let _ = fs::remove_file(index_path);
        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn persistent_source_index_skips_generated_book_output() {
        let root_path = env::current_dir().unwrap().join("target").join(format!(
            "source-index-generated-book-fixture-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(root_path.join("mdbook/src")).unwrap();
        fs::create_dir_all(root_path.join("mdbook/book")).unwrap();
        fs::write(
            root_path.join("mdbook/src/language.md"),
            "# Language\n\nAuthored btree retrieval notes.\n",
        )
        .unwrap();
        fs::write(
            root_path.join("mdbook/book/searchindex.js"),
            "window.search = { docs: ['generated btree search index'] };\n",
        )
        .unwrap();

        let index_path = temp_index_path("generated-book");
        build_source_index_file(&root_path, &index_path).unwrap();
        with_source_index_and_root_env(
            &index_path,
            &root_path,
            Some(Path::new("/bin/false")),
            || {
                let mut runner = SourceRunner::new_empty();
                load_source_relations_at(&mut runner, &root_path.display().to_string());
                let report = runner
                    .run_source(
                        "let has_authored_doc = false\n\
                         let has_generated_book = false\n\
                         for unit in source/IndexedTextUnit(?unit, ?ordinal, ?kind, ?title, ?path, ?start_line, ?end_line, ?model, ?text)\n\
                           if unit[:path] == \"mdbook/src/language.md\"\n\
                             has_authored_doc = true\n\
                           end\n\
                           if unit[:path] == \"mdbook/book/searchindex.js\"\n\
                             has_generated_book = true\n\
                           end\n\
                         end\n\
                         return [has_authored_doc, has_generated_book]",
                    )
                    .unwrap();
                let TaskOutcome::Complete { value, .. } = report.outcome else {
                    panic!("expected complete outcome, got {:?}", report.outcome);
                };
                value
                    .with_list(|values| {
                        assert_eq!(values[0], Value::bool(true));
                        assert_eq!(values[1], Value::bool(false));
                    })
                    .expect("expected generated book exclusion tuple");
            },
        );
        let _ = fs::remove_file(index_path);
        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn persistent_source_index_text_search_ranks_and_filters_source_units() {
        let root_path = env::current_dir().unwrap().join("target").join(format!(
            "source-index-text-search-fixture-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(root_path.join("crates/relation-kernel/src")).unwrap();
        fs::create_dir_all(root_path.join("crates/relation-kernel/tests")).unwrap();
        fs::create_dir_all(root_path.join("sketches")).unwrap();
        fs::create_dir_all(root_path.join("mdbook/book")).unwrap();
        fs::write(
            root_path.join("crates/relation-kernel/src/btree.rs"),
            "pub struct BTreeIndex;\nimpl BTreeIndex {\n    pub fn search_btree_node(&self) {}\n}\n",
        )
        .unwrap();
        fs::write(
            root_path.join("crates/relation-kernel/tests/btree_tests.rs"),
            "#[test]\nfn btree_insert_visible_in_tests() {}\n",
        )
        .unwrap();
        fs::write(
            root_path.join("sketches/btree-notes.md"),
            "# BTree notes\n\nSketches about btree relation indexing.\n",
        )
        .unwrap();
        fs::write(
            root_path.join("mdbook/book/searchindex.js"),
            "window.search = { docs: ['generated btree search index'] };\n",
        )
        .unwrap();

        let index_path = temp_index_path("text-search");
        build_source_index_file(&root_path, &index_path).unwrap();
        with_source_index_and_root_env(
            &index_path,
            &root_path,
            Some(Path::new("/bin/false")),
            || {
                let mut runner = SourceRunner::new_empty();
                load_source_relations_at(&mut runner, &root_path.display().to_string());
                let report = runner
                    .run_source(
                        "let first_path = none\n\
                         let first_line = 0\n\
                         let first_snippet = none\n\
                         let saw_generated_book = false\n\
                         for result in source/TextSearch(\"btree\", 8, \"all\", ?unit, ?score, ?path, ?start_line, ?end_line, ?kind, ?title, ?snippet)\n\
                           if first_path == none\n\
                             first_path = result[:path]\n\
                             first_line = result[:start_line]\n\
                             first_snippet = result[:snippet]\n\
                           end\n\
                           if result[:path] == \"mdbook/book/searchindex.js\"\n\
                             saw_generated_book = true\n\
                           end\n\
                         end\n\
                         let tests_only = true\n\
                         let tests_count = 0\n\
                         for result in source/TextSearch(\"btree\", 8, \"tests\", ?unit, ?score, ?path, ?start_line, ?end_line, ?kind, ?title, ?snippet)\n\
                           tests_count = tests_count + 1\n\
                           if string_contains(result[:path], \"/tests/\") == false\n\
                             tests_only = false\n\
                           end\n\
                         end\n\
                         return [first_path, first_line, string_contains(first_snippet, \"btree\") || string_contains(first_snippet, \"BTree\"), saw_generated_book, tests_count, tests_only]",
                    )
                    .unwrap();
                let TaskOutcome::Complete { value, .. } = report.outcome else {
                    panic!("expected complete outcome, got {:?}", report.outcome);
                };
                value
                    .with_list(|values| {
                        assert_eq!(
                            values[0],
                            Value::string("crates/relation-kernel/src/btree.rs")
                        );
                        assert_eq!(values[1].as_int(), Some(1));
                        assert_eq!(values[2], Value::bool(true));
                        assert_eq!(values[3], Value::bool(false));
                        assert_eq!(values[4].as_int(), Some(1));
                        assert_eq!(values[5], Value::bool(true));
                    })
                    .expect("expected text search tuple");
            },
        );
        let _ = fs::remove_file(index_path);
        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn persistent_source_index_keeps_mica_namespace_symbols_whole() {
        let root_path = env::current_dir().unwrap().join("target").join(format!(
            "source-index-mica-symbol-fixture-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root_path).unwrap();
        let source_path = "session.mica";
        let source = "make_relation(:session/CanAssumeActor, 2)\n\
                      assert session/CanAssumeActor(#web, #alice)\n";
        fs::write(root_path.join(source_path), source).unwrap();

        let index_path = temp_index_path("mica-symbol");
        build_source_index_file(&root_path, &index_path).unwrap();
        with_source_index_env(&index_path, Some(Path::new("/bin/false")), || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &root_path.display().to_string());
            let offset =
                source.find("session/CanAssumeActor(#web").unwrap() + "session/CanAssume".len();

            let report = runner
                .run_source(&format!(
                    "for def in source/DefinitionAt(#repo, #rev, {source_path:?}, {offset}, ?symbol, ?name, ?kind, ?target_path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider)\n\
                       return [def[:name], def[:kind], def[:target_path], def[:start_line], def[:provider]]\n\
                     end\n\
                     return none",
                    source_path = source_path,
                ))
                .unwrap();
            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            value
                .with_list(|values| {
                    assert_eq!(values[0], Value::string("session/CanAssumeActor"));
                    assert_eq!(values[1], Value::string("relation"));
                    assert_eq!(values[2], Value::string(source_path));
                    assert_eq!(values[3].as_int(), Some(1));
                    assert_eq!(
                        values[4],
                        Value::string("mica-source-index/static-analysis 4")
                    );
                })
                .expect("expected Mica definition tuple");
        });
        let _ = fs::remove_file(index_path);
        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn persistent_source_index_status_reports_build_failures() {
        let index_path = temp_index_path("failed");
        write_failed_source_index_file(Path::new("."), &index_path, "synthetic failure").unwrap();
        with_source_index_env(&index_path, None, || {
            let mut runner = SourceRunner::new_empty();
            load_source_relations(&mut runner);
            let report = runner
                .run_source(
                    "for index in source/SourceIndex(?index)\n\
                       let exactly {:status -> status} = source/IndexStatus(index[:index], ?status)\n\
                       let exactly {:error -> error} = source/IndexBuildError(index[:index], ?error)\n\
                       return [status, error]\n\
                     end\n\
                     return none",
                )
                .unwrap();
            let TaskOutcome::Complete { value, .. } = report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            value
                .with_list(|values| {
                    assert_eq!(values[0], Value::string("failed"));
                    assert_eq!(values[1], Value::string("synthetic failure"));
                })
                .expect("expected index status tuple");
        });
        let _ = fs::remove_file(index_path);
    }

    #[test]
    fn rust_analyzer_definition_accepts_identifier_start_offset() {
        if !rust_analyzer_available() {
            return;
        }

        with_source_provider_env(|| {
            let mut runner = SourceRunner::new_empty();
            load_source_relations(&mut runner);
            let source = fs::read_to_string("src/retrieval.rs").unwrap();
            let offset = source.find("fn parse_vector").unwrap() + "fn ".len();

            let report = runner
            .run_source(&format!(
                "for def in source/DefinitionAt(#repo, #rev, \"src/retrieval.rs\", {offset}, ?symbol, ?name, ?kind, ?target_path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider)\n\
                   if def[:target_path] == \"src/retrieval.rs\"\n\
                     return some(def[:provider])\n\
                   end\n\
                 end\n\
                 return none"
            ))
            .unwrap();
            let TaskOutcome::Complete { value, .. } = &report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            if value == &Value::option_none() {
                return;
            }
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. }
                    if value.option_payload().flatten().and_then(|value| value.with_str(|provider| provider.contains("rust-analyzer"))).unwrap_or(false)
            ));
        });
    }

    #[test]
    fn rust_analyzer_module_definition_can_link_to_target_file() {
        if !rust_analyzer_available() {
            return;
        }

        with_source_provider_env(|| {
            let mut runner = SourceRunner::new_empty();
            load_source_relations(&mut runner);
            let source = fs::read_to_string("src/lib.rs").unwrap();
            let offset = source.find("mod retrieval").unwrap() + "mod ".len();

            let report = runner
            .run_source(&format!(
                "for def in source/DefinitionAt(#repo, #rev, \"src/lib.rs\", {offset}, ?symbol, ?name, ?kind, ?target_path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider)\n\
                   if def[:target_path] == \"src/retrieval.rs\"\n\
                     return some(def[:start_line])\n\
                   end\n\
                 end\n\
                 return none"
            ))
            .unwrap();
            let TaskOutcome::Complete { value, .. } = &report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            if value == &Value::option_none() {
                return;
            }
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. }
                    if value.option_payload().flatten().and_then(|value| value.as_int()) == Some(1)
            ));
        });
    }

    #[test]
    fn rust_analyzer_module_definition_uses_workspace_relative_path() {
        if !rust_analyzer_available() {
            return;
        }

        let current_dir = env::current_dir().unwrap();
        let workspace = current_dir.parent().and_then(Path::parent).unwrap();
        with_source_root_env(workspace, || {
            let workspace = workspace.display().to_string();
            let mut runner = SourceRunner::new_empty();
            load_source_relations_at(&mut runner, &workspace);
            let source = fs::read_to_string("src/lib.rs").unwrap();
            let offset = source.find("mod retrieval").unwrap() + "mod ".len();

            let report = runner
            .run_source(&format!(
                "for def in source/DefinitionAt(#repo, #rev, \"crates/runtime/src/lib.rs\", {offset}, ?symbol, ?name, ?kind, ?target_path, ?start_line, ?end_line, ?start_byte, ?end_byte, ?provider)\n\
                   if def[:target_path] == \"crates/runtime/src/retrieval.rs\"\n\
                     return some(def[:start_line])\n\
                   end\n\
                 end\n\
                 return none"
            ))
            .unwrap();
            let TaskOutcome::Complete { value, .. } = &report.outcome else {
                panic!("expected complete outcome, got {:?}", report.outcome);
            };
            if value == &Value::option_none() {
                return;
            }
            assert!(matches!(
                report.outcome,
                TaskOutcome::Complete { value, .. }
                    if value.option_payload().flatten().and_then(|value| value.as_int()) == Some(1)
            ));
        });
    }
}
