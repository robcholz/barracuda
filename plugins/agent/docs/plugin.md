# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: `webserver`, `workflow`
- Provided typed capabilities: `AgentToolRegistry`
- Required typed capabilities: `WebServer` from `webserver`,
  `WorkflowActionRegistry` and `WorkflowService` from `workflow`
- Owned Components: none
- Plugin-owned tasks: Agent runtime and `session.event` forwarding

System constructs the Agent Plugin from the common `PluginContext`. During
registration it creates the Agent runtime, publishes `AgentToolRegistry`, and
registers the former Agent session RPC surface as direct Workflow Actions. The
Workflow Plugin remains independent of Agent; Agent is the capability consumer.

The Plugin owns and drives `RuntimeService` directly. Alongside it, the
Agent-to-Workflow adapter only forwards open-session output as `session.event`;
it does not own or run either the Agent runtime or the Workflow runtime. No
Event Router component, RPC lane, frame-size constant, or RPC-to-Action
conversion remains in this path.

Dependent Plugins may still register native Agent `ToolGroup`s through
`AgentToolRegistry` before startup. The Agent startup hook starts that complete
Tool Registry before spawning the runtime task.

The shared `WebServer` capability continues to own `POST /api/model-api`.
The retained route registration is released automatically when the Agent Plugin
unloads.
