---
name: design-doc
description: Write and maintain subsystem design docs (design.md) in this repo, and audit a crate against its design doc. Use when creating or editing a design doc, reviewing its style or tradeoffs, or checking whether code matches its doc.
---

# Design Docs Aligned with Code

Design docs in this repo describe a subsystem's actual design for maintainers.
They are not marketing, not changelogs, and not code walkthroughs. A design
doc must read as a design and match what the code actually does.

## Style

- The "why vs why not" style — presenting every choice against its rejected
  alternative — is a reward-learning artifact, not technical writing. These
  are technical docs; write them as such.
- State the design positively. Do not define it by what it is not ("No X
  exists", "we never do Y") as the default sentence shape. Negation is allowed
  only where it is a real invariant or boundary, and then with a reason.
- Give decision + tradeoff treatment only to genuinely key choices — one or
  two per doc. For those, name the real cost in one or two sentences. Every
  other choice is a plain positive statement; do not turn every micro-decision
  into a "why vs why not" discussion.
- Do not add a Status / Implemented / "not yet implemented" section. Design
  docs describe the design; implementation state lives elsewhere.
- Describe the design, not the mechanics. Use conceptual terms ("bake the
  schema into the firmware image") instead of implementation detail
  (`include_str!`, `serde_derive_internals`, exact method signatures, crate
  names). Keep code-level detail in the code.
- Cover all real responsibilities. If the crate has behavior the doc omits
  (call context, lifecycle, constraints), the doc is incomplete even when the
  prose is good.

## Structure

A typical design doc has: scope, an architecture boundary diagram,
the model (types and registry), call paths (unified primitive + wrappers),
invariants, and one section per major subsystem. Adapt to the subject; do not
force a fixed template. Use `plugins/imessage-gateway/docs/design.md` as the
in-repo reference for voice and shape.

## Doc ↔ code alignment

When writing or reviewing, audit the crate against the doc:

1. Match the described layers to real modules. The doc's architecture should
   be recognizable in the code.
2. Find undocumented behavior. Anything real the crate does that the doc does
   not mention is a gap (e.g. nested-call context, registry lifecycle,
   multicast input constraints).
3. Find cross-crate leakage:
   - public APIs whose only purpose is another subsystem's logic;
   - wording from unrelated subsystems in docs or errors;
   - dependencies outside the crate's domain.
4. Fix leaks at the ownership boundary, not by redocumenting them. The
   subsystem's logic moves to the subsystem; the shared crate keeps only
   neutral primitives (e.g. expose type-identity accessors and let the
   workflow implement its link rule). Update callers and tests in the same
   change.
5. Keep edits minimal and reviewable: fix wording, fill gaps, lift
   implementation detail to concepts. Do not restructure the doc or code
   beyond what the alignment requires.

## Process

- Redundant docs have a predictable origin: while doing A, an unrequested B
  gets added, then removed, leaving doc comments that justify the absence
  ("why we don't do B", "no B exists"). This pattern cannot be prevented; the
  job is recognition and cleanup.
- Recognize the symptom: a comment or paragraph whose only purpose is to
  explain why something is not done. Absence needs no documentation. Delete
  the note entirely; do not keep a reworded or softened version.
- If you notice B while still doing A, stop and flag it instead of folding it
  into the result; this reduces how much cleanup is needed later.
- Align on the issue list with the user before a large edit; for small fixes,
  apply directly and show the diff.
- Follow the crate's conventions (e.g. getset over hand-written getters).
- Commit logically: docs separately from refactors.
