# IMessage Gateway Design

## Purpose

Provide one provider-neutral messaging boundary. Callers select a route and a
delivery shape; providers decide how much of the shape they can present.

## Outbound APIs

| RPC | Cardinality | Dynamic | Use |
| --- | --- | --- | --- |
| `gateway.send` | unary -> unary | yes | simple complete text and JSON/dynamic callers |
| `gateway.send_stream` | streaming -> unary | no | live text plus typed extra frames |
| `gateway.send_media` | streaming -> unary | no | binary media |

`gateway.send_stream` frames contain one `field` identifying their content.
Route fields form a finite prefix. Content fields are `Text`, `Reasoning`,
`EffectResult`, `Notice`, `Event`, and the structured tool-result fields. Each
content value also carries a `More` or `Complete` boundary on the provider
model; wire enum variants encode both properties in one fixed-layout field.

## Provider projection

The default `MessageChannel::send_stream` implementation consumes every frame
and forwards only `Text` through `send_message`. This is the required behavior
for plain IM providers. Rich providers override `send_stream`; Web publishes
primary text as `message.delta` and extras as `message.extra`.

## Streaming invariant

The RPC handler reads only the route prefix before invoking the provider. It
then maps the remaining RPC input directly into the provider stream. It never
collects the full content stream, so ordering, bounded backpressure, and early
provider rendering are preserved.

## Ownership

Gateway owns routes, generic content fields, provider registration, and
delivery errors. Agent-specific events are mapped before entering this
boundary. Workflows and provider-specific SDK types do not belong here.
