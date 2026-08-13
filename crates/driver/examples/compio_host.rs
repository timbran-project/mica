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

use mica_driver::{
    CompioTaskDriver, DriverEvent, DriverResources, ExternalRequestFuture, ExternalRequestHandler,
    Symbol, TaskOutcome, Value,
};
use std::error::Error;
use std::num::NonZeroUsize;
use std::sync::Arc;

const HOST_UNIT: &str = r#"
make_identity(:shell)
make_relation(:CanInvoke, 2)
make_relation(:CanEffect, 1)
assert CanInvoke(#shell, :native_round_trip)
assert CanEffect(#shell)

verb native_round_trip(actor @ #shell, text)
  emit(actor, {:kind -> :redraw, :text -> text})
  return external_request(:native_echo, text, 1)
end
"#;

fn main() -> Result<(), Box<dyn Error>> {
    compio::runtime::Runtime::new()?.block_on(run())
}

async fn run() -> Result<(), Box<dyn Error>> {
    let resources = DriverResources::new(NonZeroUsize::new(2).unwrap());
    let external_handler: ExternalRequestHandler = Arc::new(|context, request| {
        Box::pin(async move {
            assert_eq!(request.service, Symbol::intern("native_echo"));
            println!("native request from endpoint {:?}", context.endpoint);
            request.payload
        }) as ExternalRequestFuture
    });
    let driver = CompioTaskDriver::builder(resources)
        .initial_filein(HOST_UNIT, None)
        .external_request_handler(external_handler)
        .build()?;

    let endpoint = driver.allocate_ephemeral_identity()?;
    let actor = driver.named_identity(Symbol::intern("shell"))?;
    driver.open_endpoint(endpoint, Some(actor), Symbol::intern("native-shell"))?;

    // Translate one native input action into a Mica named-role invocation.
    let submitted = driver
        .submit_invocation_for_endpoint(
            endpoint,
            Symbol::intern("native_round_trip"),
            vec![(
                Symbol::intern("text"),
                Value::string("hello from native input"),
            )],
        )
        .await?;
    assert!(matches!(submitted.outcome, TaskOutcome::Suspended { .. }));

    let result = 'events: loop {
        for event in driver.wait_events().await {
            match event {
                DriverEvent::Effect(effect) => {
                    // A graphical host would invalidate or redraw native state here.
                    println!("redraw: {}", driver.format_value(&effect.value));
                }
                DriverEvent::TaskCompleted { task_id, value } if task_id == submitted.task_id => {
                    break 'events value;
                }
                DriverEvent::TaskAborted { task_id, error } if task_id == submitted.task_id => {
                    return Err(
                        format!("Mica task aborted: {}", driver.format_value(&error)).into(),
                    );
                }
                DriverEvent::TaskFailed { task_id, error } if task_id == submitted.task_id => {
                    return Err(format!("Mica task failed: {error}").into());
                }
                DriverEvent::TaskCancelled { task_id, reason } if task_id == submitted.task_id => {
                    return Err(format!("Mica task was cancelled: {reason:?}").into());
                }
                _ => {}
            }
        }
    };
    println!("completed: {}", driver.format_value(&result));

    driver.close_endpoint(endpoint).await;
    driver.shutdown().await?;
    Ok(())
}
