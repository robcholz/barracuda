List active subagents in your subtree, with each one's id, kind, and status. Use
it to see what is still running before you watch, retask, or stop a specific
subagent.

`completed_pending_delivery` is a short-lived graph record: physical cleanup
has finished and the detached result is waiting for its parent Agent to accept
it. The record is removed by the detached completion acknowledgement.
