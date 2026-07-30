# claw-memory

The agent memory subsystem.

Three independent pieces live here: the **`TranscriptStore`** — a pure,
append-only verbatim record of a conversation's turns — **`ProfileStore`** for
editable global profile documents (`soul.md`, `identity.md`, `user.md`), and
**long-term memory** (durable facts). These stores know nothing about prompt
assembly, summarization, token budgets, or agent tools. Assembling an LLM
context window is the *agent layer's* job, built on top of the stores via
context providers in `claw-core`.

The crate only defines the `Compactor` **seam** — the contract for folding an
aged window of messages into a shorter summary. It carries no LLM dependency;
the ready-made LLM-backed compactor (`LlmCompactor`) and the rolling-summary
provider that drives it both live in `claw_core` (the layer that owns the LLM
client). The store is never asked to compact.

As a core crate it depends only on the `claw-interface` `ClawFs` persistence
seam, never on the platform boundary (`claw-sys`). The concrete filesystem is
selected by the store type parameter (device firmware uses its real FS type;
host CLIs and tests use `claw_interface::MemFs` / `DiskFs`), so the crate is
fully host-testable.

## Public API

| Item | Role |
|---|---|
| `Transcript` | The sole type-erased transcript interface used by the agent runtime. It hides the concrete filesystem-backed store type: `open_turn()`, `turns()`, and the two version counters. |
| `TranscriptStore<F>` | The concrete per-conversation verbatim store for filesystem `F`. A non-empty turn is persisted when its handle drops. |
| `Turn` / `TurnId` | One turn (`id: Option<TurnId>` + `messages`) yielded by `turns()`, and its monotonic logical id. Committed turns carry `Some(id)`; the trailing open turn carries `None`. |
| `TurnHandle` | The non-generic RAII scope returned by `open_turn()`. It opens role-specific child handles and commits plus persists the turn on drop. |
| `UserHandle` / `AssistantHandle` / `ToolHandle` | Nested message scopes. Their only mutation is `append()`; dropping one finishes its message. |
| `Compactor` / `CompactError` | The summarization seam: fold an aged message window into a shorter summary. Driven by the agent layer, **not** the store. |
| `ProfileStore` and friends | Editable global profile documents: `Soul`, assistant identity, and user profile. Pure whole-file storage over `ClawFs`; projected into context by `claw-core`. |
| `LongTermMemory` and friends | Durable per-agent / global fact storage. |
| `NoopCompactor` | *(feature `compactor-stub`)* A never-compacts stub for host CLIs and tests. |

### How a turn flows

1. Call `store.open_turn()`, then open `user()`, `assistant()`, or `tool()` child
   scopes and append role-specific fragments.
2. Appended fragments are visible through `turns()` as the trailing open turn
   (`id == None`). Dropping a child finishes its message; dropping the turn
   commits it, advances `turn_version`, and persists one durable record.
3. `store.turns()` is the sole read surface: committed turns (`id == Some(_)`)
   followed by any open turn. The full verbatim transcript you feed to the model
   is `turns().iter().flat_map(|t| &t.messages)` — no summaries spliced in; the
   store keeps everything.
4. Persistence is automatic at turn drop; no explicit finish or checkpoint call.

**Compaction is not the store's concern.** In `claw-core`, a
`RollingSummaryContextProvider` reads aged turns via `turns()`,
summarizes them through an injected `Compactor`, and a
`RecentMessagesContextProvider` renders the verbatim tail. The two coordinate
through a shared cursor marking the boundary between the summarized prefix and
the verbatim tail. Bounding on-disk growth (retention) is likewise a separate,
future concern — not the store's.

## Features

| Feature | Default | Effect |
|---|---|---|
| `compactor-stub` | no | Adds `NoopCompactor` (host-only convenience). |

## Example

```bash
cargo run -p claw-memory --example conversation --target x86_64-unknown-linux-gnu
```

Drives a `TranscriptStore` through a few turns over an in-memory `MemFs`, then
prints the verbatim message list the model would receive. See the crate-level
rustdoc for the same flow with inline commentary.

## Where it fits

A pure-Rust core crate (no platform/FFI). It persists through the injected
`ClawFs`, so it is fully host-testable; the tests under `tests/` exercise it
over both in-memory and on-disk `ClawFs` doubles.
