# http-client

The thin boundary between Platform networking and `reqwless`.

`ClientFactory` owns or borrows a TCP connector, DNS resolver, and optional
Platform TLS configuration. Calling `ClientFactory::create()` creates a raw
`reqwless::HttpClient`. This crate does not define request or response models,
buffer responses, manage persistent connections, stream bodies, retry
requests, or expose an HTTP backend trait.

Platform composition constructs the default Embassy-backed resources:

```rust,ignore
let http_clients = http_client::ClientFactory::new(platform.ip_stack, move || tls.config());
```

Subsystems that already own another `embedded-nal-async` TCP/DNS pair use the
same production API:

```rust,ignore
let http_clients = http_client::ClientFactory::from_network(&tcp, &dns);
let (mut client, tls_configured) = http_clients.create();
```

Protocol-specific policy belongs to its consumer. The Agent model API owns its
persistent connection and streaming behavior; the IMessage Gateway owns its
ordinary request and multipart helpers.
