# IMessage Gateway Design

## Boundary

`IMessageGateway` and `MessageChannel` are typed Plugin-to-Plugin capabilities.
They preserve provider registration, direct provider dispatch, and live Rust
streams. Agent/Workflow operations are JSON contracts on Event Router lanes.

## Complete messages

`gateway.send` is unary JSON request to unary JSON response. It is the natural
operation for one already-complete text message. The complete encoded JSON
document, rather than an independent text-field limit, must fit one RPC lane.

## Application streams

`gateway.send_stream` is a unary JSON event-ingress API backed by one live typed
provider stream per active Agent turn:

```text
turn_started -> semantic events -> turn_ended
```

Every call contains the route and one complete `session.event` document. The
Agent session is the correlation key and Agent sequence is the only order. The
Gateway understands only `turn_started` and `turn_ended` as stream boundaries;
it forwards those records and every record between them without interpreting
their payloads. A provider chooses which semantic event types it consumes.

Every accepted event returns `accepted_sequence`. Events are rejected when the
stream is absent, duplicated, out of order, changes route, or its bounded queue
is full. A `busy` response is backpressure: the caller yields and retries the
same event. Accepting `turn_ended` closes input; provider completion is reported
by one terminal Event rather than holding an RPC lane. The complete request,
including route and payload, must fit one 512-byte RPC lane.

`gateway.send_media` retains its explicit `start -> chunk -> finish` state
machine. Media is Base64 only because JSON cannot carry raw binary.

Four event-stream workers and four media workers match the four retained stream slots;
all accepted streams can therefore make progress concurrently.

## Inbound messages

Each normalized provider message produces exactly one
`gateway.message.received` JSON Event containing its route, provider message ID,
and complete text. Provider ingress validates the fully encoded document against
the available Event input capacity before queueing it.

This keeps inbound routing atomic for Workflow consumers. Its cost is an
explicit size boundary: a message whose complete Event does not fit one lane is
rejected at provider ingress.
