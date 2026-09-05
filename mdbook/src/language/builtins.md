# Built-in Functions

Mica installs the following core functions in a normal runtime. Calls are positional unless a
signature says otherwise. Operations that require authority check the task's authority context.
Argument and permission failures reach
the host as runtime errors unless the operation explicitly raises a language error. Builtins that
return an `option` or `result` represent absence or an expected failure as ordinary values.

## Scalar and Collection Functions

| Function                                   | Result                                               |
| ------------------------------------------ | ---------------------------------------------------- |
| `string_len(text)`                         | number of Unicode scalar values                      |
| `string_chars(text)`                       | list of one-character strings                        |
| `string_slice(text, start, end)`           | end-exclusive character slice                        |
| `string_from_chars(chars)`                 | string assembled from character strings              |
| `string_concat(@parts)`                    | concatenated strings; accepts zero or more arguments |
| `string_join(parts, separator)`            | joined list of strings                               |
| `string_starts_with(text, prefix)`         | boolean prefix test                                  |
| `string_contains(text, subject)`           | boolean substring test                               |
| `string_equal_fold(left, right)`           | equality after Unicode lowercasing                    |
| `lower(text)`                              | lowercase string                                     |
| `words(text)`                              | parsed word list                                     |
| `edit_distance(left, right)`               | character edit distance                              |
| `parse_ordinal(text)`                      | `result<int>`                                        |
| `url_encode_component(text)`               | percent-encoded URL component                        |
| `url_decode_component(text)`               | decoded URL component                                |
| `sort(list)`                               | canonically sorted list                              |
| `to_symbol(text)`                          | named symbol                                         |
| `to_literal(value)`                        | parseable Mica value text                            |
| `from_literal(text)`                       | `result<dynamic>`                                    |
| `map_pairs(map)`                           | list of two-item key/value lists                     |
| `index_or(collection, index, default)`     | list, map, or relation lookup with a default         |
| `json_encode(value)` / `json_decode(text)` | JSON conversion                                      |
| `os_getenv(name)`                          | `option<string>`                                     |

`os_getenv` requires root authority or an invoke grant for `:os_getenv`, such as
`CanInvoke(#reader, :os_getenv)` or a matching `RoleCanInvoke` grant. This grants access to the host
process environment, including values that may contain credentials; give it only to trusted code.
Environment values are configuration for that process, not durable world state.

### Text Positions and Construction

String positions count Unicode scalar values. An accented character such as `é` occupies one
position regardless of its UTF-8 byte length. A letter followed by a combining accent occupies two
positions. Use these operations for character-based text processing; a host that lays out text may
group several scalars into one displayed character.

`string_slice` uses an exclusive end position and accepts an empty interval. A list range such as
`items[1..3]` includes position 3. Write the bounds for the operation being called:

```mica,eval
require string_len("AéB") == 3
require string_slice("AéB", 1, 2) == "é"
require string_slice("AéB", 3, 3) == ""
require string_chars("AéB") == ["A", "é", "B"]
require string_from_chars(["A", "é", "B"]) == "AéB"
```

Bounds must satisfy `0 <= start <= end <= string_len(text)`. A slice outside the string raises
`E_INDEX`. `string_from_chars` accepts strings containing exactly one scalar each.

Use `string_concat` for a fixed set of pieces and `string_join` for a list separated by punctuation:

```mica,eval
let labels = ["inspect", "repair", "calibrate"]
require string_join(labels, ", ") == "inspect, repair, calibrate"
require string_concat("Task: ", "inspect") == "Task: inspect"
require string_concat() == ""
```

Both operations require string pieces. Use `to_literal` explicitly when the text should contain a
Mica representation of another value.

### Parsing Small Inputs

`words` splits on whitespace, groups text inside double quotes, and lets a backslash quote the next
character. It is useful for a command protocol whose arguments are words and quoted phrases:

```mica,eval
require words("take \"red key\" now") == ["take", "red key", "now"]
```

`parse_ordinal` accepts positive numeric ordinals such as `"2"` and `"21st"`, and English forms
such as `"first"` and `"twenty-first"`. Inspect its result before using the number:

```mica,eval
let selected = match parse_ordinal("twenty-first")
case ok(number)
  number
case err(problem)
  raise problem
end
require selected == 21
```

URL component encoding percent-encodes UTF-8 bytes outside the unreserved alphabet. Decoding
accepts percent escapes and interprets `+` as a space. Encode individual component values before
assembling a URL; separators such as `/`, `?`, and `&` have meaning in the assembled URL.

`sort` returns a sorted list and retains duplicates. It uses canonical value order, the same order
used to organize map keys and relation rows. Use values of a consistent kind when the order should
represent a numeric ranking or an alphabetical list.

## Relation Algebra

| Function                          | Result                                        |
| --------------------------------- | --------------------------------------------- |
| `project(relation, :column, ...)` | selected heading columns                      |
| `union(left, right)`              | rows present in either equal-heading relation |
| `difference(left, right)`         | left rows absent from the right relation      |
| `natural_join(left, right)`       | natural join over shared heading names        |

See [Relations](./relations.md#relation-value-algebra) for heading and duplicate semantics.

## World Definition and Introspection

| Function                                                     | Result                                     |
| ------------------------------------------------------------ | ------------------------------------------ |
| `make_identity(:name)`                                       | named identity                             |
| `make_relation(:Name, arity[, durability])`                  | relation identity                          |
| `make_functional_relation(:Name, arity, keys[, durability])` | functional relation identity               |
| `destroy_identity(#identity)`                                | number of retracted facts                  |
| `rules(:Relation)`                                           | active rule identities for a head relation |
| `describe_rule(#rule)`                                       | installed rule source                      |
| `disable_rule(#rule)`                                        | `()`                                       |
| `fileout(:unit)`                                             | source owned by a filein unit              |
| `fileout_rules([:Relation])`                                 | active rule source                         |
| `tasks()`                                                    | current task snapshots                     |

The optional durability symbol is `:durable` or `:volatile`. Definition, destruction, and
rule-disabling operations require administrative authority.

## Runtime Context, Effects, and Coordination

| Function                                | Result                            |
| --------------------------------------- | --------------------------------- |
| `actor()`                               | `option<identity>`                |
| `principal()`                           | `option<identity>`                |
| `endpoint()`                            | current endpoint identity         |
| `assume_actor(#actor)`                  | newly bound actor identity        |
| `emit(target, value)`                   | emitted value                     |
| `log(message)` / `log(:level, message)` | `()`                              |
| `mailbox()`                             | `[receiver, sender]` capabilities |
| `mailbox_send(sender, value)`           | sent value                        |
| `mailbox_close(receiver)`               | `()`                              |
| `subscribe_changes(...)`                | subscription capability           |
| `cancel_subscription(subscription)`     | `()`                              |

Log levels are `:trace`, `:debug`, `:info`, `:warn`, and `:error`. Effects and mailbox sends are
published at commit. Subscriptions are specified in [Subscriptions](../runtime/subscriptions.md).

## Frobs, DOM, XML, and Embeddings

| Function                                     | Result                      |
| -------------------------------------------- | --------------------------- |
| `frob(delegate, value)`                      | frob value                  |
| `frob_delegate(value)` / `frob_value(value)` | frob components             |
| `is_frob(value)`                             | boolean                     |
| `dom_text(text)` / `dom_raw(text)`           | DOM node map                |
| `dom_element(tag, attrs, children)`          | DOM element map             |
| `dom_html(node)`                             | rendered HTML               |
| `to_xml(node)` / `from_xml(text)`            | XML conversion              |
| `dom_diff(before, after)`                    | DOM patch list              |
| `dom_snapshot_payload(view, revision, root)` | serialized snapshot payload |
| `sync_signature(revision, payload)`          | synchronization signature   |
| `embed_text(model, text)`                    | embedding vector            |

The DOM helpers underlie [DOM Markup](./dom-markup.md). Embedding availability depends on the
configured provider; see [Retrieval and Embeddings](../runtime/retrieval-and-embeddings.md).

## Compiler-Recognized Runtime Forms

These calls resemble built-ins but compile directly to task operations:

| Form                                            | Meaning                                            |
| ----------------------------------------------- | -------------------------------------------------- |
| `commit()`                                      | publish and resume with a fresh transaction        |
| `suspend([seconds])`                            | publish and suspend indefinitely or for a duration |
| `read([metadata])`                              | publish and wait for endpoint input                |
| `mailbox_recv(receivers[, timeout])`            | publish and wait for mailboxes                     |
| `external_request(service, payload[, timeout])` | request a host service                             |
| `invoke(selector, roles)`                       | dynamic named-role dispatch                        |

`spawn` is a language form rather than a function. See [Task Control](../runtime/task-control.md).
Hosts may register additional request functions. Those are deployment APIs, not part of this core
catalogue.
