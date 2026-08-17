# Mica External HTTP Host

`mica-external-http` provides reusable Compio handlers for Mica external HTTP requests. It supports
the generic `http` service, embedding requests, and OpenAI-compatible Chat Completions and Responses
requests. Streaming LLM responses are normalized into the typed mailbox events consumed by the Mica
agent fileins.

Embedded hosts install the handlers on `DriverOwner::builder`:

```rust
let owner = DriverOwner::builder(resources)
    .external_request_handler(mica_external_http::handler())
    .external_stream_request_handler(mica_external_http::stream_handler())
    .build()?;
```

The daemon wraps the same request functions with its telemetry. Other hosts can use the handlers
directly without depending on the daemon, its transports, or its authentication stack.
