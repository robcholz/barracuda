---
name: create-bugfix-pr
description: Review, prepare, create, or update a Barracuda bug-fix pull request with evidence for the symptom, root cause, corrected behavior, and regression coverage. Use when a PR primarily corrects faulty or regressed behavior; use create-pr for features, refactors, or documentation-only changes.
---

# Create a Barracuda Bug-Fix Pull Request

Use this skill when the primary outcome is correcting existing behavior. The PR
must let a maintainer connect the reported symptom to its trigger, root cause,
fix, and regression evidence without reconstructing that chain from the diff.

## Establish the fix

- Read the repository instructions and inspect the complete diff against the
  intended base. New Barracuda PRs target `master` unless the user names
  another base.
- Characterize the narrowest reproducible trigger, expected behavior, and
  actual behavior before describing the fix. Do not turn an unconfirmed
  hypothesis into a root-cause claim.
- Identify the exact faulty control flow, state transition, ownership boundary,
  or invariant. Distinguish that cause from the visible symptom.
- Add focused regression coverage that fails for the original defect and passes
  with the fix when practical. If the failure cannot run locally, state what
  evidence reproduced it and what substitute verification was performed.
- Review adjacent branches and failure paths that share the corrected logic.
  Keep unrelated cleanup out of the bug-fix PR.
- Do not run the full workspace test suite locally. Run focused tests and checks
  for affected packages and leave the complete matrix to CI.

Creating or updating a PR is an external mutation. Do it only when the user
asked to publish the work, and never merge the PR unless separately requested.

## Title

Use a concise conventional-commit-style title beginning with `fix`, for example:

```text
fix(workflow): retain registrations during plugin reload
```

Name the corrected behavior rather than the implementation technique. Include
an issue identifier only when one is already associated with the fix.

## Body

Use these six sections in this order.

```markdown
## What's Changed

<The faulty observable behavior that is now corrected, its trigger or affected
surface, and the scope of the fix.>

## Root Cause

<The confirmed mechanism that produced the bug and why the existing code or
coverage did not prevent it.>

## Behavior and Example

### Before the Fix

**Behavior:** <The previous caller-visible failure, including the trigger and
observable result.>

**Key code:** <The smallest relevant excerpt from before the fix that caused
the failure.>

### After the Fix

**Behavior:** <The corrected caller-visible behavior under the same trigger.>

**Key code:** <The smallest relevant excerpt from the final code that enforces
the corrected behavior.>

### After-Fix Example

<A realistic example using the final code, including the observable result.>

## Code Review

<The reviewed surfaces and invariants, findings fixed before publication, and
remaining risks or unresolved findings.>

## Impact

<Affected callers or deployments, compatibility and migration consequences,
data or security exposure, rollout risk, and important unchanged boundaries.>

## Verification

<The regression evidence and exact focused commands with their results; state
that the full matrix is left to CI.>
```

### What's Changed

- Lead with the corrected externally observable behavior, not the patch
  mechanics.
- State the conditions that triggered the defect and the affected surface.
- Bound the fix explicitly when similar-looking cases remain unsupported or
  intentionally unchanged.
- Link an issue or incident only when the source already exists; do not invent
  identifiers or imply production impact without evidence.

### Root Cause

- Explain the causal chain from input or state to the incorrect result.
- Name the violated invariant and the code boundary that should have enforced
  it.
- Separate contributing conditions from the root cause. Avoid restating the
  symptom or merely saying that a check was missing.
- Explain a coverage gap only when the diff, test history, or other reviewed
  evidence supports it.

### Behavior and Example

- Use `Before the Fix`, `After the Fix`, and `After-Fix Example` in that order.
- Compare the same trigger and observation point before and after the fix.
- In both behavior subsections, state the observable result first and then show
  the smallest relevant code excerpt from the corresponding side of the diff.
  Do not use pseudocode.
- In `After-Fix Example`, use the final API or workflow and include the result
  that demonstrates the bug no longer occurs.
- Keep implementation excerpts distinct from the caller-facing example unless
  the corrected contract is itself an implementation API.

### Code Review

- State which changed and adjacent surfaces were inspected and which invariants
  were checked.
- Include relevant error paths, boundary values, state transitions, lifecycle
  behavior, and concurrency or cancellation behavior when applicable.
- List material findings fixed during review, the failures they could have
  caused, and the coverage added for them.
- Report remaining risks honestly. Say that no blocking findings remain only
  after self-review and focused verification; do not invent an independent
  review.

### Impact

- Identify affected callers, versions, configurations, platforms, or persisted
  states when known.
- State compatibility, migration, rollout, rollback, performance, security, and
  data-integrity consequences that materially apply.
- Call out preserved boundaries that reduce risk, such as unchanged schemas,
  public APIs, storage paths, or ownership authority.
- Do not speculate about incident scope or claim zero risk without evidence.

### Verification

- Name the regression test or reproduction that covers the original trigger
  and state the observed result.
- List exact commands that actually ran and whether each passed. Distinguish
  tests, checks, formatting, linting, manual reproduction, and CI-only coverage.
- When practical, report that the regression test failed against the pre-fix
  code and passed after the fix. Do not claim this comparison unless it ran.
- If automated regression coverage is impractical, explain why and record the
  strongest repeatable substitute. Never claim the full suite passed when it
  was not run.

## Publish and verify

Push a fresh `codex/<topic>` head branch, then create the PR with an explicit
base and head. Use `gh pr create --base master --head <branch>` unless the user
selected another base. Read the created PR back with `gh pr view` and verify:

- the base and head branches are correct;
- the title begins with `fix` and describes the corrected behavior;
- all six body sections are present and supported by the reviewed diff;
- reproduction and verification claims match commands or evidence that
  actually ran.

When updating an existing bug-fix PR, inspect its current body first and
preserve useful human-authored reproduction or incident context while bringing
it into this structure.
