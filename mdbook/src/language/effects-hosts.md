# Effects and Hosts

Effects are committed outputs from Mica tasks to the outside world. They are not performed directly
by arbitrary task code.

```mica
emit(actor, "Calibration recorded.")
```

`emit` always has a target. The target is an identity chosen by the world or host integration: an
actor, endpoint, session, tool bridge, browser projection, or other effect sink. The runtime records
the pending effect; it does not know how a telnet host, HTTP host, browser client, or tool runner
will deliver it.

An effect says "if this task commits, deliver this value to this target." The target must be an
identity representing an actor, endpoint, or other host-owned sink. The value may be a string for a
simple text host, or a richer value for a structured host.

Agent and tool integrations should usually use structured values rather than plain strings:

```mica
emit(#planner, #tool_call<{:tool -> :search, :query -> "open issues"}>)
emit(#user, #user_message<"The release notes are ready.">)
```

Other structured effect values might request an embedding, deliver a webhook, or update a projected
UI relation:

```mica
emit(#embedding_host, #embedding_request<{:text -> summary, :reply_to -> tx}>)
emit(#webhook_host, #webhook_delivery<{:url -> url, :body -> payload}>)
emit(#browser, #ui_patch<{:target -> #status_panel, :text -> "running"}>)
```

The task records committed intent. The host decides how to perform the tool call, send the user
message, request an embedding, or deliver a webhook.

The runtime records effects when the task reaches a successful commit boundary. Hosts decide how to
deliver those effects to endpoints, consoles, telnet connections, HTTP clients, browser projections,
or other external surfaces.

This keeps external action behind the same transaction discipline as relation writes. A task that
aborts does not publish its pending effects.

Hosts are intentionally outside the language core. A telnet host, HTTP host, browser projection, or
tool bridge can all consume committed effects without changing the semantics of task execution.

Effects are not a general escape hatch around authority. Recording an effect requires effect
authority, and a host should still validate what it is willing to perform. For example, a tool host
may accept `#tool_call` values only from actors whose session authority includes that tool surface.
