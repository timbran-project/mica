# Structural Options and Results

[`apps/examples/structural-values.mica`](../../../apps/examples/structural-values.mica) is a small
filein focused on value boundaries rather than durable domain state. Load it with:

```sh
cargo run --bin mica -- filein apps/examples/structural-values.mica
```

The `example/describe_optional` verb distinguishes an absent outer option, a present outer option
whose inner option is absent, and a present string:

```mica
:example/describe_optional(value: none)
:example/describe_optional(value: some(none))
:example/describe_optional(value: some(some("ready")))
```

These return `"not supplied"`, `"supplied without text"`, and `"ready"`. The parameter annotation
is necessary because it gives `match` the closed nested option shape; the branch locals need no
annotations.

`example/parse_ordinal` matches the `result<int>` returned by `parse_ordinal`. Its error branch gives
the dynamically accessed error field its `option<string>` boundary before matching it. A returned
`err` is a recoverable value, while a raised error still follows `try`/`catch` control flow.
