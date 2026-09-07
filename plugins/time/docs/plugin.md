# Time Plugin

- Plugin ID: `time`
- Depends on: none
- Provided capability: `UtcClock`
- Background tasks: one cancellable SNTP synchronization loop

The Time Plugin owns network synchronization and publishes only the typed,
read-only `UtcClock` capability. It does not know about Agent Tools, Workflow
Actions, or JSON transport. `agent-time` is the separate Agent adapter.
