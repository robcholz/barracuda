# IMessage Gateway Components

The Plugin loads ten independently polled Event Router Components:

- `GatewayComponent` owns `gateway.send`, `gateway.send_stream`, and
  `gateway.send_media` JSON registrations.
- `GatewayInboundComponent` drains the bounded provider-ingress queue and emits
  one complete `gateway.message.received` Event per accepted message.
- Four `GatewayEventStreamComponent` workers concurrently drive the four
  accepted semantic-event stream slots and emit their terminal Events.
- Four `GatewayMediaStreamComponent` workers do the same for binary media.

Separating these workers prevents one active provider stream from blocking
another stream or inbound Event emission. Semantic event payloads and decoded
media chunks use bounded inline stream storage after the RPC call is accepted.
