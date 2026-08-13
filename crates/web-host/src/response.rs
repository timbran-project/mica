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

use crate::codec::{
    HttpCodecError, HttpRequest, HttpResponse, is_valid_header_name, is_valid_header_value,
    is_valid_response_reason,
};
use mica_runtime::TaskOutcome;
use mica_var::{Symbol, Value};

pub(crate) fn route_request(request: &HttpRequest, close: bool) -> HttpResponse {
    let response = match request.method.as_str() {
        "GET" if request.path == "/healthz" => HttpResponse::text(200, "OK", "ok\n"),
        "GET" if is_sync_client_path(&request.path) => HttpResponse::new(
            200,
            "OK",
            include_bytes!("../../webtransport-host/sync-client.js").to_vec(),
        )
        .with_header("content-type", b"text/javascript; charset=utf-8".as_slice())
        .with_header("cache-control", b"no-store".as_slice()),
        "GET" if request.path == "/" => HttpResponse::html(
            200,
            "OK",
            concat!(
                "<!doctype html><html><head><meta charset=\"utf-8\">",
                "<title>Mica</title></head><body><main>",
                "<h1>Mica</h1><p>HTTP/1.1 host is running.</p>",
                "</main></body></html>\n"
            ),
        ),
        "GET" => HttpResponse::text(404, "Not Found", "not found\n"),
        _ => HttpResponse::text(405, "Method Not Allowed", "method not allowed\n")
            .with_header("Allow", b"GET".as_slice()),
    };
    with_connection_header(response, close)
}

pub(crate) fn is_sync_client_path(path: &str) -> bool {
    path == "/sync-client.js" || path.starts_with("/sync-client.js?")
}

pub(crate) fn query_params(path: &str) -> std::collections::HashMap<String, String> {
    let mut params = std::collections::HashMap::new();
    if let Some((_, query)) = path.split_once('?') {
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                let key = url_decode(key);
                let value = url_decode(value);
                params.insert(key, value);
            }
        }
    }
    params
}

fn url_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut bytes = input.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next().and_then(hex_val);
            let lo = bytes.next().and_then(hex_val);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                result.push((hi << 4 | lo) as char);
            } else {
                result.push('%');
            }
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn error_response(error: HttpCodecError, close: bool) -> HttpResponse {
    let response = match error {
        HttpCodecError::UnsupportedTransferEncoding => HttpResponse::text(
            501,
            "Not Implemented",
            "transfer encoding is not supported\n",
        ),
        HttpCodecError::HeaderTooLarge | HttpCodecError::BodyTooLarge => {
            HttpResponse::text(413, "Content Too Large", "request is too large\n")
        }
        HttpCodecError::TooManyHeaders => {
            HttpResponse::text(431, "Request Header Fields Too Large", "too many headers\n")
        }
        _ => HttpResponse::text(400, "Bad Request", "bad request\n"),
    };
    with_connection_header(response, close)
}

pub(crate) fn internal_error_response(message: impl Into<String>, close: bool) -> HttpResponse {
    with_connection_header(
        HttpResponse::text(500, "Internal Server Error", message.into()),
        close,
    )
}

pub(crate) fn response_from_outcome(outcome: TaskOutcome, close: bool) -> HttpResponse {
    match outcome {
        TaskOutcome::Complete { value, .. } => {
            decode_response_value(value, close).unwrap_or_else(|error| {
                internal_error_response(format!("invalid HTTP response value: {error}"), close)
            })
        }
        TaskOutcome::Aborted { error, .. } => {
            internal_error_response(format!("HTTP handler aborted with error: {error}"), close)
        }
        TaskOutcome::Suspended { .. } => {
            internal_error_response("HTTP handler suspended before returning a response", close)
        }
    }
}

pub(crate) fn decode_response_value(value: Value, close: bool) -> Result<HttpResponse, String> {
    if let Some(text) = value.with_str(str::to_owned) {
        return Ok(with_connection_header(
            HttpResponse::text(200, "OK", text),
            close,
        ));
    }
    if value == Value::nothing() {
        return Ok(with_connection_header(
            HttpResponse::new(204, "No Content", Vec::new()),
            close,
        ));
    }
    if value.map_len().is_none() {
        return Err("response must be a string, nothing, or response map".to_owned());
    }
    let status = map_int(&value, "status")?.unwrap_or(200);
    if !(100..=999).contains(&status) {
        return Err("status must be between 100 and 999".to_owned());
    }
    let reason =
        map_string(&value, "reason")?.unwrap_or_else(|| standard_reason(status as u16).to_owned());
    if !is_valid_response_reason(&reason) {
        return Err(":reason contains invalid HTTP reason phrase characters".to_owned());
    }
    let body = map_body(&value)?.unwrap_or_default();
    let mut response = HttpResponse::new(status as u16, reason, body);
    for (name, value) in map_headers(&value)? {
        validate_response_header(&name, &value)?;
        response = response.with_header(name, value);
    }
    Ok(with_connection_header(response, close))
}

fn map_int(value: &Value, key: &str) -> Result<Option<i64>, String> {
    let Some(value) = value.map_get(&Value::symbol(Symbol::intern(key))) else {
        return Ok(None);
    };
    value
        .as_int()
        .map(Some)
        .ok_or_else(|| format!(":{key} must be an integer"))
}

fn map_string(value: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = value.map_get(&Value::symbol(Symbol::intern(key))) else {
        return Ok(None);
    };
    value
        .with_str(str::to_owned)
        .map(Some)
        .ok_or_else(|| format!(":{key} must be a string"))
}

fn map_body(value: &Value) -> Result<Option<Vec<u8>>, String> {
    let Some(body) = value.map_get(&Value::symbol(Symbol::intern("body"))) else {
        return Ok(None);
    };
    if let Some(text) = body.with_str(str::to_owned) {
        return Ok(Some(text.into_bytes()));
    }
    body.with_bytes(<[u8]>::to_vec)
        .map(Some)
        .ok_or_else(|| ":body must be a string or bytes".to_owned())
}

fn map_headers(value: &Value) -> Result<Vec<(String, Vec<u8>)>, String> {
    let Some(headers) = value.map_get(&Value::symbol(Symbol::intern("headers"))) else {
        return Ok(Vec::new());
    };
    headers
        .with_list(|headers| {
            headers
                .iter()
                .map(header_pair)
                .collect::<Result<Vec<_>, _>>()
        })
        .ok_or(":headers must be a list")?
}

fn header_pair(value: &Value) -> Result<(String, Vec<u8>), String> {
    value
        .with_list(|parts| {
            let [name, value] = parts else {
                return Err("header entries must be [name, value]".to_owned());
            };
            let name = name
                .with_str(str::to_owned)
                .ok_or_else(|| "header name must be a string".to_owned())?;
            let value = if let Some(text) = value.with_str(str::to_owned) {
                text.into_bytes()
            } else {
                value
                    .with_bytes(<[u8]>::to_vec)
                    .ok_or_else(|| "header value must be a string or bytes".to_owned())?
            };
            Ok((name, value))
        })
        .ok_or_else(|| "header entries must be lists".to_owned())?
}

fn validate_response_header(name: &str, value: &[u8]) -> Result<(), String> {
    if !is_valid_header_name(name) {
        return Err("header name contains invalid HTTP token characters".to_owned());
    }
    if !is_valid_header_value(value) {
        return Err("header value contains invalid HTTP field characters".to_owned());
    }
    Ok(())
}

fn standard_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

fn with_connection_header(response: HttpResponse, close: bool) -> HttpResponse {
    if close {
        response.with_header("Connection", b"close".as_slice())
    } else {
        response.with_header("Connection", b"keep-alive".as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::HttpRequest;

    #[test]
    fn routes_health_check() {
        let response = route_request(
            &HttpRequest {
                method: "GET".to_owned(),
                path: "/healthz".to_owned(),
                version: 1,
                headers: Vec::new(),
                body: Vec::new(),
            },
            false,
        );

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok\n");
        assert_eq!(
            response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("connection"))
                .map(|header| header.value.as_slice()),
            Some(b"keep-alive".as_slice())
        );
    }

    #[test]
    fn routes_sync_client_module() {
        let response = route_request(
            &HttpRequest {
                method: "GET".to_owned(),
                path: "/sync-client.js".to_owned(),
                version: 1,
                headers: Vec::new(),
                body: Vec::new(),
            },
            false,
        );

        assert_eq!(response.status, 200);
        assert!(
            response
                .body
                .windows(b"MicaWebTransportSyncClient".len())
                .any(|window| window == b"MicaWebTransportSyncClient")
        );
        assert_eq!(
            response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("content-type"))
                .map(|header| header.value.as_slice()),
            Some(b"text/javascript; charset=utf-8".as_slice())
        );
        assert_eq!(
            response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("cache-control"))
                .map(|header| header.value.as_slice()),
            Some(b"no-store".as_slice())
        );

        let cache_busted = route_request(
            &HttpRequest {
                method: "GET".to_owned(),
                path: "/sync-client.js?surface=mud".to_owned(),
                version: 1,
                headers: Vec::new(),
                body: Vec::new(),
            },
            false,
        );
        assert_eq!(cache_busted.status, 200);
    }

    #[test]
    fn rejects_unsupported_methods() {
        let response = route_request(
            &HttpRequest {
                method: "POST".to_owned(),
                path: "/".to_owned(),
                version: 1,
                headers: Vec::new(),
                body: Vec::new(),
            },
            true,
        );

        assert_eq!(response.status, 405);
        assert_eq!(
            response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("connection"))
                .map(|header| header.value.as_slice()),
            Some(b"close".as_slice())
        );
    }

    #[test]
    fn decodes_mica_response_map() {
        let response = decode_response_value(
            Value::map([
                (
                    Value::symbol(Symbol::intern("status")),
                    Value::int(201).unwrap(),
                ),
                (
                    Value::symbol(Symbol::intern("headers")),
                    Value::list([Value::list([
                        Value::string("content-type"),
                        Value::string("text/plain"),
                    ])]),
                ),
                (
                    Value::symbol(Symbol::intern("body")),
                    Value::string("created"),
                ),
            ]),
            false,
        )
        .unwrap();

        assert_eq!(response.status, 201);
        assert_eq!(response.reason, "Created");
        assert_eq!(response.body, b"created");
        assert_eq!(
            response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("content-type"))
                .map(|header| header.value.as_slice()),
            Some(b"text/plain".as_slice())
        );
    }

    #[test]
    fn rejects_mica_response_header_name_injection() {
        let error = decode_response_value(
            Value::map([
                (
                    Value::symbol(Symbol::intern("headers")),
                    Value::list([Value::list([
                        Value::string("x-test\r\nset-cookie"),
                        Value::string("1"),
                    ])]),
                ),
                (Value::symbol(Symbol::intern("body")), Value::string("bad")),
            ]),
            false,
        )
        .unwrap_err();

        assert_eq!(error, "header name contains invalid HTTP token characters");
    }

    #[test]
    fn rejects_mica_response_header_value_injection() {
        let error = decode_response_value(
            Value::map([
                (
                    Value::symbol(Symbol::intern("headers")),
                    Value::list([Value::list([
                        Value::string("x-test"),
                        Value::string("ok\r\nset-cookie: bad"),
                    ])]),
                ),
                (Value::symbol(Symbol::intern("body")), Value::string("bad")),
            ]),
            false,
        )
        .unwrap_err();

        assert_eq!(error, "header value contains invalid HTTP field characters");
    }

    #[test]
    fn rejects_mica_response_reason_injection() {
        let error = decode_response_value(
            Value::map([
                (
                    Value::symbol(Symbol::intern("reason")),
                    Value::string("OK\r\nInjected"),
                ),
                (Value::symbol(Symbol::intern("body")), Value::string("bad")),
            ]),
            false,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ":reason contains invalid HTTP reason phrase characters"
        );
    }
}
