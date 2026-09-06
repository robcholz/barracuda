# IMessage Gateway Components

The Plugin loads ten independently polled Event Router Components:

- `GatewayComponent` owns `gateway.send`, `gateway.send_stream`, and
  `gateway.send_media` JSON registrations.
- `GatewayInboundComponent` drains the bounded provider-ingress queue and emits
  `gateway.message.received` Events.
- Four `GatewayTextStreamComponent` workers concurrently drive the four
  accepted text-stream slots and emit their terminal Events.
- Four `GatewayMediaStreamComponent` workers do the same for binary media.

Separating these workers prevents one active provider stream from blocking
another stream or inbound Event emission. A queued chunk retains its RPC input
lane until its worker consumes it; the queue does not allocate another text or
binary payload.
