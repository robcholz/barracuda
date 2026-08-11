# claw-interface

The OS / platform abstraction layer for the claw Rust crates.

This is the **inbound boundary** (C / OS → Rust): it defines the
**dependency-injection traits** for filesystems. HTTP is provided by `claw-net`,
and deadlines/backoff use the global `embassy-time` driver.

## What's here

### `fs` — the `ClawFs` persistence seam

The byte-oriented filesystem injection point for everything that must survive a
reboot (conversation tapes, profile/long-term memory, …). Two write disciplines
coexist: `append` + `read_at` for append-only journals, and `write_atomic` for
tear-free whole-file checkpoints.

| Item | Role |
|---|---|
| `ClawFs` | The trait: `read`, `read_at`, `len`, `write_atomic`, `append`, `create_dir_all`, `exists`, `remove`, `list_dir`. |
| `FsError` | Coarse failure: `NotFound` vs `Io(..)`. |

## Host-only reference implementations (opt-in features)

These live beside the traits only to keep the few distinct implementations in
one place. They are **never** enabled in a device build.

| Feature | Provides |
|---|---|
| built in | `MemFs` — a per-instance in-memory `ClawFs` implementation (no extra deps). |
| `diskfs` | `DiskFs` — a `std::fs`-backed `ClawFs` for host CLIs and disk tests. |
| `diskfs-pretty` | `DiskFs` that pretty-prints `.json` writes (implies `diskfs`). |

## Example

```bash
cargo run -p claw-interface --example di_seams
```

Exercises the `ClawFs` seam with `MemFs`.

## Where it fits

Filesystem access remains injected here. Networking is platform-agnostic at the
`embedded-nal-async` boundary in `claw-net`; time uses `embassy-time` directly.
