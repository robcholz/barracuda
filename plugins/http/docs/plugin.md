# HTTP Plugin

The HTTP Plugin owns outbound HTTP: the TLS engine, the connection pool, the
typed request capability, and the `http.request` Workflow Action.

- Plugin ID: `http`
- Dependencies: `workflow`
- Settings: none
- Context resources: `entropy`, `ip_stack`
- Provided capabilities: `Http`, `ClientFactory` (`http_client::ClientFactory`)
- Required capabilities: `WorkflowActionRegistry` from `workflow`
- Workflow: Actions `http.request`, see `action.md`
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: none
- Storage: none

The constructor starts TLS from the Platform's entropy and builds the
`http_client::ClientFactory` connection pool on the IP stack. Without TLS,
`https://` requests fail rather than fall back to plaintext. The Plugin
publishes the factory as a capability for Plugins that build their own HTTP
clients, wraps it in the typed `Http` capability, and registers the same
operation as the `http.request` Workflow Action.

The capability preallocates two reusable HTTP workspaces. Each workspace owns
its response-header and read buffers and retains the response body's high-water
capacity. A third simultaneous request returns `busy`. Response bodies are
bounded at 64 KiB. Requests are bounded to a 2 KiB URL, 32 headers totaling at
most 16 KiB, and a 32 KiB body. The complete DNS, connection, TLS, HTTP, and
body-read operation has a 30 second deadline.
