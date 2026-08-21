# Workflow Design

This document covers the `barracuda-workflow` crate: durable definitions,
event ingress, links, and the unified frame-flow model. It is written for
maintainers; callers should read the Event Router usage guide.

## Model

A Workflow is an ordered RPC chain started by an Event. Step 0 is the
**ingress**: the Event payload becomes its request. Every later step is fed by
the previous step's response through its **link**. Workflows are loaded through
the durable `workflow.load` control RPC or restored from disk at startup; a
persistence `index` file is authoritative on restart.

## Links

A **link** is the edge contract between two consecutive steps: how the previous
response becomes the next request. It is declared implicitly by the step's
`arguments`:

| `arguments` | Link | Meaning |
| --- | --- | --- |
| absent | `Direct` | Previous response frame passed byte-for-byte. |
| present, no `$` | `Literal` | Request built entirely from the literal arguments, independent of the previous step. |
| present, with `$` | `Mapping` | Request built from literal arguments, then each referenced field is copied wire-to-wire from the previous response. |

Reference grammar: `$previous.output.<field>`. Only `previous` / `output` are
defined today; `$previous.input`, `$previous.error`, and absolute step
selectors are reserved. The field is mandatory and a single top-level JSON
name (the serde name); a bare `$previous.output` is rejected with a hint to
use a `Direct` link, and nested paths are rejected.

## Unified frame-flow model

Frame flow is orthogonal to the link kind, to whether methods are
runtime-dynamic, and to the call modality (typed vs `call_json`): the driver
moves frames and never distinguishes how a value was produced. It is defined
by the source method's `Output` mode and the destination method's `Input`
mode:

| prev Output → this Input | Behavior |
| --- | --- |
| `Unary → Unary` | Read 1 frame → transform → write 1 frame → EOF |
| `Unary → Streaming` | Read 1 frame → transform → write 1 frame → EOF (destination sees a stream of length 1) |
| `Streaming → Streaming` | Loop: read frame → transform → write; source EOF propagates to destination EOF (record-preserving, backpressure via lane reservation) |
| `Streaming → Unary` | **Invalid** — rejected at link validation for every link kind |

The **transform** is the link's per-frame function:

- `Direct`: identity — bytes copied verbatim (frame sizes must match).
- `Mapping`: encode the literal arguments into the request frame, then patch
  each `$previous.output.<field>` reference wire-to-wire.
- `Literal`: encode the arguments once; the previous response is drained (not
  consumed) so its producer completes. The transform is constant (identical
  for every frame); today the driver applies it once per edge.

### Why `Streaming → Unary` is invalid

A unary consumer reads exactly one frame and does not check for extras;
feeding it a stream would leave surplus frames unconsumed, stalling or
corrupting the pipeline. There is deliberately no implicit reduction: a
consumer that needs one value must be fed by a producer that emits one.

## Validation

Every execution resolves each step's method projection and validates every
link before any downstream RPC is invoked:

- `Direct`: response type identity (`links_to`).
- `Literal`/`Mapping`: target must be runtime-dynamic; mapping references must
  resolve on both sides with `source_size <= dest_size`.
- Cardinality: `Streaming → Unary` rejected for every link kind.

Validation currently runs at execution setup (the closest point with a client);
load-time validation is the agreed target, with execution-time checks remaining
as a backstop for startup restore and dynamic registration changes.

## Status

Implemented:

- Link classification (`Direct` / `Literal` / `Mapping`), `$previous.output`
  grammar, wire-to-wire field mapping (unary edges).
- Link validation at execution setup (Direct type identity; mapping field
  existence and `source_size <= dest_size`).
- Cardinality validation: `Streaming → Unary` rejected for every link kind
  (`RpcMethodInfo::input_mode` / `output_mode`).

Agreed design, not yet implemented:

- Driver-level `Streaming → Streaming` loop applying the per-frame transform
  (Mapping/Literal edges are unary today).
- Setup-time "arguments match request" validation (today literal arguments
  are transcoded at runtime; a mismatch surfaces as `JsonRequestInvalid`
  during execution).
- Load-time link validation.

