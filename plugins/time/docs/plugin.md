# Time Plugin

The Time Plugin synchronizes UTC time over SNTP and publishes it as a typed,
read-only clock capability.

- Plugin ID: `time`
- Dependencies: none
- Settings: none
- Context resources: `ip_stack`
- Provided capabilities: `UtcClock`
- Required capabilities: none
- Workflow: none
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: one cancellable SNTP synchronization loop (`time_sync_task`, pool
  size 1)
- Storage: none

The Time Plugin owns network synchronization and publishes only the typed,
read-only `UtcClock` capability. It does not know about Agent Tools, Workflow
Actions, or JSON transport. `agent-time` is the separate Agent adapter.
