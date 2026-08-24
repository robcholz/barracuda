# barracuda-agent-memory

The agent memory subsystem.

Three independent pieces live here: the **`TranscriptStore`** — a pure,
append-only verbatim record of a conversation's turns — **`ProfileStore`** for
editable global profile documents (`soul.md`, `identity.md`, `user.md`), and
**long-term memory** (durable facts). These stores know nothing about prompt
assembly, summarization, token budgets, or agent tools. Assembling an LLM
context window is the *agent layer's* job, built on top of the stores via
context providers in `barracuda-agent`.

The crate only defines the `Compactor` **seam** — the contract for folding an
aged window of messages into a shorter summary. It carries no LLM dependency;
the ready-made LLM-backed compactor (`LlmCompactor`) and the rolling-summary
provider that drives it both live in `barracuda_agent` (the layer that owns the LLM
client). The store is never asked to compact.

As a core crate it depends only on `barracuda_vfs::ScopedVfs`, never on a
concrete Platform or filesystem backend. Device firmware receives a private
Plugin namespace from System; host tests use the same API over `vfs-memfs`.

## Public API

| Item | Role |
|---|---|
| `Transcript` | The sole type-erased transcript interface used by the agent runtime. It hides the concrete filesystem-backed store type: `open_turn()`, `turns()`, and the two version counters. |
| `TranscriptStore` | The concrete per-conversation verbatim store over one scoped VFS. Dropping a non-empty turn commits it in memory; `flush().await` persists queued turns. |
| `Turn` / `TurnId` | One turn (`id: Option<TurnId>` + `messages`) yielded by `turns()`, and its monotonic logical id. Committed turns carry `Some(id)`; the trailing open turn carries `None`. |
| `TurnHandle` | The non-generic RAII scope returned by `open_turn()`. It opens role-specific child handles and commits plus persists the turn on drop. |
| `UserHandle` / `AssistantHandle` / `ToolHandle` | Nested message scopes. Their only mutation is `append()`; dropping one finishes its message. |
| `Compactor` / `CompactError` | The summarization seam: fold an aged message window into a shorter summary. Driven by the agent layer, **not** the store. |
| `ProfileStore` and friends | Editable global profile documents: `Soul`, assistant identity, and user profile. Pure whole-file storage over the scoped VFS; projected into context by `barracuda-agent`. |
| `LongTermMemory` and friends | Durable per-agent / global fact storage. |

### How a turn flows

1. Call `store.open_turn()`, then open `user()`, `assistant()`, or `tool()` child
   scopes and append role-specific fragments.
2. Appended fragments are visible through `turns()` as the trailing open turn
   (`id == None`). Dropping a child finishes its message; dropping the turn
   commits it and advances `turn_version`. Call `flush().await` at an async
   persistence boundary to write queued records.
3. `store.turns()` is the sole read surface: committed turns (`id == Some(_)`)
   followed by any open turn. The full verbatim transcript you feed to the model
   is `turns().iter().flat_map(|t| &t.messages)` — no summaries spliced in; the
   store keeps everything.
4. Turn commit is synchronous; durable VFS I/O is explicit through
   `flush().await`.

**Compaction is not the store's concern.** In `barracuda-agent`, a
`RollingSummaryContextProvider` reads aged turns via `turns()`,
summarizes them through an injected `Compactor`, and a
`RecentMessagesContextProvider` renders the verbatim tail. The two coordinate
through a shared cursor marking the boundary between the summarized prefix and
the verbatim tail. Bounding on-disk growth (retention) is likewise a separate,
future concern — not the store's.

## Features

| Feature | Default | Effect |
|---|---|---|

## Example

```bash
cargo run -p barracuda-agent-memory --example conversation --target x86_64-unknown-linux-gnu
```

Drives a `TranscriptStore` through a few turns over an in-memory VFS, then
prints the verbatim message list the model would receive. See the crate-level
rustdoc for the same flow with inline commentary.

## Where it fits

A pure-Rust core crate (no platform/FFI). It persists through an injected
`ScopedVfs`, so the same code runs over LittleFS on-device and MemFS in tests.
