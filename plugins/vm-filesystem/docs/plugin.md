# VM Filesystem Plugin

- Plugin ID: `vm-filesystem`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Owned tasks: none

This Plugin adapts its Plugin Manager-provided private `ScopedVfs` to Lua's
standard file API. It registers the adapter during Plugin registration and
retains that registration for its complete lifecycle.

## Ownership and sandbox boundary

`vm-filesystem` owns no global or host filesystem. The Plugin Manager gives each
Plugin instance a private `ScopedVfs`; this Plugin stores that scope in its Lua
package and passes Lua paths to it unchanged. Paths may address only mounts
visible in the scope:
Plugin-private `/data`, `/media`, `/cache`, and read-only `/resources`, plus
the shared `/workspace/media`, `/workspace/cache`, and read-only
`/workspace/resources`. Normal VFS path normalization, mount permissions, and
cross-mount rename rules remain in force.

There is deliberately no vm-filesystem current-working-directory policy. In the
normal Plugin scope, callers use explicit paths such as `/data/file.txt` or
`/workspace/media/file.txt`; an unmounted relative path fails through the VFS
instead of being silently redirected.

The VM runtime does not acquire a VFS dependency. Disabling or revoking this
Plugin removes file authority without changing the computation-only Lua
sandbox.

## Lua API

The Plugin augments the built-in `io` table rather than replacing its virtual
standard streams. It provides:

- `io.open(filename [, mode])` with `r`, `w`, `a`, `r+`, `w+`, and `a+` modes;
  the optional `b` marker is accepted as Lua requires;
- `io.tmpfile()`;
- `file:close()`, `file:flush()`, `file:lines(...)`, `file:read(...)`,
  `file:seek([whence [, offset]])`, `file:setvbuf(mode [, size])`, and
  `file:write(...)`;
- `io.input([file|string])`, `io.output([file|string])`, `io.read(...)`,
  `io.write(...)`, `io.flush()`, `io.close([file])`, `io.lines([filename, ...])`,
  and `io.type(value)`;
- the standard read formats `l`/`*l`, `L`/`*L`, `a`/`*a`, `n`/`*n`, and
  non-negative byte counts, including multiple formats in one call;
- only `os.remove`, `os.rename`, and `os.tmpname` from the standard `os` table.

File handle metatables and their backing Rust userdata are private. Lua can use
ordinary metatables, but cannot extract or rewrite native userdata finalizers.
Package revocation is checked again by callbacks that were already installed
in a running Lua state.

## Intentional limits and deviations

- At most 16 file handles may be open at once.
- One file read or individual write may transfer at most 32 KiB.
- File paths may contain at most 1,024 bytes.
- Writes are flushed immediately. `setvbuf` validates the standard modes but is
  otherwise a compatibility no-op.
- `os.tmpname` atomically creates an empty name under `/cache` instead of merely
  predicting a path. `io.tmpfile` removes that file on explicit `close`; a temp
  file discarded by Lua collection remains in disposable `/cache` until normal
  cache eviction.
- This Plugin does not provide `io.popen` or process, environment, locale,
  wall-clock, dynamic-loader, and host-file-descriptor APIs. `vm-time` may
  independently extend the same VM-owned `os` table with UTC calendar APIs.
