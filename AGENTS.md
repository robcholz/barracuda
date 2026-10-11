# Repository Conventions

See [.agents/docs/codestyle.md](.agents/docs/codestyle.md) for the Rust API.

See [.agents/docs/platform-architecture.md](.agents/docs/platform-architecture.md)
for the authoritative Platform, capability ownership, implementation placement,
application filesystem composition, and zero-overhead architecture. Treat
current violations as migration work, not precedent.

See [.agents/docs/plugin-api.md](.agents/docs/plugin-api.md) for the
authoritative Plugin contract: the scope rules, layout, `plugin.toml` and
settings, construction, lifecycle, collaboration contracts, files, tests, and
the required `plugin.md` form.

See [.agents/docs/execution-ownership.md](.agents/docs/execution-ownership.md)
for the authoritative ownership and cancellation rules for long-lived Embassy
tasks.

## Local verification

Do not run the full workspace test suite locally. Use focused tests and checks
for the affected packages and targets; leave the complete test matrix to CI.
