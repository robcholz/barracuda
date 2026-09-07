# Agent VM Plugin

`agent-vm` depends on `agent` and `vm`. During Plugin registration it adds the
awaited `vm_run`, `vm_input`, and `vm_cancel` Tools to `AgentToolRegistry`. It
owns no VM runtime, execution state, or background task.
