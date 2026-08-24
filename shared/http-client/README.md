# http-client

The workspace-wide, executor-neutral HTTP client for Barracuda's `no_std`
components. `Client` is the public facade; it hides reqwless, TCP, DNS, TLS
selection, connection state, and buffer ownership from ordinary consumers.

## Basic use

```rust,ignore
let http = http_factory.create();
let response = http
    .post("https://example.test/messages")
    .header("Authorization", token)
    .json(payload)
    .send()
    .await?;
```

System creates one `ClientFactory` from the Platform-owned IP stack and TLS
capability. Plugins clone the factory and call `create()` when they need an
independent connection owner. Business and protocol code receives `Client`,
not the factory's TCP/DNS/TLS inputs.

`send()` buffers one complete response. `send_stream()` yields a
`ResponsePart::Head` followed by body chunks; the concrete shared client reads
those chunks directly from the connection. Lower-level `Request` and
`HttpClient` APIs remain available for protocol adapters and test doubles.

Platform composition is the only production construction site:

```rust,ignore
let tls = platform_resources.tls;
let http_factory = http_client::ClientFactory::new(
    platform_resources.ip_stack,
    move || tls.config(),
);
```

`Client::from_network` and `Client::from_backend` are narrow integration/test
constructors. Ordinary components should not use them.
