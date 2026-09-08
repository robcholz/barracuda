# HTTP Plugin

- Plugin ID: `http`
- Direct Plugin dependencies: `workflow`
- Provided typed capabilities: `Http`
- Required typed capabilities: `WorkflowActionRegistry`

The HTTP Plugin adapts System's shared `http_client::ClientFactory` into a typed
capability and registers the same operation as the `http.request` Workflow
Action.

The capability preallocates two reusable HTTP workspaces. Each workspace owns
its response-header and read buffers and retains the response body's high-water
capacity. A third simultaneous request returns `busy`. Response bodies are
bounded at 64 KiB. Requests are bounded to a 2 KiB URL, 32 headers totaling at
most 16 KiB, and a 32 KiB body. The complete DNS, connection, TLS, HTTP, and
body-read operation has a 30 second deadline.
