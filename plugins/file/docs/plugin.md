# File Plugin

- Plugin ID: `file`
- Direct Plugin dependencies: none
- Provided typed capabilities: `FileSystem`
- Required typed capabilities: none
- Owned Components: File JSON RPC Component
- Plugin-owned tasks: none

The File Plugin publishes the portable `FileSystem` capability for other
Plugins. Plugin Manager grants it a private filesystem namespace rooted at
`/plugins/file`; capability and RPC callers use paths relative to that scope
and cannot access the process-wide mount table.

`FileSystem` is the direct Plugin-to-Plugin API and retains the full VFS byte
semantics. The Event Router Component separately exposes bounded UTF-8 JSON RPCs
for Agents and Workflows. Request fields share one lane-sized decoding scratch;
they do not reserve independent path or content buffers. The Component has no
independent runtime loop beyond serving those contracts.
