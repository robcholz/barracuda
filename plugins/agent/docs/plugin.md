# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: none
- Provided typed capabilities: `AgentSetApi`

System constructs the Agent Plugin from the common `PluginContext`. The Plugin
clones its HTTP client factory from the context, receives its private filesystem
during registration, composes the Agent runtime's fixed tool providers, starts
its services, and loads the standalone `barracuda-agent-component` into Event
Router.

It owns the Agent Component and provides the typed `AgentSetApi` capability so
dependent Plugins can configure model APIs with the normal `ModelApiConfig` and
`ApiPurpose` Rust types. The Plugin also owns the dedicated adapter that
projects the fixed Event Router RPC graph into Agent tools.

The adapter is consumed once when the Agent service initializes, after every
Plugin and Component has registered and before normal Event Router execution.
It does not monitor a runtime catalog or add a mutable tool-registration
lifecycle. It preserves each Event Router group as a loadable Agent discovery
group. Its first surface is deliberately unary request / unary response: a
method is projected when it is runtime-dynamic, unary in both directions, and
carries a baked request schema. Tool names encode the complete RPC address
injectively. Arguments are checked against the compiled baked schema and the
registered dynamic JSON codec before the ordinary JSON RPC call runs.
