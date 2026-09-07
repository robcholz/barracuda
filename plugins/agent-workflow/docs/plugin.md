# Agent Workflow Plugin

`agent-workflow` depends on `agent` and `workflow`. During registration it adds
the awaited `workflow_actions`, `workflow_list`, `workflow_load`, and
`workflow_unload` Tools to `AgentToolRegistry`.

`workflow_actions` exposes the address and static request/response schemas of
every currently registered Workflow Action. The Agent uses that catalog to
construct valid Workflow DSL documents before calling `workflow_load`.
