# Workflow Plugin

- Plugin ID: `workflow`
- Direct Plugin dependencies: none
- Provided typed capabilities: `WorkflowActionRegistry`, `WorkflowService`

The Workflow Plugin owns durable Workflow definitions, Event matching, and
cooperative execution. A single execution awaits its Actions in document order;
different executions are polled concurrently by the Workflow task.

Dependent Plugins register direct typed Actions with
`WorkflowActionRegistry::add_action(handler)`. Each handler declares Rust
`Request` and `Response` types plus one static schema contract. The contract
embeds request and response JSON Schemas and compiles both validators at build
time; the Workflow runtime owns validation and Serde conversion around the
typed handler. Registered descriptors expose those schemas for a future Agent
Workflow builder without turning the Action itself into an Agent Tool.

Production Actions use `workflow_action_schema!("address")`, which loads
`schemas/action/<address>/request.json` and `response.json` from the owning
Plugin. The registry contains no RPC transport, lanes, or runtime registration
phase. `WorkflowService` loads and unloads definitions at runtime, lists the
loaded catalog, and emits Events.

```rust
#[derive(serde::Deserialize)]
struct Request {
    message: String,
}

#[derive(serde::Serialize)]
struct Response {
    delivered: bool,
}

struct SendMessage;

impl WorkflowActionHandler for SendMessage {
    type Request = Request;
    type Response = Response;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("message.send");

    fn invoke(&self, request: Request) -> WorkflowActionFuture<'_, Response> {
        Box::pin(async move {
            send(request.message).await?;
            Ok(Response { delivered: true })
        })
    }
}

let registration = actions.add_action(SendMessage)?;
context.retain(registration);
```

The Plugin uses a private filesystem for `workflows.json`. Its Embassy task
restores that catalog before it starts advancing emitted executions. Removing a
definition prevents later Events from starting it and does not cancel execution
snapshots that already started.
