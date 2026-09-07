# Scheduler Plugin

- Plugin ID: `scheduler`
- Depends on: `time`, `workflow`
- Provided capability: `Scheduler`
- Storage: Plugin-scoped KV records keyed by schedule ID
- Background tasks: one cancellable due-occurrence loop

The Scheduler Plugin owns schedule state, persistence, timing, and
`scheduler.triggered` Workflow Event emission. It publishes typed `schedule`
and `cancel` methods through the `Scheduler` capability and contains no Agent
Tool registration. `agent-scheduler` is the separate Agent adapter.
