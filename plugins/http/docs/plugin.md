# HTTP Plugin

- Plugin ID: `http`
- Direct Plugin dependencies: none
- Provided typed capabilities: none
- Required typed capabilities: none
- Owned Components: inline HTTP Component

The HTTP Plugin adapts System's shared `http_client::ClientFactory` into the
public `http.request` JSON RPC. It performs bounded, buffered outbound HTTP and
HTTPS requests without depending on a concrete Platform or creating a second
HTTP transport implementation.

The Component decodes all request strings into one lane-sized fixed scratch
buffer and retains ranges into that shared storage. It preallocates two reusable
HTTP workspaces when the Plugin is registered. Each workspace owns its response-
header and response-body buffers, so network calls do not allocate those large
buffers per request. A third simultaneous request fails with
`RpcError::ResourceExhausted` instead of allocating more memory.
