# Event Router examples

Each example covers one public Event Router usage and verifies its observable
result with assertions.

| Usage | Example | What it demonstrates |
| --- | --- | --- |
| Components | `components` | `EventRouter::new`, Component RPC registration, `RunContext::rpc`, explicit polling, `load`, `unload`, and teardown |
| Events | `events` | Streaming `workflow.load`, durable JSON, `EventEmitter`, matching execution, and immutable Workflow snapshots |
| JSON calls | `rpc_json` | `#[rpc_json]` on a typed method and runtime `RpcClient::call_json(address, value)` returning `{ ok, ... }` |

Run them from the `agent-framework` workspace root:

```console
cargo run -p barracuda-event-router --example components
cargo run -p barracuda-event-router --example events
cargo run -p barracuda-event-router --example rpc_json
```

The examples depend only on `barracuda-event-router` and do not access its internal
`WorkflowRuntime` composition.

Lower-level APIs have their own examples:

- `core/rpc/examples/`
- `core/router/examples/`
- `core/workflow/examples/`
