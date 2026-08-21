---
name: usage-doc
description: Write or edit usage guides (usage.md) for Event Router and its crates. Use when creating, reviewing, or revising a how-to guide for callers.
---

# Usage Guides

Usage guides are how-tos for callers. They tell the reader what to write and
what happens, in plain positive sentences.

## Voice

- Write like a human how-to. Reward-style phrasing — "why not", "we do not",
  "without X", "X is rejected" — is a code smell in this doc type; replace it
  with a positive statement of what to do.
- State what the API does and what the reader writes. Use negation only for a
  real boundary, and give the reason.
- Describe outcomes in conceptual terms. Keep `include_str!`, macro internals,
  and crate names out of prose where possible.
- Code blocks carry the primary instruction. Keep them minimal, complete, and
  matching the real public API.

## Structure

Quick start first, then one numbered section per caller action (define a DTO,
declare a method, register a component, call RPCs, emit events and load
workflows, bake schemas), then an Examples list and the cardinality rules.
Adapt to the API rather than forcing the template.

## Keep in sync

- Snippets compile against the public API; update the guide in the same change
  that changes the API.
- The Examples list names what actually exists; add and remove entries as
  examples land.
- Link the design docs for rationale and the examples for runnable versions.

In-repo voice: `core/event-router/docs/usage.md`. The `design-doc` skill holds
the shared "positive, no why-vs-why-not" writing rules.
