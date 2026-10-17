Use `background_input` with exactly one of `input` or `eof: true`, typically
after a `[background:progress]` update asks for input. What the input means
depends on the tool: a VM run reads it from `io.read()`, and a subagent
receives it as a new task that interrupts its current one.
