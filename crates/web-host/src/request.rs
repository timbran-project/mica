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

use crate::codec::{HttpRequest, HttpResponse};
use crate::response::{
    internal_error_response, is_sync_client_path, response_from_submitted, route_request,
};
use crate::{InProcessWebHost, RequestBinding, format_driver_error};
use mica_driver::{CompioTaskDriver, DriverError};
use mica_runtime::Tuple;
use mica_var::{Identity, Symbol, Value};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug)]
pub(crate) struct RequestFact {
    relation: Symbol,
    tuple: Tuple,
}

impl RequestFact {
    fn new(relation: Symbol, values: impl IntoIterator<Item = Value>) -> Self {
        Self {
            relation,
            tuple: Tuple::new(values),
        }
    }
}

struct RequestFactScope {
    driver: Arc<CompioTaskDriver>,
    endpoint: Identity,
    tuples: Option<Vec<(Symbol, Tuple)>>,
}

impl RequestFactScope {
    fn new(
        driver: Arc<CompioTaskDriver>,
        endpoint: Identity,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Self {
        Self {
            driver,
            endpoint,
            tuples: Some(tuples),
        }
    }

    fn close(mut self) -> Result<usize, DriverError> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<usize, DriverError> {
        let Some(tuples) = self.tuples.take() else {
            return Ok(0);
        };
        self.driver
            .close_endpoint_and_retract_volatile_tuples_named(self.endpoint, tuples)
            .map(|report| report.relation_changes)
    }
}

impl Drop for RequestFactScope {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            tracing::warn!(
                endpoint = self.endpoint.raw(),
                error = %self.driver.format_error(&error),
                "failed to clean up volatile request facts"
            );
        }
    }
}

pub(crate) async fn handle_in_process_request(
    host: &InProcessWebHost,
    binding: &RequestBinding,
    request: &HttpRequest,
    close: bool,
) -> HttpResponse {
    if request.method == "GET"
        && request.path == "/_mica/metrics/snapshot"
        && metrics_snapshot_enabled()
    {
        let body = serde_json::to_vec_pretty(&crate::metrics::metrics_snapshot_json())
            .unwrap_or_else(|_| b"{\"error\":\"failed to serialize metrics\"}".to_vec());
        return HttpResponse::new(200, "OK", body)
            .with_header("Content-Type", b"application/json; charset=utf-8")
            .with_header("Cache-Control", b"no-store");
    }

    if request.method == "GET" && (request.path == "/healthz" || is_sync_client_path(&request.path))
    {
        return route_request(request, close);
    }

    if let Some(auth) = &host.auth {
        if let Some(response) = auth.handle_auth_start_github(request).await {
            return response;
        }
        if let Some(response) = auth.handle_auth_callback(request).await {
            return response;
        }
        if let Some(response) = auth.handle_auth_login_local(request).await {
            return response;
        }
        if let Some(response) = auth.handle_auth_create_local(request).await {
            return response;
        }
        if let Some(response) = auth.handle_auth_logout(request).await {
            return response;
        }
    }

    let effective_actor = if let Some(auth) = &host.auth {
        if crate::auth::is_pre_auth_login_path(&request.path) {
            match host
                .driver
                .named_identity(Symbol::intern(&auth.config.login_actor))
            {
                Ok(actor) => Some(actor),
                Err(error) => {
                    tracing::warn!(
                        actor_name = %auth.config.login_actor,
                        error = %error,
                        "failed to resolve pre-auth login actor"
                    );
                    return HttpResponse::new(
                        500,
                        "Internal Server Error",
                        b"Failed to resolve login identity".to_vec(),
                    );
                }
            }
        } else if !crate::auth::is_unauthenticated_path(&request.path) {
            let cookie_header = request
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("cookie"))
                .map(|h| std::str::from_utf8(&h.value).unwrap_or(""));

            match auth.resolve_auth_context(cookie_header).await {
                Ok(Some(ctx)) => {
                    match host.driver.named_identity(Symbol::intern(&ctx.actor_name)) {
                        Ok(actor) => Some(actor),
                        Err(error) => {
                            tracing::warn!(
                                actor_name = %ctx.actor_name,
                                error = %error,
                                "failed to resolve authenticated actor"
                            );
                            return HttpResponse::new(
                                500,
                                "Internal Server Error",
                                b"Failed to resolve user identity".to_vec(),
                            );
                        }
                    }
                }
                Ok(None) => {
                    if request.method == "GET" {
                        return crate::auth::login_redirect_response(&request.path);
                    }
                    return HttpResponse::new(
                        401,
                        "Unauthorized",
                        b"Authentication required".to_vec(),
                    );
                }
                Err(error) => {
                    tracing::warn!(error = %error, "authentication failed");
                    if request.method == "GET" {
                        tracing::info!(
                            path = %request.path,
                            "clearing invalid session cookie and redirecting to login"
                        );
                        return crate::auth::clear_session_login_redirect_response(
                            &request.path,
                            &auth.config.cookie_name,
                            auth.config.cookie_secure,
                        );
                    }
                    return HttpResponse::new(
                        401,
                        "Unauthorized",
                        b"Invalid or expired session".to_vec(),
                    );
                }
            }
        } else {
            binding.actor
        }
    } else {
        binding.actor
    };

    let request_id = match host.allocate_request() {
        Ok(request_id) => request_id,
        Err(error) => return internal_error_response(error, close),
    };
    let request_endpoint = match host.allocate_endpoint() {
        Ok(endpoint) => endpoint,
        Err(error) => return internal_error_response(error, close),
    };
    let request_facts = request_facts(request_id, binding.principal, effective_actor, request);
    let request_tuples = request_facts
        .iter()
        .map(|fact| (fact.relation, fact.tuple.clone()))
        .collect::<Vec<_>>();
    if let Err(error) = host
        .driver
        .open_endpoint_with_context_and_volatile_tuples_named(
            request_endpoint,
            Some(binding.principal),
            effective_actor,
            Symbol::intern("http-request"),
            request_tuples.clone(),
        )
    {
        return internal_error_response(format_driver_error(&host.driver, error), close);
    }
    let request_scope =
        RequestFactScope::new(Arc::clone(&host.driver), request_endpoint, request_tuples);

    let submitted = host
        .driver
        .submit_invocation_for_endpoint(
            request_endpoint,
            Symbol::intern("http_request"),
            vec![(Symbol::intern("request"), Value::identity(request_id))],
        )
        .await;
    if let Err(error) = request_scope.close() {
        return internal_error_response(format_driver_error(&host.driver, error), close);
    }

    match submitted {
        Ok(submitted) => response_from_submitted(submitted, close),
        Err(error) => internal_error_response(format_driver_error(&host.driver, error), close),
    }
}

fn metrics_snapshot_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MICA_METRICS_SNAPSHOT")
            .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    })
}

fn visit_request_facts<E>(
    request_id: Identity,
    principal: Identity,
    actor: Option<Identity>,
    request: &HttpRequest,
    mut visit: impl FnMut(RequestFact) -> Result<(), E>,
) -> Result<(), E> {
    let request_value = Value::identity(request_id);
    visit(RequestFact::new(
        Symbol::intern("HttpRequest"),
        [request_value.clone()],
    ))?;
    visit(RequestFact::new(
        Symbol::intern("RequestMethod"),
        [request_value.clone(), Value::string(&request.method)],
    ))?;
    visit(RequestFact::new(
        Symbol::intern("RequestPath"),
        [request_value.clone(), Value::string(&request.path)],
    ))?;
    visit(RequestFact::new(
        Symbol::intern("RequestVersion"),
        [
            request_value.clone(),
            Value::int(i64::from(request.version)).unwrap(),
        ],
    ))?;
    visit(RequestFact::new(
        Symbol::intern("RequestPrincipal"),
        [request_value.clone(), Value::identity(principal)],
    ))?;
    if let Some(actor) = actor {
        visit(RequestFact::new(
            Symbol::intern("RequestActor"),
            [request_value.clone(), Value::identity(actor)],
        ))?;
    }
    for header in &request.headers {
        visit(RequestFact::new(
            Symbol::intern("RequestHeader"),
            [
                request_value.clone(),
                Value::string(header.name.to_ascii_lowercase()),
                Value::bytes(&header.value),
            ],
        ))?;
    }
    if !request.body.is_empty() {
        visit(RequestFact::new(
            Symbol::intern("RequestBody"),
            [request_value, Value::bytes(&request.body)],
        ))?;
    }
    Ok(())
}

fn request_facts(
    request_id: Identity,
    principal: Identity,
    actor: Option<Identity>,
    request: &HttpRequest,
) -> Vec<RequestFact> {
    let mut facts = Vec::new();
    visit_request_facts(request_id, principal, actor, request, |fact| {
        facts.push(fact);
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap();
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_runtime::{SYSTEM_ENDPOINT, SourceRunner, TaskOutcome};
    use std::cell::Cell;
    use std::future::pending;
    use std::num::NonZeroUsize;
    use std::rc::Rc;
    use std::time::Duration;

    fn test_host(source: &str) -> (Arc<InProcessWebHost>, Identity) {
        let mut runner = SourceRunner::new_empty();
        runner.run_filein(source).unwrap();
        let web = runner.named_identity(Symbol::intern("web")).unwrap();
        let driver = CompioTaskDriver::spawn_with_workers(runner, NonZeroUsize::new(1)).unwrap();
        (Arc::new(InProcessWebHost::new(driver)), web)
    }

    fn test_request(path: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".to_owned(),
            path: path.to_owned(),
            version: 1,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn open_request_scope(
        host: &Arc<InProcessWebHost>,
        binding: &RequestBinding,
        request: &HttpRequest,
    ) -> (Identity, RequestFactScope) {
        let request_id = host.allocate_request().unwrap();
        let endpoint = host.allocate_endpoint().unwrap();
        let facts = request_facts(request_id, binding.principal, binding.actor, request);
        let tuples = facts
            .into_iter()
            .map(|fact| (fact.relation, fact.tuple))
            .collect::<Vec<_>>();
        host.driver
            .open_endpoint_with_context_and_volatile_tuples_named(
                endpoint,
                Some(binding.principal),
                binding.actor,
                Symbol::intern("http-request"),
                tuples.clone(),
            )
            .unwrap();
        let scope = RequestFactScope::new(Arc::clone(&host.driver), endpoint, tuples);
        (request_id, scope)
    }

    async fn query_as(host: &InProcessWebHost, actor: Identity, source: &str) -> Value {
        let request = mica_runtime::TaskRequest {
            actor: Some(actor),
            ..SourceRunner::root_source_request(source)
        };
        let submitted = host
            .driver
            .submit_source(SYSTEM_ENDPOINT, request)
            .await
            .unwrap();
        let TaskOutcome::Complete { value, .. } = submitted.outcome else {
            panic!("lifecycle query did not complete")
        };
        value
    }

    async fn assert_request_lifecycle_empty(host: &InProcessWebHost, actor: Identity) {
        for (source, heading) in [
            ("return HttpRequest(?request)", "request"),
            ("return EndpointOpen(?endpoint)", "endpoint"),
        ] {
            assert_eq!(
                query_as(host, actor, source).await,
                Value::relation([Symbol::intern(heading)], std::iter::empty::<Tuple>(),).unwrap()
            );
        }
    }

    #[test]
    fn request_facts_include_core_request_neighbourhood() {
        let request_id = Identity::new(0x00eb_0000_0000_0001).unwrap();
        let principal = Identity::new(0x00e0_0000_0000_0001).unwrap();
        let facts = request_facts(
            request_id,
            principal,
            None,
            &HttpRequest {
                method: "GET".to_owned(),
                path: "/hello".to_owned(),
                version: 1,
                headers: vec![crate::codec::HttpHeader::new("Accept", b"text/plain")],
                body: Vec::new(),
            },
        );

        assert!(
            facts
                .iter()
                .any(|fact| fact.relation == Symbol::intern("HttpRequest")
                    && fact.tuple.values() == [Value::identity(request_id)])
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.relation == Symbol::intern("RequestPath")
                    && fact.tuple.values()
                        == [Value::identity(request_id), Value::string("/hello")])
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.relation == Symbol::intern("RequestHeader")
                    && fact.tuple.values()
                        == [
                            Value::identity(request_id),
                            Value::string("accept"),
                            Value::bytes(b"text/plain")
                        ])
        );
    }

    #[test]
    fn request_facts_include_principal_and_optional_actor_binding() {
        let request_id = Identity::new(0x00eb_0000_0000_0002).unwrap();
        let principal = Identity::new(0x00e0_0000_0000_0002).unwrap();
        let actor = Identity::new(0x00e0_0000_0000_0003).unwrap();
        let facts = request_facts(
            request_id,
            principal,
            Some(actor),
            &HttpRequest {
                method: "GET".to_owned(),
                path: "/secure".to_owned(),
                version: 1,
                headers: Vec::new(),
                body: Vec::new(),
            },
        );

        assert!(
            facts
                .iter()
                .any(|fact| fact.relation == Symbol::intern("RequestPrincipal")
                    && fact.tuple.values()
                        == [Value::identity(request_id), Value::identity(principal)])
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.relation == Symbol::intern("RequestActor")
                    && fact.tuple.values()
                        == [Value::identity(request_id), Value::identity(actor)])
        );
    }

    #[test]
    fn in_process_request_cleans_lifecycle_before_returning_response() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let (host, web) = test_host(include_str!("../../../apps/web/http-core.mica"));
            let response = handle_in_process_request(
                &host,
                &RequestBinding {
                    principal: web,
                    actor: Some(web),
                },
                &test_request("/hello"),
                false,
            )
            .await;
            assert_eq!(response.status, 200);
            assert!(
                host.driver
                    .inner_runner()
                    .named_identity(Symbol::intern("endpoint:0"))
                    .is_err()
            );
            assert_request_lifecycle_empty(&host, web).await;
        });
    }

    #[test]
    fn aborted_handler_cleans_request_lifecycle() {
        const ABORTING_HTTP: &str = r#"
            make_relation(:HttpRequest, 1, :volatile)
            make_relation(:RequestMethod, 2, :volatile)
            make_relation(:RequestPath, 2, :volatile)
            make_relation(:RequestVersion, 2, :volatile)
            make_relation(:RequestPrincipal, 2, :volatile)
            make_relation(:RequestActor, 2, :volatile)
            make_relation(:RequestHeader, 3, :volatile)
            make_relation(:RequestBody, 2, :volatile)
            make_relation(:CanRead, 2)
            make_relation(:CanInvoke, 2)
            make_identity(:web)

            grant #web
              read:
                :HttpRequest
                :RequestMethod
                :RequestPath
                :RequestVersion
                :RequestPrincipal
                :RequestActor
                :RequestHeader
                :RequestBody
              invoke:
                :http_request
            end

            verb http_request(request)
              raise E_INVARG, "request failed"
            end
        "#;

        compio::runtime::Runtime::new().unwrap().block_on(async {
            let (host, web) = test_host(ABORTING_HTTP);
            let response = handle_in_process_request(
                &host,
                &RequestBinding {
                    principal: web,
                    actor: Some(web),
                },
                &test_request("/abort"),
                false,
            )
            .await;

            assert_eq!(response.status, 500);
            assert!(String::from_utf8_lossy(&response.body).contains("request failed"));
            assert_request_lifecycle_empty(&host, web).await;
        });
    }

    #[test]
    fn cancelled_request_scope_cleans_only_its_owned_rows() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let (host, web) = test_host(include_str!("../../../apps/web/http-core.mica"));
            let binding = RequestBinding {
                principal: web,
                actor: Some(web),
            };
            let (cancelled_request, cancelled_scope) =
                open_request_scope(&host, &binding, &test_request("/cancelled"));
            let (live_request, live_scope) =
                open_request_scope(&host, &binding, &test_request("/live"));
            let started = Rc::new(Cell::new(false));
            let task_started = Rc::clone(&started);
            let task = compio::runtime::spawn(async move {
                let _scope = cancelled_scope;
                task_started.set(true);
                pending::<()>().await;
            });
            while !started.get() {
                compio::time::sleep(Duration::from_millis(1)).await;
            }

            assert!(task.cancel().await.is_none());
            assert_eq!(
                query_as(&host, web, "return HttpRequest(?request)").await,
                Value::relation(
                    [Symbol::intern("request")],
                    [Tuple::from([Value::identity(live_request)])],
                )
                .unwrap()
            );
            assert_ne!(cancelled_request, live_request);

            drop(live_scope);
            assert_request_lifecycle_empty(&host, web).await;
        });
    }

    #[test]
    fn concurrent_requests_keep_their_lifecycle_rows_independent() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let (host, web) = test_host(include_str!("../../../apps/web/http-core.mica"));
            let binding = RequestBinding {
                principal: web,
                actor: Some(web),
            };
            let first_host = Arc::clone(&host);
            let first_binding = binding.clone();
            let first = compio::runtime::spawn(async move {
                handle_in_process_request(&first_host, &first_binding, &test_request("/"), false)
                    .await
            });
            let second_host = Arc::clone(&host);
            let second = compio::runtime::spawn(async move {
                handle_in_process_request(&second_host, &binding, &test_request("/hello"), false)
                    .await
            });

            let first = first.await.unwrap();
            let second = second.await.unwrap();
            assert_eq!(first.status, 200);
            assert_eq!(
                first.body,
                b"<!doctype html><title>Mica</title><h1>Mica</h1><p>Hello from Mica.</p>"
            );
            assert_eq!(second.status, 200);
            assert_eq!(second.body, b"hello from mica");
            assert_request_lifecycle_empty(&host, web).await;
        });
    }
}
