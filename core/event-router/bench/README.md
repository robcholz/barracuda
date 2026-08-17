# Event Router measurement workloads

Event Router keeps two internal Cargo benchmark targets under this directory:

- `profile/` is built with `--profile profiling` and measures heap allocation
  volume, peak live memory, and retained memory.
- `throughput/` uses Cargo's `bench` profile and measures ingress and routed
  bytes/s across N/M/Q, payload, fan-out, and slow-consumer configurations.

Both are `harness = false` targets declared in `core/event-router/Cargo.toml`.
They are not standalone workspace packages and do not add publishable crates.
