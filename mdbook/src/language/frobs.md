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

## A little history lesson...

Mica borrows the name and the basic idea from Greg Hudson's Coldmud. Its release notes introduce
frobs in version 0.4, dated September 19, 1993. Coldmud treats a frob as a list or dictionary paired
with a database object reference called its _class_. A message sent to the frob goes to that class,
with the representation inserted as the first argument. This provided behaviour for small pieces of
data without creating a full database object for each one, which also motivates Mica's frobs. See
the [Coldmud release notes][coldmud-changes] and [original frobs chapter][coldmud-frobs].

Coldmud's implementation used reference counts and copy-on-write storage for the
[list][coldmud-lists] or [dictionary][coldmud-dicts] representation. The
[frob wrapper itself was copied][coldmud-data] when the value was duplicated. So, like Mica's frobs:
these were lightweight values whose data could share storage and whose behaviour came from an
existing object.

The word family is much older. The _Jargon File_ records Pete Samson's recollection of storage boxes
under MIT's Tech Model Railroad Club layout in 1958, managed by David R. Sawyer and bearing fanciful
labels such as "Frobnitz Coil Oil". Its entry describes _frob_ as an abbreviation of _frobnitz_ and
credits Zork with popularizing the variant _frobozz_. That places the name in older hacker
vocabulary, although it does not establish Hudson's particular inspiration. See the
[_frobnitz_ entry, including Samson's recollection][frobnitz].

(There is also Jon Blow's [Frobozz Magic Programming Language][fmpl-name], or FMPL of Accardi, at
Berkeley's Experimental Computing Facility. His [Release 1 announcement][fmpl-announcement], dated
June 2, 1992 and preserved in the Object-Oriented Technology FAQ, describes a language with
prototype-based objects, lambda-calculus constructs, and events driven by input/output streams. The
announcement predates Coldmud's frobs by about a year. I don't recall if Hudson borrowed the
reference from FMPL, or if it was an independent production. Jonathan Blow is now a big shot game
developer and Kinda Famous.)

[coldmud-changes]: https://github.com/rdaum/coldmud/blob/main/CHANGES#L223-L269
[coldmud-frobs]: https://github.com/rdaum/coldmud/blob/main/html/Frobs.html
[coldmud-lists]: https://github.com/rdaum/coldmud/blob/main/src/list.c#L267-L289
[coldmud-dicts]: https://github.com/rdaum/coldmud/blob/main/src/dict.c#L256-L275
[coldmud-data]: https://github.com/rdaum/coldmud/blob/main/src/data.c#L158-L197
[frobnitz]: https://www.lysator.liu.se/hackdict/split2/frobnitz.html
[fmpl-name]: https://foldoc.org/Frobozz+Magic+Programming+Language
[fmpl-announcement]: https://www.cs.cmu.edu/Groups/AI/html/faqs/lang/oop/faq-doc-12.html
