The child does not see the parent conversation; make `goal` complete and
standalone. This tool runs in the background and delivers the final result
automatically. Continue useful independent work after spawning; do not
repeatedly call `subagent_watch` just to wait for completion. Use
`subagent_watch` only when the current status is needed for a decision, and do
not call it in consecutive iterations. Use `subagent_run` when the current tool
call must wait for the result.
