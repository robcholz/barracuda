# Plugin Communication

Barracuda uses typed capabilities for direct Plugin collaboration, Workflow
Actions and Events for Workflow integration, and Agent Tools for model-facing
operations. Choose the contract that matches the caller and keep execution
ownership with the implementing subsystem.

## Typed capabilities

A provider publishes a typed capability during `Plugin::register` with
`PluginRegisterContext::provide`. A consumer declares the provider in
`plugin.toml` and obtains the same value with
`PluginRegisterContext::require`.

Typed capabilities are the default for direct Plugin-to-Plugin collaboration.
They expose domain operations and registrations without JSON adaptation or
runtime lookup outside the declared Plugin dependency graph.

## Workflow contracts

An operation callable from the Workflow DSL implements
`WorkflowActionHandler` and registers with `WorkflowActionRegistry`. Its stable
address and request/response schemas live with the owning Plugin under
`schemas/action/<address>/`.

An asynchronous fact implements `Event` and is emitted through
`WorkflowService`. Event payloads are ordinary bounded Serde values. Producers
include correlation and ordering fields when a logical operation spans several
Events.

A Plugin may expose one implementation through both a typed capability and
Workflow Actions. The capability remains the direct machine-oriented API; the
Action contract remains stable, serializable, and suitable for the Workflow
DSL.

## Agent contracts

Model-facing operations are native Agent Tools registered through
`AgentToolRegistry`. Adapter Plugins translate Tool calls into a provider's
typed capability or `WorkflowService` operation and retain the Tool
registration for their lifecycle. This keeps the provider independent of Agent
prompting and Tool response conventions.

## Plugin lifecycle

`Plugin::register` synchronously constructs the complete capability graph,
publishes provided capabilities, requires declared dependencies, registers
Workflow Actions and Agent Tools, and retains every registration guard.

`Plugin::start` is an optional synchronous post-registration hook. Its
`PluginStartContext` cannot publish capabilities. Plugin-owned long-running
work starts through the System-installed Embassy spawner only after every
Plugin has registered. See [`execution-ownership.md`](execution-ownership.md)
for task ownership and cancellation.

## Plugin documentation

Every `plugins/<plugin>/docs/plugin.md` states:

- the exact Plugin ID and direct Plugin dependencies;
- provided and required typed capabilities using exact public Rust type names;
- registered Workflow Actions, emitted Events, and Agent Tools;
- owned long-running tasks and their capacity;
- the Plugin's storage and retained-registration responsibilities.

Write `none` for an empty capability category when the summary uses that
category.
