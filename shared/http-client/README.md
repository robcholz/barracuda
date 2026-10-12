# http-client

The thin boundary between Embassy networking and `reqwless`. It is a library:
the Plugin that owns HTTP builds the factory and hands clients to others.

`ClientFactory` owns or borrows a TCP connector, DNS resolver, and optional
`barracuda_tls::Tls`. Calling `ClientFactory::create()` creates a raw
`reqwless::HttpClient`. This crate does not define request or response models,
buffer responses, manage persistent connections, stream bodies, retry
requests, or expose an HTTP backend trait.

The owner constructs the default Embassy-backed resources:

```rust,ignore
let tls = barracuda_tls::Tls::new(entropy).ok();
let http_clients = http_client::ClientFactory::new(ip_stack, tls);
```

Subsystems that already own another `embedded-nal-async` TCP/DNS pair use the
same production API:

```rust,ignore
let http_clients = http_client::ClientFactory::from_network(&tcp, &dns);
let (mut client, tls_configured) = http_clients.create();
```

Protocol-specific policy, such as a persistent connection, streaming, or
multipart bodies, belongs to the consumer that needs it.

## Receive slots

Long-lived receive loops use a separate pool so they never take a connection
from the request pool. The slots' socket buffers are static and sized at build
time by their owner, which declares one `ReceiveBuffers` and builds the pool
from the request factory, sharing its DNS and TLS:

```rust,ignore
static RECEIVE_BUFFERS: http_client::ReceiveBuffers<SLOTS> = http_client::ReceiveBuffers::new();
let slots = http_client::ReceiveSlots::new(stack, &http_clients, RECEIVE_BUFFERS.take());
```

An owner that needs no slots declares none and links no receive buffers. A
receive loop leases a slot while it receives:

```rust,ignore
let lease = slots.acquire().ok_or(NoSlot)?;      // or slots.acquire_when_free().await
let (mut client, _tls) = lease.client_factory().create();
// ...or a raw stream for a protocol such as WebSocket:
let mut stream = lease.connect_stream("wss://example.com/gateway").await?;
let (mut reader, mut writer) = stream.split().await?; // reader.read is cancel-safe
```

`capacity()` and `in_use()` report the limit and its use; `wait_available()`
completes when a dropped lease frees a slot. Each slot holds one connection at
a time, with 1 KiB send and 4 KiB receive buffers in static memory, TCP
keep-alive after 30 s idle, and a 60 s dead-peer timeout. Dropping a
connection resets it.
