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

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use mica_compiler::parse;
use mica_driver::{CompioTaskDriver, DriverError, DriverEvent};
use mica_relation_kernel::FjallDurabilityMode;
use mica_runtime::{EmbeddingProviderKind, FileinMode, SourceRunner, SuspendKind, TaskOutcome};
use mica_source_provider::{
    SourceIndexRoot, build_source_index_file_for_roots, write_failed_source_index_file,
};
use mica_var::{Identity, Symbol};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const REPL_SETTLE_LIMIT: Duration = Duration::from_millis(50);
const CLI_ENDPOINT_ID: u64 = 0x00ee_0000_0000_0000;

#[derive(Parser)]
#[command(name = "mica", about = "Run Mica source, fileins, fileouts, and REPLs")]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = StorageMode::Memory)]
    storage: StorageMode,
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    #[arg(long, global = true, value_enum, default_value_t = DurabilityMode::Relaxed)]
    durability: DurabilityMode,
    #[arg(long, global = true, value_enum, default_value_t = EmbeddingProviderMode::Deterministic)]
    embedding_provider: EmbeddingProviderMode,
    #[arg(long, global = true, value_name = "IDENTITY")]
    actor: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum StorageMode {
    Memory,
    Fjall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DurabilityMode {
    Relaxed,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum EmbeddingProviderMode {
    Deterministic,
    Disabled,
    Vllm,
}

impl From<DurabilityMode> for FjallDurabilityMode {
    fn from(value: DurabilityMode) -> Self {
        match value {
            DurabilityMode::Relaxed => Self::Relaxed,
            DurabilityMode::Strict => Self::Strict,
        }
    }
}

impl From<EmbeddingProviderMode> for EmbeddingProviderKind {
    fn from(value: EmbeddingProviderMode) -> Self {
        match value {
            EmbeddingProviderMode::Deterministic => Self::Deterministic,
            EmbeddingProviderMode::Disabled => Self::Disabled,
            EmbeddingProviderMode::Vllm => Self::Vllm,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    Run {
        file: PathBuf,
    },
    Filein {
        #[arg(long)]
        unit: Option<String>,
        #[arg(long)]
        replace: bool,
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
    },
    Fileout {
        unit: String,
        output: Option<PathBuf>,
    },
    Eval {
        #[arg(long = "filein", action = ArgAction::Append, value_name = "FILE")]
        filein: Vec<PathBuf>,
        #[arg(required = true, trailing_var_arg = true)]
        source: Vec<String>,
    },
    SourceIndex {
        #[arg(long, action = ArgAction::Append, value_name = "NAME=DIR")]
        root: Vec<String>,
        #[arg(
            long,
            default_value = ".cache/source-index/mica-worktree.json",
            value_name = "FILE"
        )]
        output: PathBuf,
    },
    Repl,
}

fn main() -> ExitCode {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        match run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        }
    })
}

async fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command.as_ref().unwrap_or(&Command::Repl) {
        Command::Run { file } => {
            let source = fs::read_to_string(file)
                .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
            let session = open_cli_session(&cli, Symbol::intern("cli"))?;
            let source_name = file.display().to_string();
            let report = submit_cli_source(&session, &cli, source, Some(&source_name)).await?;
            print_report_and_follow(&session.driver, report).await;
            let _ = session.driver.close_endpoint(session.endpoint).await;
            Ok(())
        }
        Command::Filein {
            unit,
            replace,
            files,
        } => {
            reject_actor(&cli)?;
            let mut runner = open_runner(&cli)?;
            if let Some(unit) = unit {
                let [file] = files.as_slice() else {
                    return Err("--unit can only be used with one filein file".to_owned());
                };
                let source = fs::read_to_string(file)
                    .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
                let include_base = file.parent().unwrap_or_else(|| std::path::Path::new("."));
                let mode = if *replace {
                    FileinMode::Replace
                } else {
                    FileinMode::Add
                };
                let report = runner
                    .run_filein_with_unit_and_include_loader(
                        Symbol::intern(unit.trim_start_matches(':')),
                        &source,
                        mode,
                        |path| read_filein_include(include_base, path),
                    )
                    .map_err(|error| format_source_error_with_source(error, file, &source))?;
                for report in report.reports {
                    print_report(report);
                }
            } else {
                for file in files {
                    let source = fs::read_to_string(file)
                        .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
                    let include_base = file.parent().unwrap_or_else(|| std::path::Path::new("."));
                    for report in runner
                        .run_filein_with_include_loader(&source, |path| {
                            read_filein_include(include_base, path)
                        })
                        .map_err(|error| format_source_error_with_source(error, file, &source))?
                    {
                        print_report(report);
                    }
                }
            }
            Ok(())
        }
        Command::Fileout { unit, output } => {
            reject_actor(&cli)?;
            let runner = open_runner(&cli)?;
            let source = runner
                .fileout_unit(Symbol::intern(unit.trim_start_matches(':')))
                .map_err(format_source_error)?;
            if let Some(output) = output {
                fs::write(output, source)
                    .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
            } else {
                println!("{source}");
            }
            Ok(())
        }
        Command::Eval { filein, source } => {
            let source = source.join(" ");
            let session = open_cli_session_with_fileins(&cli, Symbol::intern("cli"), filein)?;
            let report = submit_cli_source(&session, &cli, source, Some("<eval>")).await?;
            print_report_and_follow(&session.driver, report).await;
            let _ = session.driver.close_endpoint(session.endpoint).await;
            Ok(())
        }
        Command::SourceIndex { root, output } => {
            reject_actor(&cli)?;
            let roots = parse_source_index_roots(root)?;
            let error_root = roots
                .first()
                .map(|root| root.root.clone())
                .unwrap_or_else(|| PathBuf::from("."));
            match build_source_index_file_for_roots(&roots, output) {
                Ok(()) => {
                    println!("wrote source index {}", output.display());
                    Ok(())
                }
                Err(error) => {
                    let _ = write_failed_source_index_file(&error_root, output, &error);
                    Err(error)
                }
            }
        }
        Command::Repl => repl(&cli).await,
    }
}

fn parse_source_index_roots(root_specs: &[String]) -> Result<Vec<SourceIndexRoot>, String> {
    let specs = if root_specs.is_empty() {
        vec!["default=.".to_owned()]
    } else {
        root_specs.to_vec()
    };
    specs
        .into_iter()
        .map(|spec| {
            let (name, root) = match spec.split_once('=') {
                Some((name, root)) => (name.trim().to_owned(), PathBuf::from(root.trim())),
                None => {
                    let root = PathBuf::from(spec.trim());
                    let name = root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or("default")
                        .to_owned();
                    (name, root)
                }
            };
            if name.is_empty() {
                return Err("source index root name must not be empty".to_owned());
            }
            if root.as_os_str().is_empty() {
                return Err(format!("source index root {name} has an empty path"));
            }
            Ok(SourceIndexRoot { name, root })
        })
        .collect()
}

struct CliSession {
    driver: CompioTaskDriver,
    endpoint: Identity,
}

async fn submit_cli_source(
    session: &CliSession,
    cli: &Cli,
    source: String,
    source_name: Option<&str>,
) -> Result<mica_runtime::RunReport, String> {
    let diagnostic_source = source.clone();
    session
        .driver
        .submit_source_report(
            session.endpoint,
            cli.actor.as_deref().map(actor_symbol),
            source,
        )
        .await
        .map_err(|error| format_driver_error_with_source(error, source_name, &diagnostic_source))
}

fn actor_symbol(actor: &str) -> Symbol {
    Symbol::intern(actor.trim().trim_start_matches('#').trim_start_matches(':'))
}

fn reject_actor(cli: &Cli) -> Result<(), String> {
    if cli.actor.is_some() {
        return Err("--actor is only supported for run, eval, and repl".to_owned());
    }
    Ok(())
}

fn read_filein_include(base: &std::path::Path, path: &str) -> Result<String, String> {
    let include_path = base.join(path);
    fs::read_to_string(&include_path)
        .map_err(|error| format!("failed to read {}: {error}", include_path.display()))
}

fn open_runner(cli: &Cli) -> Result<SourceRunner, String> {
    let use_fjall = cli.storage == StorageMode::Fjall || cli.store.is_some();
    if !use_fjall {
        return Ok(SourceRunner::new_empty_with_embedding_provider(
            cli.embedding_provider.into(),
        ));
    }
    let store = cli
        .store
        .as_ref()
        .ok_or_else(|| "--store is required with --storage fjall".to_owned())?;
    SourceRunner::open_fjall_with_embedding_provider(
        store,
        cli.durability.into(),
        cli.embedding_provider.into(),
    )
}

fn open_cli_session(cli: &Cli, protocol: Symbol) -> Result<CliSession, String> {
    open_cli_session_with_fileins(cli, protocol, &[])
}

fn open_cli_session_with_fileins(
    cli: &Cli,
    protocol: Symbol,
    fileins: &[PathBuf],
) -> Result<CliSession, String> {
    let mut runner = open_runner(cli)?;
    load_fileins(&mut runner, fileins)?;
    let actor = cli
        .actor
        .as_deref()
        .map(actor_symbol)
        .map(|actor| runner.named_identity(actor).map_err(format_source_error))
        .transpose()?;
    let driver = CompioTaskDriver::spawn(runner).map_err(format_driver_error)?;
    let endpoint = cli_endpoint();
    driver
        .open_endpoint(endpoint, actor, protocol)
        .map_err(format_driver_error)?;
    Ok(CliSession { driver, endpoint })
}

fn load_fileins(runner: &mut SourceRunner, fileins: &[PathBuf]) -> Result<(), String> {
    for file in fileins {
        let source = fs::read_to_string(file)
            .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
        let include_base = file.parent().unwrap_or_else(|| std::path::Path::new("."));
        runner
            .run_filein_with_include_loader(&source, |path| read_filein_include(include_base, path))
            .map_err(|error| format_source_error_with_source(error, file, &source))?;
    }
    Ok(())
}

fn cli_endpoint() -> Identity {
    Identity::new(CLI_ENDPOINT_ID).unwrap()
}

async fn repl(cli: &Cli) -> Result<(), String> {
    let mut editor =
        DefaultEditor::new().map_err(|error| format!("failed to initialize repl: {error}"))?;
    let session = open_cli_session(cli, Symbol::intern("repl"))?;
    let result = repl_loop(cli, &session, &mut editor).await;
    let _ = session.driver.close_endpoint(session.endpoint).await;
    result
}

async fn repl_loop(
    cli: &Cli,
    session: &CliSession,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    let mut buffer = String::new();

    println!("Mica REPL. Enter :quit to exit. Blank line forces evaluation.");
    loop {
        print_driver_events(session.driver.drain_events());
        let prompt = if buffer.is_empty() {
            "mica> "
        } else {
            "....> "
        };
        match editor.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if buffer.is_empty() && matches!(trimmed, ":quit" | ":q") {
                    return Ok(());
                }
                if buffer.is_empty() && matches!(trimmed, ":help" | ":h") {
                    print_repl_help();
                    continue;
                }
                if buffer.is_empty() && matches!(trimmed, ":poll" | ":p") {
                    print_driver_events(session.driver.drain_events());
                    continue;
                }
                if trimmed.is_empty() {
                    if !buffer.trim().is_empty() {
                        evaluate_buffer(session, cli, &mut buffer).await;
                    }
                    continue;
                }

                let _ = editor.add_history_entry(line.as_str());
                buffer.push_str(&line);
                buffer.push('\n');
                if parse(&buffer).errors.is_empty() {
                    evaluate_buffer(session, cli, &mut buffer).await;
                }
            }
            Err(ReadlineError::Interrupted) => {
                buffer.clear();
                println!("^C");
            }
            Err(ReadlineError::Eof) => return Ok(()),
            Err(error) => return Err(format!("repl error: {error}")),
        }
    }
}

async fn evaluate_buffer(session: &CliSession, cli: &Cli, buffer: &mut String) {
    match submit_cli_source(session, cli, buffer.clone(), Some("<repl>")).await {
        Ok(report) => {
            let task_id = report.task_id;
            let outcome = report.outcome.clone();
            print_report(report);
            print_driver_events_without_initial_report(
                task_id,
                &outcome,
                session.driver.drain_events(),
            );
            settle_repl_task(&session.driver, task_id, &outcome).await;
        }
        Err(error) => eprintln!("{error}"),
    }
    buffer.clear();
}

fn print_report(report: mica_runtime::RunReport) {
    println!("{}", report.render());
}

fn format_source_error(error: mica_runtime::SourceTaskError) -> String {
    mica_runtime::format_source_task_error(&error)
}

fn format_source_error_with_source(
    error: mica_runtime::SourceTaskError,
    path: &std::path::Path,
    source: &str,
) -> String {
    let path = path.display().to_string();
    mica_runtime::format_source_task_error_with_source(&error, Some(&path), source)
}

fn format_driver_error(error: DriverError) -> String {
    format!("error: {error}")
}

fn format_driver_error_with_source(
    error: DriverError,
    source_name: Option<&str>,
    source: &str,
) -> String {
    if let Some(error) = error.source() {
        return mica_runtime::format_source_task_error_with_source(error, source_name, source);
    }
    format_driver_error(error)
}

async fn print_report_and_follow(driver: &CompioTaskDriver, report: mica_runtime::RunReport) {
    let task_id = report.task_id;
    let outcome = report.outcome.clone();
    let mut suspended = suspended_kind(&outcome);
    print_report(report);

    while let Some(kind) = suspended {
        let Some(duration) = follow_delay(&kind) else {
            break;
        };
        compio::time::sleep(duration).await;
        suspended = None;
        for event in driver.drain_events() {
            match &event {
                DriverEvent::TaskSuspended {
                    task_id: event_task,
                    kind,
                } if *event_task == task_id => {
                    suspended = Some(kind.clone());
                }
                DriverEvent::TaskCompleted {
                    task_id: event_task,
                    ..
                }
                | DriverEvent::TaskAborted {
                    task_id: event_task,
                    ..
                }
                | DriverEvent::TaskFailed {
                    task_id: event_task,
                    ..
                } if *event_task == task_id => {
                    print_driver_event(event);
                    return;
                }
                DriverEvent::TaskSuspended {
                    task_id: event_task,
                    ..
                } if *event_task == task_id => {}
                _ => print_driver_event(event),
            }
        }
    }

    print_driver_events_without_initial_report(task_id, &outcome, driver.drain_events());
}

async fn settle_repl_task(driver: &CompioTaskDriver, task_id: u64, outcome: &TaskOutcome) {
    let mut suspended = suspended_kind(outcome);
    for _ in 0..8 {
        let Some(kind) = suspended else {
            return;
        };
        let Some(duration) = repl_settle_delay(&kind) else {
            return;
        };
        compio::time::sleep(duration).await;
        suspended = None;
        let events = driver.drain_events();
        for event in events {
            match &event {
                DriverEvent::TaskSuspended {
                    task_id: event_task,
                    kind,
                } if *event_task == task_id => {
                    suspended = Some(kind.clone());
                }
                _ => print_driver_event(event),
            }
        }
    }
}

fn suspended_kind(outcome: &TaskOutcome) -> Option<SuspendKind> {
    match outcome {
        TaskOutcome::Suspended { kind, .. } => Some(kind.clone()),
        TaskOutcome::Complete { .. } | TaskOutcome::Aborted { .. } => None,
    }
}

fn follow_delay(kind: &SuspendKind) -> Option<Duration> {
    match kind {
        SuspendKind::Commit => Some(Duration::from_millis(1)),
        SuspendKind::TimedMillis(millis) => {
            Some(Duration::from_millis(*millis).max(Duration::from_millis(1)))
        }
        SuspendKind::MailboxRecv(request) => request
            .timeout_millis
            .map(|millis| Duration::from_millis(millis).max(Duration::from_millis(1))),
        SuspendKind::Spawn(_) => Some(Duration::from_millis(1)),
        SuspendKind::ExternalRequest(request) => request
            .timeout_millis
            .map(|millis| Duration::from_millis(millis).max(Duration::from_millis(1))),
        SuspendKind::Never | SuspendKind::WaitingForInput(_) => None,
    }
}

fn repl_settle_delay(kind: &SuspendKind) -> Option<Duration> {
    follow_delay(kind).filter(|duration| *duration <= REPL_SETTLE_LIMIT)
}

fn print_driver_events(events: Vec<DriverEvent>) {
    for event in events {
        print_driver_event(event);
    }
}

fn print_driver_events_without_initial_report(
    task_id: u64,
    outcome: &TaskOutcome,
    events: Vec<DriverEvent>,
) {
    for event in events {
        if event_matches_initial_report(task_id, outcome, &event) {
            continue;
        }
        print_driver_event(event);
    }
}

fn event_matches_initial_report(task_id: u64, outcome: &TaskOutcome, event: &DriverEvent) -> bool {
    match (outcome, event) {
        (
            TaskOutcome::Complete { .. },
            DriverEvent::TaskCompleted {
                task_id: event_task,
                ..
            },
        )
        | (
            TaskOutcome::Aborted { .. },
            DriverEvent::TaskAborted {
                task_id: event_task,
                ..
            },
        )
        | (
            TaskOutcome::Aborted { .. },
            DriverEvent::TaskFailed {
                task_id: event_task,
                ..
            },
        )
        | (
            TaskOutcome::Suspended { .. },
            DriverEvent::TaskSuspended {
                task_id: event_task,
                ..
            },
        ) => *event_task == task_id,
        (_, DriverEvent::Effect(effect)) => effect.task_id == task_id,
        _ => false,
    }
}

fn print_driver_event(event: DriverEvent) {
    println!("event: {event:?}");
}

fn print_repl_help() {
    println!(
        ":quit exits. :poll drains pending events. Blank line forces evaluation of an incomplete buffer."
    );
}
