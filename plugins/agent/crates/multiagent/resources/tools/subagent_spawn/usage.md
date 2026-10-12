The child does not see the parent conversation; make `goal` complete and
standalone. This is a background tool call: its accepted result carries a
background `id`, and the final result is delivered automatically. Continue
useful independent work after spawning; do not poll for completion. Use
`background_wait` with that `id` only when the next step needs the result,
`background_input` to interrupt the child with a new task, and
`background_cancel` to delete it. Use `subagent_run` when the current tool call
must wait for the result.

At most 3 subagents may be live at once across the whole tree, nested ones
included. A spawn beyond that is rejected; wait for a result or delete a
subagent you no longer need first.
