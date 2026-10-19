# Contributing to Barracuda

Thank you for helping build Barracuda. We welcome new plugins, new boards,
fixes, and docs. This guide covers how to go from an idea to a merged pull
request.

## Contributor License Agreement

Before we can merge your first pull request, you need to accept the
[Contributor License Agreement](CLA.md). You keep the copyright in your work;
the agreement lets the project offer Barracuda under the
[AGPL v3](LICENSE) and under commercial licenses.

To accept it, add this comment to your pull request:

```text
I have read the Barracuda CLA and I accept it.
```

You only need to do this once. If you contribute on behalf of your employer,
make sure you have its permission first.

## Issue to PR

1. [Search existing issues](https://github.com/robcholz/barracuda/issues) for
   your bug or feature request.
2. If nothing exists, [open an issue](https://github.com/robcholz/barracuda/issues/new).
   For a bug, include the board you use, what you did, what you expected, and
   what happened. For a feature, describe the problem it solves.
3. For a large change, such as a new plugin, board, or platform, wait for a
   maintainer to agree on the design in the issue before you write the code.
4. Open a pull request that links the issue and accept the CLA in it.

Report security vulnerabilities privately with **Report a vulnerability** under
the repository's **Security** tab, never in a public issue.

## Work with your coding agent

The repository is built to be worked on by coding agents. Its
[skills](.agents/skills/) teach Claude Code, Cursor, and Codex how Barracuda is
built:

| Skill | Use it to |
| --- | --- |
| `create-plugin` | Add a capability, such as support for a new sensor or service |
| `create-board` | Add a board on an existing platform |
| `create-platform` | Add a new chip family |
| `create-pr` | Prepare a feature, refactor, or docs pull request |
| `create-bugfix-pr` | Prepare a bug-fix pull request with its evidence and regression test |

You are responsible for every line your agent writes: read the diff before you
open the pull request.

## Build and test

Build and run Barracuda on your computer, with virtual peripherals in place of
hardware:

```bash
cargo board select local-linux                      # or local-macos, or any board under boards/configs/
sudo sh platforms/linux/provision.sh barracuda0     # Linux only, once per boot
BARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run  # build and run
```

Before you push, run the checks for what you changed:

```bash
cargo fmt --all --check
cargo test --package <crate you changed>
cargo clippy --package <crate you changed> --all-targets
```

Leave the full test matrix to CI. If you changed docs, run
`npx mint validate` and `npx mint broken-links` from `docs/`.

## Conventions

[AGENTS.md](AGENTS.md) is the engineering guide for people and agents alike. It
links the authoritative documents for:

- the [plugin contract](docs/architecture/plugin-api.mdx)
- the [platform architecture](docs/architecture/platform-architecture.mdx)
- the [Rust code style](docs/architecture/codestyle.mdx)
- [writing docs](docs/contributing/docs.mdx), in English and Simplified Chinese

## Code of Conduct

Everyone who takes part in Barracuda follows the
[Code of Conduct](CODE_OF_CONDUCT.md). Report violations to
[finn.robcholz.dev@gmail.com](mailto:finn.robcholz.dev@gmail.com).
