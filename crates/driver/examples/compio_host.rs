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
    DriverEvent, DriverOwner, DriverResources, EndpointConfiguration, ExternalRequestFuture,
    ExternalRequestHandler, InvocationOutcome, Symbol, Value,
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
    let mut owner = DriverOwner::builder(resources)
        .initial_filein(HOST_UNIT, None)
        .external_request_handler(external_handler)
        .build()?;
    let mut event_pump = owner.take_event_pump()?;
    let client = owner.client();

    let actor = client.named_identity(Symbol::intern("shell"))?;
    let endpoint = client
        .open_endpoint(EndpointConfiguration::new(Symbol::intern("native-shell")).actor(actor))?;

    // Translate one native input action into a Mica named-role invocation.
    let invocation = endpoint
        .invoke(
            Symbol::intern("native_round_trip"),
            vec![(
                Symbol::intern("text"),
                Value::string("hello from native input"),
            )],
        )
        .await?;

    let outcome = event_pump
        .drive_invocation(&invocation, |event| {
            if let DriverEvent::Effect(effect) = event {
                // A graphical host would invalidate or redraw native state here.
                println!("redraw: {}", client.format_value(&effect.value));
            }
        })
        .await;
    let result = match outcome {
        InvocationOutcome::Completed(value) => value,
        InvocationOutcome::Aborted(error) => {
            return Err(format!("Mica task aborted: {}", client.format_value(&error)).into());
        }
        InvocationOutcome::Failed(error) => return Err(format!("Mica task failed: {error}").into()),
        InvocationOutcome::Cancelled(reason) => {
            return Err(format!("Mica task was cancelled: {reason:?}").into());
        }
    };
    println!("completed: {}", client.format_value(&result));

    endpoint.close_with_pump(&mut event_pump, |_| {}).await?;
    owner.shutdown(&mut event_pump, |_| {}).await?;
    Ok(())
}
