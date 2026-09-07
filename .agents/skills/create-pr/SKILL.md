---
name: create-pr
description: Review, prepare, create, or update a Barracuda pull request with the repository's required PR body style. Use when opening a PR, drafting its title or body, or revising an existing PR description.
---

# Create a Barracuda Pull Request

Open a PR only after reviewing the complete diff against its intended base.
The PR body is a maintainer-facing explanation of observable behavior,
review findings, and operational impact; it is not a commit log.

## Prepare the branch

- Read the repository instructions before reviewing or publishing changes.
- Confirm the intended base and head. New Barracuda PRs target `master` unless
  the user explicitly names another base.
- Do not reuse the branch of a merged or closed PR for new work. Create a fresh
  `codex/<topic>` branch and verify its merge base with the intended base.
- Inspect the complete `base...HEAD` diff, not only the last commit or working
  tree. Keep commits atomic and exclude unrelated changes.
- Perform a real self-review before pushing. Check behavior, API boundaries,
  failure semantics, compatibility, tests, and documentation relevant to the
  change. Fix findings and repeat focused verification before opening the PR.
- Do not run the full workspace test suite locally. Run focused tests and
  checks for affected packages and leave the complete matrix to CI.

Creating or updating a PR is an external mutation. Do it only when the user
asked to publish the work, and never merge the PR unless separately requested.

## Title

Use a concise conventional-commit-style title that describes the PR's primary
outcome, for example:

```text
refactor(plugin): add semantic filesystem mount namespaces
```

Choose the narrowest accurate scope. Do not concatenate commit subjects or
include implementation trivia.

## Body

Use these four sections in this order. Add `Verification` after them when
commands or CI results are available.

```markdown
## What's Changed

<Concrete behavior and architecture changes.>

## Example

<A caller-visible example, before/after, path mapping, request/response, or
short code sample that makes the new behavior unambiguous.>

## Code Review

<What was reviewed, findings fixed before publication, and any remaining
risks or unresolved findings.>

## Impact

<Compatibility, migration, persistence, runtime, deployment, performance,
security, or operational consequences.>

## Verification

<Exact focused commands and results; state that the full matrix is left to CI.>
```

### What's Changed

- Lead with the delivered behavior and architectural boundary.
- Group related changes into a small number of concrete bullets.
- Name removed or migrated public APIs when callers must change.
- State intentionally unavailable or deferred behavior only when it prevents a
  reviewer from assuming functionality that does not exist.

### Example

- Show one realistic, current example that demonstrates the core contract.
- Prefer observable inputs and results over internal implementation snippets.
- For path or namespace changes, show logical-to-physical mappings and the
  resulting errors or permissions.
- Keep the example consistent with the final code; do not use pseudocode that
  would fail against the actual API.

### Code Review

- State the surfaces inspected and the invariants checked.
- List material findings that were fixed before the PR, including the failure
  they would have caused and the regression coverage added.
- Report remaining findings and risks honestly. Say that no blocking findings
  remain only after completing the review and focused verification.
- Do not use generic claims such as "code reviewed" or invent an independent
  review that did not happen.

### Impact

- Describe caller compatibility and any required migration.
- Cover persistent-data paths and upgrade behavior when storage changes.
- Explain runtime or deployment consequences, including intentionally absent
  optional services.
- Mention important unchanged boundaries when they materially reduce rollout
  risk, such as preserved on-disk paths or unchanged ownership authority.
- Avoid promotional language and repeat neither the summary nor the example.

### Verification

- List exact commands that actually ran and whether they passed.
- Distinguish tests, checks, formatting, linting, and CI-only coverage.
- Never claim a full suite passed when it was not run.

## Publish and verify

Push the fresh head branch, then create the PR with an explicit base and head.
Use `gh pr create --base master --head <branch>` unless the user selected a
different base. Read the created PR back with `gh pr view` and verify:

- the PR is new when new work was requested;
- base and head branches are correct;
- title and all required body sections are present;
- the published body matches the reviewed final diff.

When the user asks to update an existing PR, inspect its current body first and
preserve useful human-authored context while bringing it into this structure.
