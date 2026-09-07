Use `vm_run` with a complete Lua source document. Acceptance immediately returns
the `run_id` needed by `vm_input` and `vm_cancel`. The ordered `io.print(...)`
messages and terminal outcome are delivered automatically when execution ends.
