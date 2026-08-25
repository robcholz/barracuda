# Captive Portal Plugin

- Plugin ID: `captive-portal`
- Direct Plugin dependencies: `agent`, `webserver`
- Provided typed capabilities: none

The Captive Portal Plugin requires the Agent Plugin's typed `AgentSetApi`
capability and the shared `WebServer` capability. It mounts the Agent model API
configuration contract as an ordinary HTTP endpoint and retains the scoped
route registration for its Plugin lifetime.

The Plugin owns no Event Router Component and provides no typed capability.
Its external HTTP contract is documented in [http.md](http.md).
