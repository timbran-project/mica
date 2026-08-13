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

use crate::codec::{HttpCodec, HttpResponse, encode_response};
use crate::metrics::{HttpRequestKind, connection_ended, connection_started};
use crate::request::handle_in_process_request;
use crate::response::{error_response, route_request};
use crate::sync::{self, SyncRequestKind};
use crate::{InProcessWebHost, RequestBinding, format_driver_error};
use compio::io::{AsyncRead, AsyncWriteExt};
use compio::net::{TcpListener, TcpStream};
use mica_driver::EndpointConfiguration;
use mica_var::Symbol;
use std::sync::Arc;

pub async fn serve(listener: TcpListener, max_connections: Option<usize>) -> Result<(), String> {
    let mut accepted = 0usize;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("failed to accept connection: {error}"))?;
        connection_started();
        compio::runtime::spawn(async move {
            if let Err(error) = handle_connection(stream).await {
                tracing::warn!(error = %error, "HTTP connection failed");
            }
            connection_ended();
        })
        .detach();
        accepted += 1;
        if max_connections.is_some_and(|max| accepted >= max) {
            break;
        }
    }
    Ok(())
}

pub async fn serve_in_process(
    listener: TcpListener,
    host: InProcessWebHost,
    binding: RequestBinding,
    max_connections: Option<usize>,
) -> Result<(), String> {
    let host = Arc::new(host);
    let mut accepted = 0usize;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("failed to accept connection: {error}"))?;
        connection_started();
        let host = host.clone();
        let binding = binding.clone();
        compio::runtime::spawn(async move {
            if let Err(error) = handle_in_process_connection(stream, host, binding).await {
                tracing::warn!(error = %error, "HTTP connection failed");
            }
            connection_ended();
        })
        .detach();
        accepted += 1;
        if max_connections.is_some_and(|max| accepted >= max) {
            break;
        }
    }
    Ok(())
}

async fn handle_connection(mut stream: TcpStream) -> Result<(), String> {
    let mut codec = HttpCodec::new();
    loop {
        let (result, buffer) = stream.read([0u8; 8192]).await.into();
        let bytes = result.map_err(|error| {
            crate::metrics::metrics().connection_read_errors.inc();
            format!("failed to read from connection: {error}")
        })?;
        if bytes == 0 {
            return Ok(());
        }
        match codec.decode(&buffer[..bytes]) {
            Ok(requests) => {
                for request in requests {
                    let start = std::time::Instant::now();
                    let close = request.connection_should_close();
                    let response = route_request(&request, close);
                    record_http_response(HttpRequestKind::Static, &request, &response, start);
                    write_response(&mut stream, response).await?;
                    if close {
                        return Ok(());
                    }
                }
            }
            Err(error) => {
                let response = error_response(error, true);
                crate::metrics::metrics()
                    .requests
                    .inc(HttpRequestKind::DecodeError);
                crate::metrics::metrics()
                    .responses
                    .inc(crate::metrics::status_class(response.status));
                crate::metrics::metrics()
                    .response_body_bytes
                    .add(response.body.len() as isize);
                write_response(&mut stream, response).await?;
                return Ok(());
            }
        }
    }
}

async fn handle_in_process_connection(
    mut stream: TcpStream,
    host: Arc<InProcessWebHost>,
    binding: RequestBinding,
) -> Result<(), String> {
    let connection_endpoint = host.allocate_endpoint()?;
    let mut configuration = EndpointConfiguration::new(Symbol::intern("http"))
        .endpoint(connection_endpoint)
        .principal(binding.principal);
    if let Some(actor) = binding.actor {
        configuration = configuration.actor(actor);
    }
    let endpoint = host
        .client
        .open_endpoint(configuration)
        .map_err(|error| format_driver_error(&host.client, error))?;
    let close_client = host.client.clone();
    let result = async move {
        let mut codec = HttpCodec::new();
        loop {
            let (result, buffer) = stream.read([0u8; 8192]).await.into();
            let bytes = result.map_err(|error| {
                crate::metrics::metrics().connection_read_errors.inc();
                format!("failed to read from connection: {error}")
            })?;
            if bytes == 0 {
                return Ok(());
            }
            match codec.decode(&buffer[..bytes]) {
                Ok(requests) => {
                    for request in requests {
                        match sync::request_kind(&request) {
                            Some(SyncRequestKind::EventStream) => {
                                crate::metrics::metrics()
                                    .requests
                                    .inc(HttpRequestKind::SyncEvents);
                                return sync::serve_event_stream(stream, host, binding, &request)
                                    .await;
                            }
                            Some(SyncRequestKind::Input) => {
                                let start = std::time::Instant::now();
                                let close = request.connection_should_close();
                                let response = sync::handle_sync_input_request(
                                    &host, &binding, &request, close,
                                )
                                .await;
                                record_http_response(
                                    HttpRequestKind::SyncInput,
                                    &request,
                                    &response,
                                    start,
                                );
                                write_response(&mut stream, response).await?;
                                if close {
                                    return Ok(());
                                }
                                continue;
                            }
                            None => {}
                        }
                        let start = std::time::Instant::now();
                        let close = request.connection_should_close();
                        let response =
                            handle_in_process_request(&host, &binding, &request, close).await;
                        let kind = if request.method == "GET"
                            && (request.path == "/healthz"
                                || crate::response::is_sync_client_path(&request.path))
                        {
                            HttpRequestKind::Static
                        } else {
                            HttpRequestKind::InProcess
                        };
                        record_http_response(kind, &request, &response, start);
                        write_response(&mut stream, response).await?;
                        if close {
                            return Ok(());
                        }
                    }
                }
                Err(error) => {
                    let response = error_response(error, true);
                    crate::metrics::metrics()
                        .requests
                        .inc(HttpRequestKind::DecodeError);
                    crate::metrics::metrics()
                        .responses
                        .inc(crate::metrics::status_class(response.status));
                    crate::metrics::metrics()
                        .response_body_bytes
                        .add(response.body.len() as isize);
                    write_response(&mut stream, response).await?;
                    return Ok(());
                }
            }
        }
    }
    .await;
    endpoint
        .close_in_background()
        .map_err(|error| format_driver_error(&close_client, error))?;
    result
}

async fn write_response(stream: &mut TcpStream, response: HttpResponse) -> Result<(), String> {
    let mut out = Vec::new();
    encode_response(&response, &mut out)
        .map_err(|error| format!("failed to encode HTTP response: {error}"))?;
    let (result, _) = stream.write_all(out).await.into();
    result.map_err(|error| {
        crate::metrics::metrics().response_write_errors.inc();
        format!("failed to write to connection: {error}")
    })
}

fn record_http_response(
    kind: HttpRequestKind,
    request: &crate::codec::HttpRequest,
    response: &HttpResponse,
    start: std::time::Instant,
) {
    let elapsed = start.elapsed();
    crate::metrics::metrics().requests.inc(kind);
    crate::metrics::metrics()
        .request_duration_us
        .record(kind, crate::metrics::duration_us(elapsed));
    crate::metrics::metrics()
        .request_duration
        .record_elapsed(kind, elapsed);
    crate::metrics::metrics()
        .responses
        .inc(crate::metrics::status_class(response.status));
    crate::metrics::metrics()
        .request_body_bytes
        .add(request.body.len() as isize);
    crate::metrics::metrics()
        .response_body_bytes
        .add(response.body.len() as isize);
}
