# Agent Workflow Plugin

Exposes Workflow discovery and durable control as native Agent Tools.

- Plugin ID: `agent-workflow`
- Dependencies: `agent`, `workflow`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `AgentToolRegistry` from `agent`,
  `WorkflowActionRegistry` from `workflow`, `WorkflowService` from `workflow`
- Workflow: none
- Agent Tools: `workflow_actions`, `workflow_list`, `workflow_load`,
  `workflow_unload`, `workflow_switch`
- Lua packages: none
- Web: none
- Tasks: none
- Storage: none

During registration it adds the awaited `workflow_actions`, `workflow_list`,
`workflow_load`, `workflow_unload`, and `workflow_switch` Tools to
`AgentToolRegistry`. `workflow_list` names the loaded Workflows and those
that are off; `workflow_switch` turns one on or off without unloading it.

`workflow_actions` exposes the address and static request/response schemas of
every currently registered Workflow Action. The Agent uses that catalog to
construct valid Workflow DSL documents before calling `workflow_load`.
