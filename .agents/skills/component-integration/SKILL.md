---
name: component-integration
description: Add a new Event Router Component to this repo, covering crate layout under components/, RPC and event contract design, rpc_dynamic support, and high-frequency mapping components.
---

# Event Router Component Integration

Add integrations to Event Router as Components, one folder per Component under
`components/`. This skill covers where crates go, how to design and document RPC
and event contracts, and when to add schema-baked wire and mapping crates. Follow
[.agents/docs/codestyle.md](.agents/docs/codestyle.md) for getset,
`From`/`TryFrom`, and lint conventions while implementing.

## Layout

Create `components/<name>/`.

- Single crate: the Component is one crate at `components/<name>/`; its
  filesystem name is `<name>`.
- Multiple crates: create `components/<name>/crates/` and put each sub-crate
  there. The public interface crate has filesystem name `component` at
  `components/<name>/crates/component`; the other crates are that Component's
  infrastructure.
- The Component crate's package name is always `barracuda-<name>-component`,
  regardless of layout.
- Register the new crates in the root `Cargo.toml` `members`: a glob such as
  `components/<name>/crates/*`, or a single path.

In-repo references: single-crate `components/gateway-agent-adapter`;
multi-crate `components/agent` and `components/message-gateway`.

## Contract design

Design RPCs and events like a REST API: expose the minimal surface, stay
independent of callers, and do not assume which callers or workflows consume
them. Match the shape and naming of existing RPCs and events so new contracts
do not drift stylistically.

Follow the existing component code layout: one RPC module per address that also
exports a reusable `*_handler` constructor, and `component.rs` for registration
and lifecycle only.

## Type style

- Prefer named `enum` and `struct` types over raw byte arrays.
- Text serializes as a string, never as a byte or numeric array. Represent text
  with a named C-style string newtype: fixed capacity, UTF-8 contents,
  `\0`-terminated, and its `Serialize`/schema output is a JSON string.
- A bare `u8` array is allowed only when the value is genuinely opaque bytes,
  not text.

## Documentation

In the same change as the contract, create:

- `components/<name>/docs/rpc.md` — a REST-style API reference. For each RPC,
  list the address, request, response, and method error with every error
  variant.
- `components/<name>/docs/event.md` — if the Component emits events, list each
  event, its message type and cardinality, and exactly when it is emitted.

## rpc_dynamic and schema

Add runtime-dynamic (JSON/wire) support only when the user explicitly asks for
it. `#[rpc_dynamic]` works with fixed-layout request and response types defined
in the Component crate itself; it does not need a separate crate.

A separate wire crate is required only when the Component also bakes request
schemas, because the bake `build.rs` is a separate compilation unit that must
link against the wire DTOs. In that case:

- Add `components/<name>/crates/wire` with package name `barracuda-<name>-wire`.
- A single-crate Component switches to the `crates/` layout: `crates/component`
  plus `crates/wire`.

## Mapping components

When a workflow is a high-frequency hot path and a JSON call would cost too
much, add a dedicated conversion Component at `components/<from>-<to>-mapping/`
(for example `gateway-agent-mapping`), with package name
`barracuda-<from>-<to>-mapping`. It performs the typed, high-performance
conversion between the two named Components.
