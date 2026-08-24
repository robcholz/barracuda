# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: none
- Provided typed capabilities: `AgentSetApi`

The Agent Plugin constructs the Agent runtime from platform-provided filesystem,
storage, networking, and model API abstractions, starts its services, and loads
the standalone `barracuda-agent-component` into Event Router.

It owns the Agent Component and provides the typed `AgentSetApi` capability so
dependent Plugins can configure model APIs with the normal `ModelApiConfig` and
`ApiPurpose` Rust types. It does not require another Plugin capability.
