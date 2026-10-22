---
name: create-plugin
description: Create, migrate, or update a Barracuda Plugin with typed capabilities, Workflow Actions and Events, Agent Tools, settings, lifecycle ownership, schemas, and focused tests.
---

# Create Plugin

[Plugin API](../../../docs/architecture/plugin-api.mdx) is the contract; this skill
applies it in order. Read the contract first. Inspect the implementation and
its real callers before changing an existing contract.

## 1. Decide the scope

- Name the feature the Plugin owns. Everything for that feature goes into the
  Plugin or into Plugins that depend on it; nothing goes into core, System,
  composition, Platforms, Boards, `shared/`, or applications.
- List the Plugins whose capabilities it uses. These, and only these, are its
  `depends-on`. Do not depend on a Plugin to reach its internals.
- If the feature needs something another Plugin owns, use or extend that
  Plugin's capability. If it needs a raw Platform or Board resource, take it
  from `PluginContext`. If it needs a Board peripheral, declare a slot. If it
  needs a value that differs by Board, declare a setting.

## 2. Lay out the directory

Create `plugins/<id>/plugin.toml` with `id`, `depends-on`, and a one-sentence
`description`, and `crates/plugin` with package `barracuda-<id>-plugin`. Add
only the layout entries the Plugin needs: `schemas/`, `resources/`,
`filesystem/`, `docs/`, further crates. Enable it for the Boards that ship it,
in a profile under `boards/profiles/` or a Board's `plugins.<id>.enabled`.
Run `cargo plugin sync`.

## 3. Construct

Write `pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) ->
Self` on the type annotated with `#[barracuda_plugin::macros::plugin]`. Take
or clone the context resources it needs and build its own values from them;
register nothing and spawn nothing.

For a setting, add `[config.<name>]` to `plugin.toml`, a `build.rs` calling
`barracuda_plugin_build::build()`, and `mod config { barracuda_plugin::config!(); }`;
set non-default values in Boards' `plugins.<id>` sections.

For a peripheral, declare its slot under `[peripherals.<kind>]` in
`plugin.toml` (`"primary"`, `"any"`, `"*"`, or `{ fill, optional = true }`),
add the same `build.rs` and `config` module, and take the device with
`config::peripherals::<KIND>_<SLOT>` and
`context.hal.peripherals.take_<peripheral>(instance)`. Never name a Board
device in the Plugin; a Board that needs another device wires the slot in
`plugins.<id>.peripherals`. A device that failed to initialize is `None` at
run time: an optional slot treats it as absent, and a required slot returns
`PluginError::PeripheralUnavailable` from `register`, which leaves the Plugin
out. A Lua package over Board devices takes a `"*"` slot, lists the devices,
and defaults to the first, the primary one.

## 4. Register and start

In `register`, require capabilities from `depends-on` Plugins, build the
service graph, `provide` each capability, install Workflow Actions, Agent
Tools, Lua packages, portal entries, and routes, and `retain` every guard. In
`start`, spawn each long-running task with its own task token, cancellation
safe on every await path, following
[Execution ownership](../../../docs/architecture/execution-ownership.mdx).

## 5. Add the contracts its callers need

- Another Plugin: a typed capability, the provider's whole public surface.
- A Workflow: an Action with `schemas/action/<address>/{request,response}.json`,
  or an Event with `schemas/event/<event-id>.json`. A Workflow joining two
  Plugins ships in the Plugin that depends on both.
- The model: an Agent Tool in an `agent-<name>` adapter Plugin, with
  `crates/<crate>/resources/tools/<tool>/{schema.json,usage.md}`.
- Lua: a package in a `vm-<name>` Plugin.
- A person: a portal entry, its page in `resources/web/` built to
  `filesystem/resources/`, and private routes on `WebServer`.

## 6. Test

Test through the production API with the real Plugins of the dependency graph
and `barracuda-platform-test` for the Platform; add nothing to the API for a
test. Cover Action addresses and schemas, request and response validation,
business failures, capability publication and consumption, retained
registrations, and task cancellation as they apply. Run formatting, the
Plugin's focused tests, clippy, `cargo plugin sync --check`, and
`git diff --check`; for web pages, the root `bun run` checks and tests.

A runnable example goes in the crate's `examples/*.rs`: one usage per file,
named in its doc comment, checked with `assert!` against observable state,
under the crate's lints, and run from the Plugin page with
`cargo run -p <package> --example <name> --target <host-triple>`.

## 7. Document

Write the Plugin page `docs/plugins/<id>.mdx` in the
[template form](../../../docs/architecture/plugin-api.mdx#plugin-documentation),
plus `docs/plugins/<id>/action.mdx` and `event.mdx` when schemas exist. A
Plugin with a large surface may add topic pages beside them, such as
`usage.mdx` for callers and `design.mdx` for maintainers;
`docs/plugins/imessage-gateway/` is the reference.

- Describe the current contract. History belongs in version control, and a
  page has no status or "not yet implemented" section.
- State the design positively. Use negation only for a real invariant or
  boundary, with its reason, and delete any note whose only job is to explain
  why something is absent. Give a decision and its cost only for the one or
  two choices that matter.
- Describe behavior in concepts. Code blocks are minimal and match the real
  public API; update them in the change that changes the API.
- Follow [Writing docs](../../../docs/contributing/docs.mdx) for the
  frontmatter, the first screen, and the words for Plugin developers.

In the same change, add the Chinese copy of every page under
`docs/zh/plugins/`, add each page to the Plugin's category in both
navigations of `docs/docs.json`, and run `npx mint validate` and
`npx mint broken-links` from `docs/`.

## Migrating an old contract

1. Identify the business operation and every real caller.
2. Move each piece to its owner by the scope rules: feature code into the
   Plugin, cross-Plugin calls onto capabilities, per-Board values into
   settings.
3. Remove the superseded types, adapters, names, schemas, docs, and tests.
4. Update callers, tests, and examples to the production contract.
5. Search the whole repository for the removed vocabulary and paths.
