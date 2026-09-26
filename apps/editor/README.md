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
It also renders the page shell. Browser transport remains pending.

Load `host-policy.mica` after the editor verbs to enable the shared workspace role.
The policy enrols `#web` for a local unauthenticated host. Authenticated actors require an explicit `HasRole(actor, #editor/user)` fact.
Members can edit all editor buffers, markers, and application state. This role does not isolate users' documents.
Members can invoke installed editor commands and request host effects. They cannot change authority policy or install code.
The filein captures installed editor selectors; added commands require a grant or policy reload.
New tasks and commit continuations rebuild authority from current policy. Tests check tagged edits and role revocation under ordinary actor authority.

The daemon exposes fixed file services when started with one or more `--editor-root DIR` arguments.
Relative paths start at the first root. Absolute paths must resolve within a configured root.
Directory capabilities confine reads and replacements. Completion skips symbolic links and returns at most 100 entries.
Reads and saves accept UTF-8 text up to 8 MiB, including encoded CRLF bytes on save.

File stamps contain a byte count, a nanosecond timestamp string, and a SHA-256 content hash.
The application treats stamps as opaque values. It compares the expected stamp before replacement and requests confirmation after an external change.
Saves preserve file permissions, flush a temporary file, replace the destination, and flush the parent directory.
Saves through one host are serialized. Uncoordinated external writers can still change a file between the final stamp check and replacement.
An error after replacement reports that the file changed but directory durability could not be confirmed.

Run the file-service and application integration checks:

```sh
cargo test -p mica-web-host --lib editor_files
cargo test -p mica-daemon editor_visits_saves
```

The application test uses the real driver and external-request handler to visit, edit, save, detect a disk change, and confirm replacement.

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
