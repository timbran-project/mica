# Frobs

Frobs are lightweight parameterized values with a delegate identity and a payload:

```mica
#inspection_event<{:actor -> #alice, :subject -> #sensor, :result -> :passed}>
```

They are useful when a value needs behaviour or interpretation without becoming a durable object
identity. An event value, a substitution template node, or a rendered fragment may need structured
data and dispatchable behaviour, but it does not necessarily deserve a permanent identity in the
world.

The design goal is to avoid polluting the durable identity space with short lived structured things.
If every event, rendered fragment, or substitution piece became a full identity, the world would
accumulate objects that are not really world entities. A frob keeps the structure in a value while
still giving dispatch something meaningful to restrict on.

A frob has two parts:

- the delegate identity, such as `#inspection_event`;
- the payload value, such as `{:actor -> #alice, :subject -> #sensor, :result -> :passed}`.

Access the delegate and payload through builtins:

```mica
let delegate = frob_delegate(event)
let payload = frob_value(event)
```

The payload can be any value appropriate for the domain:

```mica
#message<"hello">
#status_event<{:work -> #inspection, :from -> :pending, :to -> :complete}>
#html_node<{:tag -> :a, :attrs -> {:href -> "/docs"}, :children -> ["docs"]}>
```

The delegate identity says how the value should be interpreted. The payload is the data being
interpreted.

Frobs can participate in dispatch restrictions:

```mica
verb render(event @ #event<_>)
  return frob_value(event)[:message]
end
```

The restriction `#event<_>` means "a frob whose delegate matches `#event`, with any payload". This
gives libraries a way to define behaviour over families of structured values.

Restrictions can be more specific when the caller wants a particular delegate:

```mica
verb render(event @ #status_event<_>)
  let data = frob_value(event)
  return [data[:work], " changed status."]
end
```

This is not the same as prototype delegation between durable identities. A frob delegates at the
value level: the value carries a delegate identity and a payload. Prototype delegation is world
state expressed through `Delegates` facts and is used to decide whether identities match role
restrictions.

Persistability depends on the payload. A frob containing ephemeral capability values cannot be filed
out as durable source.

Frobs should be used for values that need structure and interpretation. Use an identity when the
thing should have durable facts, policy, history, or authorable behaviour attached to it.
