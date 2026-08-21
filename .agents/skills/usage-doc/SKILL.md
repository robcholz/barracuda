---
name: usage-doc
description: Write or edit usage guides (usage.md) for any crate in this repo. Use when creating, reviewing, or revising a caller-facing how-to guide.
---

# Usage Guides

Usage guides are how-tos for a crate's callers. They tell the reader what to
write and what happens, in plain positive sentences.

## Voice

- Write like a human how-to. Reward-style phrasing — "why not", "we do not",
  "without X", "X is rejected" — is a code smell in this doc type; replace it
  with a positive statement of what to do.
- State what the API does and what the reader writes. Use negation only for a
  real boundary, and give the reason.
- Describe outcomes in conceptual terms. Keep implementation internals and
  crate-internal names out of prose where possible.
- Code blocks carry the primary instruction. Keep them minimal, complete, and
  matching the real public API.

## Structure

Quick start first, then one numbered section per caller action, then an
Examples list and any constraints. Adapt to the crate rather than forcing a
template. `core/event-router/docs/usage.md` is the in-repo reference for voice
and shape.

## Keep in sync

- Snippets compile against the public API; update the guide in the same change
  that changes the API.
- The Examples list names what actually exists; add and remove entries as
  examples land.
- Link the design docs for rationale and the examples for runnable versions.
- Follow [.agents/docs/codestyle.md](.agents/docs/codestyle.md) for API surface
  references.

In-repo voice: `core/event-router/docs/usage.md`. The `design-doc` skill holds
the shared "positive, no why-vs-why-not" writing rules.
