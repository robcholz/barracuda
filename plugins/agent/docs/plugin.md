# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: none
- Provided typed capabilities: `AgentSetApi`

System constructs the Agent Plugin from the common `PluginContext`. The Plugin
clones its HTTP client factory from the context, receives its private filesystem
during registration, starts its services, and loads the standalone
`barracuda-agent-component` into Event Router.

It owns the Agent Component and provides the typed `AgentSetApi` capability so
dependent Plugins can configure model APIs with the normal `ModelApiConfig` and
`ApiPurpose` Rust types. It does not require another Plugin capability.
