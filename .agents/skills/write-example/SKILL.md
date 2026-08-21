---
name: write-example
description: Write or edit runnable examples in this repo (Event Router and lower-level rpc/router/workflow crates). Use when creating, fixing, or reviewing an example.
---

# Runnable Examples

Examples in this repo teach one public usage at a time and verify it with
assertions against observable state. They are runnable documentation, not
throwaway snippets.

## Where an example lives

- Single-file example: `examples/*.rs` inside a crate, which Cargo discovers
  automatically. Depend only on the crate's public surface. Event Router
  examples depend on `barracuda-event-router` and stay out of internal crates.
- Standalone example crate: `core/event-router/example-crates/*`. Use one when
  the example needs its own `Cargo.toml`, `build.rs`, or feature wiring; the
  schema bake demo is the model. Add the directory to the workspace `members`
  glob, set `publish = false`, and use relative `path` deps with
  `workspace = true` for shared deps.

## What makes a good example

- One example, one usage. State what it demonstrates in the file's doc comment.
- Verify the result with `assert!` / `assert_eq!`; the example is its own test.
- Drive the router explicitly: `poll_fn` with `Pin::new(&mut router).poll`, or
  `block_on`. A `run` future that returns `Ok(())` terminates the router, so
  long-lived components stay on `pending()`.
- Facade examples inherit the crate's deny-level clippy lints. Write code that
  avoids `unwrap`, `expect`, `panic`, slice indexing, and `+`/`-` arithmetic;
  use `?`, `saturating_*`, and destructuring instead.

## State: observe or not

- To observe a component result from `main`, share `Rc<Cell<T>>` for `Copy`
  values or `Rc<RefCell<T>>` otherwise. `register_rpc` handlers are `'static`
  and `run` futures are boxed, so capture owned `Rc` clones by value.
- To skip observation, keep plain fields on the component and mutate them
  through `&mut self` in `run`; `main` drives until the router terminates.
- Components exchange data through RPC (`RunContext::rpc()`), not shared state.

## Messages

For a fixed-layout message, prefer `#[rpc_message]` (adds serde + zerocopy +
`RpcWire`) together with `#[repr(C)]` and any `Clone`/`Copy`/`Debug` needed.
Enums receive the same set without `RpcWire`.

## Keep in sync

When adding or removing an example, update the crate's `examples/README.md`
table and the usage guide's Examples list in the same change.

In-repo voice: `core/event-router/examples/*.rs` and
`core/event-router/example-crates/*`.
