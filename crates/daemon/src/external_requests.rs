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

use crate::metrics::{self, ExternalService};
use mica_driver::{ExternalRequestHandler, ExternalStreamRequestHandler};
use mica_var::{Symbol, Value};
use mica_web_host::editor_files::EditorFiles;
use std::sync::Arc;
use std::time::Instant;

pub fn handler(editor_files: Arc<EditorFiles>) -> ExternalRequestHandler {
    Arc::new(move |context, request| {
        let editor_files = editor_files.clone();
        Box::pin(async move {
            if EditorFiles::handles(request.service) {
                return compio::runtime::spawn_blocking(move || {
                    if context.cancellation.is_cancelled() {
                        return Value::error(
                            Symbol::intern("ExternalError"),
                            Some("file request cancelled"),
                            None,
                        );
                    }
                    editor_files.handle(request.service, &request.payload)
                })
                .await
                .unwrap_or_else(|_| {
                    Value::error(
                        Symbol::intern("ExternalError"),
                        Some("file service worker failed"),
                        None,
                    )
                });
            }
            let service = external_service_label(request.service);
            let start = Instant::now();
            metrics::metrics().external_requests.inc(service);
            let result = mica_external_http::perform_external_request(
                request,
                &mica_external_http::ExternalHttpConfig::default(),
            )
            .await;
            let elapsed = start.elapsed();
            metrics::metrics()
                .external_request_duration_us
                .record(service, metrics::duration_us(elapsed));
            metrics::metrics()
                .external_request_duration
                .record_elapsed(service, elapsed);
            match result {
                Ok(value) => {
                    tracing::debug!(
                        service = ?service,
                        elapsed_us = elapsed.as_micros(),
                        "external request completed"
                    );
                    value
                }
                Err(message) => {
                    metrics::metrics().external_request_errors.inc(service);
                    tracing::warn!(
                        service = ?service,
                        elapsed_us = elapsed.as_micros(),
                        error = %message,
                        "external request failed"
                    );
                    Value::error(Symbol::intern("ExternalError"), Some(message), None)
                }
            }
        })
    })
}

pub fn stream_handler() -> ExternalStreamRequestHandler {
    Arc::new(move |_, request, emitter| {
        Box::pin(async move {
            compio::runtime::spawn(async move {
                let service = external_service_label(request.service);
                let start = Instant::now();
                metrics::metrics().external_requests.inc(service);
                let result =
                    mica_external_http::perform_external_stream_request(request, &emitter).await;
                let elapsed = start.elapsed();
                metrics::metrics()
                    .external_request_duration_us
                    .record(service, metrics::duration_us(elapsed));
                metrics::metrics()
                    .external_request_duration
                    .record_elapsed(service, elapsed);
                if let Err(message) = result {
                    metrics::metrics().external_request_errors.inc(service);
                    tracing::warn!(
                        service = ?service,
                        elapsed_us = elapsed.as_micros(),
                        error = %message,
                        "external stream request failed"
                    );
                    let event = Value::map([
                        (
                            Value::symbol(Symbol::intern("type")),
                            Value::symbol(Symbol::intern("error")),
                        ),
                        (
                            Value::symbol(Symbol::intern("message")),
                            Value::string(message),
                        ),
                    ]);
                    let _ = emitter.emit(event).await;
                }
            })
            .detach();
            Value::map([(Value::symbol(Symbol::intern("started")), Value::bool(true))])
        })
    })
}

fn external_service_label(service: Symbol) -> ExternalService {
    match service.name() {
        Some("http") => ExternalService::Http,
        Some("openai" | "openai_responses") => ExternalService::Openai,
        Some("embedding") => ExternalService::Embedding,
        _ => ExternalService::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_driver::{DriverEventPump, DriverOwner, DriverResources, InvocationOutcome};
    use mica_runtime::{SourceRunner, TaskOutcome};
    use std::fs;
    use std::num::NonZeroUsize;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Workspace(PathBuf);
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    async fn evaluate(owner: &DriverOwner, pump: &mut DriverEventPump, source: &str) -> Value {
        let invocation = owner
            .administrator()
            .evaluate(source.to_owned())
            .await
            .unwrap();
        let outcome = pump.drive_invocation(&invocation, |_| {}).await;
        let InvocationOutcome::Completed(value) = outcome else {
            panic!("{source}: {outcome:?}");
        };
        value
    }

    #[test]
    fn editor_visits_saves_and_confirms_external_file_changes_through_the_driver() {
        let workspace = Workspace(std::env::temp_dir().join(format!(
                "mica-editor-host-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        fs::create_dir(&workspace.0).unwrap();
        fs::write(workspace.0.join("notes"), "one\r\né").unwrap();
        let mut runner = SourceRunner::new_empty();
        let apps = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps");
        for name in [
            "shared/buffers",
            "editor/schema",
            "editor/windows",
            "editor/buffers",
            "editor/keymaps",
            "editor/undo",
            "editor/commands",
            "editor/session",
            "editor/picker",
            "editor/minibuffer",
            "editor/files",
            "editor/ui",
            "editor/defaults",
        ] {
            let source = fs::read_to_string(apps.join(format!("{name}.mica"))).unwrap();
            for report in runner.run_filein(&source).unwrap() {
                assert!(
                    matches!(report.outcome, TaskOutcome::Complete { .. }),
                    "{}",
                    report.render()
                );
            }
        }
        let files = Arc::new(EditorFiles::new([workspace.0.clone()]).unwrap());
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut owner = DriverOwner::builder(DriverResources::new(NonZeroUsize::MIN))
                .source_runner(runner)
                .external_request_handler(handler(files))
                .build()
                .unwrap();
            let mut pump = owner.take_event_pump().unwrap();
            assert_eq!(
                evaluate(
                    &owner,
                    &mut pump,
                    r#"
                editor/session_create(:file/session, :file/actor, :default)
                let result = editor/file_visit(:file/session, 1, 1, "notes")
                return result[:status]
            "#
                )
                .await,
                Value::symbol(Symbol::intern("ok"))
            );
            assert_eq!(
                evaluate(
                    &owner,
                    &mut pump,
                    r#"
                let buffer = editor/window_buffer(:file/session, 1)
                require buffer_text(buffer) == "one\né"
                require not editor/buffer_modified(buffer)
                buffer_insert(buffer, buffer_len(buffer), "!")
                return true
            "#
                )
                .await,
                Value::bool(true)
            );
            let save = r#"
                let buffer = editor/window_buffer(:file/session, 1)
                return editor/save_buffer_command(:file/session, 1, 1, buffer, 0, none, none, {})
            "#;
            let saved = evaluate(&owner, &mut pump, save).await;
            assert_eq!(
                saved.map_get(&Value::symbol(Symbol::intern("status"))),
                Some(Value::symbol(Symbol::intern("ok")))
            );
            assert_eq!(
                fs::read_to_string(workspace.0.join("notes")).unwrap(),
                "one\r\né!"
            );
            assert_eq!(
                evaluate(
                    &owner,
                    &mut pump,
                    r#"
                let buffer = editor/window_buffer(:file/session, 1)
                require not editor/buffer_modified(buffer)
                buffer_insert(buffer, buffer_len(buffer), "?")
                return true
            "#
                )
                .await,
                Value::bool(true)
            );
            fs::write(workspace.0.join("notes"), "external").unwrap();
            let changed = evaluate(&owner, &mut pump, save).await;
            assert!(
                changed
                    .map_get(&Value::symbol(Symbol::intern("message")))
                    .unwrap()
                    .with_str(|message| message.contains("repeat C-x C-s"))
                    .unwrap()
            );
            assert_eq!(
                fs::read_to_string(workspace.0.join("notes")).unwrap(),
                "external"
            );
            evaluate(&owner, &mut pump, save).await;
            assert_eq!(
                fs::read_to_string(workspace.0.join("notes")).unwrap(),
                "one\r\né!?"
            );
            assert_eq!(
                evaluate(
                    &owner,
                    &mut pump,
                    r#"
                return not editor/buffer_modified(editor/window_buffer(:file/session, 1))
            "#
                )
                .await,
                Value::bool(true)
            );
            assert_eq!(
                evaluate(&owner, &mut pump, r#"
                    editor/file_visit(:file/session, 1, 1, "created")
                    let buffer = editor/window_buffer(:file/session, 1)
                    require editor/file_stamp(buffer) == none
                    buffer_insert(buffer, 0, "new é🦀")
                    return true
                "#).await,
                Value::bool(true)
            );
            assert_eq!(
                evaluate(&owner, &mut pump, r#"
                    let buffer = editor/window_buffer(:file/session, 1)
                    let result = editor/save_buffer_command(:file/session, 1, 1, buffer, 0, none, none, {})
                    require string_starts_with(result[:message], "Wrote ")
                    return not editor/buffer_modified(buffer)
                "#).await,
                Value::bool(true)
            );
            assert_eq!(fs::read_to_string(workspace.0.join("created")).unwrap(), "new é🦀");
            owner.shutdown(&mut pump, |_| {}).await.unwrap();
        });
    }
}
