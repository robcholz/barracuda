# Agent HTTP Plugin

`agent-http` depends on `agent` and `http`. During registration it adds the
awaited `http_request` Tool to `AgentToolRegistry`. It owns no transport,
workspace, or background task.
