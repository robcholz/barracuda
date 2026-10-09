# IMessage Gateway Design

## Scope

IMessage Gateway normalizes provider registration, inbound messages, complete
text delivery, semantic Agent streams, and binary media streams. One typed
`IMessageGateway` capability is the boundary shared by providers and adapter
Plugins.

```text
channel providers ── register/publish ──┐
                                       v
Agent tools ── typed request ──> IMessageGateway <── typed request ── Workflow Actions
                                       |
                                       v
                              registered MessageChannel

provider ingress/stream completion ──> WorkflowService ──> Workflow matching
```

## Capability and adapters

`IMessageGateway` owns the provider registry and delegates outbound operations
to one shared runtime. `agent-imessage-gateway` translates native tool calls
into these typed operations. The Gateway Plugin independently registers the same
operations as `gateway.send`, `gateway.send_stream`, and `gateway.send_media`
Workflow Actions with static request and response schemas.

Provider Plugins require only `IMessageGateway`. Their registration guard
defines channel ownership, and dropping the guard unregisters the channel.

## Inbound messages

Providers publish a complete normalized message containing route, provider
message ID, and text. The Gateway validates required routing identity and emits
one `gateway.message.received` value through Workflow matching.

## Outbound streams

Semantic text streams use the Agent session and sequence as their identity and
ordering contract. `turn_started` reserves a worker, intermediate events enter
a bounded per-stream queue, and `turn_ended` or `stream_error` closes it.

Media streams use explicit `start`, `chunk`, and `finish` commands with a
caller-owned stream ID and sequence. Base64 chunks are decoded before they
enter the provider-facing stream.

The runtime owns four text workers and four media workers. This bounds active
provider work and lets independent streams progress concurrently. Queue
backpressure returns the stable `busy` business error.

## Completion and errors

Provider completion emits `gateway.send_stream.finished` or
`gateway.send_media.finished` with the original correlation key and terminal
sequence. Business failures use stable serializable error codes in both Action
responses and terminal Events. Schema, decode, or serialization failures stay
at the adapter boundary.

## Provider HTTP

Providers make their HTTP calls through the Gateway's `send` helper, which opens
a fresh connection per request and buffers the response. Each exchange has a
30 s deadline, matching the `http` Plugin's request deadline; a missed deadline
drops the connection and returns `Error::Timeout`, which providers report as a
`Transport` channel error. A body of unknown length is a caller-fed media
stream, so time spent waiting for its next chunk does not count: connecting,
each write, and the response after the last chunk are each bounded by 30 s.
