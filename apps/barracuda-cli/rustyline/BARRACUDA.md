# rustyline, patched for Barracuda

This is [rustyline](https://github.com/kkawakam/rustyline) 18.0.1 as published
on crates.io, used through `[patch.crates-io]` in the workspace manifest. The
`[[example]]` entries are dropped because the examples are not vendored, and
`[lints.rust]` allows `unconditional_panic`: a path dependency is linted
without the cap that registry crates get, and `src/keymap.rs` indexes a key
sequence behind a length check that rustc does not see. Everything else is the
published crate plus the change below. It is a
candidate to send upstream.

| Change | Files | Why |
| --- | --- | --- |
| `PosixRawReader::select` returns a buffered key before waiting on the terminal | `src/tty/unix.rs` | With an external printer, `select` waited for the terminal to become readable even when earlier input was already buffered. A paste without bracketed-paste markers or an input-method commit such as `你好` arrives in one read, so everything after its first character stayed hidden until the next keystroke. The Barracuda CLI always uses an external printer to stream replies above the prompt. |
