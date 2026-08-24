# Plugin Communication

Event Router is designed primarily for Workflow and Agent call contracts.
Other Plugin-to-Plugin integrations generally use typed capabilities. Treat
this as design guidance rather than a prohibition: choose the mechanism that
best matches the Plugin's ownership, lifecycle, and contract requirements.

## Plugin documentation

Every `plugins/<plugin>/docs/plugin.md` must state the typed capabilities that
the Plugin provides. Use the exact public Rust type names. When the Plugin
provides no typed capability, write `none` explicitly.

Use this field in the Plugin summary:

```text
- Provided typed capabilities: `CapabilityType`
```

or:

```text
- Provided typed capabilities: none
```
