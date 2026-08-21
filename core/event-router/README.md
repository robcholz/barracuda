# Barracuda Event Router

The Event Router composes **Components** (RPC and Event providers) and
**Workflows** (durable, event-driven RPC chains) on a fixed-capacity,
full-duplex, `no_std` cooperative runtime.

- [RPC design](crates/rpc/docs/design.md) — wire layer, dynamic modality,
  JSON calls, schema bake.
- [Workflow design](crates/workflow/docs/design.md) — links, the unified
  frame-flow model, ingress, persistence.
- [Usage](docs/usage.md) — how callers define DTOs and methods, register
  components, call RPCs, define and load workflows, and bake schemas.
