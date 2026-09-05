# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: `webserver`
- Provided typed capabilities: none
- Required typed capabilities: `WebServer` from `webserver`
- Owned Components: `AgentComponent`
- Plugin-owned tasks: none

System constructs the Agent Plugin from the common `PluginContext`. The Plugin
clones its HTTP client factory from the context, receives its private filesystem
during registration, starts its services, and loads the standalone
`barracuda-agent-component` into Event Router.

It requires the shared `WebServer` capability and registers
`POST /api/model-api` during registration. The endpoint accepts model API
configurations for root Agents, subagents, memory extraction, and context
compaction. The retained route registration is released automatically when the
Plugin unloads.

The Plugin publishes no typed capability. System-owned callers, including
Workflow, use the Component's `"system"` JSON RPCs and consume the
`session.event` JSON Event. The Component directly owns the runtime service and
the open-session event streams, so Event Router polling advances both without a
separate task or a native RPC compatibility layer.
