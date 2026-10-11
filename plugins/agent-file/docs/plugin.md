# Agent File Plugin

Exposes the Plugin filesystem to Agents as bounded native file Tools.

- Plugin ID: `agent-file`
- Dependencies: `agent`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `AgentToolRegistry` from `agent`
- Workflow: none
- Agent Tools: `file_read`, `file_list`, `file_write`, `file_edit`, `file_move`,
  `file_delete`
- Lua packages: none
- Web: none
- Tasks: none
- Storage: no KV keys; the Tools read and write caller-supplied paths in the
  Plugin's `ScopedVfs` (`/resources`, `/data`, `/cache`, `/media`,
  `/workspace/resources`, `/workspace/cache`, `/workspace/media`)

The Plugin requires a private filesystem (`PluginFilesystem::Private`), which
gives it the Plugin-private mounts and the shared Workspace mounts.

The Plugin declares filesystem access and receives its ordinary `ScopedVfs`
from `PluginRegisterContext`. During registration it gives that same scope to
the awaited `file_read`, `file_list`, `file_write`, `file_edit`, `file_move`,
and `file_delete` Tools registered directly in `AgentToolRegistry`. System owns
mount construction, policy, and backing filesystem selection.

The scope exposes the Plugin-private `/resources`, `/data`, `/cache`, and
`/media` mounts plus the shared `/workspace/resources`, `/workspace/cache`, and
`/workspace/media` mounts. The file Tools can therefore exchange files with
other filesystem-enabled Plugins through the shared Workspace while private
Plugin paths remain isolated.

Reads and directory listings are bounded and paginated. Writes and edits are
bounded to 32 KiB and use `ScopedVfs::write_atomic`. A create does not replace
an existing path unless the caller explicitly opts in. Exact-text editing
rejects missing or ambiguous matches before writing. Moves do not replace a
destination, and deletion is limited to one file or one empty directory.

Path strings are passed directly to the Plugin `ScopedVfs`; this Plugin does
not duplicate path syntax, containment, root, read-only mount, optional mount,
or cross-mount policy. Stable filesystem failures are returned in the Tool
output with `ok = false`; malformed arguments remain framework-level Tool
failures.
