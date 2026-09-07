# Workflow Plugin

- Plugin ID: `workflow`
- Direct Plugin dependencies: none
- Provided typed capabilities: `WorkflowActionRegistry`, `WorkflowService`

The Workflow Plugin owns durable Workflow definitions, Event matching, and
cooperative execution. A single execution awaits its Actions in document order;
different executions are polled concurrently by the Workflow task.

Dependent Plugins register direct JSON Actions through
`WorkflowActionRegistry`. The registry contains no RPC transport, lanes, or
runtime registration phase. `WorkflowService` loads and unloads definitions at
runtime, lists the loaded catalog, and emits Events.

The Plugin uses a private filesystem for `workflows.json`. Its Embassy task
restores that catalog before it starts advancing emitted executions. Removing a
definition prevents later Events from starting it and does not cancel execution
snapshots that already started.
