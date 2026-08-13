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

use crate::{
    CompioTaskDriver, DriverError, DriverEvent, DriverOwner, DriverResources,
    DriverSubscriptionRequest, EPHEMERAL_HOST_IDENTITY_START, TaskCancellationReason,
};
use mica_runtime::{
    AuthorityContext, EmbeddingProviderKind, FileinMode, ReadOnlySourceQueryOptions,
    ReadOnlySourceQueryStatus, RuntimeError, SourceTaskError, SubscriptionInitialDelivery,
    SubscriptionSubject, TaskError, TaskInput, TaskManagerError, TaskRequest,
};
use mica_runtime::{SourceRunner, SuspendKind, TaskOutcome};
use mica_var::{Identity, Symbol, Value};
use std::future::pending;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const TEST_WORKERS: Option<NonZeroUsize> = NonZeroUsize::new(1);

fn endpoint(offset: u64) -> Identity {
    Identity::new(0x00ee_0000_0000_0000 + offset).unwrap()
}

fn root_source(source: &str) -> TaskRequest {
    SourceRunner::root_source_request(source)
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn driver_runs_source_on_compio_task() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(1), root_source("return 1 + 1"))
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::int(2).unwrap()
        ));
        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && *value == Value::int(2).unwrap()
        )));
    });
}

#[test]
fn driver_allocates_one_ephemeral_identity_sequence_for_host_objects() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();

        let endpoint = driver.allocate_ephemeral_identity().unwrap();
        let request = driver.allocate_ephemeral_identity().unwrap();

        assert_eq!(endpoint.raw(), EPHEMERAL_HOST_IDENTITY_START);
        assert_eq!(request.raw(), EPHEMERAL_HOST_IDENTITY_START + 1);
        assert_eq!(driver.resource_snapshot().ephemeral_identities, 2);
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn driver_enforces_ephemeral_identity_capacity() {
    crate::test_support::run(async {
        let mut resources = DriverResources::new(TEST_WORKERS.unwrap());
        resources.ephemeral_identity_capacity = NonZeroUsize::new(1).unwrap();
        let driver =
            CompioTaskDriver::spawn_with_resources(SourceRunner::new_empty(), resources).unwrap();

        driver.allocate_ephemeral_identity().unwrap();
        assert!(matches!(
            driver.allocate_ephemeral_identity(),
            Err(DriverError::Configuration(message))
                if message.contains("ephemeral identity capacity")
        ));
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn driver_rejects_unallocated_endpoints_in_its_ephemeral_identity_range() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let endpoint = Identity::new(EPHEMERAL_HOST_IDENTITY_START).unwrap();

        assert!(matches!(
            driver.open_endpoint_with_context_and_volatile_tuples_named(
                endpoint,
                None,
                None,
                Symbol::intern("test"),
                Vec::new(),
            ),
            Err(DriverError::Configuration(message))
                if message.contains("was not allocated by this driver")
        ));

        let allocated = driver.allocate_ephemeral_identity().unwrap();
        assert_eq!(allocated, endpoint);
        driver
            .open_endpoint_with_context_and_volatile_tuples_named(
                allocated,
                None,
                None,
                Symbol::intern("test"),
                Vec::new(),
            )
            .unwrap();
        driver.close_endpoint_resources(allocated).await.unwrap();
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn driver_events_can_be_awaited() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(29), root_source("return 3 + 4"))
            .await
            .unwrap();

        let events = driver.wait_events().await;

        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && *value == Value::int(7).unwrap()
        )));
    });
}

#[test]
fn timed_suspend_wakes_and_resumes_task() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(2), root_source("suspend(0.001)\nreturn \"awake\""))
            .await
            .unwrap();
        assert!(
            matches!(submitted.outcome, TaskOutcome::Suspended { .. }),
            "{:?}",
            submitted.outcome
        );

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && *value == Value::string("awake")
        )));
    });
}

#[test]
fn external_request_suspends_and_resumes_from_handler() {
    crate::test_support::run(async {
        let handler = Arc::new(|_, request: mica_runtime::ExternalRequest| {
            Box::pin(async move {
                Value::list([
                    Value::symbol(request.service),
                    request.payload,
                    Value::int(request.timeout_millis.unwrap_or_default() as i64).unwrap(),
                ])
            }) as crate::types::ExternalRequestFuture
        });
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            SourceRunner::new_empty(),
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let submitted = driver
            .submit_source(
                endpoint(30),
                root_source("return external_request(:echo, \"hello\", 0.005)"),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::ExternalRequest(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && *value == Value::list([
                        Value::symbol(Symbol::intern("echo")),
                        Value::string("hello"),
                        Value::int(5).unwrap(),
                    ])
        )));
    });
}

#[test]
fn external_request_admission_bounds_handler_concurrency() {
    crate::test_support::run(async {
        let starts = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum_active = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let handler = {
            let starts = Arc::clone(&starts);
            let active = Arc::clone(&active);
            let maximum_active = Arc::clone(&maximum_active);
            let release = Arc::clone(&release);
            Arc::new(move |_, _: mica_runtime::ExternalRequest| {
                let starts = Arc::clone(&starts);
                let active = Arc::clone(&active);
                let maximum_active = Arc::clone(&maximum_active);
                let release = Arc::clone(&release);
                Box::pin(async move {
                    starts.fetch_add(1, Ordering::AcqRel);
                    let now_active = active.fetch_add(1, Ordering::AcqRel) + 1;
                    maximum_active.fetch_max(now_active, Ordering::AcqRel);
                    while !release.load(Ordering::Acquire) {
                        compio::time::sleep(Duration::from_millis(1)).await;
                    }
                    active.fetch_sub(1, Ordering::AcqRel);
                    Value::unit()
                }) as crate::types::ExternalRequestFuture
            })
        };
        let mut resources = DriverResources::new(TEST_WORKERS.unwrap());
        resources.external_request_capacity = NonZeroUsize::new(1).unwrap();
        let driver = DriverOwner::builder(resources)
            .external_request_handler(handler)
            .build_driver()
            .unwrap();

        let first = driver
            .submit_source(
                endpoint(60),
                root_source("return external_request(:hold, none)"),
            )
            .await
            .unwrap();
        let second = driver
            .submit_source(
                endpoint(60),
                root_source("return external_request(:hold, none)"),
            )
            .await
            .unwrap();
        assert!(matches!(first.outcome, TaskOutcome::Suspended { .. }));
        assert!(matches!(second.outcome, TaskOutcome::Suspended { .. }));

        compio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(starts.load(Ordering::Acquire), 1);
        assert_eq!(maximum_active.load(Ordering::Acquire), 1);

        release.store(true, Ordering::Release);
        for _ in 0..50 {
            if starts.load(Ordering::Acquire) == 2 && active.load(Ordering::Acquire) == 0 {
                break;
            }
            compio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(starts.load(Ordering::Acquire), 2);
        assert_eq!(maximum_active.load(Ordering::Acquire), 1);
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn external_request_context_is_cancelled_with_its_task() {
    crate::test_support::run(async {
        let observed = Arc::new(Mutex::new(None));
        let handler = {
            let observed = Arc::clone(&observed);
            Arc::new(
                move |context: crate::ExternalRequestContext, _: mica_runtime::ExternalRequest| {
                    *observed.lock().unwrap() = Some(context.clone());
                    Box::pin(async move {
                        context.cancellation.cancelled().await;
                        Value::unit()
                    }) as crate::types::ExternalRequestFuture
                },
            )
        };
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            SourceRunner::new_empty(),
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let endpoint = endpoint(61);
        let submitted = driver
            .submit_source(
                endpoint,
                root_source("return external_request(:hold, none)"),
            )
            .await
            .unwrap();

        for _ in 0..50 {
            if observed.lock().unwrap().is_some() {
                break;
            }
            compio::time::sleep(Duration::from_millis(1)).await;
        }
        let context = observed
            .lock()
            .unwrap()
            .clone()
            .expect("external handler did not start");
        assert_eq!(context.task_id, submitted.task_id);
        assert_eq!(context.endpoint, endpoint);
        assert_eq!(context.principal, None);
        assert_eq!(context.actor, None);

        driver.cancel_task(submitted.task_id).await.unwrap();

        assert!(context.cancellation.is_cancelled());
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn external_stream_request_delivers_events_to_mica_mailbox() {
    crate::test_support::run(async {
        let stream_handler = Arc::new(
            |_,
             request: mica_runtime::ExternalRequest,
             emitter: crate::types::ExternalStreamEmitter| {
                Box::pin(async move {
                    assert_eq!(request.service, Symbol::intern("llm_stream"));
                    emitter
                        .emit(Value::map([
                            (
                                Value::symbol(Symbol::intern("type")),
                                Value::symbol(Symbol::intern("text_delta")),
                            ),
                            (
                                Value::symbol(Symbol::intern("delta")),
                                Value::string("hello"),
                            ),
                        ]))
                        .await
                        .unwrap();
                    Value::map([(Value::symbol(Symbol::intern("started")), Value::bool(true))])
                }) as crate::types::ExternalRequestFuture
            },
        );
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handlers(
            SourceRunner::new_empty(),
            TEST_WORKERS,
            None,
            Some(stream_handler),
        )
        .unwrap();
        let submitted = driver
            .submit_source(
                endpoint(30),
                root_source(
                    "let caps = mailbox()\n\
                     external_request(:llm_stream, {:stream_to -> caps[1]})\n\
                     let ready = mailbox_recv([caps[0]], 1)\n\
                     return ready[0][1][0]",
                ),
            )
            .await
            .unwrap();

        assert!(
            matches!(submitted.outcome, TaskOutcome::Suspended { .. }),
            "{:?}",
            submitted.outcome
        );
        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && value.map_get(&Value::symbol(Symbol::intern("type")))
                        == Some(Value::symbol(Symbol::intern("text_delta")))
                    && value.map_get(&Value::symbol(Symbol::intern("delta")))
                        == Some(Value::string("hello"))
        )));
    });
}

#[test]
fn missing_external_stream_handler_delivers_an_error_event() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint(31),
                root_source(
                    "let caps = mailbox()\n\
                     external_request(:llm_stream, {:stream_to -> caps[1]})\n\
                     let ready = mailbox_recv([caps[0]], 1)\n\
                     return ready[0][1][0]",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));
        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && value.map_get(&Value::symbol(Symbol::intern("type")))
                        == Some(Value::symbol(Symbol::intern("error")))
                    && value.map_get(&Value::symbol(Symbol::intern("message")))
                        == Some(Value::string("no external stream request handler is configured"))
        )));
    });
}

#[test]
fn vllm_embed_text_suspends_as_embedding_external_request() {
    crate::test_support::run(async {
        let handler = Arc::new(|_, request: mica_runtime::ExternalRequest| {
            Box::pin(async move {
                assert_eq!(request.service, Symbol::intern("embedding"));
                assert_eq!(
                    request
                        .payload
                        .map_get(&Value::symbol(Symbol::intern("model"))),
                    Some(Value::string("source-workspace"))
                );
                assert_eq!(
                    request
                        .payload
                        .map_get(&Value::symbol(Symbol::intern("text"))),
                    Some(Value::string("red brass lamp"))
                );
                Value::list([
                    Value::float(0.25).unwrap(),
                    Value::float(0.5).unwrap(),
                    Value::float(0.75).unwrap(),
                ])
            }) as crate::types::ExternalRequestFuture
        });
        let runner = SourceRunner::new_empty_with_embedding_provider(EmbeddingProviderKind::Vllm);
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            runner,
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let submitted = driver
            .submit_source(
                endpoint(33),
                root_source("return embed_text(\"source-workspace\", \"red brass lamp\")"),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::ExternalRequest(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && *value == Value::list([
                        Value::float(0.25).unwrap(),
                        Value::float(0.5).unwrap(),
                        Value::float(0.75).unwrap(),
                    ])
        )));
    });
}

#[test]
fn openai_chat_completion_suspends_as_openai_external_request() {
    crate::test_support::run(async {
        let handler = Arc::new(|_, request: mica_runtime::ExternalRequest| {
            Box::pin(async move {
                assert_eq!(request.service, Symbol::intern("openai"));
                assert_eq!(
                    request
                        .payload
                        .map_get(&Value::symbol(Symbol::intern("model"))),
                    Some(Value::string("~openai/gpt-latest"))
                );
                let messages = request
                    .payload
                    .map_get(&Value::symbol(Symbol::intern("messages")))
                    .expect("host request should include messages");
                assert_eq!(messages.list_len(), Some(1));
                assert_eq!(request.timeout_millis, Some(60_000));
                Value::map([(
                    Value::symbol(Symbol::intern("choices")),
                    Value::list([Value::map([(
                        Value::symbol(Symbol::intern("message")),
                        Value::map([(
                            Value::symbol(Symbol::intern("content")),
                            Value::string("pong"),
                        )]),
                    )])]),
                )])
            }) as crate::types::ExternalRequestFuture
        });
        let runner = SourceRunner::new_empty();
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            runner,
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let submitted = driver
            .submit_source(
                endpoint(33),
                root_source(
                    "return openai_chat_completion(\"~openai/gpt-latest\", [{:role -> \"user\", :content -> \"ping\"}])",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::ExternalRequest(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && value
                        .map_get(&Value::symbol(Symbol::intern("choices")))
                        .and_then(|choices| choices.with_list(|choices| choices.first().cloned()).flatten())
                        .and_then(|choice| choice.map_get(&Value::symbol(Symbol::intern("message"))))
                        .and_then(|message| message.map_get(&Value::symbol(Symbol::intern("content"))))
                        == Some(Value::string("pong"))
        )));
    });
}

fn load_agent_app(runner: &mut SourceRunner) {
    for filein in [
        include_str!("../../../apps/shared/sync-host.mica"),
        include_str!("../../../apps/shared/string.mica"),
        include_str!("../../../apps/shared/events.mica"),
        include_str!("../../../apps/shared/llm.mica"),
        include_str!("../../../apps/agent/core.mica"),
        include_str!("../../../apps/agent/workspaces.mica"),
        include_str!("../../../apps/agent/tools.mica"),
        include_str!("../../../apps/shared/sync-dom.mica"),
        include_str!("../../../apps/agent/ui-session.mica"),
        include_str!("../../../apps/agent/transcript.mica"),
        include_str!("../../../apps/agent/ui-compose.mica"),
        include_str!("../../../apps/agent/ui-actions.mica"),
        include_str!("../../../apps/agent/http.mica"),
    ] {
        runner.run_filein(filein).unwrap();
    }
}

#[test]
fn agent_command_sync_event_appends_user_message_and_suspends_for_llm() {
    crate::test_support::run(async {
        let stream_handler = Arc::new(
            |_, request: mica_runtime::ExternalRequest, emitter: crate::ExternalStreamEmitter| {
                Box::pin(async move {
                    assert_eq!(request.service, Symbol::intern("openai_responses"));
                    let input = request
                        .payload
                        .map_get(&Value::symbol(Symbol::intern("input")))
                        .and_then(|value| value.with_list(<[Value]>::to_vec))
                        .unwrap();
                    assert_eq!(input.len(), 1);
                    assert_eq!(
                        input[0].map_get(&Value::symbol(Symbol::intern("role"))),
                        Some(Value::string("user"))
                    );
                    assert!(
                        request
                            .payload
                            .map_get(&Value::symbol(Symbol::intern("previous_response_id")))
                            .is_none()
                    );
                    emitter
                        .emit(Value::map([
                            (
                                Value::symbol(Symbol::intern("type")),
                                Value::symbol(Symbol::intern("text_delta")),
                            ),
                            (
                                Value::symbol(Symbol::intern("delta")),
                                Value::string("synthetic assistant reply"),
                            ),
                        ]))
                        .await
                        .unwrap();
                    emitter
                        .emit(Value::map([
                            (
                                Value::symbol(Symbol::intern("type")),
                                Value::symbol(Symbol::intern("completed")),
                            ),
                            (
                                Value::symbol(Symbol::intern("response")),
                                Value::map([
                                    (
                                        Value::symbol(Symbol::intern("id")),
                                        Value::string("resp_test"),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("model")),
                                        Value::string("test-model"),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("usage")),
                                        Value::map([(
                                            Value::symbol(Symbol::intern("output_tokens")),
                                            Value::int(3).unwrap(),
                                        )]),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("output")),
                                        Value::list([Value::map([(
                                            Value::symbol(Symbol::intern("type")),
                                            Value::string("message"),
                                        )])]),
                                    ),
                                ]),
                            ),
                        ]))
                        .await
                        .unwrap();
                    Value::map([(Value::symbol(Symbol::intern("started")), Value::bool(true))])
                }) as crate::types::ExternalRequestFuture
            },
        );

        let prior = std::env::var_os("MICA_SOURCE_ROOT");
        unsafe {
            std::env::set_var("MICA_SOURCE_ROOT", "/tmp/agent-driver-test-root");
        }
        let mut runner = SourceRunner::new_empty();
        load_agent_app(&mut runner);
        match prior {
            Some(value) => unsafe { std::env::set_var("MICA_SOURCE_ROOT", value) },
            None => unsafe { std::env::remove_var("MICA_SOURCE_ROOT") },
        }

        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let agent = runner
            .named_identity(Symbol::intern("agent/default"))
            .unwrap();
        let ep = endpoint(40);
        runner
            .open_endpoint_with_context(ep, Some(web), Some(agent), Symbol::intern("web"))
            .unwrap();

        let driver = CompioTaskDriver::spawn_with_workers_and_external_handlers(
            runner,
            TEST_WORKERS,
            None,
            Some(stream_handler),
        )
        .unwrap();

        let submitted = driver
            .submit_source(
                ep,
                root_source(
                    "return sync_event(endpoint(), none, 31, \"submit\", \"\", \"agent_command\", {:text -> \"hello from test\"})",
                ),
            )
            .await
            .unwrap();

        // The loop calls ui/flush (which calls commit()) before the LLM
        // request, so the first suspend may be a Commit. Wait for the
        // task to complete (the external stream handler sends canned events).
        assert!(
            matches!(submitted.outcome, TaskOutcome::Suspended { .. }),
            "{:?}",
            submitted.outcome
        );
        let mut completed = None;
        for _ in 0..100 {
            for event in driver.drain_events() {
                match event {
                    DriverEvent::TaskCompleted { task_id, value }
                        if task_id == submitted.task_id =>
                    {
                        completed = Some(value);
                        break;
                    }
                    _ => {}
                }
            }
            if completed.is_some() {
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        let value = completed.expect("agent_command task did not complete");
        assert_eq!(value, Value::bool(true));

        let query = driver
            .submit_source(
                ep,
                root_source(
                    "let exactly {:value -> t} = agent/transcript(#agent/default)\n\
                     let role = none\n\
                     let content = none\n\
                     let status = none\n\
                     let response_id = none\n\
                     for message in agent/messages_ordered(t)\n\
                       role = message.messageRole\n\
                       content = message.messageContent\n\
                       let statuses = MessageStatus(message, ?status)\n\
                       if statuses\n\
                         let exactly {:status -> current_status} = statuses\n\
                         status = current_status\n\
                       end\n\
                       let response_ids = MessageResponseId(message, ?response_id)\n\
                       if response_ids\n\
                         let exactly {:response_id -> current_response_id} = response_ids\n\
                         response_id = current_response_id\n\
                       end\n\
                     end\n\
                     return [role, content, status, response_id]",
                ),
            )
            .await
            .unwrap();
        assert!(
            matches!(
                query.outcome,
                TaskOutcome::Complete { ref value, .. } if *value == Value::list([
                    Value::string("assistant"),
                    Value::string("synthetic assistant reply"),
                    Value::string("complete"),
                    Value::string("resp_test"),
                ])
            ),
            "{:?}",
            query.outcome
        );

        let input_query = driver
            .submit_source(
                ep,
                root_source(
                    "let exactly {:value -> t} = agent/transcript(#agent/default)\n\
                     return agent/llm_responses_input(t)",
                ),
            )
            .await
            .unwrap();
        let TaskOutcome::Complete { value, .. } = input_query.outcome else {
            panic!("Responses input query did not complete")
        };
        let input = value.with_list(<[Value]>::to_vec).unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(
            input[1].map_get(&Value::symbol(Symbol::intern("type"))),
            Some(Value::string("message"))
        );

        let streaming_query = driver
            .submit_source(ep, root_source("return endpoint().session/isStreaming"))
            .await
            .unwrap();
        assert!(matches!(
            streaming_query.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::bool(false)
        ));
    });
}

#[test]
fn agent_responses_tool_call_round_trip_resubmits_full_context() {
    crate::test_support::run(async {
        let request_count = Arc::new(AtomicUsize::new(0));
        let stream_handler = {
            let request_count = Arc::clone(&request_count);
            Arc::new(
                move |_,
                      request: mica_runtime::ExternalRequest,
                      emitter: crate::ExternalStreamEmitter| {
                    let turn = request_count.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        assert_eq!(request.service, Symbol::intern("openai_responses"));
                        let input = request
                            .payload
                            .map_get(&Value::symbol(Symbol::intern("input")))
                            .and_then(|value| value.with_list(<[Value]>::to_vec))
                            .unwrap();
                        assert!(
                            request
                                .payload
                                .map_get(&Value::symbol(Symbol::intern("previous_response_id")))
                                .is_none()
                        );

                        if turn == 0 {
                            assert_eq!(input.len(), 1);
                            emitter
                                .emit(Value::map([
                                    (
                                        Value::symbol(Symbol::intern("type")),
                                        Value::symbol(Symbol::intern("tool_call_ready")),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("call_id")),
                                        Value::string("call_test"),
                                    ),
                                    (Value::symbol(Symbol::intern("name")), Value::string("read")),
                                    (
                                        Value::symbol(Symbol::intern("arguments")),
                                        Value::string("{\"path\":\"missing.txt\"}"),
                                    ),
                                ]))
                                .await
                                .unwrap();
                            emitter
                                .emit(Value::map([
                                    (
                                        Value::symbol(Symbol::intern("type")),
                                        Value::symbol(Symbol::intern("completed")),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("response")),
                                        Value::map([
                                            (
                                                Value::symbol(Symbol::intern("id")),
                                                Value::string("resp_tool"),
                                            ),
                                            (
                                                Value::symbol(Symbol::intern("output")),
                                                Value::list([Value::map([
                                                    (
                                                        Value::symbol(Symbol::intern("type")),
                                                        Value::string("function_call"),
                                                    ),
                                                    (
                                                        Value::symbol(Symbol::intern("call_id")),
                                                        Value::string("call_test"),
                                                    ),
                                                    (
                                                        Value::symbol(Symbol::intern("name")),
                                                        Value::string("read"),
                                                    ),
                                                    (
                                                        Value::symbol(Symbol::intern("arguments")),
                                                        Value::string("{\"path\":\"missing.txt\"}"),
                                                    ),
                                                ])]),
                                            ),
                                        ]),
                                    ),
                                ]))
                                .await
                                .unwrap();
                        } else {
                            assert_eq!(turn, 1);
                            assert_eq!(input.len(), 3);
                            assert_eq!(
                                input[1].map_get(&Value::symbol(Symbol::intern("type"))),
                                Some(Value::string("function_call"))
                            );
                            assert_eq!(
                                input[2].map_get(&Value::symbol(Symbol::intern("type"))),
                                Some(Value::string("function_call_output"))
                            );
                            assert_eq!(
                                input[2].map_get(&Value::symbol(Symbol::intern("call_id"))),
                                Some(Value::string("call_test"))
                            );
                            emitter
                                .emit(Value::map([
                                    (
                                        Value::symbol(Symbol::intern("type")),
                                        Value::symbol(Symbol::intern("text_delta")),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("delta")),
                                        Value::string("tool result received"),
                                    ),
                                ]))
                                .await
                                .unwrap();
                            emitter
                                .emit(Value::map([(
                                    Value::symbol(Symbol::intern("type")),
                                    Value::symbol(Symbol::intern("completed")),
                                )]))
                                .await
                                .unwrap();
                        }
                        Value::map([(Value::symbol(Symbol::intern("started")), Value::bool(true))])
                    }) as crate::types::ExternalRequestFuture
                },
            )
        };

        let prior = std::env::var_os("MICA_SOURCE_ROOT");
        unsafe {
            std::env::set_var("MICA_SOURCE_ROOT", "/tmp/agent-tool-test-root");
        }
        let mut runner = SourceRunner::new_empty();
        load_agent_app(&mut runner);
        match prior {
            Some(value) => unsafe { std::env::set_var("MICA_SOURCE_ROOT", value) },
            None => unsafe { std::env::remove_var("MICA_SOURCE_ROOT") },
        }

        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let agent = runner
            .named_identity(Symbol::intern("agent/default"))
            .unwrap();
        let ep = endpoint(42);
        runner
            .open_endpoint_with_context(ep, Some(web), Some(agent), Symbol::intern("web"))
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handlers(
            runner,
            TEST_WORKERS,
            None,
            Some(stream_handler),
        )
        .unwrap();

        let submitted = driver
            .submit_source(
                ep,
                root_source(
                    "return sync_event(endpoint(), none, 31, \"submit\", \"\", \"agent_command\", {:text -> \"use a tool\"})",
                ),
            )
            .await
            .unwrap();
        assert!(
            matches!(submitted.outcome, TaskOutcome::Suspended { .. }),
            "{:?}",
            submitted.outcome
        );

        let mut completed = false;
        for _ in 0..200 {
            completed |= driver.drain_events().iter().any(|event| {
                matches!(event, DriverEvent::TaskCompleted { task_id, .. } if *task_id == submitted.task_id)
            });
            if completed {
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(completed, "agent tool-call task did not complete");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);

        let query = driver
            .submit_source(
                ep,
                root_source(
                    "let exactly {:call -> found} = ToolCallId(?call, \"call_test\")\n\
                     let exactly {:value -> t} = agent/transcript(#agent/default)\n\
                     let last_content = none\n\
                     for message in agent/messages_ordered(t)\n\
                       last_content = message.messageContent\n\
                     end\n\
                     return [found.toolCallStatus, last_content]",
                ),
            )
            .await
            .unwrap();
        let TaskOutcome::Complete { value, .. } = query.outcome else {
            panic!("tool state query did not complete: {:?}", query.outcome)
        };
        assert_eq!(
            value,
            Value::list([
                Value::string("error"),
                Value::string("tool result received"),
            ])
        );
    });
}

#[test]
fn agent_steering_cancels_the_active_stream_and_resubmits_full_context() {
    crate::test_support::run(async {
        let request_count = Arc::new(AtomicUsize::new(0));
        let producer_observed_close = Arc::new(AtomicBool::new(false));
        let stream_handler = {
            let request_count = Arc::clone(&request_count);
            let producer_observed_close = Arc::clone(&producer_observed_close);
            Arc::new(
                move |_,
                      request: mica_runtime::ExternalRequest,
                      emitter: crate::ExternalStreamEmitter| {
                    let turn = request_count.fetch_add(1, Ordering::SeqCst);
                    let producer_observed_close = Arc::clone(&producer_observed_close);
                    Box::pin(async move {
                        let input = request
                            .payload
                            .map_get(&Value::symbol(Symbol::intern("input")))
                            .and_then(|value| value.with_list(<[Value]>::to_vec))
                            .unwrap();
                        if turn == 0 {
                            assert_eq!(input.len(), 1);
                            compio::runtime::spawn(async move {
                                emitter
                                    .emit(Value::map([
                                        (
                                            Value::symbol(Symbol::intern("type")),
                                            Value::symbol(Symbol::intern("text_delta")),
                                        ),
                                        (
                                            Value::symbol(Symbol::intern("delta")),
                                            Value::string("partial"),
                                        ),
                                    ]))
                                    .await
                                    .unwrap();
                                compio::time::sleep(Duration::from_millis(150)).await;
                                let closed = emitter
                                    .emit(Value::map([(
                                        Value::symbol(Symbol::intern("type")),
                                        Value::symbol(Symbol::intern("text_delta")),
                                    )]))
                                    .await
                                    .is_err();
                                producer_observed_close.store(closed, Ordering::SeqCst);
                            })
                            .detach();
                        } else {
                            assert_eq!(input.len(), 3);
                            assert_eq!(
                                input[2].map_get(&Value::symbol(Symbol::intern("content"))),
                                Some(Value::string("change direction"))
                            );
                            emitter
                                .emit(Value::map([
                                    (
                                        Value::symbol(Symbol::intern("type")),
                                        Value::symbol(Symbol::intern("text_delta")),
                                    ),
                                    (
                                        Value::symbol(Symbol::intern("delta")),
                                        Value::string("final reply"),
                                    ),
                                ]))
                                .await
                                .unwrap();
                            emitter
                                .emit(Value::map([(
                                    Value::symbol(Symbol::intern("type")),
                                    Value::symbol(Symbol::intern("completed")),
                                )]))
                                .await
                                .unwrap();
                        }
                        Value::map([(Value::symbol(Symbol::intern("started")), Value::bool(true))])
                    }) as crate::types::ExternalRequestFuture
                },
            )
        };

        let prior = std::env::var_os("MICA_SOURCE_ROOT");
        unsafe {
            std::env::set_var("MICA_SOURCE_ROOT", "/tmp/agent-steering-test-root");
        }
        let mut runner = SourceRunner::new_empty();
        load_agent_app(&mut runner);
        match prior {
            Some(value) => unsafe { std::env::set_var("MICA_SOURCE_ROOT", value) },
            None => unsafe { std::env::remove_var("MICA_SOURCE_ROOT") },
        }
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let agent = runner
            .named_identity(Symbol::intern("agent/default"))
            .unwrap();
        let ep = endpoint(41);
        runner
            .open_endpoint_with_context(ep, Some(web), Some(agent), Symbol::intern("web"))
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handlers(
            runner,
            TEST_WORKERS,
            None,
            Some(stream_handler),
        )
        .unwrap();

        let first = driver
            .submit_source(
                ep,
                root_source(
                    "return sync_event(endpoint(), none, 31, \"submit\", \"\", \"agent_command\", {:text -> \"initial request\"})",
                ),
            )
            .await
            .unwrap();
        assert!(
            matches!(first.outcome, TaskOutcome::Suspended { .. }),
            "{:?}",
            first.outcome
        );

        for _ in 0..100 {
            if request_count.load(Ordering::SeqCst) > 0 {
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        let steering = driver
            .submit_source(
                ep,
                root_source(
                    "return sync_event(endpoint(), none, 31, \"submit\", \"\", \"agent_command\", {:text -> \"change direction\"})",
                ),
            )
            .await
            .unwrap();
        assert!(matches!(steering.outcome, TaskOutcome::Complete { .. }));

        let mut first_completed = false;
        for _ in 0..200 {
            first_completed |= driver.drain_events().iter().any(|event| {
                matches!(event, DriverEvent::TaskCompleted { task_id, .. } if *task_id == first.task_id)
            });
            if first_completed && producer_observed_close.load(Ordering::SeqCst) {
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(first_completed);
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert!(producer_observed_close.load(Ordering::SeqCst));

        let query = driver
            .submit_source(
                ep,
                root_source(
                    "let exactly {:value -> t} = agent/transcript(#agent/default)\n\
                     let rows = []\n\
                     for message in agent/messages_ordered(t)\n\
                       let status = none\n\
                       let statuses = MessageStatus(message, ?status)\n\
                       if statuses\n\
                         let exactly {:status -> current_status} = statuses\n\
                         status = current_status\n\
                       end\n\
                       rows = [@rows, [message.messageRole, message.messageContent, status]]\n\
                     end\n\
                     return rows",
                ),
            )
            .await
            .unwrap();
        assert!(matches!(
            query.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::list([
                Value::list([Value::string("user"), Value::string("initial request"), Value::option_none()]),
                Value::list([Value::string("assistant"), Value::string("partial"), Value::string("cancelled")]),
                Value::list([Value::string("user"), Value::string("change direction"), Value::option_none()]),
                Value::list([Value::string("assistant"), Value::string("final reply"), Value::string("complete")]),
            ])
        ));
    });
}

#[test]
fn mica_query_host_request_runs_read_only_query_and_resumes_task() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "make_identity(:web)\n\
                 make_identity(:lamp)\n\
                 make_relation(:CanRead, 2)\n\
                 make_relation(:CanWrite, 2)\n\
                 make_relation(:ThingName, 2)\n\
                 assert CanRead(#web, :ThingName)\n\
                 assert CanWrite(#web, :ThingName)\n\
                 assert ThingName(#lamp, \"Lamp\")\n",
            )
            .unwrap();
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let endpoint = endpoint(64);
        runner
            .open_endpoint(endpoint, Some(web), Symbol::intern("web"))
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint,
                root_source(
                    "return mica_query(\"let exactly {:name -> name} = ThingName(#lamp, ?name)\\nreturn name\", {:max_output_chars -> 100})",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::ExternalRequest(request),
                ..
            } if request.timeout_millis == Some(5_000)
        ));

        let mut completed = None;
        for _ in 0..50 {
            for event in driver.drain_events() {
                if let DriverEvent::TaskCompleted { task_id, value } = event
                    && task_id == submitted.task_id
                {
                    completed = Some(value);
                    break;
                }
            }
            if completed.is_some() {
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        let value = completed.expect("mica_query task did not complete");
        assert_eq!(
            value.map_get(&Value::symbol(Symbol::intern("status"))),
            Some(Value::string("complete"))
        );
        assert_eq!(
            value.map_get(&Value::symbol(Symbol::intern("value"))),
            Some(Value::option_some(Value::string("Lamp")))
        );
        assert_eq!(
            value.map_get(&Value::symbol(Symbol::intern("rendered"))),
            Some(Value::string("\"Lamp\""))
        );
    });
}

#[test]
fn root_startup_source_can_resume_vllm_embed_text() {
    crate::test_support::run(async {
        let handler = Arc::new(|_, request: mica_runtime::ExternalRequest| {
            Box::pin(async move {
                assert_eq!(request.service, Symbol::intern("embedding"));
                Value::list([Value::float(1.0).unwrap(), Value::float(0.0).unwrap()])
            }) as crate::types::ExternalRequestFuture
        });
        let runner = SourceRunner::new_empty_with_embedding_provider(EmbeddingProviderKind::Vllm);
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            runner,
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let report = driver
            .submit_root_source_report(
                "return embed_text(\"source-workspace\", \"lamp\")".to_owned(),
            )
            .await
            .unwrap();

        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == report.task_id
                    && *value == Value::list([Value::float(1.0).unwrap(), Value::float(0.0).unwrap()])
        )));
    });
}

#[test]
fn external_request_requires_effect_authority() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let request = TaskRequest {
            principal: None,
            actor: None,
            endpoint: endpoint(31),
            authority: AuthorityContext::empty(),
            input: TaskInput::Source("return external_request(:echo, \"hello\")".to_owned()),
        };

        let denied = driver
            .submit_source(endpoint(31), request)
            .await
            .unwrap_err();
        assert!(driver.format_error(&denied).contains("permission denied"));
    });
}

#[test]
fn log_requires_effect_authority() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let request = TaskRequest {
            principal: None,
            actor: None,
            endpoint: endpoint(33),
            authority: AuthorityContext::empty(),
            input: TaskInput::Source("return log(:info, \"hello\")".to_owned()),
        };

        let denied = driver
            .submit_source(endpoint(33), request)
            .await
            .unwrap_err();
        assert!(driver.format_error(&denied).contains("permission denied"));
    });
}

#[test]
fn log_returns_unit_with_effect_authority() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(34), root_source("return log(:debug, \"hello\")"))
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::unit()
        ));
    });
}

#[test]
fn external_request_timeout_resumes_with_error_value() {
    crate::test_support::run(async {
        let dropped = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::new(Mutex::new(None));
        let handler = {
            let dropped = Arc::clone(&dropped);
            let cancellation = Arc::clone(&cancellation);
            Arc::new(
                move |context: crate::ExternalRequestContext,
                      _request: mica_runtime::ExternalRequest| {
                    *cancellation.lock().unwrap() = Some(context.cancellation.clone());
                    let drop_flag = DropFlag(Arc::clone(&dropped));
                    Box::pin(async move {
                        let _drop_flag = drop_flag;
                        pending().await
                    }) as crate::types::ExternalRequestFuture
                },
            )
        };
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            SourceRunner::new_empty(),
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let submitted = driver
            .submit_source(
                endpoint(32),
                root_source("return external_request(:slow, \"hello\", 0.001)"),
            )
            .await
            .unwrap();
        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && value.error_code_symbol() == Some(Symbol::intern("ExternalTimeout"))
        )));
        assert!(dropped.load(Ordering::Acquire));
        assert!(
            cancellation
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(crate::ExternalRequestCancellation::is_cancelled)
        );
    });
}

#[test]
fn commit_yields_and_immediately_resumes_task() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(3), root_source("commit()\nreturn \"committed\""))
            .await
            .unwrap();
        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Commit,
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && *value == Value::string("committed")
        )));
    });
}

#[test]
fn spawn_commits_parent_and_runs_child_task() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "make_relation(:Seen, 1)\n\
                 verb child(endpoint)\n\
                   if Seen(:parent)\n\
                     emit(endpoint, \"saw parent\")\n\
                   else\n\
                     emit(endpoint, \"missed parent\")\n\
                   end\n\
                   return ()\n\
                 end\n",
            )
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint(31),
                root_source(
                    "assert Seen(:parent)\n\
                     let child = spawn :child(endpoint: endpoint()) after 0.001\n\
                     return child",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Spawn(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        let events = driver.drain_events();
        let child_task_id = events.iter().find_map(|event| match event {
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && value.as_int().is_some() =>
            {
                Some(value.as_int().unwrap() as u64)
            }
            _ => None,
        });
        let child_task_id = child_task_id.expect("parent completed with spawned child task id");
        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::Effect(effect)
                if effect.task_id == child_task_id && effect.value == Value::string("saw parent")
        )));
    });
}

#[test]
fn spawn_runs_receiver_positional_child_task() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        let coin = runner.run_source("return make_identity(:coin)").unwrap();
        let TaskOutcome::Complete { value: coin, .. } = coin.outcome else {
            panic!("expected coin identity creation to complete");
        };
        let alice = runner.run_source("return make_identity(:alice)").unwrap();
        let TaskOutcome::Complete { value: alice, .. } = alice.outcome else {
            panic!("expected alice identity creation to complete");
        };
        runner
            .run_filein(
                "verb parent()\n\
                 let child = spawn #coin:inspect(#alice) after 0\n\
                 return child\n\
               end\n\
               verb inspect(receiver, actor)\n\
                 emit(endpoint(), [receiver, actor])\n\
                 return ()\n\
               end\n",
            )
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(32), root_source("return :parent()"))
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Spawn(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::Effect(effect)
                if effect.value == Value::list([coin.clone(), alice.clone()])
        )));
    });
}

#[test]
fn endpoint_input_resumes_reading_task() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let endpoint = endpoint(4);
        let submitted = driver
            .submit_source(endpoint, root_source("return read(:line)"))
            .await
            .unwrap();
        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::WaitingForInput(_),
                ..
            }
        ));

        let outcomes = driver.input(endpoint, Value::string("look")).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            &outcomes[0],
            TaskOutcome::Complete { value, .. } if *value == Value::string("look")
        ));
    });
}

#[test]
fn mailbox_recv_drains_messages_sent_before_wait() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "verb send_reply(reply)\n\
                 mailbox_send(reply, \"done\")\n\
                 return ()\n\
               end\n",
            )
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint(32),
                root_source(
                    "let caps = mailbox()\n\
                     let rx = caps[0]\n\
                     let tx = caps[1]\n\
                     let child = spawn :send_reply(reply: tx) after 0\n\
                     return mailbox_recv([rx], 1)",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Spawn(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && value.with_list(|groups| groups.len()) == Some(1)
                    && value.with_list(|groups| groups[0].with_list(|group| {
                        group.len() == 2 && group[1] == Value::list([Value::string("done")])
                    })) == Some(Some(true))
        )));
    });
}

#[test]
fn mailbox_recv_waits_until_sender_commits() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "verb delayed_send(reply)\n\
                 suspend(0.001)\n\
                 mailbox_send(reply, \"late\")\n\
                 return ()\n\
               end\n",
            )
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint(33),
                root_source(
                    "let caps = mailbox()\n\
                     let rx = caps[0]\n\
                     let tx = caps[1]\n\
                     let child = spawn :delayed_send(reply: tx) after 0\n\
                     return mailbox_recv([rx], 1)",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));

        compio::time::sleep(Duration::from_millis(30)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id
                    && value.with_list(|groups| groups.len()) == Some(1)
                    && value.with_list(|groups| groups[0].with_list(|group| {
                        group.len() == 2 && group[1] == Value::list([Value::string("late")])
                    })) == Some(Some(true))
        )));
    });
}

#[test]
fn relation_subscription_delivery_wakes_mailbox_receiver_after_publication() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner.run_source("make_relation(:Observed, 1)").unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let subscriber = driver
            .submit_source(
                endpoint(35),
                root_source(
                    "let caps = mailbox()\n\
                     let subscription = subscribe_changes(caps[1], :relation, some(:Observed), [none], :changes)\n\
                     commit()\n\
                     return mailbox_recv([caps[0]], 1)",
                ),
            )
            .await
            .unwrap();
        assert!(matches!(
            subscriber.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Commit,
                ..
            }
        ));
        compio::time::sleep(Duration::from_millis(20)).await;

        let writer = driver
            .submit_source(endpoint(36), root_source("assert Observed(1)"))
            .await
            .unwrap();
        assert!(matches!(writer.outcome, TaskOutcome::Complete { .. }));
        compio::time::sleep(Duration::from_millis(20)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == subscriber.task_id
                    && value.with_list(|groups| groups.len()) == Some(1)
                    && value.with_list(|groups| groups[0].with_list(|group| {
                        group.len() == 2
                            && group[1].with_list(|messages| messages.len()) == Some(1)
                    })) == Some(Some(true))
        )));
    });
}

#[test]
fn relation_subscription_delivery_notifies_external_driver_mailbox() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_source(
                "make_identity(:observer)\n\
                 make_relation(:Observed, 1)\n\
                 make_relation(:CanRead, 2)\n\
                 assert CanRead(#observer, :Observed)",
            )
            .unwrap();
        let observer = runner.named_identity(Symbol::intern("observer")).unwrap();
        let observed = runner.named_relation(Symbol::intern("Observed")).unwrap().0;
        let observer_endpoint = endpoint(37);
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        driver
            .open_endpoint(observer_endpoint, Some(observer), Symbol::intern("test"))
            .unwrap();
        let mailbox = driver.create_subscription_mailbox().unwrap();
        let subscription = driver
            .register_subscription_for_endpoint(
                observer_endpoint,
                &mailbox,
                DriverSubscriptionRequest {
                    subject: SubscriptionSubject::Relation {
                        relation: observed,
                        bindings: vec![None],
                    },
                    initial_delivery: SubscriptionInitialDelivery::ChangesOnly,
                    cursor: None,
                    queue_budget: None,
                },
            )
            .await
            .unwrap();

        driver
            .submit_source(endpoint(38), root_source("assert Observed(1)"))
            .await
            .unwrap();
        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::SubscriptionReady { mailbox: ready } if *ready == mailbox.id()
        )));
        let messages = driver.drain_subscription_mailbox(&mailbox).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].with_map(|message| {
                message.iter().find_map(|(key, value)| {
                    (key == &Value::symbol(Symbol::intern("kind"))).then(|| value.clone())
                })
            }),
            Some(Some(Value::symbol(Symbol::intern("changes"))))
        );

        driver
            .cancel_subscription_for_endpoint(observer_endpoint, subscription)
            .unwrap();
        driver
            .submit_source(endpoint(39), root_source("assert Observed(2)"))
            .await
            .unwrap();
        assert!(!driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::SubscriptionReady { mailbox: ready } if *ready == mailbox.id()
        )));
        assert!(
            driver
                .drain_subscription_mailbox(&mailbox)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn mailbox_recv_zero_timeout_returns_empty_list() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint(34),
                root_source(
                    "let caps = mailbox()\n\
                     return mailbox_recv([caps[0]], 0)",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::MailboxRecv(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(5)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && *value == Value::list([])
        )));
    });
}

#[test]
fn mailbox_recv_reports_which_mailbox_is_ready() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(
                endpoint(36),
                root_source(
                    "let first = mailbox()\n\
                     let second = mailbox()\n\
                     mailbox_send(second[1], \"second\")\n\
                     let ready = mailbox_recv([first[0], second[0]], 0)\n\
                     return ready[0][0] == second[0] && ready[0][1][0] == \"second\"",
                ),
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::MailboxRecv(_),
                ..
            }
        ));

        compio::time::sleep(Duration::from_millis(5)).await;

        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, value }
                if *task_id == submitted.task_id && *value == Value::bool(true)
        )));
    });
}

#[test]
fn mailbox_caps_are_directional() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let error = driver
            .submit_source(
                endpoint(35),
                root_source(
                    "let caps = mailbox()\n\
                     return mailbox_recv([caps[1]], 0)",
                ),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error.source(),
            Some(SourceTaskError::TaskManager(TaskManagerError::Task(
                TaskError::Runtime(RuntimeError::InvalidMailboxCapability {
                    operation: "recv",
                    ..
                })
            )))
        ));
    });
}

#[test]
fn driver_submit_source_sets_endpoint_context() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let endpoint = endpoint(5);
        let submitted = driver
            .submit_source(endpoint, root_source("return endpoint()"))
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::identity(endpoint)
        ));
    });
}

#[test]
fn driver_runs_bounded_read_only_source_query_as_endpoint_actor() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "make_identity(:web)\n\
                 make_identity(:lamp)\n\
                 make_relation(:CanRead, 2)\n\
                 make_relation(:CanWrite, 2)\n\
                 make_relation(:ThingName, 2)\n\
                 assert CanRead(#web, :ThingName)\n\
                 assert CanWrite(#web, :ThingName)\n\
                 assert ThingName(#lamp, \"Lamp\")\n",
            )
            .unwrap();
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let endpoint = endpoint(61);
        runner
            .open_endpoint(endpoint, Some(web), Symbol::intern("web"))
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();

        let report = driver
            .run_read_only_source_query(
                endpoint,
                "let names = []\n\
                 for found in ThingName(?thing, ?name)\n\
                   names = [@names, found[:name]]\n\
                 end\n\
                 return names"
                    .to_owned(),
                ReadOnlySourceQueryOptions::default(),
            )
            .await
            .unwrap();

        assert_eq!(report.status, ReadOnlySourceQueryStatus::Complete);
        assert_eq!(report.value, Some(Value::list([Value::string("Lamp")])));
        assert_eq!(report.rendered, "[\"Lamp\"]");
        assert!(!report.rendered_truncated);
        assert_eq!(report.diagnostics, Vec::<String>::new());
    });
}

#[test]
fn driver_read_only_source_query_rejects_mutation_and_effects() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "make_identity(:web)\n\
                 make_identity(:lamp)\n\
                 make_relation(:CanRead, 2)\n\
                 make_relation(:CanWrite, 2)\n\
                 make_relation(:ThingName, 2)\n\
                 assert CanRead(#web, :ThingName)\n\
                 assert CanWrite(#web, :ThingName)\n\
                 assert ThingName(#lamp, \"Lamp\")\n",
            )
            .unwrap();
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let endpoint = endpoint(62);
        runner
            .open_endpoint(endpoint, Some(web), Symbol::intern("web"))
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();

        let mutation = driver
            .run_read_only_source_query(
                endpoint,
                "assert ThingName(#lamp, \"Desk\")\nreturn 1".to_owned(),
                ReadOnlySourceQueryOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(mutation.status, ReadOnlySourceQueryStatus::Rejected);
        assert!(mutation.task_id.is_none());
        assert!(
            mutation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("cannot assert or retract facts"))
        );

        let effect = driver
            .run_read_only_source_query(
                endpoint,
                "return log(:info, \"hello\")".to_owned(),
                ReadOnlySourceQueryOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(effect.status, ReadOnlySourceQueryStatus::Rejected);
        assert!(
            effect
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("cannot call `log`"))
        );

        let spawn = driver
            .run_read_only_source_query(
                endpoint,
                "let child = spawn :inspect(#lamp) after 0\nreturn child".to_owned(),
                ReadOnlySourceQueryOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(spawn.status, ReadOnlySourceQueryStatus::Rejected);
        assert!(
            spawn
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("cannot spawn tasks"))
        );

        let dispatch = driver
            .run_read_only_source_query(
                endpoint,
                "return #lamp:inspect(#web)".to_owned(),
                ReadOnlySourceQueryOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(dispatch.status, ReadOnlySourceQueryStatus::Rejected);
        assert!(
            dispatch
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("cannot invoke methods"))
        );
    });
}

#[test]
fn driver_read_only_source_query_bounds_rendered_output() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein("make_identity(:web)\nmake_relation(:CanRead, 2)\n")
            .unwrap();
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let endpoint = endpoint(63);
        runner
            .open_endpoint(endpoint, Some(web), Symbol::intern("web"))
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();

        let report = driver
            .run_read_only_source_query(
                endpoint,
                "return \"abcdef\"".to_owned(),
                ReadOnlySourceQueryOptions {
                    max_output_chars: 3,
                    ..ReadOnlySourceQueryOptions::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(report.status, ReadOnlySourceQueryStatus::Complete);
        assert_eq!(report.value, Some(Value::string("abcdef")));
        assert!(report.rendered_truncated);
        assert!(report.rendered.ends_with("... truncated"));
    });
}

#[test]
fn driver_submit_invocation_overrides_request_endpoint_context() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "verb report_endpoint(endpoint)\n\
                   return endpoint\n\
                 end\n",
            )
            .unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let actual_endpoint = endpoint(6);
        let stale_endpoint = endpoint(7);

        let submitted = driver
            .submit_invocation(
                actual_endpoint,
                TaskRequest {
                    principal: None,
                    actor: None,
                    endpoint: stale_endpoint,
                    authority: AuthorityContext::root(),
                    input: TaskInput::Invocation {
                        selector: Symbol::intern("report_endpoint"),
                        roles: Vec::new(),
                    },
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            submitted.outcome,
            TaskOutcome::Complete { value, .. } if value == Value::identity(actual_endpoint)
        ));
    });
}

#[test]
fn driver_routes_actor_effects_to_open_endpoints() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner.run_source("make_identity(:alice)").unwrap();
        let alice = Identity::new(0x00e0_0000_0000_0000).unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let endpoint = endpoint(10);
        driver
            .open_endpoint(endpoint, Some(alice), Symbol::intern("telnet"))
            .unwrap();

        let submitted = driver
            .submit_source(endpoint, root_source("emit(#alice, \"hello\")"))
            .await
            .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Complete { .. }));
        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::Effect(effect)
                if effect.task_id == submitted.task_id
                    && effect.target == endpoint
                    && effect.value == Value::string("hello")
        )));
    });
}

#[test]
fn driver_stops_routing_after_endpoint_close() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner.run_source("make_identity(:alice)").unwrap();
        let alice = Identity::new(0x00e0_0000_0000_0000).unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let endpoint = endpoint(11);
        driver
            .open_endpoint(endpoint, Some(alice), Symbol::intern("telnet"))
            .unwrap();
        let report = driver.close_endpoint(endpoint).await;
        assert_eq!(report.relation_changes, 4);
        assert!(report.cancelled_tasks.is_empty());

        let error = driver
            .submit_source(endpoint, root_source("emit(#alice, \"hello\")"))
            .await
            .unwrap_err();
        assert!(matches!(error, DriverError::EndpointClosed(closed) if closed == endpoint));
    });
}

#[test]
fn driver_routes_endpoint_input() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let endpoint = endpoint(27);
        let submitted = driver
            .submit_source(endpoint, root_source("return read(:line)"))
            .await
            .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));
        let outcomes = driver
            .input(endpoint, Value::string("north"))
            .await
            .unwrap();

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            &outcomes[0],
            TaskOutcome::Complete { value, .. } if *value == Value::string("north")
        ));
    });
}

#[test]
fn driver_routes_actor_effects_to_open_endpoints_after_setup() {
    crate::test_support::run(async {
        let mut runner = SourceRunner::new_empty();
        runner.run_source("make_identity(:alice)").unwrap();
        let alice = Identity::new(0x00e0_0000_0000_0000).unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, TEST_WORKERS).unwrap();
        let endpoint = endpoint(28);
        driver
            .open_endpoint(endpoint, Some(alice), Symbol::intern("telnet"))
            .unwrap();

        let submitted = driver
            .submit_source(endpoint, root_source("emit(#alice, \"hello\")"))
            .await
            .unwrap();

        assert!(matches!(submitted.outcome, TaskOutcome::Complete { .. }));
        let events = driver.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::Effect(effect)
                if effect.task_id == submitted.task_id
                    && effect.target == endpoint
                    && effect.value == Value::string("hello")
        )));
        assert_eq!(driver.close_endpoint(endpoint).await.relation_changes, 4);
    });
}

#[test]
fn endpoint_close_cancels_suspended_tasks() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let endpoint = endpoint(40);
        driver
            .open_endpoint(endpoint, None, Symbol::intern("shell"))
            .unwrap();
        let submitted = driver
            .submit_source(endpoint, root_source("return read(:line)"))
            .await
            .unwrap();
        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));

        let report = driver.close_endpoint(endpoint).await;

        assert_eq!(report.relation_changes, 3);
        assert_eq!(report.cancelled_tasks, vec![submitted.task_id]);
        assert_eq!(driver.inner_runner().suspended_len(), 0);
        assert_eq!(driver.inner_runner().cancelled_len(), 0);
        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCancelled { task_id, reason }
                if *task_id == submitted.task_id
                    && *reason == TaskCancellationReason::EndpointClosed
        )));
        assert!(matches!(
            driver.resume(submitted.task_id, Value::unit()).await,
            Err(DriverError::TaskCancelled(task_id)) if task_id == submitted.task_id
        ));
    });
}

#[test]
fn explicit_cancellation_stops_timed_resume() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let submitted = driver
            .submit_source(endpoint(41), root_source("suspend(0.01)\nreturn 1"))
            .await
            .unwrap();
        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));

        driver.cancel_task(submitted.task_id).await.unwrap();
        compio::time::sleep(Duration::from_millis(20)).await;

        let events = driver.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::TaskCancelled { task_id, reason }
                if *task_id == submitted.task_id
                    && *reason == TaskCancellationReason::Requested
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            DriverEvent::TaskCompleted { task_id, .. } | DriverEvent::TaskFailed { task_id, .. }
                if *task_id == submitted.task_id
        )));
    });
}

#[test]
fn event_queue_backpressures_producers_without_losing_terminal_events() {
    crate::test_support::run(async {
        let mut resources = DriverResources::new(TEST_WORKERS.unwrap());
        resources.event_queue_capacity = NonZeroUsize::new(1).unwrap();
        let driver =
            CompioTaskDriver::spawn_with_resources(SourceRunner::new_empty(), resources).unwrap();

        let first = driver
            .submit_source(endpoint(43), root_source("return 1"))
            .await
            .unwrap();
        let second_driver = driver.clone();
        let second = compio::runtime::spawn(async move {
            second_driver
                .submit_source(endpoint(43), root_source("return 2"))
                .await
        });
        compio::time::sleep(Duration::from_millis(10)).await;

        assert!(!second.is_finished());
        assert!(matches!(
            driver.drain_events().as_slice(),
            [DriverEvent::TaskCompleted { task_id, .. }] if *task_id == first.task_id
        ));

        let second = second.await.unwrap().unwrap();
        assert!(matches!(
            driver.drain_events().as_slice(),
            [DriverEvent::TaskCompleted { task_id, .. }] if *task_id == second.task_id
        ));
    });
}

#[test]
fn driver_enforces_task_and_terminal_resource_budgets() {
    crate::test_support::run(async {
        let mut resources = DriverResources::new(TEST_WORKERS.unwrap());
        resources.active_task_capacity = NonZeroUsize::new(1).unwrap();
        resources.suspended_task_capacity = NonZeroUsize::new(1).unwrap();
        resources.timer_capacity = NonZeroUsize::new(1).unwrap();
        resources.terminal_task_retention = NonZeroUsize::new(2).unwrap();
        let driver =
            CompioTaskDriver::spawn_with_resources(SourceRunner::new_empty(), resources).unwrap();

        let suspended = driver
            .submit_source(endpoint(44), root_source("return suspend()"))
            .await
            .unwrap();
        let rejected = driver
            .submit_source(endpoint(44), root_source("return 2"))
            .await
            .unwrap();
        let events = driver.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::TaskFailed { task_id, error }
                if *task_id == rejected.task_id && error.contains("active task capacity")
        )));
        assert_eq!(driver.resource_snapshot().active_tasks, 1);

        driver.cancel_task(suspended.task_id).await.unwrap();
        driver.drain_events();
        for value in 0..4 {
            driver
                .submit_source(endpoint(44), root_source(&format!("return {value}")))
                .await
                .unwrap();
            driver.drain_events();
        }

        let snapshot = driver.resource_snapshot();
        assert_eq!(snapshot.active_tasks, 0);
        assert_eq!(snapshot.suspended_tasks, 0);
        assert_eq!(snapshot.retained_terminal_tasks, 2);
        assert_eq!(driver.inner_runner().completed_len(), 0);
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn driver_rejects_suspensions_above_timer_budget() {
    crate::test_support::run(async {
        let mut resources = DriverResources::new(TEST_WORKERS.unwrap());
        resources.active_task_capacity = NonZeroUsize::new(4).unwrap();
        resources.suspended_task_capacity = NonZeroUsize::new(4).unwrap();
        resources.timer_capacity = NonZeroUsize::new(1).unwrap();
        let driver =
            CompioTaskDriver::spawn_with_resources(SourceRunner::new_empty(), resources).unwrap();

        let first = driver
            .submit_source(endpoint(45), root_source("suspend(60)\nreturn 1"))
            .await
            .unwrap();
        let second = driver
            .submit_source(endpoint(45), root_source("suspend(60)\nreturn 2"))
            .await
            .unwrap();
        let events = driver.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::TaskFailed { task_id, error }
                if *task_id == second.task_id && error.contains("timer capacity")
        )));
        let snapshot = driver.resource_snapshot();
        assert_eq!(snapshot.active_tasks, 1);
        assert_eq!(snapshot.suspended_tasks, 1);
        assert_eq!(snapshot.timers, 1);

        driver.cancel_task(first.task_id).await.unwrap();
        driver.drain_events();
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn driver_rejects_suspensions_above_suspended_task_budget() {
    crate::test_support::run(async {
        let mut resources = DriverResources::new(TEST_WORKERS.unwrap());
        resources.active_task_capacity = NonZeroUsize::new(4).unwrap();
        resources.suspended_task_capacity = NonZeroUsize::new(1).unwrap();
        resources.timer_capacity = NonZeroUsize::new(4).unwrap();
        let driver =
            CompioTaskDriver::spawn_with_resources(SourceRunner::new_empty(), resources).unwrap();

        let first = driver
            .submit_source(endpoint(46), root_source("return suspend()"))
            .await
            .unwrap();
        let second = driver
            .submit_source(endpoint(46), root_source("return suspend()"))
            .await
            .unwrap();
        let events = driver.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            DriverEvent::TaskFailed { task_id, error }
                if *task_id == second.task_id && error.contains("suspended task capacity")
        )));
        let snapshot = driver.resource_snapshot();
        assert_eq!(snapshot.active_tasks, 1);
        assert_eq!(snapshot.suspended_tasks, 1);

        driver.cancel_task(first.task_id).await.unwrap();
        driver.drain_events();
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn shutdown_cancels_async_workers_and_joins_dispatcher() {
    crate::test_support::run(async {
        let handler = Arc::new(|_, _: mica_runtime::ExternalRequest| {
            Box::pin(pending()) as crate::types::ExternalRequestFuture
        });
        let driver = CompioTaskDriver::spawn_with_workers_and_external_handler(
            SourceRunner::new_empty(),
            TEST_WORKERS,
            Some(handler),
        )
        .unwrap();
        let endpoint = endpoint(42);
        driver
            .open_endpoint(endpoint, None, Symbol::intern("shell"))
            .unwrap();
        let submitted = driver
            .submit_source(
                endpoint,
                root_source("return external_request(:pending, none)"),
            )
            .await
            .unwrap();
        assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));

        driver.shutdown().await.unwrap();

        assert!(driver.is_shutdown());
        assert_eq!(driver.inner_runner().suspended_len(), 0);
        assert!(driver.drain_events().iter().any(|event| matches!(
            event,
            DriverEvent::TaskCancelled { task_id, reason }
                if *task_id == submitted.task_id
                    && *reason == TaskCancellationReason::DriverShutdown
        )));
        assert!(matches!(
            driver
                .submit_source(endpoint, root_source("return 1"))
                .await,
            Err(DriverError::DriverStopped)
        ));
        driver.shutdown().await.unwrap();
    });
}

#[test]
fn driver_can_be_constructed_and_shutdown_repeatedly() {
    crate::test_support::run(async {
        for _ in 0..8 {
            let driver =
                CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS)
                    .unwrap();
            driver.shutdown().await.unwrap();
            assert!(driver.is_shutdown());
        }
    });
}

#[test]
fn driver_checks_installs_replaces_and_files_out_units() {
    crate::test_support::run(async {
        let driver =
            CompioTaskDriver::spawn_with_workers(SourceRunner::new_empty(), TEST_WORKERS).unwrap();
        let unit = Symbol::intern("equipment");
        let include_loader = Arc::new(|path: &str| match path {
            "label.txt" => Ok("original".to_owned()),
            _ => Err(format!("unknown include {path}")),
        });

        driver
            .filein_unit(
                unit,
                "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 assert Label(#sensor, include_text(\"label.txt\"))\n"
                    .to_owned(),
                FileinMode::Add,
                Some(include_loader),
            )
            .await
            .unwrap();
        driver
            .check_filein("make_identity(:checked_only)".to_owned(), None)
            .await
            .unwrap();
        assert!(
            driver
                .named_identity(Symbol::intern("checked_only"))
                .is_err()
        );

        driver
            .filein_unit(
                unit,
                "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 assert Label(#sensor, \"replacement\")\n"
                    .to_owned(),
                FileinMode::Replace,
                None,
            )
            .await
            .unwrap();

        let source = driver.fileout_unit(unit).await.unwrap();
        assert!(source.contains("replacement"));
        assert!(!source.contains("original"));
        driver.shutdown().await.unwrap();
    });
}
