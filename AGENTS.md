# Repository Conventions

All documentation lives in the Mintlify site under [docs/](docs/), with its
navigation in [docs/docs.json](docs/docs.json). Pages are MDX: write `<`, `{`,
and `}` in prose as `\<`, `\{`, and `\}`, or inside code spans. Add every new
page to `docs/docs.json`, and run `npx mint validate` and
`npx mint broken-links` from `docs/` after changing pages.

See [docs/develop/codestyle.mdx](docs/develop/codestyle.mdx) for the Rust API.

See [docs/develop/platform-architecture.mdx](docs/develop/platform-architecture.mdx)
for the authoritative Platform, capability ownership, implementation placement,
application filesystem composition, and zero-overhead architecture. Treat
current violations as migration work, not precedent.

See [docs/develop/plugin-api.mdx](docs/develop/plugin-api.mdx) for the
authoritative Plugin contract: the scope rules, layout, `plugin.toml` and
settings, construction, lifecycle, collaboration contracts, files, tests, and
the required Plugin page form under `docs/plugins/`.

See [docs/develop/device-ui.mdx](docs/develop/device-ui.mdx) for the
authoritative split of the device UI into the system, system apps, and
installable apps.

See [docs/develop/execution-ownership.mdx](docs/develop/execution-ownership.mdx)
for the authoritative ownership and cancellation rules for long-lived Embassy
tasks.

## Local verification

Do not run the full workspace test suite locally. Use focused tests and checks
for the affected packages and targets; leave the complete test matrix to CI.
