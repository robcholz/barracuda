# Scheduler Plugin

- Plugin ID: `scheduler`
- Direct Plugin dependencies: `time`
- Required typed capabilities: `UtcClock` from `time`
- Provided typed capabilities: none
- Persistent storage: Plugin-scoped KV
- Private filesystem: none
- Owned Event Router Components: `SchedulerComponent`
- Owned tasks: none

The Scheduler Plugin owns a persistent collection of one-time and finite
interval schedules. Its Component exposes the Agent/Workflow-facing
`scheduler.schedule` and `scheduler.cancel` JSON RPCs and emits the
`scheduler.triggered` JSON Event when an occurrence becomes due.

During registration the Plugin obtains the Time Plugin's read-only `UtcClock`
capability with `require::<UtcClock>("time")` and passes it directly to the
Component. Scheduler never calls `time.now` through Event Router and cannot
mutate the clock. The Component run loop remains Event Router-owned because
polling it directly produces scheduled Events.

Each schedule uses its ID as one key and a 32-byte fixed-layout value in the
Plugin's isolated KV namespace. Registration streams the namespace's live keys
and restores each record before loading the Component. Schedule, cancellation,
and occurrence-commit mutations atomically replace or delete only the affected
key, so schedules survive Component unload and process restart without a fixed
schedule count.
