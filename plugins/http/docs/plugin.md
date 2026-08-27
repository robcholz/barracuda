# HTTP Plugin

- Plugin ID: `http`
- Direct Plugin dependencies: none
- Provided typed capabilities: none
- Required typed capabilities: none
- Owned Components: inline HTTP Component

The HTTP Plugin adapts System's shared `http_client::ClientFactory` into the
dynamic `http.request` Event Router RPC. It performs buffered outbound HTTP and
HTTPS requests without depending on a concrete Platform or creating a second
HTTP transport implementation.
