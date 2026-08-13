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
use cyper::Client;
use futures_util::StreamExt;
use http::Method;
use mica_driver::{ExternalRequestHandler, ExternalStreamEmitter, ExternalStreamRequestHandler};
use mica_runtime::ExternalRequest;
use mica_runtime::json::{
    json_from_value as runtime_json_from_value, json_null_value,
    value_from_json as runtime_value_from_json,
    value_from_json_text as runtime_value_from_json_text,
};
use mica_var::{Symbol, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

pub fn handler() -> ExternalRequestHandler {
    handler_with_config(ExternalHttpConfig::default())
}

pub fn stream_handler() -> ExternalStreamRequestHandler {
    Arc::new(move |_, request, emitter| {
        Box::pin(async move {
            compio::runtime::spawn(async move {
                handle_external_stream_request(request, emitter).await;
            })
            .detach();
            Value::map([(Value::symbol(Symbol::intern("started")), Value::bool(true))])
        })
    })
}

#[cfg(test)]
fn handler_with_embedding_base_url(base_url: String) -> ExternalRequestHandler {
    handler_with_config(ExternalHttpConfig {
        embedding_base_url: Some(base_url),
    })
}

fn handler_with_config(config: ExternalHttpConfig) -> ExternalRequestHandler {
    let config = Arc::new(config);
    Arc::new(move |_, request| {
        let config = Arc::clone(&config);
        Box::pin(async move { handle_external_request(request, &config).await })
    })
}

#[derive(Default)]
struct ExternalHttpConfig {
    embedding_base_url: Option<String>,
}

async fn handle_external_request(request: ExternalRequest, config: &ExternalHttpConfig) -> Value {
    let service = external_service_label(request.service);
    let start = Instant::now();
    metrics::metrics().external_requests.inc(service);
    let result = perform_external_request(request, config).await;
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
            external_error(message)
        }
    }
}

fn external_service_label(service: Symbol) -> ExternalService {
    match service.name() {
        Some("http") => ExternalService::Http,
        Some("openai" | "openai_responses") => ExternalService::Openai,
        Some("embedding") => ExternalService::Embedding,
        _ => ExternalService::Unknown,
    }
}

async fn handle_external_stream_request(request: ExternalRequest, emitter: ExternalStreamEmitter) {
    let service = external_service_label(request.service);
    let start = Instant::now();
    metrics::metrics().external_requests.inc(service);
    let result = perform_external_stream_request(request, &emitter).await;
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
        let _ = emitter.emit(stream_error_event(message)).await;
    }
}

async fn perform_external_stream_request(
    request: ExternalRequest,
    emitter: &ExternalStreamEmitter,
) -> Result<(), String> {
    let timeout_millis = request.timeout_millis;
    let wire_api = match request.service.name() {
        Some("openai") => OpenaiWireApi::ChatCompletions,
        Some("openai_responses") => OpenaiWireApi::Responses,
        _ => {
            return Err(format!(
                "external service {:?} does not support streaming",
                request.service
            ));
        }
    };
    let spec = OpenaiRequestSpec::from_stream_payload(&request.payload, wire_api)?;
    let stream = perform_openai_stream(spec, wire_api, emitter);
    let Some(timeout_millis) = timeout_millis else {
        return stream.await;
    };
    compio::time::timeout(
        std::time::Duration::from_millis(timeout_millis.max(1)),
        stream,
    )
    .await
    .map_err(|_| format!("LLM stream timed out after {timeout_millis} ms"))?
}

async fn perform_external_request(
    request: ExternalRequest,
    config: &ExternalHttpConfig,
) -> Result<Value, String> {
    match request.service.name() {
        Some("http") => {
            let spec = HttpRequestSpec::from_http_payload(&request.payload)?;
            perform_http_request(spec).await
        }
        Some("openai") => {
            let spec = OpenaiRequestSpec::from_payload(&request.payload)?;
            perform_openai_request(spec).await
        }
        Some("embedding") => {
            let spec = HttpRequestSpec::from_embedding_payload(
                &request.payload,
                config.embedding_base_url.as_deref(),
            )?;
            perform_embedding_request(spec).await
        }
        _ => Err(format!("unknown external service {:?}", request.service)),
    }
}

#[derive(Clone)]
struct HttpRequestSpec {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct OpenaiRequestSpec {
    request: HttpRequestSpec,
    response_mode: OpenaiResponseMode,
    model: String,
    message_count: Option<usize>,
    provider: String,
}

#[derive(Clone, Copy)]
enum OpenaiResponseMode {
    Http,
    Json,
    Stream,
}

#[derive(Clone, Copy)]
enum OpenaiWireApi {
    ChatCompletions,
    Responses,
}

struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

struct HttpResponseData {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequestSpec {
    fn from_http_payload(payload: &Value) -> Result<Self, String> {
        let url = map_string(payload, "url")?;
        let method = optional_map_text(payload, "method")?.unwrap_or_else(|| "GET".to_owned());
        let headers = optional_map_headers(payload, "headers")?.unwrap_or_default();
        let body = request_body(payload)?;
        Ok(Self {
            method,
            url,
            headers,
            body,
        })
    }
}

impl OpenaiRequestSpec {
    fn from_payload(payload: &Value) -> Result<Self, String> {
        let model = map_string(payload, "model")?;
        let message_count = map_get(payload, "messages")
            .and_then(|messages| messages.with_list(|items| items.len()));
        let base_url = optional_map_string(payload, "base_url")?
            .or_else(|| std::env::var("MICA_OPENAI_BASE_URL").ok())
            .or_else(|| std::env::var("OPENROUTER_BASE_URL").ok())
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_owned());
        let path =
            optional_map_string(payload, "path")?.unwrap_or_else(|| "/chat/completions".to_owned());
        let url = join_url_path(&base_url, &path);
        let mut headers = optional_map_headers(payload, "headers")?.unwrap_or_default();
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        if let Some(api_key) = optional_map_string(payload, "api_key")?
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        {
            headers.push(("Authorization".to_owned(), format!("Bearer {api_key}")));
        }
        if let Some(referer) = optional_map_string(payload, "referer")?
            .or_else(|| std::env::var("MICA_OPENROUTER_REFERER").ok())
            .or_else(|| std::env::var("OPENROUTER_HTTP_REFERER").ok())
        {
            headers.push(("HTTP-Referer".to_owned(), referer));
        }
        if let Some(title) = optional_map_string(payload, "title")?
            .or_else(|| std::env::var("MICA_OPENROUTER_TITLE").ok())
            .or_else(|| std::env::var("OPENROUTER_TITLE").ok())
        {
            headers.push(("X-OpenRouter-Title".to_owned(), title));
        }
        let (body, response_mode) =
            if map_get(payload, "body").is_some() || map_get(payload, "json").is_some() {
                (request_body(payload)?, OpenaiResponseMode::Http)
            } else {
                (
                    serde_json::to_vec(&openai_chat_completion_json(payload, false)?).map_err(
                        |error| format!("failed to encode OpenAI chat completion request: {error}"),
                    )?,
                    OpenaiResponseMode::Json,
                )
            };
        let provider = if base_url.contains("openrouter") {
            "openrouter"
        } else if base_url.contains("api.openai.com") {
            "openai"
        } else {
            "api"
        }
        .to_owned();
        Ok(Self {
            request: HttpRequestSpec {
                method: "POST".to_owned(),
                url,
                headers,
                body,
            },
            response_mode,
            model,
            message_count,
            provider,
        })
    }

    fn from_stream_payload(payload: &Value, wire_api: OpenaiWireApi) -> Result<Self, String> {
        let model = map_string(payload, "model")?;
        let (input_key, default_path, body) = match wire_api {
            OpenaiWireApi::ChatCompletions => (
                "messages",
                "/chat/completions",
                openai_chat_completion_json(payload, true)?,
            ),
            OpenaiWireApi::Responses => ("input", "/responses", openai_responses_json(payload)?),
        };
        let message_count =
            map_get(payload, input_key).and_then(|input| input.with_list(|items| items.len()));
        let base_url = optional_map_string(payload, "base_url")?
            .or_else(|| std::env::var("MICA_OPENAI_BASE_URL").ok())
            .or_else(|| std::env::var("OPENROUTER_BASE_URL").ok())
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_owned());
        let path = optional_map_string(payload, "path")?.unwrap_or_else(|| default_path.to_owned());
        let url = join_url_path(&base_url, &path);
        let mut headers = optional_map_headers(payload, "headers")?.unwrap_or_default();
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        if let Some(api_key) = optional_map_string(payload, "api_key")?
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        {
            headers.push(("Authorization".to_owned(), format!("Bearer {api_key}")));
        }
        if let Some(referer) = optional_map_string(payload, "referer")?
            .or_else(|| std::env::var("MICA_OPENROUTER_REFERER").ok())
            .or_else(|| std::env::var("OPENROUTER_HTTP_REFERER").ok())
        {
            headers.push(("HTTP-Referer".to_owned(), referer));
        }
        if let Some(title) = optional_map_string(payload, "title")?
            .or_else(|| std::env::var("MICA_OPENROUTER_TITLE").ok())
            .or_else(|| std::env::var("OPENROUTER_TITLE").ok())
        {
            headers.push(("X-OpenRouter-Title".to_owned(), title));
        }
        let provider = if base_url.contains("openrouter") {
            "openrouter"
        } else if base_url.contains("api.openai.com") {
            "openai"
        } else {
            "api"
        }
        .to_owned();
        Ok(Self {
            request: HttpRequestSpec {
                method: "POST".to_owned(),
                url,
                headers,
                body: serde_json::to_vec(&body)
                    .map_err(|error| format!("failed to encode LLM stream request: {error}"))?,
            },
            response_mode: OpenaiResponseMode::Stream,
            model,
            message_count,
            provider,
        })
    }
}

impl HttpRequestSpec {
    fn from_embedding_payload(
        payload: &Value,
        configured_base_url: Option<&str>,
    ) -> Result<Self, String> {
        let model = map_string(payload, "model")?;
        let text = map_string(payload, "text")?;
        let base_url = optional_map_string(payload, "base_url")?
            .or_else(|| configured_base_url.map(str::to_owned))
            .or_else(|| std::env::var("MICA_VLLM_BASE_URL").ok())
            .unwrap_or_else(|| "http://127.0.0.1:8000/v1".to_owned());
        let path =
            optional_map_string(payload, "path")?.unwrap_or_else(|| "/embeddings".to_owned());
        let url = join_url_path(&base_url, &path);
        let mut body = serde_json::json!({
            "input": text,
            "model": model,
        });
        if let Some(truncate_prompt_tokens) = truncate_prompt_tokens()? {
            body["truncate_prompt_tokens"] = serde_json::json!(truncate_prompt_tokens);
        }
        let mut headers = optional_map_headers(payload, "headers")?.unwrap_or_default();
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        if let Some(api_key) = optional_map_string(payload, "api_key")?
            .or_else(|| std::env::var("MICA_VLLM_API_KEY").ok())
        {
            headers.push(("Authorization".to_owned(), format!("Bearer {api_key}")));
        }
        Ok(Self {
            method: "POST".to_owned(),
            url,
            headers,
            body: serde_json::to_vec(&body)
                .map_err(|error| format!("failed to encode embedding request: {error}"))?,
        })
    }
}

async fn perform_http_request(spec: HttpRequestSpec) -> Result<Value, String> {
    let response = perform_http_bytes(spec).await?;
    let status = i64::from(response.status);
    let headers = response
        .headers
        .into_iter()
        .map(|(name, value)| Value::list([Value::string(name), Value::string(value)]))
        .collect::<Vec<_>>();
    let body = String::from_utf8_lossy(&response.body).into_owned();
    Ok(Value::map([
        (
            Value::symbol(Symbol::intern("status")),
            Value::int(status).unwrap(),
        ),
        (
            Value::symbol(Symbol::intern("headers")),
            Value::list(headers),
        ),
        (Value::symbol(Symbol::intern("body")), Value::string(body)),
    ]))
}

async fn perform_openai_request(spec: OpenaiRequestSpec) -> Result<Value, String> {
    let url = spec.request.url.clone();
    let body_bytes = spec.request.body.len();
    let model = spec.model.clone();
    let message_count = spec.message_count.unwrap_or(0);
    tracing::info!(
        model = %model,
        url = %url,
        body_bytes,
        message_count,
        response_mode = match spec.response_mode {
            OpenaiResponseMode::Http => "http",
            OpenaiResponseMode::Json => "json",
            OpenaiResponseMode::Stream => "stream",
        },
        "OpenAI request prepared"
    );
    let start = Instant::now();
    match spec.response_mode {
        OpenaiResponseMode::Http => {
            let result = perform_http_request(spec.request).await;
            tracing::info!(
                model = %model,
                elapsed_ms = start.elapsed().as_millis(),
                ok = result.is_ok(),
                "OpenAI HTTP-mode request finished"
            );
            result
        }
        OpenaiResponseMode::Json => {
            let response = perform_http_bytes(spec.request).await?;
            tracing::info!(
                model = %model,
                status = response.status,
                response_bytes = response.body.len(),
                elapsed_ms = start.elapsed().as_millis(),
                "OpenAI response received"
            );
            if !(200..300).contains(&response.status) {
                let message = String::from_utf8_lossy(&response.body);
                return Err(format!(
                    "OpenAI chat completion failed with HTTP {}: {message}",
                    response.status
                ));
            }
            let raw = std::str::from_utf8(&response.body)
                .map_err(|error| format!("OpenAI response is not UTF-8 JSON: {error}"))?;
            let raw_value = runtime_value_from_json_text(raw).map_err(|error| error.to_string())?;
            normalize_openai_tool_call_value(raw_value)
        }
        OpenaiResponseMode::Stream => {
            Err("streaming request was sent through the one-shot external handler".to_owned())
        }
    }
}

/// Normalizes DSML leaked into an OpenAI response without round-tripping the
/// response through `serde_json::Value`.
///
/// The raw response has already been parsed with `value_from_json_text`, so
/// untouched values retain whether their source JSON token was an integer or
/// float. Only the synthetic tool-call fields are newly constructed here.
fn normalize_openai_tool_call_value(response: Value) -> Result<Value, String> {
    let Some(choices) = map_get(&response, "choices") else {
        return Ok(response);
    };
    let Some(mut choices) = choices.with_list(<[Value]>::to_vec) else {
        return Ok(response);
    };

    let mut changed = false;
    for choice in &mut choices {
        let Some(message) = map_get(choice, "message") else {
            continue;
        };
        if map_get(&message, "tool_calls").is_some() {
            continue;
        }
        let Some(content) =
            map_get(&message, "content").and_then(|value| value.with_str(str::to_owned))
        else {
            continue;
        };
        let tool_calls = parse_dsml_tool_calls(&content);
        if tool_calls.is_empty() {
            continue;
        }
        tracing::warn!(
            tool_call_count = tool_calls.len(),
            "normalized leaked DSML tool calls from OpenAI response content"
        );
        let tool_calls = value_from_json(&serde_json::Value::Array(tool_calls))?;
        let message = message
            .map_set(Value::symbol(Symbol::intern("tool_calls")), tool_calls)
            .and_then(|message| {
                message.map_set(Value::symbol(Symbol::intern("content")), json_null_value())
            })
            .expect("OpenAI message must remain a map");
        *choice = choice
            .map_set(Value::symbol(Symbol::intern("message")), message)
            .expect("OpenAI choice must remain a map");
        changed = true;
    }

    if !changed {
        return Ok(response);
    }
    response
        .map_set(
            Value::symbol(Symbol::intern("choices")),
            Value::list(choices),
        )
        .ok_or_else(|| "OpenAI response must remain a map".to_owned())
}

#[cfg(test)]
fn normalize_openai_tool_call_response(response: &mut serde_json::Value) -> bool {
    let mut changed = false;
    let Some(choices) = response
        .get_mut("choices")
        .and_then(|value| value.as_array_mut())
    else {
        return false;
    };
    for choice in choices {
        let Some(message) = choice
            .get_mut("message")
            .and_then(|value| value.as_object_mut())
        else {
            continue;
        };
        if message.contains_key("tool_calls") {
            continue;
        }
        let Some(content) = message.get("content").and_then(|value| value.as_str()) else {
            continue;
        };
        let tool_calls = parse_dsml_tool_calls(content);
        if tool_calls.is_empty() {
            continue;
        }
        tracing::warn!(
            tool_call_count = tool_calls.len(),
            "normalized leaked DSML tool calls from OpenAI response content"
        );
        message.insert(
            "tool_calls".to_owned(),
            serde_json::Value::Array(tool_calls),
        );
        message.insert("content".to_owned(), serde_json::Value::Null);
        changed = true;
    }
    changed
}

fn parse_dsml_tool_calls(content: &str) -> Vec<serde_json::Value> {
    if !content.contains("DSML") || !content.contains("invoke name=\"") {
        return Vec::new();
    }
    let mut calls = Vec::new();
    let mut cursor = 0;
    let marker = "invoke name=\"";
    while let Some(relative_start) = content[cursor..].find(marker) {
        let invoke_start = cursor + relative_start;
        let name_start = invoke_start + marker.len();
        let Some(relative_name_end) = content[name_start..].find('"') else {
            break;
        };
        let name_end = name_start + relative_name_end;
        let name = &content[name_start..name_end];
        let Some(relative_tag_end) = content[name_end..].find('>') else {
            break;
        };
        let tag_end = name_end + relative_tag_end;
        let next_invoke = content[tag_end + 1..]
            .find(marker)
            .map(|relative| tag_end + 1 + relative)
            .unwrap_or(content.len());
        let block = &content[tag_end + 1..next_invoke];
        let args = parse_dsml_parameters(block);
        let arguments = serde_json::to_string(&serde_json::Value::Object(args))
            .unwrap_or_else(|_| "{}".to_owned());
        calls.push(serde_json::json!({
            "id": format!("dsml_tool_{}", calls.len() + 1),
            "type": "function",
            "function": {
                "name": decode_dsml_text(name),
                "arguments": arguments,
            },
        }));
        cursor = next_invoke;
    }
    calls
}

fn parse_dsml_parameters(block: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut args = serde_json::Map::new();
    let mut cursor = 0;
    let marker = "parameter name=\"";
    while let Some(relative_start) = block[cursor..].find(marker) {
        let parameter_start = cursor + relative_start;
        let name_start = parameter_start + marker.len();
        let Some(relative_name_end) = block[name_start..].find('"') else {
            break;
        };
        let name_end = name_start + relative_name_end;
        let name = decode_dsml_text(&block[name_start..name_end]);
        let Some(relative_tag_end) = block[name_end..].find('>') else {
            break;
        };
        let tag_end = name_end + relative_tag_end;
        let tag = &block[parameter_start..=tag_end];
        let value_start = tag_end + 1;
        let Some(relative_value_end) = block[value_start..].find("</") else {
            break;
        };
        let value_end = value_start + relative_value_end;
        let raw_value = decode_dsml_text(&block[value_start..value_end]);
        let is_string = quoted_attr_value(tag, "string").as_deref() != Some("false");
        args.insert(name, dsml_parameter_json_value(&raw_value, is_string));
        cursor = value_end + 2;
    }
    args
}

fn quoted_attr_value(tag: &str, attr: &str) -> Option<String> {
    let marker = format!("{attr}=\"");
    let value_start = tag.find(&marker)? + marker.len();
    let value_end = tag[value_start..].find('"')? + value_start;
    Some(tag[value_start..value_end].to_owned())
}

fn dsml_parameter_json_value(raw_value: &str, is_string: bool) -> serde_json::Value {
    if is_string {
        return serde_json::Value::String(raw_value.to_owned());
    }
    let trimmed = raw_value.trim();
    match trimmed {
        "true" => return serde_json::Value::Bool(true),
        "false" => return serde_json::Value::Bool(false),
        "null" => return serde_json::Value::Null,
        _ => {}
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(value.into());
    }
    if let Ok(value) = trimmed.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(value)
    {
        return serde_json::Value::Number(number);
    }
    serde_json::Value::String(raw_value.to_owned())
}

fn decode_dsml_text(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

async fn perform_embedding_request(spec: HttpRequestSpec) -> Result<Value, String> {
    let response = perform_http_bytes(spec).await?;
    if !(200..300).contains(&response.status) {
        let message = String::from_utf8_lossy(&response.body);
        return Err(format!(
            "embedding request failed with HTTP {}: {message}",
            response.status
        ));
    }
    let raw = std::str::from_utf8(&response.body)
        .map_err(|error| format!("embedding response is not UTF-8 JSON: {error}"))?;
    let response_value = runtime_value_from_json_text(raw)
        .map_err(|error| format!("invalid embedding response: {error}"))?;
    let Some(data) = map_get(&response_value, "data") else {
        return Err("embedding response did not contain data[0].embedding".to_owned());
    };
    let Some(item) = data.with_list(|values| values.first().cloned()).flatten() else {
        return Err("embedding response did not contain data[0].embedding".to_owned());
    };
    let Some(embedding) = map_get(&item, "embedding") else {
        return Err("embedding response did not contain data[0].embedding".to_owned());
    };
    let Some(values) = embedding.with_list(<[Value]>::to_vec) else {
        return Err("embedding response did not contain data[0].embedding".to_owned());
    };
    let values = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_float()
                .or_else(|| value.as_int().map(|value| value as f32))
                .ok_or_else(|| {
                    format!("embedding value at index {index} was not a finite binary32")
                })
                .and_then(|value| {
                    Value::float(value).map_err(|_| {
                        format!("embedding value at index {index} was not a finite binary32")
                    })
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::list(values))
}

async fn perform_http_bytes(spec: HttpRequestSpec) -> Result<HttpResponseData, String> {
    let response = send_http_request(spec).await?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read HTTP response body: {error}"))?;
    Ok(HttpResponseData {
        status,
        headers,
        body: body.as_ref().to_vec(),
    })
}

async fn send_http_request(spec: HttpRequestSpec) -> Result<cyper::Response, String> {
    let client = Client::new();
    let method = spec
        .method
        .parse::<Method>()
        .map_err(|error| format!("invalid HTTP method {:?}: {error}", spec.method))?;
    let mut request = client
        .request(method, &spec.url)
        .map_err(|error| format!("invalid HTTP request URL {:?}: {error}", spec.url))?;
    for (name, value) in spec.headers {
        request = request
            .header(name.as_str(), value.as_str())
            .map_err(|error| format!("invalid HTTP header {name:?}: {error}"))?;
    }
    request
        .body(spec.body)
        .send()
        .await
        .map_err(|error| format!("HTTP request failed: {error}"))
}

async fn perform_openai_stream(
    spec: OpenaiRequestSpec,
    wire_api: OpenaiWireApi,
    emitter: &ExternalStreamEmitter,
) -> Result<(), String> {
    tracing::info!(
        model = %spec.model,
        url = %spec.request.url,
        body_bytes = spec.request.body.len(),
        input_count = spec.message_count.unwrap_or(0),
        wire_api = match wire_api {
            OpenaiWireApi::ChatCompletions => "chat_completions",
            OpenaiWireApi::Responses => "responses",
        },
        "LLM stream request prepared"
    );
    let mut attempt = 0u32;
    let response = loop {
        attempt += 1;
        match send_http_request(spec.request.clone()).await {
            Ok(response) if (200..300).contains(&response.status().as_u16()) => break response,
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response
                    .bytes()
                    .await
                    .map_err(|error| format!("failed to read LLM error response: {error}"))?;
                if attempt < 3 && is_retryable_llm_status(status) {
                    tracing::warn!(status, attempt, "retrying rejected LLM stream request");
                    compio::time::sleep(stream_retry_delay(attempt)).await;
                    continue;
                }
                return Err(format!(
                    "LLM stream failed with HTTP {status}: {}",
                    String::from_utf8_lossy(&body)
                ));
            }
            Err(error) if attempt < 3 => {
                tracing::warn!(attempt, error = %error, "retrying failed LLM stream request");
                compio::time::sleep(stream_retry_delay(attempt)).await;
            }
            Err(error) => return Err(error),
        }
    };

    let mut decoder = OpenaiEventDecoder::new(wire_api, spec.provider);
    let mut batcher = OpenaiMailboxBatcher::new();
    let stream = response.bytes_stream();
    futures_util::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("failed to read LLM stream: {error}"))?;
        batcher.accept(decoder.push(&chunk)?, emitter).await?;
    }
    batcher.accept(decoder.finish()?, emitter).await?;
    batcher.flush(emitter).await?;
    Ok(())
}

fn is_retryable_llm_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429) || status >= 500
}

fn stream_retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(250 * u64::from(attempt))
}

struct OpenaiMailboxBatcher {
    events: Vec<Value>,
    delta_bytes: usize,
    last_flush: Instant,
    text_started: bool,
}

impl OpenaiMailboxBatcher {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            delta_bytes: 0,
            last_flush: Instant::now(),
            text_started: false,
        }
    }

    async fn accept(
        &mut self,
        events: Vec<Value>,
        emitter: &ExternalStreamEmitter,
    ) -> Result<(), String> {
        for event in events {
            let kind = map_get(&event, "type")
                .and_then(|value| value.as_symbol())
                .and_then(Symbol::name);
            self.delta_bytes += map_get(&event, "delta")
                .and_then(|value| value.with_str(str::len))
                .unwrap_or(0);
            let first_text = kind == Some("text_delta") && !self.text_started;
            if kind == Some("text_delta") {
                self.text_started = true;
            }
            self.events.push(event);
            let terminal = matches!(kind, Some("completed" | "incomplete" | "error"));
            let boundary = matches!(kind, Some("tool_call_ready"));
            if first_text
                || terminal
                || boundary
                || self.delta_bytes >= 128
                || self.last_flush.elapsed().as_millis() >= 40
            {
                self.flush(emitter).await?;
            }
        }
        Ok(())
    }

    async fn flush(&mut self, emitter: &ExternalStreamEmitter) -> Result<(), String> {
        if self.events.is_empty() {
            return Ok(());
        }
        let event = if self.events.len() == 1 {
            self.events.pop().unwrap()
        } else {
            stream_event(
                "batch",
                None,
                vec![("events", Value::list(std::mem::take(&mut self.events)))],
            )
        };
        self.events.clear();
        self.delta_bytes = 0;
        self.last_flush = Instant::now();
        emitter.emit(event).await
    }
}

struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut data = Vec::new();
        while let Some((frame_end, delimiter_len)) = sse_frame_end(&self.buffer) {
            let frame = self.buffer[..frame_end].to_vec();
            self.buffer.drain(..frame_end + delimiter_len);
            if let Some(value) = sse_frame_data(&frame)? {
                data.push(value);
            }
        }
        Ok(data)
    }

    fn finish(&mut self) -> Result<Vec<String>, String> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }
        let frame = std::mem::take(&mut self.buffer);
        Ok(sse_frame_data(&frame)?.into_iter().collect())
    }
}

fn sse_frame_end(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index..].starts_with(b"\r\n\r\n") {
            return Some((index, 4));
        }
        if bytes[index..].starts_with(b"\n\n") {
            return Some((index, 2));
        }
    }
    None
}

fn sse_frame_data(frame: &[u8]) -> Result<Option<String>, String> {
    let text = std::str::from_utf8(frame)
        .map_err(|error| format!("LLM stream contained non-UTF-8 SSE data: {error}"))?;
    let mut lines = Vec::new();
    for line in text.lines() {
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };
        lines.push(value.strip_prefix(' ').unwrap_or(value));
    }
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(lines.join("\n")))
}

struct OpenaiEventDecoder {
    sse: SseDecoder,
    wire_api: OpenaiWireApi,
    provider: String,
    started: bool,
    terminal: bool,
    chat_tool_calls: BTreeMap<usize, ToolCallAccumulator>,
    ready_tool_calls: HashSet<String>,
}

impl OpenaiEventDecoder {
    fn new(wire_api: OpenaiWireApi, provider: String) -> Self {
        Self {
            sse: SseDecoder::new(),
            wire_api,
            provider,
            started: false,
            terminal: false,
            chat_tool_calls: BTreeMap::new(),
            ready_tool_calls: HashSet::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        let frames = self.sse.push(bytes)?;
        self.decode_frames(frames)
    }

    fn finish(&mut self) -> Result<Vec<Value>, String> {
        let frames = self.sse.finish()?;
        let mut events = self.decode_frames(frames)?;
        if !self.terminal {
            events.push(stream_error_event(
                "LLM stream ended without a terminal event".to_owned(),
            ));
            self.terminal = true;
        }
        Ok(events)
    }

    fn decode_frames(&mut self, frames: Vec<String>) -> Result<Vec<Value>, String> {
        let mut events = Vec::new();
        for data in frames {
            if data.trim() == "[DONE]" {
                if matches!(self.wire_api, OpenaiWireApi::ChatCompletions) && !self.terminal {
                    events.push(stream_event("completed", None, Vec::new()));
                    self.terminal = true;
                }
                continue;
            }
            let json: serde_json::Value = serde_json::from_str(&data)
                .map_err(|error| format!("invalid LLM SSE data: {error}"))?;
            if !self.started {
                events.push(stream_event(
                    "started",
                    Some(&json),
                    vec![("provider", Value::string(&self.provider))],
                ));
                self.started = true;
            }
            let normalized = match self.wire_api {
                OpenaiWireApi::Responses => normalize_responses_event(&json, &mut self.terminal)?,
                OpenaiWireApi::ChatCompletions => {
                    normalize_chat_event(&json, &mut self.terminal, &mut self.chat_tool_calls)?
                }
            };
            for event in normalized {
                if event.map_get(&Value::symbol(Symbol::intern("type")))
                    == Some(Value::symbol(Symbol::intern("tool_call_ready")))
                    && let Some(call_id) = event
                        .map_get(&Value::symbol(Symbol::intern("call_id")))
                        .and_then(|value| value.with_str(str::to_owned))
                    && !self.ready_tool_calls.insert(call_id)
                {
                    continue;
                }
                events.push(event);
            }
        }
        Ok(events)
    }
}

fn normalize_responses_event(
    json: &serde_json::Value,
    terminal: &mut bool,
) -> Result<Vec<Value>, String> {
    let event_type = json.get("type").and_then(serde_json::Value::as_str);
    let mut events = Vec::new();
    match event_type {
        Some("response.created") => {}
        Some("response.output_text.delta" | "response.refusal.delta") => {
            let mut fields = Vec::new();
            copy_json_field(&mut fields, json, "delta", "delta")?;
            copy_json_field(&mut fields, json, "item_id", "item_id")?;
            copy_json_field(&mut fields, json, "output_index", "output_index")?;
            copy_json_field(&mut fields, json, "content_index", "content_index")?;
            events.push(stream_event("text_delta", None, fields));
        }
        Some("response.output_item.added")
            if json
                .pointer("/item/type")
                .and_then(serde_json::Value::as_str)
                == Some("function_call") =>
        {
            events.push(tool_event("tool_call_started", json)?);
        }
        Some("response.function_call_arguments.delta") => {
            let mut fields = Vec::new();
            copy_json_field(&mut fields, json, "delta", "delta")?;
            copy_json_field(&mut fields, json, "item_id", "item_id")?;
            copy_json_field(&mut fields, json, "output_index", "output_index")?;
            events.push(stream_event("tool_arguments_delta", None, fields));
        }
        Some("response.function_call_arguments.done") => {
            events.push(tool_event("tool_call_ready", json)?);
        }
        Some("response.output_item.done")
            if json
                .pointer("/item/type")
                .and_then(serde_json::Value::as_str)
                == Some("function_call") =>
        {
            events.push(tool_event("tool_call_ready", json)?);
        }
        Some("response.completed") => {
            if let Some(output) = json
                .pointer("/response/output")
                .and_then(serde_json::Value::as_array)
            {
                for item in output {
                    if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                    {
                        events.push(tool_event("tool_call_ready", item)?);
                    }
                }
            }
            events.push(response_terminal_event("completed", json)?);
            *terminal = true;
        }
        Some("response.incomplete") => {
            events.push(response_terminal_event("incomplete", json)?);
            *terminal = true;
        }
        Some("response.failed" | "error") => {
            let message = json
                .pointer("/error/message")
                .or_else(|| json.pointer("/response/error/message"))
                .or_else(|| json.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Responses API stream failed");
            events.push(stream_event(
                "error",
                Some(json),
                vec![("message", Value::string(message))],
            ));
            *terminal = true;
        }
        _ => {}
    }
    Ok(events)
}

fn normalize_chat_event(
    json: &serde_json::Value,
    terminal: &mut bool,
    tool_call_accumulators: &mut BTreeMap<usize, ToolCallAccumulator>,
) -> Result<Vec<Value>, String> {
    if let Some(error) = json.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Chat Completions stream failed");
        *terminal = true;
        return Ok(vec![stream_event(
            "error",
            Some(json),
            vec![("message", Value::string(message))],
        )]);
    }
    let mut events = Vec::new();
    let Some(choices) = json.get("choices").and_then(serde_json::Value::as_array) else {
        return Ok(events);
    };
    for choice in choices {
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        if let Some(content) = delta.get("content").and_then(serde_json::Value::as_str) {
            events.push(stream_event(
                "text_delta",
                None,
                vec![("delta", Value::string(content))],
            ));
        }
        if let Some(tool_calls) = delta
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        {
            for call in tool_calls {
                let function = call.get("function").unwrap_or(&serde_json::Value::Null);
                let index = call
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;
                let accumulator =
                    tool_call_accumulators
                        .entry(index)
                        .or_insert_with(|| ToolCallAccumulator {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                if let Some(id) = call.get("id").and_then(serde_json::Value::as_str) {
                    accumulator.id = id.to_owned();
                }
                if let Some(name) = function.get("name").and_then(serde_json::Value::as_str) {
                    accumulator.name = name.to_owned();
                }
                if let Some(arguments) = function
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                {
                    accumulator.arguments.push_str(arguments);
                }
                let mut fields = Vec::new();
                copy_json_field(&mut fields, call, "index", "output_index")?;
                copy_json_field(&mut fields, call, "id", "call_id")?;
                copy_json_field(&mut fields, function, "name", "name")?;
                copy_json_field(&mut fields, function, "arguments", "delta")?;
                events.push(stream_event("tool_arguments_delta", None, fields));
            }
        }
        if let Some(reason) = choice
            .get("finish_reason")
            .and_then(serde_json::Value::as_str)
        {
            for (index, call) in tool_call_accumulators.iter() {
                events.push(stream_event(
                    "tool_call_ready",
                    Some(json),
                    vec![
                        ("output_index", Value::int(*index as i64).unwrap()),
                        ("call_id", Value::string(&call.id)),
                        ("name", Value::string(&call.name)),
                        ("arguments", Value::string(&call.arguments)),
                    ],
                ));
            }
            events.push(stream_event(
                "completed",
                Some(json),
                vec![("stop_reason", Value::string(reason))],
            ));
            *terminal = true;
        }
    }
    Ok(events)
}

fn tool_event(kind: &str, json: &serde_json::Value) -> Result<Value, String> {
    let item = json.get("item").unwrap_or(json);
    let mut fields = Vec::new();
    copy_json_field(&mut fields, item, "id", "item_id")?;
    if item.get("call_id").is_some() {
        copy_json_field(&mut fields, item, "call_id", "call_id")?;
    } else {
        copy_json_field(&mut fields, item, "id", "call_id")?;
    }
    copy_json_field(&mut fields, item, "name", "name")?;
    copy_json_field(&mut fields, item, "arguments", "arguments")?;
    copy_json_field(&mut fields, json, "output_index", "output_index")?;
    Ok(stream_event(kind, Some(json), fields))
}

fn response_terminal_event(kind: &str, json: &serde_json::Value) -> Result<Value, String> {
    let mut fields = Vec::new();
    if let Some(response) = json.get("response") {
        fields.push(("response", value_from_json(response)?));
        if let Some(usage) = response.get("usage") {
            fields.push(("usage", value_from_json(usage)?));
        }
        let text = responses_output_text(response);
        if !text.is_empty() {
            fields.push(("text", Value::string(text)));
        }
    }
    Ok(stream_event(kind, Some(json), fields))
}

fn responses_output_text(response: &serde_json::Value) -> String {
    let Some(output) = response.get("output").and_then(serde_json::Value::as_array) else {
        return String::new();
    };
    let mut text = String::new();
    for item in output {
        let Some(content) = item.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for part in content {
            if let Some(value) = part
                .get("text")
                .or_else(|| part.get("refusal"))
                .and_then(serde_json::Value::as_str)
            {
                text.push_str(value);
            }
        }
    }
    text
}

fn copy_json_field(
    fields: &mut Vec<(&'static str, Value)>,
    json: &serde_json::Value,
    source: &str,
    target: &'static str,
) -> Result<(), String> {
    if let Some(value) = json.get(source) {
        fields.push((target, value_from_json(value)?));
    }
    Ok(())
}

fn stream_event(
    kind: &str,
    raw: Option<&serde_json::Value>,
    extra: Vec<(&'static str, Value)>,
) -> Value {
    let mut fields = Vec::with_capacity(extra.len() + 2);
    fields.push((
        Value::symbol(Symbol::intern("type")),
        Value::symbol(Symbol::intern(kind)),
    ));
    for (name, value) in extra {
        fields.push((Value::symbol(Symbol::intern(name)), value));
    }
    if let Some(raw) = raw
        && let Ok(value) = value_from_json(raw)
    {
        fields.push((Value::symbol(Symbol::intern("raw")), value));
    }
    Value::map(fields)
}

fn stream_error_event(message: String) -> Value {
    stream_event("error", None, vec![("message", Value::string(message))])
}

fn request_body(payload: &Value) -> Result<Vec<u8>, String> {
    if let Some(body) = optional_map_string(payload, "body")? {
        return Ok(body.into_bytes());
    }
    if let Some(json) = map_get(payload, "json") {
        return serde_json::to_vec(&json_from_value(&json)?)
            .map_err(|error| format!("failed to encode JSON body: {error}"));
    }
    Ok(Vec::new())
}

fn openai_chat_completion_json(payload: &Value, stream: bool) -> Result<serde_json::Value, String> {
    let model = map_string(payload, "model")?;
    let messages = map_get(payload, "messages").ok_or_else(|| "missing \"messages\"".to_owned())?;
    let mut object = match map_get(payload, "options") {
        Some(options) => match json_from_value(&options)? {
            serde_json::Value::Object(object) => object,
            _ => return Err("\"options\" must be a map".to_owned()),
        },
        None => serde_json::Map::new(),
    };
    object.insert("model".to_owned(), serde_json::Value::String(model));
    object.insert("messages".to_owned(), json_from_value(&messages)?);
    if let Some(tools) = map_get(payload, "tools") {
        object.insert("tools".to_owned(), json_from_value(&tools)?);
    }
    object.insert("stream".to_owned(), serde_json::Value::Bool(stream));
    Ok(serde_json::Value::Object(object))
}

fn openai_responses_json(payload: &Value) -> Result<serde_json::Value, String> {
    let model = map_string(payload, "model")?;
    let input = map_get(payload, "input").ok_or_else(|| "missing \"input\"".to_owned())?;
    let mut object = match map_get(payload, "options") {
        Some(options) => match json_from_value(&options)? {
            serde_json::Value::Object(object) => object,
            _ => return Err("\"options\" must be a map".to_owned()),
        },
        None => serde_json::Map::new(),
    };
    object.remove("previous_response_id");
    object.insert("model".to_owned(), serde_json::Value::String(model));
    object.insert("input".to_owned(), json_from_value(&input)?);
    object.insert("stream".to_owned(), serde_json::Value::Bool(true));
    object.insert("store".to_owned(), serde_json::Value::Bool(false));
    object
        .entry("include".to_owned())
        .or_insert_with(|| serde_json::json!(["reasoning.encrypted_content"]));
    if let Some(instructions) = map_get(payload, "instructions") {
        object.insert("instructions".to_owned(), json_from_value(&instructions)?);
    }
    if let Some(tools) = map_get(payload, "tools") {
        object.insert("tools".to_owned(), json_from_value(&tools)?);
    }
    Ok(serde_json::Value::Object(object))
}

fn json_from_value(value: &Value) -> Result<serde_json::Value, String> {
    runtime_json_from_value(value).map_err(|e| e.to_string())
}

fn value_from_json(value: &serde_json::Value) -> Result<Value, String> {
    runtime_value_from_json(value).map_err(|e| e.to_string())
}

fn optional_map_headers(
    payload: &Value,
    key: &str,
) -> Result<Option<Vec<(String, String)>>, String> {
    let Some(value) = map_get(payload, key) else {
        return Ok(None);
    };
    let headers = value
        .with_list(|headers| {
            headers
                .iter()
                .map(|header| {
                    header
                        .with_list(|pair| {
                            if pair.len() != 2 {
                                return Err("header pairs must contain name and value".to_owned());
                            }
                            Ok((value_text(&pair[0])?, value_text(&pair[1])?))
                        })
                        .ok_or_else(|| "headers must be a list of pairs".to_owned())?
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .ok_or_else(|| "headers must be a list".to_owned())??;
    Ok(Some(headers))
}

fn map_string(payload: &Value, key: &str) -> Result<String, String> {
    optional_map_string(payload, key)?.ok_or_else(|| format!("missing {key:?}"))
}

fn optional_map_string(payload: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = map_get(payload, key) else {
        return Ok(None);
    };
    Ok(Some(
        value
            .with_str(str::to_owned)
            .ok_or_else(|| format!("{key:?} must be a string"))?,
    ))
}

fn optional_map_text(payload: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = map_get(payload, key) else {
        return Ok(None);
    };
    Ok(Some(value_text(&value)?))
}

fn value_text(value: &Value) -> Result<String, String> {
    if let Some(text) = value.with_str(str::to_owned) {
        return Ok(text);
    }
    if let Some(symbol) = value.as_symbol() {
        return symbol
            .name()
            .map(str::to_owned)
            .ok_or_else(|| format!("symbol {symbol:?} has no interned name"));
    }
    Err("expected string or symbol".to_owned())
}

fn map_get(payload: &Value, key: &str) -> Option<Value> {
    payload.map_get(&Value::symbol(Symbol::intern(key)))
}

fn join_url_path(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn truncate_prompt_tokens() -> Result<Option<usize>, String> {
    match std::env::var("MICA_VLLM_TRUNCATE_PROMPT_TOKENS") {
        Ok(value) if value == "0" => Ok(None),
        Ok(value) => value.parse::<usize>().map(Some).map_err(|error| {
            format!("invalid MICA_VLLM_TRUNCATE_PROMPT_TOKENS value {value:?}: {error}")
        }),
        Err(_) => Ok(Some(512)),
    }
}

fn external_error(message: String) -> Value {
    Value::error(Symbol::intern("ExternalError"), Some(message), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio::io::{AsyncRead, AsyncWriteExt};
    use compio::net::TcpListener;
    use std::num::NonZeroUsize;

    #[test]
    fn http_external_request_returns_status_headers_and_body() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            compio::runtime::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (result, _) = stream.read([0u8; 1024]).await.into();
                assert!(result.unwrap() > 0);
                let response =
                    b"HTTP/1.1 200 OK\r\nX-Test: yes\r\nContent-Length: 4\r\nConnection: close\r\n\r\npong";
                let (result, _) = stream.write_all(response.to_vec()).await.into();
                result.unwrap();
            })
            .detach();

            let request = ExternalRequest {
                service: Symbol::intern("http"),
                payload: Value::map([
                    (
                        Value::symbol(Symbol::intern("url")),
                        Value::string(format!("http://{addr}/ping")),
                    ),
                    (
                        Value::symbol(Symbol::intern("method")),
                        Value::symbol(Symbol::intern("GET")),
                    ),
                ]),
                timeout_millis: None,
            };
            let response = handle_external_request(request, &ExternalHttpConfig::default()).await;
            assert_eq!(
                response.map_get(&Value::symbol(Symbol::intern("status"))),
                Some(Value::int(200).unwrap())
            );
            assert_eq!(
                response.map_get(&Value::symbol(Symbol::intern("body"))),
                Some(Value::string("pong"))
            );
        });
    }

    #[test]
    fn openai_external_request_posts_json_to_configured_base_url() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            compio::runtime::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request_bytes = Vec::new();
                loop {
                    let (result, buffer) = stream.read([0u8; 2048]).await.into();
                    let count = result.unwrap();
                    if count == 0 {
                        break;
                    }
                    request_bytes.extend_from_slice(&buffer[..count]);
                    if complete_http_request(&request_bytes) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request_bytes);
                assert!(request.starts_with("POST /v1/embeddings HTTP/1.1\r\n"));
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("Content-Type: application/json"))
                );
                assert!(request.contains(r#""input":"hello""#));
                assert!(request.contains(r#""model":"source-workspace""#));
                let response =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                let (result, _) = stream.write_all(response.to_vec()).await.into();
                result.unwrap();
            })
            .detach();

            let request = ExternalRequest {
                service: Symbol::intern("openai"),
                payload: Value::map([
                    (
                        Value::symbol(Symbol::intern("base_url")),
                        Value::string(format!("http://{addr}")),
                    ),
                    (
                        Value::symbol(Symbol::intern("path")),
                        Value::string("/v1/embeddings"),
                    ),
                    (
                        Value::symbol(Symbol::intern("model")),
                        Value::string("source-workspace"),
                    ),
                    (
                        Value::symbol(Symbol::intern("json")),
                        Value::map([
                            (
                                Value::symbol(Symbol::intern("model")),
                                Value::string("source-workspace"),
                            ),
                            (
                                Value::symbol(Symbol::intern("input")),
                                Value::string("hello"),
                            ),
                        ]),
                    ),
                ]),
                timeout_millis: None,
            };
            let response = handle_external_request(request, &ExternalHttpConfig::default()).await;
            assert_eq!(
                response.map_get(&Value::symbol(Symbol::intern("status"))),
                Some(Value::int(200).unwrap())
            );
        });
    }

    #[test]
    fn openai_chat_completion_posts_openrouter_request_and_returns_json() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            compio::runtime::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request_bytes = Vec::new();
                loop {
                    let (result, buffer) = stream.read([0u8; 4096]).await.into();
                    let count = result.unwrap();
                    if count == 0 {
                        break;
                    }
                    request_bytes.extend_from_slice(&buffer[..count]);
                    if complete_http_request(&request_bytes) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request_bytes);
                assert!(request.starts_with("POST /api/v1/chat/completions HTTP/1.1\r\n"));
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("Authorization: Bearer test-key"))
                );
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("HTTP-Referer: https://mica.local"))
                );
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("X-OpenRouter-Title: Mica"))
                );
                assert!(request.contains(r#""model":"~openai/gpt-latest""#));
                assert!(request.contains(r#""role":"user""#));
                assert!(request.contains(r#""content":"ping""#));
                assert!(request.contains(r#""temperature":0.2"#));
                let response_body = r#"{"id":"chatcmpl-test","choices":[{"finish_reason":"stop","index":0,"message":{"role":"assistant","content":"pong"}}],"model":"openai/test","usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let (result, _) = stream.write_all(response.into_bytes()).await.into();
                result.unwrap();
            })
            .detach();

            let request = ExternalRequest {
                service: Symbol::intern("openai"),
                payload: Value::map([
                    (
                        Value::symbol(Symbol::intern("base_url")),
                        Value::string(format!("http://{addr}/api/v1")),
                    ),
                    (
                        Value::symbol(Symbol::intern("api_key")),
                        Value::string("test-key"),
                    ),
                    (
                        Value::symbol(Symbol::intern("referer")),
                        Value::string("https://mica.local"),
                    ),
                    (
                        Value::symbol(Symbol::intern("title")),
                        Value::string("Mica"),
                    ),
                    (
                        Value::symbol(Symbol::intern("model")),
                        Value::string("~openai/gpt-latest"),
                    ),
                    (
                        Value::symbol(Symbol::intern("messages")),
                        Value::list([Value::map([
                            (
                                Value::symbol(Symbol::intern("role")),
                                Value::string("user"),
                            ),
                            (
                                Value::symbol(Symbol::intern("content")),
                                Value::string("ping"),
                            ),
                        ])]),
                    ),
                    (
                        Value::symbol(Symbol::intern("options")),
                        Value::map([(
                            Value::symbol(Symbol::intern("temperature")),
                            Value::float(0.2).unwrap(),
                        )]),
                    ),
                ]),
                timeout_millis: None,
            };
            let response = handle_external_request(request, &ExternalHttpConfig::default()).await;
            let content = response
                .map_get(&Value::symbol(Symbol::intern("choices")))
                .and_then(|choices| choices.with_list(|choices| choices.first().cloned()).flatten())
                .and_then(|choice| choice.map_get(&Value::symbol(Symbol::intern("message"))))
                .and_then(|message| message.map_get(&Value::symbol(Symbol::intern("content"))));
            assert_eq!(content, Some(Value::string("pong")));
            let usage = response
                .map_get(&Value::symbol(Symbol::intern("usage")))
                .and_then(|usage| {
                    usage.map_get(&Value::symbol(Symbol::intern("total_tokens")))
                });
            assert_eq!(usage, Some(Value::int(6).unwrap()));
        });
    }

    #[test]
    fn normalizes_leaked_dsml_tool_calls_in_openai_response() {
        let mut response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "< | DSML | tool_calls>< | DSML | invoke name=\"source_file_window\">< | DSML | parameter name=\"path\" string=\"true\">crates/relation-kernel/src/computed.rs</ | DSML | parameter>< | DSML | parameter name=\"start_line\" string=\"false\">150</ | DSML | parameter>< | DSML | parameter name=\"line_count\" string=\"false\">100</ | DSML | parameter></ | DSML | invoke></ | DSML | tool_calls>"
                }
            }]
        });

        normalize_openai_tool_call_response(&mut response);

        let message = response["choices"][0]["message"]
            .as_object()
            .expect("message should stay an object");
        assert!(message["content"].is_null());
        let tool_calls = message["tool_calls"]
            .as_array()
            .expect("tool_calls should be normalized");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "source_file_window");
        let arguments: serde_json::Value =
            serde_json::from_str(tool_calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(arguments["path"], "crates/relation-kernel/src/computed.rs");
        assert_eq!(arguments["start_line"], 150);
        assert_eq!(arguments["line_count"], 100);
    }

    #[test]
    fn dsml_normalization_preserves_raw_response_number_kinds() {
        let response = runtime_value_from_json_text(
            r#"{
                "request_score": 1.0,
                "choices": [{
                    "message": {
                        "content": "< | DSML | invoke name=\"read\">< / DSML | invoke>"
                    }
                }]
            }"#,
        )
        .unwrap();

        let normalized = normalize_openai_tool_call_value(response).unwrap();
        assert_eq!(
            map_get(&normalized, "request_score").unwrap().as_float(),
            Some(1.0)
        );
        let message = map_get(&normalized, "choices")
            .and_then(|choices| choices.list_get(0))
            .and_then(|choice| map_get(&choice, "message"))
            .unwrap();
        assert_eq!(map_get(&message, "content"), Some(json_null_value()));
        assert_eq!(map_get(&message, "tool_calls").unwrap().list_len(), Some(1));
    }

    #[test]
    fn preserves_openai_responses_with_native_tool_calls() {
        let mut response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_native",
                        "type": "function",
                        "function": {
                            "name": "source_search",
                            "arguments": "{\"query\":\"computed\"}"
                        }
                    }]
                }
            }]
        });

        normalize_openai_tool_call_response(&mut response);

        assert_eq!(
            response["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_native"
        );
    }

    #[test]
    fn embedding_external_request_returns_vector_from_vllm_response() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            compio::runtime::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request_bytes = Vec::new();
                loop {
                    let (result, buffer) = stream.read([0u8; 2048]).await.into();
                    let count = result.unwrap();
                    if count == 0 {
                        break;
                    }
                    request_bytes.extend_from_slice(&buffer[..count]);
                    if complete_http_request(&request_bytes) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request_bytes);
                assert!(request.starts_with("POST /v1/embeddings HTTP/1.1\r\n"));
                assert!(request.contains(r#""input":"red brass lamp""#));
                assert!(request.contains(r#""model":"source-workspace""#));
                let response_body =
                    r#"{"data":[{"embedding":[0.25,0.5,0.75],"index":0}],"object":"list"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let (result, _) = stream.write_all(response.into_bytes()).await.into();
                result.unwrap();
            })
            .detach();

            let request = ExternalRequest {
                service: Symbol::intern("embedding"),
                payload: Value::map([
                    (
                        Value::symbol(Symbol::intern("base_url")),
                        Value::string(format!("http://{addr}/v1")),
                    ),
                    (
                        Value::symbol(Symbol::intern("model")),
                        Value::string("source-workspace"),
                    ),
                    (
                        Value::symbol(Symbol::intern("text")),
                        Value::string("red brass lamp"),
                    ),
                ]),
                timeout_millis: None,
            };
            let response = handle_external_request(request, &ExternalHttpConfig::default()).await;
            assert_eq!(
                response,
                Value::list([
                    Value::float(0.25).unwrap(),
                    Value::float(0.5).unwrap(),
                    Value::float(0.75).unwrap(),
                ])
            );
        });
    }

    #[test]
    fn vllm_embed_text_runs_through_daemon_external_handler() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            compio::runtime::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request_bytes = Vec::new();
                loop {
                    let (result, buffer) = stream.read([0u8; 2048]).await.into();
                    let count = result.unwrap();
                    if count == 0 {
                        break;
                    }
                    request_bytes.extend_from_slice(&buffer[..count]);
                    if complete_http_request(&request_bytes) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request_bytes);
                assert!(request.starts_with("POST /v1/embeddings HTTP/1.1\r\n"));
                assert!(request.contains(r#""input":"red brass lamp""#));
                assert!(request.contains(r#""model":"source-workspace""#));
                let response_body =
                    r#"{"data":[{"embedding":[0.25,0.5,0.75],"index":0}],"object":"list"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let (result, _) = stream.write_all(response.into_bytes()).await.into();
                result.unwrap();
            })
            .detach();

            let runner = mica_runtime::SourceRunner::new_empty_with_embedding_provider(
                mica_runtime::EmbeddingProviderKind::Vllm,
            );
            let resources = mica_driver::DriverResources::new(NonZeroUsize::new(1).unwrap());
            let mut owner = mica_driver::DriverOwner::builder(resources)
                .source_runner(runner)
                .external_request_handler(handler_with_embedding_base_url(format!(
                    "http://{addr}/v1"
                )))
                .build()
                .unwrap();
            let mut event_pump = owner.take_event_pump().unwrap();
            let source = "return embed_text(\"source-workspace\", \"red brass lamp\")".to_owned();
            let invocation = owner.administrator().evaluate(source).await.unwrap();
            assert!(matches!(
                invocation.initial_report().outcome,
                mica_runtime::TaskOutcome::Suspended { .. }
            ));

            assert_eq!(
                event_pump.drive_invocation(&invocation, |_| {}).await,
                mica_driver::InvocationOutcome::Completed(Value::list([
                    Value::float(0.25).unwrap(),
                    Value::float(0.5).unwrap(),
                    Value::float(0.75).unwrap(),
                ]))
            );
            owner.shutdown(&mut event_pump, |_| {}).await.unwrap();
        });
    }

    fn complete_http_request(bytes: &[u8]) -> bool {
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .split("\r\n")
            .find_map(|line| {
                let (name, value) = line.split_once(": ")?;
                name.eq_ignore_ascii_case("Content-Length").then_some(value)
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default();
        bytes.len() >= header_end + 4 + content_length
    }

    #[test]
    fn responses_request_is_stateless_and_uses_full_input() {
        let payload = Value::map([
            (
                Value::symbol(Symbol::intern("model")),
                Value::string("gpt-test"),
            ),
            (
                Value::symbol(Symbol::intern("input")),
                Value::list([Value::map([
                    (Value::symbol(Symbol::intern("role")), Value::string("user")),
                    (
                        Value::symbol(Symbol::intern("content")),
                        Value::string("hello"),
                    ),
                ])]),
            ),
            (
                Value::symbol(Symbol::intern("instructions")),
                Value::string("Be concise."),
            ),
            (
                Value::symbol(Symbol::intern("options")),
                Value::map([
                    (
                        Value::symbol(Symbol::intern("previous_response_id")),
                        Value::string("resp_should_not_be_sent"),
                    ),
                    (Value::symbol(Symbol::intern("store")), Value::bool(true)),
                ]),
            ),
        ]);

        let json = openai_responses_json(&payload).unwrap();
        assert_eq!(json["model"], "gpt-test");
        assert_eq!(json["input"][0]["content"], "hello");
        assert_eq!(json["instructions"], "Be concise.");
        assert_eq!(json["stream"], true);
        assert_eq!(json["store"], false);
        assert_eq!(json["include"][0], "reasoning.encrypted_content");
        assert!(json.get("previous_response_id").is_none());
    }

    #[test]
    fn responses_decoder_handles_split_text_and_tool_events() {
        let stream = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\r\n\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hello\"}\r\n\r\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"output_index\":1,\"delta\":\"{\\\"path\\\":\"}\r\n\r\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\r\n\r\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\r\n\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\r\n\r\n"
        );
        let mut decoder = OpenaiEventDecoder::new(OpenaiWireApi::Responses, "test".to_owned());
        let mut events = Vec::new();
        for chunk in stream.as_bytes().chunks(7) {
            events.extend(decoder.push(chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());
        let kinds = events
            .iter()
            .filter_map(|event| {
                map_get(event, "type")
                    .and_then(|value| value.as_symbol())
                    .and_then(Symbol::name)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "started",
                "text_delta",
                "tool_arguments_delta",
                "tool_call_ready",
                "completed"
            ]
        );
        let tool = events
            .iter()
            .find(|event| {
                map_get(event, "type") == Some(Value::symbol(Symbol::intern("tool_call_ready")))
            })
            .unwrap();
        assert_eq!(map_get(tool, "call_id"), Some(Value::string("call_1")));
        assert_eq!(map_get(tool, "name"), Some(Value::string("read")));
    }

    #[test]
    fn chat_decoder_normalizes_text_and_assembled_tool_calls() {
        let stream = concat!(
            "data: {\"id\":\"chat_1\",\"choices\":[{\"delta\":{\"content\":\"Hello\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"id\":\"chat_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"README.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat_1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut decoder =
            OpenaiEventDecoder::new(OpenaiWireApi::ChatCompletions, "test".to_owned());
        let mut events = Vec::new();
        for chunk in stream.as_bytes().chunks(11) {
            events.extend(decoder.push(chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());

        let tool = events
            .iter()
            .find(|event| {
                map_get(event, "type") == Some(Value::symbol(Symbol::intern("tool_call_ready")))
            })
            .unwrap();
        assert_eq!(map_get(tool, "call_id"), Some(Value::string("call_1")));
        assert_eq!(map_get(tool, "name"), Some(Value::string("read")));
        assert_eq!(
            map_get(tool, "arguments"),
            Some(Value::string("{\"path\":\"README.md\"}"))
        );
        assert!(events.iter().any(|event| {
            map_get(event, "type") == Some(Value::symbol(Symbol::intern("completed")))
        }));
    }
}
