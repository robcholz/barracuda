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
AgentRuntime
├── SharedApiManager
└── RuntimeWorker<F, H, T>
    ├── SharedPersistence<F> flush boundary
    ├── AgentRunScheduler<H, T>
    └── SessionManager<F, H, T>
        ├── process-global durable ID allocators
        ├── AgentManager<F, H, T>
        └── ManagedSessions<F, H, T>
            ├── durable SessionPersistentState
            └── optional live SessionActor<F, H, T>
                ├── AgentSlots<H, T>
                ├── memory/context assembly policy
                └── optional Multiagent domain state

AgentRunScheduler<H, T>
└── AgentRun<H, T>
    └── checked-out BaseAgent<H, T>
~~~

### AgentRuntime

- `AgentRuntime` owns process-level execution: `SharedApiManager`, `link_api`,
  the single worker lifetime, the global Scheduler, and the physical
  persistence flush boundary.
- Its private `RuntimeWorker<F, H, T>` fairly rotates runtime control, Session
  ingress, live SessionActors, and the Scheduler.
- RuntimeWorker directly calls the SessionManager. There is no second Session
  handle, client, collection, or request protocol.

### SessionManager

- `SessionManager<F, H, T>` owns every Session record, AgentManager, durable ID
  allocator, and live SessionActor.
- It implements create, list, open, delete, shutdown, and one fair actor-poll
  round. It does not own the Runtime worker, global Scheduler, API
  configuration, or persistence flush.
- A managed Session consists of its persistence policy, one
  `DurableState<SessionPersistentState>`, and an optional live `SessionActor`.
  Persistent Sessions may remain dormant without an actor until open or delete.
- SessionManager owns `AgentManager`; the worker loop never reads Session
  metadata or calls AgentManager.
- `SessionPersistentState` contains only Session-owned metadata: configuration,
  the root Agent link, and root inflight-tool recovery metadata. AgentState,
  transcript, profile, and memory remain canonical in their component stores.
- Permanent deletion is two-stage: SessionActor first reaps physical Agent
  ownership and asks AgentManager to remove Agent-owned stores; SessionManager
  then removes the Session record and directory entry.
- RuntimeWorker owns only the polling/flush boundary. It forwards lifecycle
  commands to SessionManager and does not understand the Session persistence
  schema.

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
└── InFlight {
      run_id: RunId,
      metadata: AgentSlotMetadata,
      lifecycle: Running | Cancelling | Reaping,
    }
~~~

The invariant is:

~~~text
logical Agent exists
    => exactly one AgentSlot exists
    => exactly one of Resident or InFlight is true
~~~

`AgentManager` constructs and restores Agents but never owns live Agents.
There is no second Agent-owning registry or `AgentDirectory`.
`agents_overview()` is projected from retained slot metadata, including checked
out and cancelling Agents. SessionManager may aggregate these projections
across Sessions; a future directory cache must remain a rebuildable read model.

### AgentRunScheduler

- RuntimeWorker owns and polls exactly one `AgentRunScheduler<H, T>`.
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
- `AgentRun` is a Scheduler-private ownership/poll wrapper around a checked-out
  BaseAgent and that Agent's `AgentStreamHandle`. It must not define a second
  event or outcome protocol. It forwards `Result<AgentEvent, AgentError>` unchanged and
  retains the completed BaseAgent until its owner takes it back exactly once.

### BaseAgent

- `BaseAgent<H, T>` is the only concrete Agent type. There is no `GenericAgent`
  abstraction or `dyn Agent` ownership layer.
- BaseAgent has only `Running` or `Stopped(reason)`. While running, one
  `IterationLoop<P>` owns the linear `LLM stream -> ToolCalls boundary ->
  permission -> tool execution -> all-ID join` flow. Waiting for approval
  suspends the injected BaseAgent permission future; it is not a second Agent
  state machine.
- `BaseAgent::submit(&mut self, Message)` is the only task-entry API. It returns
  an `AgentStreamHandle<'_>` that exclusively borrows the BaseAgent for the
  task and implements `Stream<Item = Result<AgentEvent, AgentError>>`. The handle is the only
  event and control surface; there is no public tick API, output sender, or
  separate terminal-outcome protocol.
- The handle owns `interrupt`, `cancel`, and `resolve_approval`. Dropping it
  before terminal completion cancels the active task and leaves BaseAgent in a
  stopped state. BaseAgent directly consumes the borrowed HTTP stream, updates
  its transcript, and yields each corresponding `AgentEvent`.
- After the LLM stream completes, `IterationLoop` emits the aggregate
  `IterationEvent::BeforeToolCalls` boundary and suspends until the owner polls again.
  It then continues directly into permission and execution; BaseAgent stores no
  duplicate tool-call substate. There is no `ToolCallObserver`, `ToolStartYield`,
  or hand-built observer barrier.
- BaseAgent accepts a new `Message` through `submit` only while stopped. Its
  owner retains all message queuing policy. `interrupt` requests a stop at the
  end of the current LLM/tool loop boundary; `cancel` wakes and cooperatively
  aborts current async work.
- `BaseAgent` owns only the generic run protocol and already assembled
  dependencies: type-erased transcript, `ToolSet`, `PermissionPolicy`, agent
  instruction, inherited context, `SharedApiManager` plus its Agent `ApiUsage`,
  `Vec<Box<dyn ContextAdapter>>`, and the transient `AgentEffectInbox` used to
  reduce typed tool effects.
- At the beginning of every LLM iteration, BaseAgent snapshots its current API
  config from `SharedApiManager`, drops the manager lock, and applies that
  config directly to its LLM client. BaseAgent retains no shadow copy of the
  applied config. Multiagent never resolves `ApiUsage` or configures a BaseAgent
  LLM. Memory extraction and transcript compaction keep their own manager
  clones and dedicated usages.
- Concrete mode, conversation, profile, skill, and memory semantics live under
  `agent/context_adapters`. In particular, the BaseAgent runtime neither
  recognizes Normal/Plan nor matches on mode-specific tools.
- Every Agent owns an independent `ReasoningEffortContextAdapter` value. The
  durable Session setting is the default for future Agents and a Session-level
  update is fanned out through one typed queue per live Agent; adapters do not
  share a mutable reasoning-effort source. Each adapter creates and owns its
  receiver, returns only the sending handle to its AgentSlot, and drains its
  inbox when it contributes the next iteration's context.
- `ContextAdapter` and transcript-facing traits are consumer-owned ports under
  `agent/base_agent`; `agent/context_adapters` contains implementations only.
  The concrete `TranscriptStore<F>` binding lives at the filesystem-aware
  Manager boundary.
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
- Other pure ToolGroups are assembled directly into ToolSet by Manager; they
  are not wrapped in fake context adapters merely to deliver an Agent effect.
- Manager creates one synchronous tool-to-Agent effect channel. BaseAgent owns
  its unique, non-cloneable `AgentEffectInbox`; pure tools and adapter-owned
  tools receive clones of `AgentEffectEmitter`. `ContextAdapter` has no generic
  reverse drain/message or configuration-update API; an adapter that needs
  typed control owns its concrete inbox.
- Tools emit typed effects while they run; BaseAgent drains and reduces them
  only after the complete tool round. The channel mutex is held only for a
  bounded emit/drain operation and never across an `await`. More than one
  mutually exclusive task-boundary effect in a round fails deterministically.
- Each authoritative component owns its live recovery semantics: the mode
  adapter owns mode and the resumed adapter owns loaded-group recovery state.
  ToolSet retains only its existing runtime projection and has no persistence
  API.
- Tool execution and permission are separate. `ToolExecutor` only invokes an
  already-authorized call. The iteration tool round is generic over a statically
  dispatched `ToolPermissionPolicy`; `AllowAll` is the YOLO implementation,
  while BaseAgent injects the implementation that evaluates its configured
  policy, emits `ApprovalRequired`, and awaits its stream handle. The iteration
  loop contains no pending-approval or approval-resolution protocol.
- Scheduler schedules an entire `AgentRun`, not individual tool calls.
- `BaseAgent` does not own `SharedPersistence`, perform storage I/O, or depend
  on the filesystem type `F`.

## Identity

- `AgentId`, `RunId`, and `ToolCallId` are distinct newtypes with different
  scopes.
- `AgentId` is globally unique within the persisted installation and is never
  reused. SessionManager owns its durable allocator and injects an allocation
  handle into SessionActor.
- `RunId` identifies one checkout epoch so a stale completion cannot overwrite
  a newer slot state.
- `ToolCallId(u32)` identifies one call only inside one iteration. A fresh
  allocator starts at zero for every iteration and assigns IDs in provider call
  order. It is transient: it is never serialized, checkpointed, restored, or
  compared across iterations.
- The provider's string call ID remains separate. It is validated for presence
  and uniqueness within the response and is used only to correlate assistant
  tool calls with transcript tool-result messages.

## Construction and recovery

`SessionActor` is the single orchestration-level assembly path for roots and
workers. It selects Agent kind, lifecycle, baked policy, memory visibility,
tool filtering, context, and recovery policy.

`AgentManager<F, H, T>` is the sole concrete constructor. It supports two
entry paths that converge on one internal builder:

- `create`, which initializes a fresh recovery state;
- `resume_from`, which loads recovery state and canonical-store identities.

Manager uses claw-persistence and filesystem-backed component stores during
construction, but the resulting `BaseAgent<H, T>` retains no filesystem or
`SharedPersistence` dependency.
Manager does not choose Session, parentage, Multiagent graph, lifecycle, memory
visibility, or durability policy.

## Recovery state registration

`BaseAgent` exposes a synchronous, I/O-free projection of the state necessary
to reconstruct it:

~~~rust
struct AgentState {
    agent_mode: AgentModeState,
    resumed: ResumedState,
}

impl<H, T> BaseAgent<H, T> {
    fn recovery_state(&self) -> &DurableState<AgentState>;
}
~~~

Every field of a materialized `AgentState` is present; the aggregate does not
use `Option` to represent component defaults. During construction Manager
distributes a restored aggregate as `Some(AgentModeState)` and
`Some(ResumedState)`. For a new Agent it passes `None` to each component, and
that component owns its explicit initialization policy. Component state DTOs
do not implement `Default`.

`AgentState` is a projection, not a second mutable shadow copy. Mode adapter
and resumed adapter remain the authoritative live components.
`BaseAgent::recovery_state()` drives a generic state sink over those components;
it does not decode adapter-specific state.

The persisted schema is versioned. `ResumedState` serializes loaded tool groups
in stable canonical order. Conversation history is absent because the
canonical transcript reconstructs it. `ToolCallId`, physical tool-executor
state, active futures, and scheduler state are absent because they are
transient.

Each BaseAgent owns one `DurableState<AgentState>` projection. BaseAgent
refreshes that projection from authoritative adapters at iteration and terminal
boundaries. For a persistent root, AgentManager registers the same DurableState
with the Agent collection during create or restore; ephemeral Agents leave it
unregistered. No snapshot crosses the Scheduler protocol, and neither
SessionActor nor RuntimeWorker borrows a checked-out BaseAgent for persistence.

RuntimeWorker calls the process-wide `SharedPersistence::maybe_persist()` boundary
after every top-level poll. It does not interpret AgentState or
SessionPersistentState. SessionActor authorizes permanent Agent removal only
after physical ownership has returned, drops live component handles, and then
calls AgentManager to remove the Agent record and transcript.

### Tool-call identity boundary

At the start of each iteration, `IterationLoop` constructs a fresh
`ToolCallIdAllocator`. After validating every provider call ID, it assigns local
numeric IDs in response order. Permission decisions, approval correlation, and
the iteration result collector use those local IDs. Transcript messages use the
provider IDs. Neither identity is added to `AgentState`; a future durable
side-effect journal, if required, must define a separate durable invocation ID
rather than changing the scope of `ToolCallId`.

## Canonical and transient state

- Durable `SessionPersistentState` locates the root Agent record and carries
  only Session-owned recovery metadata.
- Agent states, transcripts, profiles, and long-term memory are
  separate canonical stores; snapshots do not duplicate transcript contents.
- `AgentRun`, Scheduler queues/readiness, active LLM/tool futures, Wakers,
  `RunId`, checkout state, and checkpoint waiters are transient.
- After a crash, Manager reconstructs Agents from durable recovery state and
  canonical stores. It never restores a physical future or checkout.
- Agent, tool, LLM, and persistence failures are outcomes. They cannot destroy
  the global loop or lose Agent ownership.

## Session stream

- `SessionStream` is the public read/control surface for one open Session. It
  owns the single event receiver and can clone a `SessionControl` capability
  for concurrent writers.
- `append(Message)` only appends to the SessionActor's FIFO inbox. It does not
  drive the Agent and does not wait for the resulting turn to finish.
- SessionActor starts at most one root turn at a time. It moves the resident
  root into the global Scheduler and starts the next queued message only after
  that Agent has physically returned to its slot.
- `interrupt` and `cancel` affect only the active run. They do not remove
  messages already queued for later turns. Queue ownership and append semantics
  never enter BaseAgent.
- `close` and permanent Session deletion cancel the active run and discard the
  Session inbox. Deletion waits for physical Agent return before removing the
  slot or its durable record.
- Dropping `SessionStream` sends a non-blocking close request so its exclusive
  lease cannot strand the Session. Explicit `close().await` is the synchronization
  point when a caller must wait for Agent return.
- SessionActor maps Agent events into Session events, owns input-request
  correlation, mutates its injected `DurableState<SessionPersistentState>`,
  and manages Agent lifecycle through AgentManager. SessionManager owns the
  Session record lifecycle. SessionActor never polls BaseAgent or AgentRun
  directly.

## Multiagent

- Multiagent is an optional tool/domain component, not a physical Agent owner
  or scheduler.
- It owns graph policy, parent/child relationships, readiness, joins,
  follow-up/delete/cancel semantics, and timeout policy.
- It has no dependency on `BaseAgent`, `AgentRun`, `AgentSlot`,
  `AgentManager`, `AgentRunScheduler`, `SessionActor`, or `SessionId`.
- A `MultiagentBridge` transports typed commands and correlated results.
  Multiagent validates domain intent and emits typed effects; SessionActor
  executes accepted physical effects without reimplementing graph policy.
- Root and worker construction use the same SessionActor assembly path and
  Manager constructor with explicit policy inputs.

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
→ AgentRuntime ingress
→ RuntimeWorker command routing
→ SessionManager lifecycle routing
→ SessionActor mailbox
→ SessionActor command handling
→ optional Multiagent command/effects
→ SessionActor assembles or checks out BaseAgent
→ Scheduler submission mailbox
→ global Scheduler fair sweep
→ checkpoint update or terminal completion
→ RuntimeWorker services the global persistence flush boundary
→ SessionActor restores AgentSlot before applying terminal outcome
→ Session event stream
~~~

RuntimeWorker rotates across command ingress, SessionManager, and Scheduler
work. One SessionManager round polls every currently live SessionActor at most
once; the global Scheduler independently gives every active AgentRun at most
one poll per fair sweep. Polling SessionManager or SessionActor never polls
an `AgentRun` directly. Persistence is serviced after every top-level worker
poll.

## Module dependency direction

~~~text
runtime
├── AgentRuntime
├── RuntimeWorker
├── scheduler
├── session
└── persistence flush boundary

session
├── SessionManager
├── SessionActor
├── AgentManager and Agent types
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
- session must not depend on runtime;
- multiagent must not depend on physical Agent, scheduler, or session types;
- BaseAgent and the Agent iteration loop must not depend on session,
  multiagent domain state, `SessionManager`, `SharedPersistence`, or filesystem
  type `F`.

There is no top-level `orchestrator` module. A future physical coordinator for
the optional Multiagent feature belongs at `multiagent::orchestrator`; it is a
Multiagent domain component, not the process runtime root.
