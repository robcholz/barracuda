# RPC Design

This document covers the `barracuda-rpc` crate: the call model and nested
calls, the registry lifecycle, the wire layer, the dynamic modality, JSON
calls, and the schema bake. It is written for maintainers; callers should
read the Event Router usage guide.

## Architecture

```text
Domain types (String / SessionId / ModelApiConfig)   — no serde, no wire commitment
        ↕ o2o + TryFrom (validation hand-written)
RPC DTO — the boundary contract (FixedString<N> / WireU32 / repr enums)
        ↙                       ↘                     ↘
fixed-layout bytes          JSON (serde)          JSON Schema (schemars)
typed RPC inside            dynamic call_json     agent discovery
```

The RPC DTO is the boundary contract between domain types and the wire. Each
DTO field has an explicit wire type and capacity, the layout is explicit and
padding-free, and the DTO is used directly as `RpcMethod::Request` /
`RpcMethod::Response`. Three encodings are derived from one DTO: fixed-layout
bytes via `zerocopy`, JSON via `serde`, and JSON Schema via `schemars`, so the
JSON surface and the wire layout are two views of the same type and cannot
drift. The cost of one DTO serving all encodings is that it must satisfy both
constraints at once: every field is a bounded fixed-layout type
(`FixedString<N>`, `WireU32`), so capacity and layout are decided explicitly
at the boundary rather than discovered at runtime.

Domain types are wire-agnostic: they carry no serde derives and no wire
layout. The conversion between domain types and DTOs lives outside this crate
(adapter code with `o2o`/`TryFrom`).

## Model

`RpcMethod` declares an address, the `Request`/`Response`/`Error` message
types, and the request/response cardinality markers (`Unary` or `Streaming`).
Messages are `RpcMessage`s: fixed-layout Zerocopy types. Endpoints live in a
task-local registry over fixed-capacity full-duplex lanes; the registry is
shared with clients and does not require handlers or futures to be `Send`.

The `RpcMethodDescriptor` is built at registration and retained with the
endpoint: address, frame sizes, type IDs, and the input/output cardinality
types. It is the runtime record of the method's layout and the basis of
signature and cardinality queries.

## Call context

Every invocation carries an `RpcContext` into its handler: a call ID, the
root call ID shared by the whole nested chain, the immediate parent call ID,
the caller endpoint (when the call is nested), and a client bound to the
current endpoint. Handlers use that client to start nested calls, and the
lineage identifies where each call sits in the chain.

Lane acquisition distinguishes root from nested calls. A root call waits
until the required lanes are free; a nested call fails with
`NestedLaneExhausted` instead of waiting, so a handler that holds every lane
cannot deadlock on its own child call. A handler calling itself directly is
rejected (`DirectSelfCall`).

## Call paths

The client exposes one unified wire-level call; the typed and JSON entry
points are wrappers over it. The layers, bottom to top:

```text
call::<M> / multicast<T, Mode>     ── zerocopy typed wrappers
call_json                          ── JSON wrapper (serde at the boundary)
                  │
                  ▼
call_payload / multicast_payload   ── the unified wire call
                  │
                  ▼
fixed-layout frames on full-duplex lanes   ── the frame convention
```

### Frame convention

Every RPC message is a fixed-layout frame on a full-duplex lane. Fixed layouts
keep frame IO zero-copy and allocation-free, which matters on `no_std`
firmware; the tradeoff is that changing a field's size or name is a
coordinated change on both ends, so wire shapes evolve deliberately. The
request direction is a stream of request frames, closed by the writer. The
response direction is a stream of response frames, optionally terminated by
one method-error frame; a terminal method error ends the response stream.
Cardinality (`Unary`/`Streaming`) is declared by the method's `Input`/`Output`
markers and governs how many frames the typed layer exchanges; the wire layer
itself does not enforce it. The outer `RpcResult` reports transport/runtime
failures; the inner typed `Result<Response, Error>` reports business outcomes
carried by the method-error frame.

### Wire call — the shared primitive

`RpcClient::call_payload(address)` returns `(RpcPayloadWriter,
RpcPayloadReader)`: asynchronous full-duplex frame IO over the lane. The
caller writes request frames and reads response/method-error frames; frames
remain borrowed from the lane until dropped. There is no compile-time
signature or cardinality checking — each written frame must match the
registered method's request wire layout.

`RpcClient::multicast_payload(addresses)` is the same call fanned out: one
shared request direction written by the caller, one independent
`RpcMulticastBranch` per target for responses and errors.

Both `call::<M>` and `call_json` are built on this primitive.

### Typed wrapper — native Rust

`RpcClient::call::<M>()` selects the method's input and output shape through
`M`; callers do not choose between separate unary and streaming entry
points. Request encoding and response decoding are zerocopy (`as_bytes` in,
`RpcFrame<T>` views out). It returns a self-driving `RpcUnaryCall` (unary) or
`RpcStream` (streaming) that advances request encoding, handler execution,
and response decoding together.

`RpcClient::multicast<T, Mode>()` encodes one typed request stream shared by
every target; every target must register the same request type and request
cardinality (`MulticastInputMismatch` otherwise). Responses stay wire-level
because multicast targets may declare different response/error/output
contracts.

### JSON wrapper

`RpcClient::call_json(address, &Value)` transcodes at the call boundary only.
The captured `JsonCodec` deserializes the JSON value into the method's
`Request` struct and takes its byte image; those exact bytes go over the
lane through `call_payload`, so the handler serves an ordinary typed frame
identical to a typed call. The single response or method-error frame is then
transcoded back to JSON. See the dynamic modality section for the envelope.

## Registry and endpoint lifecycle

Endpoints are registered through the object-safe `RpcRegistryApi`, which keeps
the frame capacity `M` on the trait so typed registration retains its
compile-time checks. Registration returns an `RpcRegistration` token that
owns the address: `unregister` removes the endpoint; `revoke` also prevents
in-flight prepared calls from polling the handler, which reports
`EndpointRevoked`. Using a token that no longer owns the address is rejected
(`StaleRegistration`).

Registration enforces the lane contract at compile time: the fixed request,
response, and error messages must fit the lane frame capacity and align with
the lane frames. The registry exposes sorted snapshots of groups and
addresses (`groups`, `rpcs`) for discovery.

## Invariants

- **Frames are the only thing that moves.** Every RPC message is a
  fixed-layout Zerocopy type; JSON is a modality layered on top and never
  travels over a lane.
- **The type is the single source of truth.** `serde` defines semantics,
  `zerocopy` defines bytes, and derives generate wire metadata. No
  hand-written layout tables.
- **One frame-flow model.** Frame flow between calls is defined by the
  methods' cardinality modes, independent of whether a method is
  "runtime-dynamic" and of the calling wrapper (typed, wire, or JSON).
- **Wire names equal JSON names.** A field's wire name is the JSON key
  `serde_json` itself computes, enforced at compile time, so wire-level field
  access and `call_json` always agree on names.

## Wire layer

`#[derive(RpcWire)]` on a message struct emits a compile-time table mapping
each field's **serde JSON name** to its byte region (`offset_of!` /
`size_of!`). Names come from serde's own naming derivation, so
`#[serde(rename)]` / `#[serde(rename_all)]` / `#[serde(skip)]` are honored
automatically, and a field whose serialize and deserialize names differ is a
compile error: the table can never disagree with the JSON surface. Structs
only; `()` exposes an empty table.

`WireSupport` bundles a method's request-write and response-read tables:

- `read_response_field(frame, name)` — the field's full byte region as a
  zero-copy borrow.
- `write_request_field(frame, name, content)` — bounded copy;
  `content.len() <= field size`; the remaining bytes of the region are left
  untouched (no implicit zero-padding).
- `request_field_size` / `response_field_size` — lookup for planning.

The framework treats every region as opaque bytes; it never interprets a
field's contents. Length prefixes, encodings, and endianness are the
caller's responsibility, including the relationship between copied regions:
a bounded copy may be shorter than its destination, whose remaining bytes
keep their prior value.

## Dynamic modality

`#[rpc_dynamic]` marks an `impl RpcMethod` block as runtime-dynamic and
bundles three capabilities into one `Dynamic` captured beside the endpoint at
registration:

1. `JsonCodec` — JSON ↔ fixed-layout frame transcoding (`call_json`).
2. `WireSupport` — field-level wire access.
3. `schema` — the baked request JSON Schema, when the schema pipeline ran.

`RpcMethod::dynamic() -> Option<Dynamic>` is the single accessor; wire
support always comes with JSON support, so they are never configured
separately.

Being "dynamic" changes the modality (the method can be addressed by JSON
field names); it does not define frame flow or cardinality. Every method's
`Input`/`Output` modes are descriptor metadata regardless of `#[rpc_dynamic]`.

### call_json

`call_json` transcodes the JSON value into the method's request struct and
moves the struct's byte image over the lane — the same fixed-layout bytes a
typed call would send. Response and error frames are viewed in place and
transcoded back to JSON. A runtime address cannot recover a Rust type, so
the transcoder is captured while the concrete method type is still known and
stored beside the endpoint; transcoding uses the two hardened derives
(`serde` for values, `zerocopy` for bytes), not reflection over field
offsets. Results use an envelope:

```json
{ "ok": true,  "value": { ... } }     // response frame
{ "ok": false, "error": "Denied" }    // terminal method-error frame
```

A method with no `Dynamic` is rejected with `NotJsonCallable`; a request
that fails serde validation reports `JsonRequestInvalid` with the message
type and serde detail.

## JSON Schema bake

Schemas are generated on the host at build time, not at runtime: `schemars`
constructs schemas at runtime (not const) and the firmware is `no_std`. The
wire table is `const` from the derive, but schema generation is an algorithm
that must run once, so it runs on the build machine and the result is
embedded. The pipeline (`barracuda-rpc-schema`) runs from a `build.rs`:

1. Each request type opts in with `register!(Type)` into an `inventory`
   collection.
2. `bake_all(OUT_DIR)` walks the collection and writes one `<Type>.json` per
   registered DTO.
3. `#[rpc_dynamic]` bakes the file into the firmware image at build time.

The bake uses LLM-friendly settings, because the schemas are for agents:
inline subschemas (no `$ref`/`$defs`), nullable type arrays for `Option`, and
no meta-`$schema`.
