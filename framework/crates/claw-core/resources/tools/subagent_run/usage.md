Run a new specialist subagent and wait for its final result in the current tool
call. The child does not see the parent conversation, so make `goal` complete
and standalone.

Set `timeout_ms` to the maximum lifetime allowed for the child and its subtree.
Use `subagent_spawn` when the parent should continue immediately while the child
runs as a detached tool.
