# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: none

The Agent Plugin constructs the Agent runtime from platform-provided filesystem,
storage, networking, and model API abstractions, starts its services, and loads
the standalone `barracuda-agent-component` into Event Router.

It owns the Agent Component and does not provide or require a typed Plugin
capability.
