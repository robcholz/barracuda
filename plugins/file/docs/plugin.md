# File Plugin

- Plugin ID: `file`
- Direct Plugin dependencies: none
- Provided typed capabilities: `FileSystem`
- Owned Components: dynamic File RPC harness

The File Plugin publishes the portable `FileSystem` capability for other
Plugins. Its namespace is rooted at `/system`; callers cannot escape that root
and never receive ownership of the process-wide mount table.

System aliases the backend directory `/system/.builtin/skills` at
`/system/skills` before constructing the Plugin. Consequently capability and
RPC callers address the alias as `/skills` within the Plugin's scoped API.
