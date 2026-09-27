# mica-daemon

`mica-daemon` starts a Mica runtime, files in whatever Mica source files you pass with `--filein`,
and binds whichever host surfaces you ask it to expose.

In practice, this is the process that turns a Mica world into a running service. It can host:

- HTTP;
- WebTransport;
- telnet;
- host RPC over ZeroMQ.

The daemon itself is not where request handling or command behaviour lives. Those stay in Mica
source. The Rust side owns transport setup, protocol parsing, connection lifecycles, and routing
host input into the runtime.

The daemon does not load an app by default. Wrapper scripts such as `scripts/chat.sh` and
`scripts/mud.sh`, or your own command line, are expected to provide the filein set.

## Browser Surface

For a minimal HTTP route, load `apps/web/http-core.mica`:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/web/http-core.mica \
  --web-bind 127.0.0.1:8008
```

HTTP requests are routed into the Mica `:http_request(...)` verb. Request handlers run as the
configured web principal, `#web` by default, so ordinary Mica authority policy still applies.

If you also want the WebTransport host:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/shared/sync-host.mica \
  --filein apps/chat/sync.mica \
  --filein apps/shared/sync-dom.mica \
  --filein apps/chat/http.mica \
  --web-bind 127.0.0.1:8008 \
  --webtransport-bind 127.0.0.1:4433 \
  --webtransport-cert cert.pem \
  --webtransport-key key.pem
```

WebTransport sessions are bound into the same runtime, under the configured WebTransport principal,
`#web` by default.

If you just want to launch the browser examples, use the wrapper scripts at the workspace root
instead:

```sh
scripts/chat.sh
scripts/mud.sh
```

## Telnet Surface

To expose the older telnet-oriented surface:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/shared/string.mica \
  --filein apps/shared/events.mica \
  --filein apps/mud/core.mica \
  --filein apps/mud/event-substitutions.mica \
  --filein apps/mud/command-parser.mica \
  --telnet-bind 127.0.0.1:7777
```

Line input is routed into the in-world `:command(...)` verb. The telnet actor defaults to `#alice`,
and can be changed with `--actor`.

## RPC Surface

To expose host RPC over ZeroMQ, for example with the MUD command surface loaded:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/shared/string.mica \
  --filein apps/shared/events.mica \
  --filein apps/mud/core.mica \
  --filein apps/mud/event-substitutions.mica \
  --filein apps/mud/command-parser.mica \
  --rpc-bind ipc:///tmp/mica-rpc.sock
```

That lets separate host processes connect to the runtime over the host protocol. For example, a
separate telnet host can bind through the RPC socket:

```sh
cargo run --bin mica-daemon -- \
  --filein apps/shared/string.mica \
  --filein apps/shared/events.mica \
  --filein apps/mud/core.mica \
  --filein apps/mud/event-substitutions.mica \
  --filein apps/mud/command-parser.mica \
  --rpc-bind ipc:///tmp/mica-rpc.sock
cargo run --bin mica-telnet-host -- --rpc ipc:///tmp/mica-rpc.sock --bind 127.0.0.1:7778
```

## Notes

- Repeat `--editor-root DIR` to enable the editor's fixed read, stat, list, and atomic-save services
  within those directories. With no configured root, these services deny access. They run through
  the existing external-request effect boundary. See
  [the editor file contract](../../apps/editor/README.md) for limits, stamps, and save semantics.
- The daemon needs at least one surface: `--rpc-bind`, `--telnet-bind`, `--web-bind`, or
  `--webtransport-bind`.
- WebTransport requires `--webtransport-cert` and `--webtransport-key`.
- Multiple surfaces can be hosted in one process.
