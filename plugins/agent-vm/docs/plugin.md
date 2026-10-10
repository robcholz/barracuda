# Agent VM Plugin

- Plugin ID: `agent-vm`
- Direct dependencies: `agent`, `vm`
- Provided typed capabilities: none
- Required typed capabilities: `AgentToolRegistry` from `agent`, `Vm` from `vm`
- Plugin-owned tasks: none

During Plugin registration, `agent-vm` adds `vm_run` to `AgentToolRegistry`.
`vm_run` is a background Tool: its accepted output contains the `run_id`, and
the run then lives in the calling Agent's background pool. When Lua blocks in
`io.read()`, it sends a non-terminal `{"kind":"input_required","run_id":...}`
progress update to the Agent. Returning from the background handler implicitly
completes the call with the ordered `print(...)` lines and terminal result.

The run's `BackgroundToolControl` reports the `running` or `input_required`
state to `background_list` and feeds `background_input` to `io.read()` as one
value or EOF. There are no VM-specific list, input, or cancel Tools.

The Agent's background pool owns the run's completion future, which owns the
`VmRun` handle. `background_cancel`, cancelling or deleting that Agent drops
the handle, which cancels an incomplete VM execution and releases its runtime
slot.

The adapter owns no VM runtime or execution state. Every update comes from
the `VmRun` handle returned by the `Vm` typed capability; it does not translate
VM state into Workflow Events.
