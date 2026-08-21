---
name: write-example
description: Write or edit runnable examples for any crate in this repo. Use when creating, fixing, or reviewing an example.
---

# Runnable Examples

Examples in this repo teach one public usage at a time and verify it with
assertions against observable state. They are runnable documentation, not
throwaway snippets.

## Where an example lives

- Single-file example: `examples/*.rs` inside the crate it demonstrates, which
  Cargo discovers automatically. Depend only on that crate's public API.
- Standalone example crate: a workspace member for an example that needs its own
  `Cargo.toml`, `build.rs`, or feature wiring. `core/event-router/example-crates/`
  is the existing model: add the directory to the workspace `members` glob, set
  `publish = false`, and use relative `path` deps with `workspace = true` for
  shared deps.

## What makes a good example

- One example, one usage. State what it demonstrates in the file's doc comment.
- Verify the result with `assert!` / `assert_eq!`; the example is its own test.
- Drive async work explicitly. A long-lived service future stays pending; a
  future that returns signals completion.
- Examples inherit the crate's lints. This repo commonly denies `unwrap`,
  `expect`, `panic`, slice indexing, and `+`/`-` arithmetic in `[lints.clippy]`,
  so use `?`, `saturating_*`, and destructuring.
- Follow [.agents/docs/codestyle.md](.agents/docs/codestyle.md) for the Rust API
  surface (getset, From/TryFrom).

## Observable state

- To read a result back from the example's driver, share `Rc<Cell<T>>` for
  `Copy` values or `Rc<RefCell<T>>` otherwise, and capture owned `Rc` clones by
  value in `'static` closures and futures.
- To skip readback, keep plain fields and mutate them through `&mut self`; the
  driver stops when the future completes.

## Keep in sync

When adding or removing an example, update the crate's `examples/README.md`
table and any usage guide's Examples list in the same change.

In-repo voice: `core/event-router/examples/` and
`core/event-router/example-crates/`.
