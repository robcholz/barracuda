# Time Plugin

- Plugin ID: `time`
- Direct Plugin dependencies: none
- Provided typed capabilities: `UtcClock`
- Required typed capabilities: none
- Owned Components: `TimeComponent`
- Owned tasks: `time_sync_task`

The Time Plugin synchronizes absolute UTC time from SNTP and keeps one accepted
network sample anchored to Embassy's monotonic clock. It retries failed
synchronization attempts, periodically refreshes successful samples, and rejects
reads after the configured holdover expires.

During registration, the Plugin creates the shared clock, publishes the
read-only `UtcClock` capability, and loads `TimeComponent`. Direct Plugin
consumers read `UnixMillis` from that capability. The write-side
`UtcClockUpdater` remains private to the Time Plugin runtime so consumers cannot
modify system time.

During startup, the Plugin launches `time_sync_task`. That Plugin-owned Embassy
task owns the SNTP source and updater; the Event Router Component does not poll
the network or timer loop. Plugin unload and startup rollback cancel the task
cooperatively.

`TimeComponent` exposes the public JSON RPC `time.now` for Agents and Workflows.
Its caller-facing response is an RFC 3339 UTC string, while typed Plugin callers
continue to use Unix milliseconds.
