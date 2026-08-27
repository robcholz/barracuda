# Event Router examples

Each example covers one public Event Router usage and verifies its observable
result with assertions.

| Usage | Example | What it demonstrates |
| --- | --- | --- |
| Components | `components` | `EventRouter::new`, a zerocopy DTO, Component RPC registration, `RunContext::rpc`, explicit polling, `load`, `unload`, and teardown |
| Events | `events` | Streaming `workflow.load`, durable JSON, `EventEmitter`, matching execution, and immutable Workflow snapshots |
| JSON calls | `component_rpc_json` | `#[rpc_dynamic]` on a Component-hosted method and runtime `RpcClient::call_json(address, value)` returning `{ ok, ... }` |
| Workflow mapping | `workflow` | A multi-step Workflow whose `$previous.output.<field>` reference is copied into the next request |
| Persistence | `persistence` | Durable restore after Event Router reconstruction, then durable `WorkflowClient::unload` |
| Streaming | `streaming` | A streaming Event fanned into a streaming Workflow step in order |
| Workflow failure | `workflow_failure` | A Method error surfaced through `WorkflowInfo::{failed_count, last_failure}` |

Run them from the `agent-framework` workspace root:

```console
cargo run -p barracuda-event-router --example components
cargo run -p barracuda-event-router --example events
cargo run -p barracuda-event-router --example component_rpc_json
cargo run -p barracuda-event-router --example workflow
cargo run -p barracuda-event-router --example persistence
cargo run -p barracuda-event-router --example streaming
cargo run -p barracuda-event-router --example workflow_failure
```

The examples depend only on `barracuda-event-router` and do not access its internal
`WorkflowRuntime` composition.

Lower-level APIs have their own examples:

- `core/rpc/examples/`
- `core/router/examples/`
- `core/workflow/examples/`
