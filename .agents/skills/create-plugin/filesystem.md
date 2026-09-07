# Plugin Filesystem

Use this contract whenever a Plugin reads, writes, or contributes files.

## Boundary

A Plugin receives an ordinary `ScopedVfs` rooted in its own namespace. System
owns mount construction, access policy, and backing filesystem selection. A
Plugin must not:

- introduce a Plugin-specific VFS abstraction;
- mount or unmount a backend;
- select FAT, LittleFS, or another concrete filesystem;
- access System paths or another Plugin's namespace.

The scoped root is a virtual namespace with four reserved mount points:

| Path | Contract |
| --- | --- |
| `/resources` | Read-only files supplied by the built image. They follow the image version and may be replaced by an upgrade. |
| `/data` | Durable read-write files required for correctness. They survive restart and are never automatically evicted. |
| `/cache` | Read-write, reproducible files that System may remove at any time. Callers must handle absence. |
| `/media` | Durable large runtime content on optional or removable storage. Callers must handle the mount being unavailable. |

Do not store files directly at the scoped root. Select one reserved mount from
the file's lifecycle and ownership semantics.

## Classification

Classify by lifecycle, not size:

- A large model bundled with the image belongs in `/resources`.
- A small mutable Workflow catalog belongs in `/data`.
- A re-downloadable model belongs in `/cache`.
- A user upload belongs in `/media`.

Keep small structured state in the Plugin's scoped KV storage instead of
`/data`. Prebuilt resources and runtime-generated media are distinct even when
both are large.

## Repository contribution

Repository files below
`plugins/<plugin>/filesystem/resources/` contribute to that Plugin's
`/resources` tree in the prebuilt filesystem:

```text
plugins/example/filesystem/resources/index.html
-> /resources/index.html
```

Do not prebuild `/data`, `/cache`, or `/media`. When mutable state needs
defaults, read the default from `/resources` and initialize `/data` only when
it is absent. A Plugin contribution is always scoped to the contributing
Plugin; it cannot target an arbitrary image path.

## Required behavior

Mount policy enforces the contract:

- mutations below `/resources` return `ReadOnly`;
- an unavailable optional volume returns `NotMounted`;
- rename across reserved mount points returns `CrossMount`;
- `..` and absolute backend paths cannot escape the Plugin scope;
- unload does not delete `/data` or `/media`.

Plugins must treat `/cache` as disposable and `/media` as potentially absent.
They must not depend on the concrete backing filesystem for correctness.
