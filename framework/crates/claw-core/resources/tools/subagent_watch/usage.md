Get the current state of a subagent by id: its kind and status. You can only
watch a subagent in your own subtree.

A completed subagent may be briefly visible as `completed_pending_delivery`
after physical cleanup and before its parent Agent accepts the detached result.
It can no longer be retasked or deleted in that state.
