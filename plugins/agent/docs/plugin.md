# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: `webserver`, `workflow`
- Provided typed capabilities: `AgentRuntime`, `AgentToolRegistry`
- Required typed capabilities: `WebServer` from `webserver`,
  `WorkflowActionRegistry` and `WorkflowService` from `workflow`
- Plugin-owned tasks: Agent runtime and `session.event` forwarding

System constructs the Agent Plugin from the common `PluginContext`. During
registration it creates and publishes `AgentRuntime`, publishes
`AgentToolRegistry`, and registers Agent session operations as direct Workflow
Actions. The Workflow Plugin remains independent of Agent; Agent is the
capability consumer.

Direct dependent Plugins use `AgentRuntime` for typed session collaboration.
Transport- or environment-specific request shaping stays in those adapter
Plugins rather than in the Agent runtime.

The Plugin owns and drives `RuntimeService` directly. Alongside it, the
Agent-to-Workflow adapter only forwards open-session output as `session.event`;
it does not own or run either the Agent runtime or the Workflow runtime.

Dependent Plugins may still register native Agent `ToolGroup`s through
`AgentToolRegistry` before startup. The Agent startup hook starts that complete
Tool Registry before spawning the runtime task.

The shared `WebServer` capability continues to own `POST /api/model-api`.
The retained route registration is released automatically when the Agent Plugin
unloads.
