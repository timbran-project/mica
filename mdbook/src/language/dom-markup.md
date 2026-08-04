# DOM Markup

Mica can build DOM trees with a JSX-like document expression:

```mica
return dom <button type="submit" class={class}>
  Save
</button>
```

The `dom <...>` form is syntax sugar for the existing DOM constructors. The example above lowers as
if it had been written:

```mica
return dom_element("button", {:type -> "submit", :class -> class}, [
  dom_text("Save")
])
```

The prefix is part of the syntax. A DOM markup expression starts with `dom` followed by an element.
Bare `<button>...</button>` is not an expression, and `dom` is not reserved outside this form:

```mica
let dom = 1
return dom < 2
```

## When To Use It

Use DOM markup when the shape of the browser view is the important part of the code. It is meant for
server-owned UI composition: sync view trees, forms, panels, lists, document shells, and other
places where the Mica code is describing nested DOM structure.

This is useful when the UI is part of the shared system rather than a separate client application.
The view can stay close to the facts, rules, authority checks, and verbs that decide what a user is
allowed to see or do, while the source still reads like the DOM tree it produces. That makes it a
good fit for small tools, inspectors, operational panels, live views, and other Mica-hosted
interfaces where the server owns the browser state.

The markup form makes those trees readable at a glance. It avoids long `dom_element(...)` calls
where the tag, attributes, and children are separated by punctuation instead of layout:

```mica
return dom <section class="source-history-subpanel">
  <h3>Changed files</h3>
  {source/changed_files_node(repository, parent, commit, selected_path)}
</section>
```

Prefer ordinary expressions when the code is mostly computation, and use `dom <...>` at the boundary
where that computed state becomes a DOM node. Helper verbs can still return DOM values, and markup
can call those helpers with `{helper(...)}` or splice lists with `{@children}`.

Keep using ordinary Mica values and helper verbs for data preparation, branching, filtering, and
highly dynamic DOM construction. The markup syntax is for making the final tree obvious, not for
replacing the rest of the language.

## Elements

DOM markup supports nested elements and self-closing elements:

```mica
dom <section class="panel">
  <h2>History</h2>
  <input type="hidden" name="commit" value={commit} />
</section>
```

Tag names use ordinary identifier spelling. The runtime DOM renderer and sync host still validate
whether a tag is supported.

Whitespace-only text between elements is ignored. Non-empty text becomes a `dom_text(...)` node.

## Attributes

Attribute values can be quoted strings or Mica expressions in braces:

```mica
dom <form
  class="source-history-commit-form"
  data-sync-action="source_select_history_commit"
  data-sync-key={string_concat("history-commit:", commit)}
  data-sync-reset="false">
  ...
</form>
```

Attribute names may include `-` and `:` segments, such as `data-sync-key` and `aria:selected`.
Attributes without an explicit value lower to boolean `true`:

```mica
dom <button disabled>Save</button>
```

## Children

Use `{expr}` to insert a dynamic child:

```mica
dom <strong>{summary}</strong>
```

If the expression produces a string, the DOM renderer and sync host treat it as a text node. If it
produces a DOM element value, that element is inserted.

Use `{@expr}` to splice a list of children:

```mica
let items = [dom <li>One</li>, dom <li>Two</li>]
return dom <ul>{@items}</ul>
```

This is the DOM equivalent of list splicing. The expression after `@` must produce a list at
runtime.

## Control Flow

DOM markup is an expression, not a template sublanguage. Use ordinary Mica code to compute values
and child lists, then insert them:

```mica
let rows = []
for row in source/ChangedFiles(repository, from_commit, to_commit, ?path, ?kind)
  rows = [@rows, dom <li data-sync-key={row[:path]}>{row[:path]}</li>]
end

return dom <ul class="source-changed-file-list">{@rows}</ul>
```

This keeps loops, conditionals, authority checks, and queries in the normal language instead of
adding a second set of template rules.

## Authority

Because `dom <...>` lowers to `dom_element` and `dom_text`, it uses the same authority surface as
calling those functions directly. Code that returns DOM from a web view still needs permission to
invoke the DOM constructors and any helpers used inside `{...}` expressions.

DOM markup defines a tree; it does not decide when the browser should receive a new version. A
synchronized view declares the relations that can change its tree, as described in
[Subscriptions](../runtime/subscriptions.md#synchronized-dom-views).
