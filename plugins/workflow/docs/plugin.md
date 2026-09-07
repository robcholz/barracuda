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
Plugin. `WorkflowService::load` and `WorkflowService::unload` manage
user-created definitions in the durable catalog. `WorkflowService::load_transient`
registers Plugin-owned definitions for the current boot without modifying that
catalog. The capability also lists loaded definitions and emits Events.

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

The Plugin stores only user-created definitions in `/data/workflows.json` in
its scoped filesystem. Its Embassy task restores that catalog before it starts
advancing emitted executions. Durable and transient load calls ensure user
restoration first and serialize with catalog changes, so a persisted user
definition takes precedence over a transient definition with the same ID.
Removing a definition prevents later Events from starting it and does not
cancel execution snapshots that already started.
