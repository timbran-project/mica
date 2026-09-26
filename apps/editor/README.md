# Programmable editor

These fileins implement editor buffers, windows, keymaps, commands, markers, undo/redo, minibuffers, and bounded browser snapshots.
The application owns editor policy. Runtime buffers own transactional text and committed revisions.

Run the application scenarios:

```sh
cargo test -p mica-runtime --test editor
```

The harness loads the shared host and buffer libraries, then the editor files in dependency order.
It runs 45 scenarios from omica and one additional keymap regression in both interpreter-only and native-enabled modes.
Each scenario commits separately. The harness resumes explicit commit boundaries to check tagged acknowledgements after publication.
It also renders the page shell. Browser transport and host file-service integration remain pending.

The source comes from omica revision `bfb368c0b7586ab98c3915c0ae7cd3b46843fc8f`.
The Rust port makes these adaptations:

- Window traversal keeps a queue index instead of calling the internal `__list_slice` builtin.
- Viewport rows and minor modes use keyed comprehensions instead of selection loops.
- Mutable working values use local bindings because Rust Mica parameters are immutable.
- Session cleanup retracts all three `SearchMatch` columns.
- Keymap refresh calls the one-argument `selected_window` verb.

The extra scenario checks that a stale browser keymap generation receives the current plan.
The cleanup scenario also checks removal of a populated search match.
The runtime's empty-buffer revision and catchable JSON errors have separate regression tests.
