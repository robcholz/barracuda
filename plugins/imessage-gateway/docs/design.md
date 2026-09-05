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

`gateway.send_stream` and `gateway.send_media` are unary JSON command APIs.
Each logical stream is a state machine:

```text
start(sequence=0) -> chunk(sequence=1..n) -> finish(sequence=n+1)
```

Every accepted command returns `accepted_sequence`. Commands are rejected when the
stream is absent, duplicated, out of order, or its bounded queue is full. A
`busy` response is backpressure: the caller yields and retries the same
sequence. `finish` acceptance closes input; provider completion is reported by
one terminal Event rather than holding an RPC lane.

The adapter turns accepted chunks into the existing typed provider stream as
they arrive. It never gathers a full text or binary body. A queued command owns
the original `JsonRef`, so the Event Router lane is the queue buffer. When a
worker consumes it, text and decoded media use inline stream items rather than
allocating per chunk. Media is Base64 only because JSON cannot carry raw binary.

Four text workers and four media workers match the four retained stream slots;
all accepted streams can therefore make progress concurrently.

## Inbound messages

Each normalized provider message produces exactly one
`gateway.message.received` JSON Event containing its route, provider message ID,
and complete text. Provider ingress validates the fully encoded document against
the available Event input capacity before queueing it.

This keeps inbound routing atomic for Workflow consumers. Its cost is an
explicit size boundary: a message whose complete Event does not fit one lane is
rejected at provider ingress.
