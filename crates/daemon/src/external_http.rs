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
use std::sync::Arc;
use std::time::Instant;

pub fn handler() -> ExternalRequestHandler {
    Arc::new(move |_, request| {
        Box::pin(async move {
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
