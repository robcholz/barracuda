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

## Event matching

Every Event has a stable type-level Event ID and may carry one concrete
`Topic`. A Workflow always matches its `match.event` glob. When
`match.topic` is present, the Event must also carry that exact Topic; when it
is absent, the Workflow accepts the Event regardless of whether the producer
supplied a Topic. Every matching Workflow receives the Event.

Topics contain 1–16 ASCII letters, digits, `_`, `-`, or `.`. The same concrete
Topic type is used by the Event and Workflow definition, so a configured Topic
is always an exact selector. Omitting `match.topic` selects every Topic for the
matched Event ID.

`internal.emit` remains a single streaming ingress RPC. Its frame order is:

1. required Event header;
2. optional fixed Topic metadata frame;
3. one or more opaque payload frames per Event message.

The receiver validates and removes Topic metadata before forwarding payload
bytes to matched Workflow ingress steps.

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

Frame flow is one layer; the link kind (the per-frame transform) is another
layer on top of it. The driver reads every response frame of the source
step, applies the link's transform, and writes the produced request frames to
the destination step, with source EOF closing the destination — for every
link kind and every valid cardinality combination. Cardinality rules are
enforced by link validation, not by the driver.

| prev Output → this Input | Behavior |
| --- | --- |
| `Unary → Unary` | Source emits 1 frame → transform → write 1 frame → EOF |
| `Unary → Streaming` | Same; destination sees a stream of length 1 |
| `Streaming → Streaming` | Loop: read frame → transform → write; source EOF propagates to destination EOF (record-preserving, backpressure via lane reservation) |
| `Streaming → Unary` | **Invalid** — rejected at link validation for every link kind |

The **transform** is the link's per-frame function:

- `Direct`: identity — bytes copied verbatim (frame sizes must match).
- `Mapping`: encode the literal arguments into the request frame, then patch
  each `$previous.output.<field>` reference wire-to-wire.
- `Literal`: encode the arguments; the transform is constant (identical for
  every frame), so a streaming source produces one identical request per
  frame.

Field copies are byte-level over opaque regions; the workflow author owns the
encoding contract of the types involved, including any length relationship
between copied regions (a source may be narrower than its destination, whose
tail keeps its prior value).

### Why `Streaming → Unary` is invalid

A unary consumer reads exactly one frame and does not check for extras;
feeding it a stream would leave surplus frames unconsumed, stalling or
corrupting the pipeline. There is deliberately no implicit reduction: a
consumer that needs one value must be fed by a producer that emits one.

## Validation

Every execution resolves each step's method projection and validates every
link before any downstream RPC is invoked:

- `Direct`: response type identity (source response and destination request
  are the same fixed-layout type).
- `Literal`/`Mapping`: target must be runtime-dynamic; mapping references must
  resolve on both sides with `source_size <= dest_size`.
- Cardinality: `Streaming → Unary` rejected for every link kind.

Validation runs at two points with the same rule set:

- **Load time (primary gate).** `workflow.load` resolves every step against the
  registry and validates all links — including a dry-run of the literal
  arguments through the destination's JSON codec — before anything is
  persisted. A workflow whose steps cannot be resolved or linked is rejected
  with `UnknownMethod` / `InvalidLink` and never enters the catalog.
- **Execution setup (backstop).** Every matched execution re-validates against
  the current registry before any downstream RPC, covering startup restore
  (no registry at restore time) and dynamic registration changes.

Load-time validation ordering constraint: a workflow can only reference
methods registered before it is loaded.

## Status

Implemented:

- Link classification (`Direct` / `Literal` / `Mapping`), `$previous.output`
  grammar, wire-to-wire field mapping.
- One frame-flow driver for every link kind: read → per-frame transform →
  write, source EOF closes the destination (u→u, u→s, and record-preserving
  s→s).
- Link validation at execution setup (Direct type identity; mapping field
  existence and `source_size <= dest_size`; literal arguments dry-run).
- Cardinality validation: `Streaming → Unary` rejected for every link kind
  (`RpcMethodInfo::input_mode` / `output_mode`).
- Load-time validation as the primary gate (`workflow.load` resolves steps
  against the registry, validates links and literal arguments, and rejects
  with `UnknownMethod` / `InvalidLink` before persistence); execution setup
  re-validates as the backstop.
