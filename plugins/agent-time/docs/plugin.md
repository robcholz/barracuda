# Agent Time Plugin

`agent-time` depends on `agent` and `time`. During Plugin registration it adds the awaited `time_now` Tool to the shared `AgentToolRegistry`. It owns no clock state or background task.
