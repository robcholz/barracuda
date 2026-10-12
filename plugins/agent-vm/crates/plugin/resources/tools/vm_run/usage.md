Use `vm_run` with a complete Lua source document. It is a background tool
call: acceptance immediately returns the background `id` and the VM `run_id`.
If execution reaches `io.read()`, an `input_required` progress update is
delivered automatically; respond with `background_input` using the background
`id`. The ordered `print(...)` lines and terminal outcome are delivered
automatically when execution ends. Use `background_cancel` with the same `id`
to stop a run.
