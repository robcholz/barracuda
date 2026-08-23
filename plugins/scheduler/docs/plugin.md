# Scheduler Plugin

- Plugin ID: `scheduler`
- Direct Plugin dependencies: `time`

The Scheduler Plugin constructs the standalone RTC-authoritative Scheduler
Component with its own internal defaults. The Component accepts schedule and
cancellation RPCs, reads `time.now` internally, and emits
`scheduler.triggered` Events when occurrences become due.

It owns the Scheduler Component and does not provide or require a typed Plugin
capability.
