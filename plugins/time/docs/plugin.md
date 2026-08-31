# Time Plugin

- Plugin ID: `time`
- Direct Plugin dependencies: none
- Provided typed capabilities: none

System constructs the Time Plugin from the common `PluginContext`, and the
Plugin copies its public `ip_stack` handle. It owns the SNTP source and uses
Embassy DNS and `UdpSocket` directly, together with its server, retry,
resynchronization, and holdover policy. Registration loads a handler-only Time
Component that exposes `time.now`. Startup launches the separate Plugin-owned
Embassy synchronization task, which updates the clock state shared with that
handler. Event Router does not poll the socket/timer loop. Plugin unload or
startup rollback signals cooperative task cancellation.

It owns the Time Component and does not provide or require a typed Plugin
capability.
