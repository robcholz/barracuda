# Repository Conventions

See [.agents/docs/codestyle.md](.agents/docs/codestyle.md) for the Rust API.

See [.agents/docs/platform-architecture.md](.agents/docs/platform-architecture.md)
for the authoritative Platform, capability ownership, implementation placement,
and zero-overhead architecture. Treat current violations as migration work, not
precedent.

See [.agents/docs/plugin-communication.md](.agents/docs/plugin-communication.md)
for guidance on choosing between Event Router contracts and typed Plugin
capabilities, and for the required capability declaration in `plugin.md`.

See [.agents/docs/execution-ownership.md](.agents/docs/execution-ownership.md)
for the authoritative boundary between Event Router Components and
owner-managed Embassy tasks.

## Local verification

Do not run the full workspace test suite locally. Use focused tests and checks
for the affected packages and targets; leave the complete test matrix to CI.
