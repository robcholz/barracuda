# File Plugin

- Plugin ID: `file`
- Direct Plugin dependencies: none
- Provided typed capabilities: `FileSystem`
- Owned Components: dynamic File RPC harness

The File Plugin publishes the portable `FileSystem` capability for other
Plugins. Plugin Manager grants it a private filesystem namespace rooted at
`/plugins/file`; capability and RPC callers use paths relative to that scope
and cannot access the process-wide mount table.
