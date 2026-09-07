# Plugin Filesystem

Use this contract whenever a Plugin reads, writes, or contributes files.

## Boundary

A Plugin receives an ordinary `ScopedVfs` containing its private namespace and
the shared Workspace. System owns mount construction, access policy, and
backing filesystem selection. The complete VFS architecture and default
backend mapping are defined in
[`platform-architecture.md`](../../docs/platform-architecture.md#filesystems-and-database).
A Plugin must not:

- introduce a Plugin-specific VFS abstraction;
- mount or unmount a backend;
- select FAT, LittleFS, or another concrete filesystem;
- access System paths or another Plugin's namespace.

The private scope has four reserved mount points:

| Path | Contract |
| --- | --- |
| `/resources` | Read-only files supplied by the built image. They follow the image version and may be replaced by an upgrade. |
| `/data` | Durable read-write files required for correctness. They survive restart and are never automatically evicted. |
| `/cache` | Read-write, reproducible files that System may remove at any time. Callers must handle absence. |
| `/media` | Durable large runtime content on optional or removable storage. Callers must handle the mount being unavailable. |

The same `ScopedVfs` exposes three shared Workspace mount points:

| Path | Contract |
| --- | --- |
| `/workspace/resources` | Shared read-only files supplied by the System image. |
| `/workspace/cache` | Shared reproducible or short-lived exchange files that System may remove at any time. |
| `/workspace/media` | Shared durable runtime files. Callers must handle the mount being unavailable. |

Do not store files directly at the scoped root or `/workspace`. Select one
reserved mount from the file's lifecycle and ownership semantics.

## Classification

Classify by lifecycle, not size:

- A large model bundled with the image belongs in `/resources`.
- A small mutable Workflow catalog belongs in `/data`.
- A re-downloadable model belongs in `/cache`.
- A user upload belongs in `/media`.
- A common immutable model or template belongs in `/workspace/resources`.
- A VM result exchanged with an Agent or Gateway belongs in `/workspace/cache`.
- A generated document that must outlive its run belongs in `/workspace/media`.

Keep small structured state in the Plugin's scoped KV storage instead of
`/data`. Shared correctness state retains an explicit owner and belongs in that
owner's `/data` or scoped KV storage. Prebuilt resources and runtime-generated
media are distinct even when both are large.

## Repository contribution

Put every asset intended for the bundled runtime filesystem below
`plugins/<plugin>/filesystem/`. The only supported bundled subtree is currently
`filesystem/resources/`; do not add `filesystem/data/`, `filesystem/cache/`,
`filesystem/media/`, or another sibling until the image builder defines that
contract.

Files below `plugins/<plugin>/filesystem/resources/` contribute to that
Plugin's `/resources` tree in the prebuilt filesystem:

```text
plugins/example/filesystem/resources/index.html
-> /resources/index.html
```

Crate-local directories such as `crates/<crate>/resources/` are compile-time
inputs only. They are not discovered or bundled into the Plugin filesystem.

Do not prebuild `/data`, `/cache`, or `/media`. When mutable state needs
defaults, read the default from `/resources` and initialize `/data` only when
it is absent. A Plugin contribution is always scoped to the contributing
Plugin; it cannot contribute to `/workspace/resources` or target another image
path. System-owned image inputs populate the shared Workspace resources.

## Required behavior

Mount policy enforces the contract:

- mutations below `/resources` and `/workspace/resources` return `ReadOnly`;
- an unavailable optional volume returns `NotMounted`;
- rename across reserved mount points returns `CrossMount`;
- `..` and absolute backend paths cannot escape the Plugin scope;
- unload does not delete `/data`, `/media`, or `/workspace/media`.

Plugins must treat `/cache` and `/workspace/cache` as disposable, and `/media`
and `/workspace/media` as potentially absent. They must not depend on the
concrete backing filesystem for correctness.
