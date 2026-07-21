# Architecture

By Finn (Ziheng) Sheng <zsheng2@ncsu.edu> or <robcholz00@gmail.com>

## Hard constraints

- The entire Agent system runs on exactly one OS thread.
- There is exactly one process-global physical Agent scheduler.
- The design provides cooperative async I/O concurrency, not preemption or
  parallel execution of synchronous CPU work.
- No lock, `RefCell` borrow, persistence guard, or slot borrow may be held while
  polling or awaiting Agent, LLM, or tool code.

## Ownership model

~~~text
Orchestrator / Engine<F, H, T>
├── SharedPersistence<F> and AgentStateStore<F>
├── process-global durable ID allocators
├── AgentRunScheduler<H, T>
└── SessionActors<F, H, T>
    ├── AgentSlots<H, T>
    ├── durable SessionState
    ├── memory/context assembly policy
    └── optional Multiagent domain state

AgentRunScheduler<H, T>
└── AgentRun<H, T>
    └── checked-out BaseAgent<H, T>
~~~

### SessionActor and AgentSlot

- `SessionActor` is the logical owner of every Agent in its Session.
- `SessionActor` stores one authoritative `AgentSlot` for every logically live
  Agent, keyed by the globally unique `AgentId`.
- A resident Agent is physically stored in its slot. At checkout, the Agent is
  moved exactly once into an `AgentRun`; it is not moved on every poll.
- While checked out, the slot retains lifecycle and overview metadata but not a
  second reference to the Agent.
- Every terminal path returns the Agent exactly once before the slot is removed
  or another run begins.

~~~text
AgentSlot<H, T>
├── Resident {
│     agent: BaseAgent<H, T>,
│     metadata: AgentSlotMetadata,
│   }
└── CheckedOut {
      run_id: RunId,
      metadata: AgentSlotMetadata,
      lifecycle: Running | Cancelling | Reaping,
    }
~~~

The invariant is:

~~~text
logical Agent exists
    => exactly one AgentSlot exists
    => exactly one of Resident or CheckedOut is true
~~~

There is no second owning `AgentManager` or `AgentDirectory`.
`agents_overview()` is projected from retained slot metadata, including checked
out and cancelling Agents. Engine may aggregate these projections across
Sessions; a future directory cache must remain a rebuildable read model.

### AgentRunScheduler

- Engine owns and polls exactly one `AgentRunScheduler<H, T>`.
- Scheduler is the only component allowed to poll `AgentRun`.
- It fairly polls all checked-out root and worker Agents across all Sessions.
- Scheduler knows no Session, Multiagent graph, parent/child, persistence, or
  memory semantics. It only understands the run protocol and opaque routes.
- Scheduler owns its active queue/state machine. It is not an
  `Arc<Mutex<AgentRunScheduler>>`; a lightweight single-thread-local submission
  handle or mailbox may be shared.
- One fair round polls each run eligible for that round at most once before any
  of those runs receives a second poll.
- Fairness exists only at poll/yield boundaries. Synchronous blocking work in a
  poll blocks the sole OS thread and must remain bounded.

### BaseAgent

- `BaseAgent<H, T>` is the only concrete Agent type. There is no `GenericAgent`
  abstraction or `dyn Agent` ownership layer.
- `BaseAgent` owns only the generic run protocol and already assembled
  dependencies: type-erased transcript, `ToolSet`, `PermissionPolicy`, agent
  instruction, inherited context, `Vec<Box<dyn ContextAdapter>>`, and the
  transient `AgentEffectInbox` used to reduce typed tool effects.
- Concrete mode, conversation, profile, skill, and memory semantics live under
  `agent/context_adapters`. In particular, the BaseAgent runtime neither
  recognizes Normal/Plan nor matches on mode-specific tools.
- `ContextAdapter` and transcript-facing traits are consumer-owned ports under
  `agent/base_agent`; `agent/context_adapters` contains implementations only.
  The concrete `TranscriptStore<F>` binding lives at the filesystem-aware
  Factory boundary.
- Each component's durable DTO is defined beside that component:
  `AgentModeState` beside `AgentModeContextAdapter`, and `ResumedState` beside
  `ResumedContextAdapter`. `agent/context_adapters/mod.rs` re-exports each
  adapter together with its State DTO so consumers never depend on
  adapter-internal module paths. The complete aggregate `AgentState` and its
  assembly sink live in `agent/base_agent/persistence.rs`; BaseAgent coordinates
  collection but does not interpret component fields.
- A ToolGroup that operates on adapter-owned state is co-located with that
  adapter (for example `context_adapters/agent_mode/tools.rs`). Only pure Agent
  tool groups with no adapter domain owner live under `agent/tools`, one group
  per file.
- `ResumedContextAdapter` owns the optional, one-shot resume reminder and
  its `context_adapters/resumed/tools.rs` discovery group. `tool_load` records
  accepted groups into the adapter-owned `ResumedState` while the existing
  `ToolDiscoveryHandle` asks ToolSet to update its runtime visibility. After a
  restart, recorded groups are rendered into the one-shot reminder; they are
  not automatically loaded into the fresh ToolSet.
- Other pure ToolGroups are assembled directly into ToolSet by Factory; they
  are not wrapped in fake context adapters merely to deliver an Agent effect.
- Factory creates one synchronous tool-to-Agent effect channel. BaseAgent owns
  its unique, non-cloneable `AgentEffectInbox`; pure tools and adapter-owned
  tools receive clones of `AgentEffectEmitter`. `ContextAdapter` has no reverse
  drain/message API.
- Tools emit typed effects while they run; BaseAgent drains and reduces them
  only after the complete tool round. The channel mutex is held only for a
  bounded emit/drain operation and never across an `await`. More than one
  mutually exclusive task-boundary effect in a round fails deterministically.
- Each authoritative component owns its live recovery semantics: the mode
  adapter owns mode, the resumed adapter owns loaded-group recovery state, and
  the Agent tool runtime owns its monotonic counter and unsettled tool calls.
  ToolSet retains only its existing runtime projection and has no persistence
  API.
- `ToolRunner` remains inside the Agent tool runtime. Scheduler schedules an
  entire `AgentRun`, not individual tool calls.
- `BaseAgent` does not own `SharedPersistence`, perform storage I/O, or depend
  on the filesystem type `F`.

## Identity

- `AgentId`, `RunId`, and `ToolCallId` are distinct newtypes.
- `AgentId` is globally unique within the persisted installation and is never
  reused. Engine owns and durably checkpoints its allocator before exposing a
  reserved ID.
- `RunId` identifies one checkout epoch so a stale completion cannot overwrite
  a newer slot state.
- `ToolCallId` identifies one physical invocation. It is monotonic and
  restart-safe within one Agent; the durable invocation identity is
  `(AgentId, ToolCallId)`.
- Allocating a ToolCall ID increments `next_tool_call_id` in the same in-memory
  mutation that inserts its unsettled record. Tool name and arguments are not
  invocation identity.

## Construction and recovery

`SessionActor` is the single orchestration-level assembly path for roots and
workers. It selects Agent kind, lifecycle, baked policy, memory visibility,
tool filtering, context, and recovery policy.

`FsAgentFactory<F, H, T>` is the sole concrete constructor. It supports two
entry paths that converge on one internal builder:

- `create_new`, which initializes a fresh recovery state;
- `restore`, which loads recovery state and canonical-store identities.

Factory may use `AgentStateStore<F>` and filesystem-backed component stores
during construction, but the resulting `BaseAgent<H, T>` does not retain them.
Factory does not choose Session, parentage, Multiagent graph, lifecycle, memory
visibility, or durability policy.

## Recovery state and checkpoint protocol

`BaseAgent` exposes a synchronous, I/O-free projection of the state necessary
to reconstruct it:

~~~rust
struct AgentState {
    agent_mode: AgentModeState,
    resumed: ResumedState,
    next_tool_call_id: ToolCallId,
    unsettled_toolcalls:
        BTreeMap<ToolCallId, UnsettledToolCallRecord>,
}

impl<H, T> BaseAgent<H, T> {
    fn recovery_state(&self) -> AgentState;
}
~~~

Every field of a materialized `AgentState` is present; the aggregate does not
use `Option` to represent component defaults. During construction Factory
distributes a restored aggregate as `Some(AgentModeState)` and
`Some(ResumedState)`. For a new Agent it passes `None` to each component, and
that component owns its explicit initialization policy. Component state DTOs
do not implement `Default`.

`AgentState` is a projection, not a second mutable shadow copy. Mode adapter,
resumed adapter, and the tool-call journal remain the authoritative live
components.
`BaseAgent::recovery_state()` drives a generic state sink over those components;
it does not decode adapter-specific state.

The persisted schema is versioned. `UnsettledToolCallRecord` is a stable
recovery record, not a transient event/future type such as `TrackedToolCall`.
`ResumedState` serializes loaded tool groups in stable canonical order.
Conversation history is absent because the canonical transcript reconstructs
it. Physical ToolRunner state, active futures, and scheduler state are absent
because they are transient.

Because an active `BaseAgent` has been moved into an `AgentRun`, Engine cannot
borrow it to call `recovery_state()`. The Agent calls the method internally
at a checkpoint boundary and exports the owned snapshot through the run
protocol:

~~~text
BaseAgent reaches a recovery boundary
→ BaseAgent mutates its in-memory recovery state
→ BaseAgent calls recovery_state()
→ AgentRunUpdate::CheckpointRequired {
      agent_id, run_id, checkpoint_id, purpose, snapshot
  }
→ Scheduler parks that AgentRun
→ Engine applies the Agent's external recovery policy
→ Engine returns the matching CheckpointResult
→ Scheduler resumes or fails the AgentRun
~~~

`checkpoint_id` is transient and scoped to one `RunId`. Scheduler validates
park/resume mechanics but never interprets or writes the snapshot. Engine owns
the persistence operation. A checkpoint error becomes a typed Agent run outcome
and must still return the Agent to its owning slot.

Every change represented by the recovery snapshot must eventually cross an
acknowledged checkpoint boundary. Implementations may coalesce ordinary mode or
loaded-tool-group changes, but they must not publish a terminal run completion
while recovery state differs from the last acknowledged snapshot. The pre-tool
boundary is non-coalescible because it guards an external side effect.

The baseline `AgentPersistencePolicy` variants are:

- `PersistentRoot`: Engine durably writes every required recovery checkpoint;
- `EphemeralRoot`: maintains the same in-memory state without a durable Agent
  record;
- `TransientWorker`: maintains the same in-memory state without a durable Agent
  record.

The policy is selected outside `BaseAgent` and retained with slot/run metadata.
An ephemeral checkpoint may be acknowledged without storage, preserving one
uniform run protocol. SessionActor authorizes permanent Agent-record deletion
only after logical deletion is committed and physical ownership has returned.

### Tool-call durability boundary

Before a persistent tool body is first polled:

~~~text
allocate ToolCallId and increment next_tool_call_id
→ insert UnsettledToolCallRecord
→ export AgentState in CheckpointRequired
→ Scheduler parks the AgentRun
→ Engine durably checkpoints the snapshot
→ resume and first-poll the tool body
~~~

If the checkpoint fails, the tool body is never polled. An active tool future is
transient; an unsettled record means the durable outcome of a possibly
side-effecting invocation is not yet known.

Settlement order is:

~~~text
tool body completes
→ append its outcome to the open transcript turn
→ keep the call unsettled during later iterations
→ commit and durably checkpoint the transcript turn
→ clear calls represented by that committed turn
→ export and durably checkpoint the new AgentState
~~~

The transcript checkpoint is fallible. A call is never cleared merely because
the tool future completed or because an open turn contains a patch. Recovery
never blindly replays an unsettled side-effecting invocation.

## Canonical and transient state

- Durable `SessionState` plus recoverable `AgentSlot` metadata locates the root
  Agent record and independent canonical stores.
- Agent states, transcripts, profiles, and long-term memory are
  separate canonical stores; snapshots do not duplicate transcript contents.
- `AgentRun`, Scheduler queues/readiness, active LLM/tool futures, Wakers,
  `RunId`, checkout state, and checkpoint waiters are transient.
- After a crash, Factory reconstructs Agents from durable recovery state and
  canonical stores. It never restores a physical future or checkout.
- A crash after transcript durability but before clearing the Agent snapshot
  may conservatively recover an invocation as unsettled; it is not replayed.
- Agent, tool, LLM, and persistence failures are outcomes. They cannot destroy
  the global loop or lose Agent ownership.

## Multiagent

- Multiagent is an optional tool/domain component, not a physical Agent owner
  or scheduler.
- It owns graph policy, parent/child relationships, readiness, joins,
  follow-up/delete/cancel semantics, and timeout policy.
- It has no dependency on `BaseAgent`, `AgentRun`, `AgentSlot`,
  `FsAgentFactory`, `AgentRunScheduler`, `SessionActor`, or `SessionId`.
- A `MultiagentBridge` transports typed commands and correlated results.
  Multiagent validates domain intent and emits typed effects; SessionActor
  executes accepted physical effects without reimplementing graph policy.
- Root and worker construction use the same SessionActor assembly path and
  Factory constructor with explicit policy inputs.

## Memory components

- Memory uses `ContextAdapter` to decouple from concrete Agent ownership.
- Each Agent sees its own transcript and conversation-history projection unless
  explicit spawn-time context is provided.
- Transcript implementations may be durable or in-memory and provide the
  fallible checkpoint used for tool settlement.
- Profiles, identity files, skills, and long-term-memory stores remain separate
  canonical components with explicit visibility and mutation policy.
- Workers do not automatically see parent or sibling transcripts. Baked policy
  filters inherited tools and write capabilities.

## Runtime dataflow

~~~text
external command
→ Engine ingress budget
→ SessionActor command handling
→ optional Multiagent command/effects
→ SessionActor assembles or checks out BaseAgent
→ Scheduler submission mailbox
→ global Scheduler fair sweep
→ checkpoint update or terminal completion
→ Engine services persistence and routes opaque results
→ SessionActor restores AgentSlot before applying terminal outcome
→ bounded outgoing work
~~~

Engine rotates bounded budgets across ingress, SessionActors, Scheduler work,
checkpoint/completion routing, persistence, and outgoing work. Polling a
SessionActor never polls an `AgentRun` directly, and Engine does not return
early merely because the first work class is ready.

## Module dependency direction

~~~text
orchestrator
├── session
├── scheduler
└── persistence / AgentStateStore

session
├── agent factory and Agent types
├── scheduler submission protocol
├── memory
└── multiagent port/domain

scheduler
└── agent run protocol

multiagent
└── domain protocol and bridge port

agent runtime
├── context adapters
└── tool interfaces
~~~

Forbidden reverse dependencies:

- scheduler must not depend on session, multiagent, or persistence;
- multiagent must not depend on physical Agent, scheduler, session, or
  orchestrator types;
- BaseAgent and the Agent iteration loop must not depend on session,
  multiagent domain state, orchestrator, `SharedPersistence`, or filesystem
  type `F`.
