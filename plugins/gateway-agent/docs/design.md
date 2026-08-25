# Gateway-Agent Design

## Purpose

Bridge provider-neutral Gateway ingress to the Agent while preserving live
primary text, reasoning, effects, tool results, notices, and lifecycle events.
Keep orchestration state in one mapper rather than exposing internal session
operations as Workflow topology.

## Architecture

```text
Gateway Event stream
  -> gateway_agent.respond
       route -> persistent session registry
       session.new/open/append (internal RPCs)
       Agent event -> Gateway frame mapping
  -> gateway.send_stream
  -> selected MessageChannel
```

The Workflow edge is direct and typed: the mapper response type is identical to
the `gateway.send_stream` request type. Both sides are streaming, so Event
Router forwards frames with bounded backpressure.

## Invariants

- The public Workflow always has exactly two calls.
- One `GatewayRoute` maps to one persistent Agent session.
- Only one active turn may consume a route's `session.open` stream.
- Route metadata precedes content frames.
- Text is primary content. Reasoning, effects, notices, tool structures, and
  generic events are extra content.
- The mapper never collapses a tool result to a status string and never
  aggregates a full response before delivery.
- Provider capability decides presentation; the mapper does not branch on
  provider type.

## Failure boundaries

Malformed ingress and Agent/session failures are mapper method errors. Once
Gateway streaming begins, provider or transport failures belong to
`gateway.send_stream`. Dropping a mapper output stream releases its per-route
busy flag.

## Verification

`tests/workflow_e2e.rs` runs a real Agent Component, Gateway Component, mapper,
Workflow runtime, and recording provider. It asserts the two-step definition,
route correlation, reply correlation, and live reasoning/text/event delivery.
