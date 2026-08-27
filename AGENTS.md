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

## Pre-commit verification

Before every commit, run all of the following commands:

```sh
cargo fmt --all
cargo clippy --all
cargo build
cargo test
```

Every command must succeed before creating the commit. Do not skip a command,
commit with a known failure, or defer verification until after the commit.
Do not replace these commands with `--all-features`: some workspace crates have
mutually exclusive features. Target-specific changes additionally require the
corresponding build, Clippy, and test commands for each affected target and
matching Board selection.
