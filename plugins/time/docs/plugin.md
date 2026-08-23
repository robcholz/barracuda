# Time Plugin

- Plugin ID: `time`
- Direct Plugin dependencies: none

The Time Plugin receives only the platform network capability. It owns the
SNTP source and its server, retry, resynchronization, and holdover policy, then
loads the standalone Time Component. The Component synchronizes UTC, maintains
RTC holdover state, and exposes `time.now`.

It owns the Time Component and does not provide or require a typed Plugin
capability.
