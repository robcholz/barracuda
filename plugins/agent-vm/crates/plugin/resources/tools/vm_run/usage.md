Use `vm_run` with a complete Lua source document. Acceptance immediately returns
the `run_id` needed by `vm_input` and `vm_cancel`. If execution reaches
`io.input()`, an `input_required` progress update is delivered automatically;
respond with `vm_input`. The ordered `io.print(...)` messages and terminal
outcome are delivered automatically when execution ends.
