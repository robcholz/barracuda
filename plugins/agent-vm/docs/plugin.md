# Agent VM Plugin

- Plugin ID: `agent-vm`
- Direct dependencies: `agent`, `vm`
- Provided typed capabilities: none
- Required typed capabilities: `AgentToolRegistry` from `agent`, `Vm` from `vm`
- Owned Components: none
- Plugin-owned tasks: none

During Plugin registration, `agent-vm` adds `vm_run`, `vm_list`, `vm_input`,
and `vm_cancel` to `AgentToolRegistry`. `vm_run` is a dynamic detached Tool: its
accepted settlement contains the `run_id`. When Lua blocks in `io.read()`, it
sends a non-terminal `{"kind":"input_required","run_id":...}` progress update
to the Agent. Returning from the detached handler implicitly completes the Tool
with the ordered `print(...)` lines and terminal result. `vm_list` returns
the `run_id` and `running` or `input_required` state of every active execution.
The other control Tools are ordinary awaited Tools.

The adapter owns no VM runtime or execution state. Both settlements come from
the `VmRun` handle returned by the `Vm` typed capability; it does not translate
VM state into Workflow Events.
