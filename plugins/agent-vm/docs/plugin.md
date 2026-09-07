# Agent VM Plugin

- Plugin ID: `agent-vm`
- Direct dependencies: `agent`, `vm`
- Provided typed capabilities: none
- Required typed capabilities: `AgentToolRegistry` from `agent`, `Vm` from `vm`
- Owned Components: none
- Plugin-owned tasks: none

During Plugin registration, `agent-vm` adds `vm_run`, `vm_input`, and
`vm_cancel` to `AgentToolRegistry`. `vm_run` is a dynamic detached Tool: its
accepted settlement contains the `run_id`, and its completion settlement
contains the ordered `io.print(...)` messages and terminal result. `vm_input`
and `vm_cancel` are ordinary awaited control Tools.

The adapter owns no VM runtime or execution state. Both settlements come from
the `VmRun` handle returned by the `Vm` typed capability.
