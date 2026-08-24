# Time Plugin

- Plugin ID: `time`
- Direct Plugin dependencies: none
- Provided typed capabilities: none

System constructs the Time Plugin with the common `embassy_net::Stack`. The
Plugin owns the SNTP source and uses Embassy DNS and `UdpSocket` directly,
together with its server, retry, resynchronization, and holdover policy, then
loads the standalone Time Component. The Component synchronizes UTC, maintains
RTC holdover state, and exposes `time.now`.

It owns the Time Component and does not provide or require a typed Plugin
capability.
