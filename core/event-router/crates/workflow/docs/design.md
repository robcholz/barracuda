# Workflow Design

This document covers the `barracuda-workflow` crate: durable definitions,
event ingress, links, and the unified frame-flow model. It is written for
maintainers; callers should read the Event Router usage guide.

## Model

A Workflow is an ordered control-flow block started by an Event. The first RPC
actually executed is the **ingress**: the Event payload becomes its request.
Every later RPC step is fed by the most recently executed RPC's response
through its **link**. Conditional nodes choose the actual execution path but do
not themselves produce an output. A block ends through natural completion or
an explicit successful `return`. Workflows are loaded
through the durable `workflow.load` control RPC or restored from disk at
startup; a single `/system/workflows.json` catalog is authoritative on restart.
The catalog is a JSON array whose objects are ordered Workflow definitions;
load appends one object and unload removes one object through an atomic file
replacement.

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

Reference grammar: `$event.input.<field>` selects the triggering Event and
`$previous.output.<field>` selects the most recently executed RPC response on
the actual path. An `if` node does not update `$previous`: an empty selected arm
therefore leaves the last executed RPC as previous without any special merge or
inheritance rule.
`$previous.input`, `$previous.error`, and absolute step selectors are reserved.
The field is mandatory and a single top-level JSON name (the serde name); a
whole-document source reference is rejected with a hint to use a `Direct` link,
and nested paths are rejected.

## Successful return

`{"return":{}}` is a block terminal that completes the current Workflow
execution successfully. It invokes no RPC, produces no failure, and no step may
follow it in the same block. A return-only Workflow is valid and can explicitly
consume a matched Event without invoking an application endpoint.

## Conditional execution

An `if` operation selects `then` or `else` from a top-level boolean field in
`$event.input.<field>` or `$previous.output.<field>`. The selected block may be
empty, may contain nested conditionals, and may complete normally into the
operations after the `if`. A `return` inside either arm terminates the entire
Workflow successfully.

Conditions and post-branch links are dynamic. Workflow loading validates the
document structure, reference grammar, and every declared RPC address, but it
does not require branch output schemas to agree or attempt to select a merged
schema. At execution time a missing or non-boolean condition fails that
execution, and each invoked RPC reports incompatible actual request data in the
normal way.

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

Every execution resolves each step's method projection before any downstream
RPC is invoked. Linear Workflows also validate every link before execution:

- `Direct`: response type identity (source response and destination request
  are the same fixed-layout type).
- `Literal`/`Mapping`: target must be runtime-dynamic; mapping references must
  resolve on both sides with `source_size <= dest_size`.
- Cardinality: `Streaming → Unary` rejected for every link kind.

Validation runs at two points:

- **Load time (primary gate).** `workflow.load` resolves every step against the
  registry. Linear Workflows also validate all links, including literal request
  shape. Conditional Workflows deliberately defer value compatibility to the
  actual execution path and do not perform branch-schema merging. An unknown
  method is rejected before persistence.
- **Execution setup (backstop).** Every matched execution resolves all methods
  again before any downstream RPC, covering startup restore (no registry at
  restore time) and dynamic registration changes. Linear definitions also
  repeat link validation; conditional definitions defer data compatibility to
  the invoked path.

Load-time validation ordering constraint: a workflow can only reference
methods registered before it is loaded.

## Validation invariants

- Link classification (`Direct` / `Literal` / `Mapping`), `$previous.output`
  grammar, wire-to-wire field mapping.
- One frame-flow driver for every link kind: read → per-frame transform →
  write, source EOF closes the destination (u→u, u→s, and record-preserving
  s→s).
- Linear link validation at execution setup (Direct type identity; mapping
  field existence and `source_size <= dest_size`; literal arguments dry-run).
- Conditional execution selects only the runtime path, keeps `$previous` bound
  to the most recently completed RPC, and defers path-dependent request and
  condition compatibility to execution.
- Cardinality validation: `Streaming → Unary` rejected for every link kind
  (`RpcMethodInfo::input_mode` / `output_mode`).
- Load-time validation as the primary gate (`workflow.load` resolves every
  declared method and validates linear links, while conditional data flow stays
  dynamic); execution setup re-resolves methods as the backstop.
