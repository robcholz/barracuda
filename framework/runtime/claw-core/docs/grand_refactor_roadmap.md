# claw-core Grand Refactor Roadmap

This document turns [architecture.md](architecture.md) into an executable
refactor plan. The architecture document defines the target system; this
roadmap defines the migration order, temporary states, verification gates, and
deletion criteria.

The design is frozen top-down, but implementation proceeds bottom-up from the
owned Agent execution contract. Each phase must leave the tree buildable and
must pass its gate before the next phase starts.

## 1. Scope

This refactor exists to establish clear ownership and scheduling boundaries so
that LLM context and memory usage can be optimized safely afterwards.

It includes:

- one process-global, fair Agent scheduler on one OS thread;
- SessionActor ownership of all logical Agents and their lifecycle metadata;
- explicit move-in/move-out ownership for checked-out Agent runs;
- Multiagent as a pluggable tool and domain component;
- Agent-owned recovery semantics exported as an `AgentRecoverySnapshot`, with
  durability selected and executed outside BaseAgent;
- one Agent assembly path for roots and workers;
- system-wide Agent overview without a second owning registry;
- removal of the current per-session direct-drive path.

It deliberately does not include:

- a second OS thread or a second async runtime;
- true parallel execution of synchronous CPU-bound tools;
- priority scheduling;
- parallel tool calls inside one Agent;
- LLM context compaction or memory optimization;
- durable recovery of the complete worker graph, unless Phase 0 changes the
  current recovery policy explicitly;
- a GenericAgent abstraction, dyn Agent, or type-erased persistence facade.

Those features may be added after this roadmap is complete, using the
boundaries created here.

## 2. Frozen architectural decisions

### 2.1 Ownership

- BaseAgent<H, T> is the only concrete Agent type.
- SessionActor is the logical owner of every Agent in that session.
- AgentSlot is the authoritative logical ownership record and is always present
  while an Agent logically exists. Its recoverable metadata is persisted only
  when the Agent's recovery policy requires it.
- SessionActor stores slots in an AgentSlots collection keyed by AgentId. The
  architecture does not freeze Vec, map, or another concrete container.
- A resident Agent is physically stored in its AgentSlot.
- A checked-out Agent is moved exactly once into the global Scheduler and is
  physically held by its AgentRun until completion.
- The Agent is not moved on every poll. It moves from AgentSlot to Scheduler at
  checkout and from Scheduler back to AgentSlot at terminal completion.
- Every terminal path, including success, LLM failure, tool failure,
  persistence failure, cancellation, and shutdown, returns the Agent exactly
  once.
- There is no AgentManager that independently owns Agents.

The core slot model is:

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

### 2.2 Identity

- AgentId is globally unique within the persisted installation and is never
  reused.
- Engine owns, registers, and persists the process-global AgentId allocator.
  SessionActor obtains the next ID through an Engine-injected allocator handle
  only after accepting a root creation or Multiagent spawn effect. Multiagent
  and Scheduler never allocate AgentIds. Failed construction may consume an ID;
  consumed IDs are not reused.
- Advancing the allocator must become durable before the reserved AgentId is
  exposed to construction or persisted Agent state. Allocation checkpoint
  failure returns a typed creation/spawn failure and creates no Agent.
- RunId identifies one checkout epoch of one Agent. It is required even though
  AgentId is globally unique, because a late completion from an earlier run
  must not overwrite a newer checkout.
- ToolCallId identifies one physical tool invocation. Tool name and arguments
  are not an identity and cannot deduplicate parallel identical calls.
- ToolCallId is monotonic and restart-safe within one Agent. The durable
  identity of an invocation is (AgentId, ToolCallId). Allocation increments the
  Agent-owned next_tool_call_id counter in the same mutation that inserts the
  unsettled call, so recovery never resets or reuses it.

Prefer newtypes for all three IDs. Do not interchange raw integers or strings
across these boundaries.

### 2.3 Scheduling

- There is exactly one process-global AgentRunScheduler<H, T>.
- The active Scheduler is owned and polled by the Orchestrator/Engine.
- The Scheduler owns a central async polling loop/state machine. It is not only
  a shared container around callers that still drive their own Agents.
- All checked-out root and worker Agents across all sessions are polled by this
  Scheduler.
- Only the Scheduler may poll AgentRun.
- The Scheduler knows no Session or Multiagent semantics. It may echo an opaque
  return token or reply port supplied at submission, but it must not interpret
  SessionId, graph structure, parentage, or Agent kind.
- Fairness is global across sessions and is measured per runnable AgentRun, not
  per Session. A Session with multiple runnable Agents owns multiple scheduling
  units; per-Session quotas are a future policy.
- One fair round gives every run eligible for that round at most one poll before
  any of those runs receives a second poll. Newly submitted or newly awakened
  runs join an eligible round according to the ready-queue policy.
- The single OS-thread constraint permits concurrent progress of async I/O
  futures. Fairness is cooperative at poll/yield boundaries, not CPU
  preemption. Synchronous filesystem, persistence, context assembly, LLM/tool,
  or other work performed inside one poll blocks the only thread and therefore
  must be bounded.
- AgentRunScheduler is not Arc<Mutex<_>>. A lightweight single-thread-local
  submission handle/mailbox may be shared, while Engine remains the sole owner
  of the active polling state.

### 2.4 Session and Multiagent

- SessionActor owns AgentSlots, session/turn state, lifecycle metadata, and the
  physical execution of accepted domain effects.
- Multiagent is exposed to an Agent as a tool.
- Multiagent owns graph policy, parent/child relationships, readiness,
  join/follow-up/delete semantics, and timeout policy.
- Multiagent does not own BaseAgent, AgentRun, AgentSlot, FsAgentFactory, or
  AgentRunScheduler.
- Multiagent does not need Session semantics. A MultiagentBridge transports
  typed commands to SessionActor; the domain validates them and emits typed
  effects, and SessionActor performs physical Agent operations.
- SessionActor executes Multiagent effects but does not reimplement Multiagent
  domain policy.
- SessionActor is the only orchestration-level assembly path for root and
  worker Agents: it selects lifecycle, baked, memory-visibility, tool, and
  context policy and constructs AgentEnvironment<F>.
- FsAgentFactory is the sole concrete BaseAgent constructor. It materializes
  invariant Agent components from the supplied environment; it does not choose
  Session, parent, graph, lifecycle, or memory-visibility policy.

### 2.5 Persistence

- BaseAgent owns the generic run/snapshot protocol but no concrete mode or
  memory semantics. Authoritative recovery values stay with their components:
  the mode adapter, ToolSet, and Agent tool-call journal. BaseAgent owns no
  persistence manager or filesystem capability.
- BaseAgent exposes one synchronous, I/O-free
  `recovery_snapshot() -> AgentRecoverySnapshot` API. The returned value is an
  owned recovery projection because it must cross the active AgentRun boundary.
- The recovery shape is:

~~~text
AgentRecoverySnapshot {
    mode: AgentMode,
    loaded_tool_groups: Vec<ToolGroupId>,
    next_tool_call_id: ToolCallId,
    unsettled_toolcalls:
        BTreeMap<ToolCallId, UnsettledToolCallRecord>,
}
~~~

- `UnsettledToolCallRecord` is a stable recovery DTO, not the transient
  `TrackedToolCall` event type. Loaded tool groups use stable typed identities
  and canonical serialized order when order has no runtime meaning.
- Filesystem remains a real static capability in Engine, Session assembly,
  Factory, transcript, and persistence stores, but it stops at the constructed
  Agent boundary:

~~~text
Engine<F, H, T> / SessionActor<F, H, T> / FsAgentFactory<F, H, T>
    → BaseAgent<H, T>
    → AgentRun<H, T>
    → AgentSlot<H, T>
    → AgentRunScheduler<H, T>
~~~

- Do not add a type-erased persistence facade merely to hide F. No facade is
  needed because BaseAgent and Scheduler do not perform persistence.
- SessionActor selects `PersistentRoot`, `EphemeralRoot`, or `TransientWorker`
  policy and retains it with slot/run metadata. BaseAgent emits the same
  checkpoint protocol regardless of the external durability decision.
- `PersistentRoot` has a versioned Agent recovery record keyed by AgentId and
  receives crash-durability guarantees. `EphemeralRoot` and `TransientWorker`
  keep the same recovery state in memory but create no durable Agent record.
- `FsAgentFactory<F, H, T>` uses `AgentStateStore<F>` to create or restore
  recovery state, then injects ordinary Agent dependencies. It is the sole
  concrete BaseAgent constructor but does not choose durability policy.
- Engine owns runtime checkpoint I/O through `AgentStateStore<F>`. SessionActor
  authorizes permanent record removal only after logical deletion is committed
  and physical Agent ownership has returned.
- SessionActor owns session state and AgentSlot lifecycle/recovery metadata.
  Persistent SessionState stores the root_agent_id and canonical store
  identities required to locate the Agent recovery snapshot after restart.
- Persistent root creation writes and checkpoints Agent state before publishing
  root_agent_id in SessionState. Permanent deletion durably removes the root
  link before removing Agent state. A crash may therefore leave an unreferenced
  Agent record, which recovery treats as garbage; a root link whose Agent state
  is missing or corrupt is a typed recovery error, never a silently rebuilt
  Agent with a new ID.
- Transcript, profile, and long-term memory remain independent canonical
  stores; they are referenced rather than duplicated into session checkpoints.
- A physical AgentRun, Scheduler queue, active tool future, and checkout state
  are transient and are never restored.
- Runtime AgentSlot state and durable Agent recovery metadata are separate
  types. RunId, Resident/CheckedOut, Running/Cancelling/Reaping, inboxes,
  Wakers, and physical handles are never serialized.
- Because a running BaseAgent has moved into AgentRun, Engine cannot call
  `recovery_snapshot()` from outside. BaseAgent calls it internally at a
  recovery boundary and emits an owned snapshot in `CheckpointRequired`.
- Scheduler parks that run and echoes the update; Engine persists or
  acknowledges it according to the slot/run policy, then sends a matching
  `CheckpointResult`. Scheduler never interprets the snapshot.
- For PersistentRoot, before a tool body is first polled, its unsettled record
  must pass a durable checkpoint. EphemeralRoot and TransientWorker record the
  same state and may receive an immediate non-durable acknowledgement.
- A persistent tool call is cleared from unsettled state only after its
  containing transcript turn is committed and durably checkpointed.
- Recovery reports an uncertain interrupted invocation as Unknown/unsettled. It
  must not blindly replay a side-effecting tool.
- No persistence guard or mutable borrow may be held across await.
- ToolRunner remains inside BaseAgent. It is not split out merely to observe
  unsettled tool calls.
- The migration baseline is PersistentRoot recovery plus EphemeralRoot and
  TransientWorker non-recovery. Durable worker tool journals or complete worker
  recovery are separate features unless Phase 0 explicitly expands the scope.

## 3. Target dependency and dataflow

### 3.1 Ownership tree

~~~text
Orchestrator / Engine<F, H, T>
├── SharedPersistence<F>
├── AgentStateStore<F>
├── process-global durable ID allocators
├── AgentRunScheduler<H, T>
└── SessionActors<F, H, T>
    ├── AgentSlots<H, T>
    ├── session and turn state
    ├── memory/context assembly policy
    └── optional Multiagent domain state

AgentRunScheduler<H, T>
└── AgentRun<H, T>
    └── checked-out BaseAgent<H, T>
~~~

### 3.2 Module dependency direction

~~~text
orchestrator
├── session
├── scheduler
└── persistence

session
├── agent
├── scheduler handle/protocol
├── memory
└── multiagent port/domain

scheduler
└── agent run protocol

multiagent
└── domain protocol and bridge port

agent
├── memory adapters
└── tool interfaces
~~~

Forbidden reverse dependencies:

- scheduler must not depend on session or multiagent;
- multiagent must not depend on physical Agent types, scheduler, session, or
  orchestrator;
- agent must not depend on session, multiagent domain state, or orchestrator.
- BaseAgent and the Agent iteration loop must not depend on
  `SharedPersistence`, `AgentStateStore`, or filesystem type F. Factory may
  depend on construction-time persistence and canonical stores.

### 3.3 Runtime dataflow

~~~text
external command
→ Engine ingress budget
→ SessionActor command handling
→ optional Multiagent domain command/effects
→ SessionActor obtains AgentId when creation is required
→ SessionActor assembles or checks out BaseAgent
→ local Scheduler submission mailbox
→ global Scheduler fair sweep
→ opaque-routed checkpoint update or terminal completion
→ Engine services checkpoint I/O and resumes/fails parked runs
→ SessionActor restores AgentSlot first
→ SessionActor applies lifecycle/domain outcome
→ persistence and outgoing budgets
~~~

Engine fairness and Scheduler fairness are separate:

- Engine rotates bounded budgets across ingress, SessionActors, Scheduler work,
  completion/outgoing routing, and persistence service;
- Scheduler independently rotates across runnable AgentRuns;
- polling a SessionActor never polls AgentRun directly;
- Engine does not return early merely because the first work class is Ready.

These rules prevent starvation only at poll/yield boundaries. They cannot
preempt synchronous work performed inside a poll.

## 4. Migration strategy

The implementation order is:

~~~text
P0 freeze contracts and baseline
 ↓
P1 establish Agent recovery snapshot and Factory create/restore contracts
 ↓
P2 establish owned AgentRun move/return protocol
 ↓
P3 build and test the global Scheduler in isolation
 ↓
P4 cut every Agent over to the Scheduler
 ↓
P5 move all AgentSlots to SessionActor
 ↓
P6 reduce Multiagent to domain logic + bridge
 ↓
P7 activate Agent-exported tool-call recovery checkpoints
 ↓
P8 close overview, assembly, and memory-visibility boundaries
 ↓
P9 audit deletions and run final verification
~~~

This is intentionally not a top-down rewrite of Orchestrator. The top-level
contract is fixed first, but code changes start at BaseAgent/AgentRun so that
Engine never depends on a half-defined ownership protocol.

Each phase should be a separate reviewable commit or short commit series.
Temporary compatibility code must carry a TODO naming the phase that deletes
it and should not survive more than the next phase.

## 5. Phase 0 — Normalize the spec and capture the baseline

### Goal

Remove ambiguity before changing production ownership.

### Changes

- Update architecture.md so SessionActor no longer claims ownership of Agent
  mode, loaded tool groups, or unsettled tool calls.
- Replace “Orchestrator drives all agents” with:
  - Orchestrator polls SessionActors by a bounded budget;
  - Orchestrator polls the global Scheduler by a bounded tick;
  - Scheduler exclusively polls AgentRuns.
- Distinguish Multiagent domain “scheduling” from physical fair run scheduling.
- Fix the MultiagentBridge naming and describe typed commands/effects.
- Write the Resident/CheckedOut invariant and exact return-on-every-outcome
  rule into the architecture.
- Replace the concrete Vec<AgentSlot> wording with an AgentSlots collection
  keyed by AgentId, and distinguish SessionActor's logical ownership from
  Scheduler's temporary physical custody.
- State that Engine owns the durable process-global AgentId allocator and that
  SessionActor only requests IDs through its injected handle.
- Require allocator advancement to be durable before an AgentId is exposed.
- Freeze AgentPersistencePolicy as PersistentRoot, EphemeralRoot, and
  TransientWorker, or explicitly expand worker durability before Phase 1.
- Freeze `BaseAgent<H, T>::recovery_snapshot()` as a synchronous, I/O-free API
  and `AgentRecoverySnapshot` as the only aggregate Agent recovery shape.
- State explicitly that BaseAgent, AgentRun, AgentSlot, and Scheduler do not
  carry filesystem type F or own SharedPersistence.
- State that Factory performs create/restore construction while Engine performs
  runtime checkpoint I/O from Agent-exported snapshots.
- Add the durable SessionState root_agent_id link, creation/deletion ordering,
  typed dangling-link failure, and orphan Agent-state cleanup rule.
- Replace “durable SessionActor” with durable SessionState and recoverable
  AgentSlot metadata; actor futures, RunId, and checkout state remain transient.
- State that checked-out overview reads retained AgentSlot metadata and never a
  second owning Agent directory.
- State that SessionActor builds AgentEnvironment<F> for both roots and workers,
  while FsAgentFactory only constructs BaseAgent from that environment.
- Correct canonical-store descriptions to match their actual persistence seam;
  in particular, long-term memory currently uses claw-memory over ClawFs rather
  than the claw-persistence state collection.
- Add the persistent tool durability barrier and the distinction between a
  transient active future and a durable unsettled invocation.
- Record the exact commit, toolchain, commands, results, and known pre-existing
  failures for every baseline suite.
- Add or identify characterization coverage for externally observable behavior:
  - root and worker construction plus baked tool filtering;
  - foreground/background spawn, result delivery, follow-up, delete, timeout,
    cancel, interrupt, and nested join semantics;
  - SessionEvent and turn/input-request ordering;
  - the current ToolStarted-before-tool-body persistence boundary;
  - persistent root recovery of mode, loaded tool groups, and uncertain calls;
  - current root/worker transcript, context, profile, long-term-memory, skill,
    and tool visibility.
- Characterization tests assert public/domain behavior, not current module
  placement, ownership tables, per-session direct polling, incidental map order,
  or unfair scheduling behavior that this refactor intentionally replaces.

### Gate

- [ ] BaseAgent, AgentRun, AgentSlot, and Scheduler contain no F,
      SharedPersistence, DurableState, or AgentStateStore dependency.
- [ ] architecture.md contains no second Agent driver.
- [ ] persistence ownership is unambiguous.
- [ ] BaseAgent has no persistence dependency and F stops at the
      construction/storage boundary.
- [ ] the snapshot export and checkpoint park/resume dataflow is documented.
- [ ] worker durability policy is explicit.
- [ ] Resident XOR CheckedOut is documented.
- [ ] AgentId allocation and durable root lookup have one documented owner and
      recovery path.
- [ ] A crash after ID reservation cannot cause AgentId reuse.
- [ ] Baseline artifacts identify commit, toolchain, exact command, result, and
      pre-existing failures for cargo test -p claw-core and the claw-agent
      persistence/session/subagent/nested/stress matrices.
- [ ] Every behavior intended to remain stable has a characterization test or
      an explicitly named existing test.
- [ ] No characterization test freezes legacy direct-drive ownership, current
      poll order, or known unfairness.

Phase 0 distinguishes preserved behavior from intentionally changed mechanics.
A currently observable implementation detail is not automatically a
compatibility contract. No production behavior changes in this phase.

## 6. Phase 1 — Establish the Agent recovery and Factory boundary

### Goal

Give the bottom-level ownership object its final in-memory recovery semantics
and give Factory one create/restore construction path, without changing how an
Agent is currently driven or performing runtime checkpoints yet.

### Primary files

- claw-core/src/agent/base_agent.rs
- claw-core/src/agent/base_agent/
- claw-core/src/agent/event.rs
- claw-core/src/agent/factory/
- claw-core/src/protocol/
- claw-core/src/session/actor.rs
- claw-core/src/multiagent/
- claw-core/src/orchestrator/engine.rs

### Changes

- Keep BaseAgent<H, T>. Do not add F, SharedPersistence, AgentStateStore, or a
  persistence callback to BaseAgent.
- Add `AgentRecoverySnapshot` and stable
  `UnsettledToolCallRecord` types. Do not persist transient `TrackedToolCall`,
  a future, Waker, RunId, or checkout state.
- Add synchronous, I/O-free
  `BaseAgent::recovery_snapshot(&self) -> AgentRecoverySnapshot`.
- Implement it as a projection over authoritative live components, not a second
  mutable snapshot cache that can drift from mode, ToolSet, or the tool-call
  journal.
- Make BaseAgent coordinate a generic recovery projection over authoritative
  components: mode is owned by `AgentModeContextAdapter`, loaded groups by
  ToolSet, and monotonic next ToolCallId/unsettled records by the Agent tool
  runtime. BaseAgent must not match on concrete modes or adapters. Continue
  emitting the legacy `ToolStarted` compatibility update until Phase 7 wires
  the final durability barrier and deletes SessionActor's mirror.
- Give `FsAgentFactory<F, H, T>` explicit `create_new` and `restore` entry
  points that converge on one private BaseAgent builder. Factory loads or
  initializes recovery state and assembles transcript, tools, mode, and memory
  adapters; BaseAgent receives only the constructed dependencies/state.
- Keep F in Engine, SessionActor/AgentEnvironment, Factory, transcript, and
  AgentStateStore. Confirm it does not propagate into BaseAgent, AgentRun,
  AgentSlot, or Scheduler.
- Keep AgentPersistencePolicy outside BaseAgent in Session/slot construction
  metadata. Policy selects whether Factory loads a durable record; it is not an
  Agent dependency.
- Move the process-global AgentId allocator definition/state out of multiagent
  ownership. Engine owns its registered durable state and exposes only a local
  allocation handle to callers. Existing Multiagent construction may use that
  handle temporarily until Phase 6 removes allocation from Multiagent.
- Keep BaseAgent as the only concrete Agent type.
- Keep protocol-only values, such as IDs and outcomes, independent of F.
- Route Engine-owned AgentStateStore to Factory rather than injecting it into
  the constructed Agent. During this phase, restore may adapt the legacy
  SessionState representation; the new durable record/write migration does not
  activate until Phase 7.
- Mark any legacy SessionState-to-snapshot restoration adapter for deletion or
  schema migration in Phase 7.
- Do not change direct-drive behavior in this phase.

### Gate

- [ ] MemFs and disk-backed F can both create and restore BaseAgent<H, T>.
- [ ] BaseAgent, AgentRun, and the temporary slot chain have no F or
      SharedPersistence generic/dependency.
- [ ] A snapshot round trip preserves mode, loaded tool groups,
      next_tool_call_id, and every unsettled record.
- [ ] Snapshot serialization is deterministic where collection ordering has no
      semantic meaning.
- [ ] Factory create_new and restore share one invariant builder.
- [ ] Root and worker construction still share the expected assembled behavior.
- [ ] Engine is the only owner/registrar of the durable AgentId allocator.
- [ ] Allocator checkpoint failure exposes no ID and constructs no Agent.
- [ ] There is no dyn Agent, GenericAgent, persistence facade, or persistence
      callback stored in BaseAgent.
- [ ] Existing session, memory, and tool behavior matches the baseline.
- [ ] cargo test -p claw-core passes or matches the recorded baseline.

## 7. Phase 2 — Define the owned AgentRun protocol

### Goal

Make checkout and return mechanically safe before introducing the Scheduler.

### Protocol

The concrete names may adjust during implementation, but the ownership shape
must remain:

~~~text
AgentRunRequest<H, T> {
    run_id,
    agent_id,
    agent: BaseAgent<H, T>,
    opaque_return_route,
    opaque_checkpoint_route,
    run_input,
}

AgentRunCompletion<H, T> {
    run_id,
    agent_id,
    agent: BaseAgent<H, T>,
    outcome,
    opaque_return_route,
}

AgentRunUpdate::CheckpointRequired {
    run_id,
    agent_id,
    checkpoint_id,
    purpose,
    snapshot: AgentRecoverySnapshot,
}

CheckpointResult {
    run_id,
    checkpoint_id,
    result: Result<(), AgentCheckpointError>,
}
~~~

AgentRun is a transient wrapper for one checkout. It is not a second Agent type.
Submitting by value transfers physical ownership. A rejected submission returns
the original request, including its Agent.

### Changes

- Add or normalize AgentId, RunId, and ToolCallId newtypes.
- Introduce explicit request, update, completion, and submit-error types.
- Preserve ToolStarted as a compatibility update until Phase 7.
- Define the typed, snapshot-carrying `CheckpointRequired` update plus
  resume/fail control in the run protocol. This phase defines ownership
  behavior only; BaseAgent does not begin emitting the final tool durability
  barrier until Phase 7.
- Keep all request/update/completion/checkpoint protocol values independent of
  filesystem type F and concrete persistence errors.
- Make every terminal outcome consume the AgentRun and return BaseAgent.
- Make cancellation cooperative and ownership-preserving.
- Echo RunId and opaque return/checkpoint routing data without interpreting
  Session or persistence policy.
- Reject stale or duplicate completion when RunId does not match the slot.
- Keep the existing direct driver temporarily, but make it use the same owned
  completion contract.

### Gate

- [ ] Resident → CheckedOut(run_id) → Resident works.
- [ ] Duplicate checkout is rejected.
- [ ] Rejected submit returns the exact original Agent.
- [ ] Every terminal outcome currently emitted by AgentRun—success, LLM/tool
      failure, cancellation, and shutdown—returns the Agent exactly once.
- [ ] The protocol can carry a typed persistence failure without losing the
      Agent; production emission and failpoint coverage activate in Phase 7.
- [ ] A late completion for an old RunId cannot mutate the current slot.
- [ ] A non-Clone drop probe proves there is no copy, leak, or double drop.

## 8. Phase 3 — Build the process-global Scheduler in isolation

### Goal

Implement the physical fair scheduling layer without yet changing production
ownership.

### Suggested files

- claw-core/src/scheduler/mod.rs
- claw-core/src/scheduler/handle.rs
- claw-core/src/scheduler/queue.rs
- claw-core/src/scheduler/run.rs
- claw-core/src/lib.rs

### Changes

- Add AgentRunScheduler<H, T>, owned by Engine.
- Add a single-thread-local SchedulerHandle or submission mailbox.
- The handle accepts a moved AgentRunRequest and supports cancellation; it does
  not poll runs.
- The Scheduler drains submissions, owns all active AgentRuns, tracks readiness,
  and emits updates/completions.
- Use a rotating cursor or ready queue with a fixed per-tick budget.
- Poll each ready run at most once per fair sweep.
- Implement park/resume/fail handling for a scripted CheckpointRequired update.
  This verifies Scheduler protocol machinery only; production Agent-exported
  tool checkpoints activate in Phase 7.
- Never busy-poll a Pending future that has not been woken.
- Do not use Arc<Mutex<AgentRunScheduler>>, spawn a thread, create another
  runtime, or require AgentRun to be Send.
- Keep scheduler imports free of session and multiagent types.

The exact queue implementation is secondary to the observable contract:
all ready runs remain concurrently registered and are fairly polled by one
central loop. No caller drives an Agent to completion.

### Gate

- [ ] Three always-ready scripted runs advance A, B, C, A, B, C.
- [ ] A self-waking hot run cannot starve the other runs.
- [ ] A Pending run does not block ready runs and is not busy-polled.
- [ ] Each tick respects a configured poll budget.
- [ ] A scripted checkpoint update parks its run until explicit resume or fail.
- [ ] All terminal paths return their Agent exactly once.
- [ ] An Rc<RefCell<_>>-capturing, non-Send test run works.
- [ ] All recorded polls occur on the same ThreadId.
- [ ] Scheduler source has no SessionId or Multiagent dependency.

## 9. Phase 4 — Cut production execution over to the Scheduler

### Goal

Make the global Scheduler the only physical Agent driver.

### Changes

- Add the Scheduler and its handle/mailbox to Engine.
- Change the Engine loop to a bounded, rotating sweep:
  1. process a bounded ingress batch;
  2. poll SessionActors by a bounded budget;
  3. drain Scheduler submissions;
  4. run one global fair Scheduler sweep;
  5. route run updates/completions;
  6. preserve the existing ToolStarted/top-level persistence ordering, service
     dormant checkpoint routing infrastructure, and process bounded outgoing
     work.
- Route completions through opaque reply ports/tokens. Scheduler must not decode
  SessionId.
- Convert the current execution slot from Idle/Running(AgentRun) into the
  Resident/CheckedOut model.
- A root-only vertical slice may be used for one short commit to validate
  wiring, but root and worker production execution must be migrated in the same
  phase.
- Remove direct AgentRun polling from NextAgentEvents and every per-session
  poller as soon as the cutover is complete.
- On completion, restore the slot first; only then interpret the business
  outcome or issue follow-up domain effects.
- On delete/close, mark a checked-out slot Cancelling/Reaping, request
  cancellation, wait for Agent return, and only then remove the slot.

Phase 4 preserves the observable rule that a tool body does not continue in the
same top-level poll that reports ToolStarted. It does not yet delete
SessionActor's legacy unsettled persistence mirror. Phase 7 replaces this
compatibility path with Agent-exported CheckpointRequired barriers.

### Gate

- [ ] Production code has exactly one call site family that polls AgentRun, all
      under scheduler/.
- [ ] Roots and workers from different sessions make alternating progress.
- [ ] Fairness is measured per runnable AgentRun; tests do not accidentally
      require equal per-Session quotas.
- [ ] A continuously-ready Engine work class cannot prevent later work classes
      from receiving their configured budget.
- [ ] An always-ready ingress source cannot starve Agents or outgoing work.
- [ ] A failing Agent does not stop another Agent, Session, or the global loop.
- [ ] Session close and shutdown reclaim every checked-out Agent.
- [ ] No Agent is reachable by both the legacy driver and Scheduler.

This phase is the first externally visible execution cutover. Do not continue
while a dual-driver state remains.

## 10. Phase 5 — Move AgentSlots into SessionActor

### Goal

Separate logical session ownership from Multiagent domain state.

### Suggested files

- claw-core/src/session/agent_slots.rs
- claw-core/src/session/actor.rs
- claw-core/src/session/state.rs
- claw-core/src/session/persistence.rs
- claw-core/src/multiagent/agents.rs
- claw-core/src/multiagent/drive.rs
- claw-core/src/multiagent/lifecycle.rs

### Changes

- Introduce an AgentSlots<H, T> collection owned only by SessionActor.
- Move root and worker slot ownership out of MultiagentRuntime.
- Keep enough metadata in CheckedOut for:
  - AgentId, kind, parent/recovery relation, and status;
  - current RunId;
  - inbox or queued signals;
  - pending cancellation/deletion;
  - overview projection.
- Validate AgentId plus RunId before accepting a completion.
- Restore the Agent to Resident before applying its outcome.
- Keep a cancelling/reaping slot visible until physical ownership returns.
- Make session close/shutdown await or drive reclamation of all checked-out
  slots without blocking the global loop.
- Remove RuntimeExecution/take_runtime paths that move or own the entire
  MultiagentRuntime. If a residual non-owning wrapper is needed for the Phase 5
  cutover, mark it for mandatory deletion in Phase 6.

### Gate

- [ ] SessionActor is the only owner of AgentSlots.
- [ ] MultiagentRuntime owns no BaseAgent, AgentRun, or AgentSlot.
- [ ] No RuntimeExecution/take_runtime path moves or owns physical
      Multiagent/Agent state.
- [ ] A checked-out Agent remains visible in agents_overview().
- [ ] Stale completion cannot overwrite a newer Resident or CheckedOut state.
- [ ] Delete removes a slot only after the Agent has returned.
- [ ] Closing one Session does not stall Agents in another Session.

## 11. Phase 6 — Make Multiagent a domain plugin behind a bridge

### Goal

Remove the circular physical dependency while preserving Multiagent behavior as
an Agent tool.

### Part A: typed bridge and pure domain effects

Define tool-facing commands and domain effects. Exact names may evolve; their
responsibilities must remain separated.

~~~text
tool command examples
├── SpawnRequested
├── FollowupRequested
├── CancelRequested
├── DeleteRequested
└── OverviewRequested

domain effect examples
├── SpawnAgent
├── DeliverMessage
├── CancelRun
├── DeleteSubtree
├── PublishResult
└── ArmTimeout
~~~

- MultiagentBridge transports typed commands; it does not expose SessionActor
  or Scheduler objects.
- Multiagent validates graph state, policy, joins, parent/child rules, and
  timeouts, then emits typed effects.
- Every effect that requires a physical resource carries an opaque correlation
  ID. Multiagent may record a pending domain transition, but it does not publish
  a live child or completed lifecycle transition until it receives the matching
  effect result.
- Multiagent domain tests run without constructing a physical Agent.

### Part B: SessionActor executes physical effects

- After accepting a root creation or typed Multiagent spawn effect, SessionActor
  obtains the next durably reserved AgentId from the Engine-owned process-global
  allocator.
- SessionActor selects session, persistence, baked, memory-visibility, tool, and
  context policy and builds one AgentEnvironment<F>.
- FsAgentFactory is the sole concrete BaseAgent constructor. Its create/restore
  entries consume that environment and converge on one invariant builder; it
  does not choose Session, parent, graph, lifecycle, or memory-visibility
  policy.
- SessionActor inserts the completed BaseAgent into AgentSlot and submits its
  checkout. Root and worker creation call this same assembly function with
  explicit policy inputs.
- SessionActor returns a typed effect result containing the correlation ID and
  either the committed AgentId/outcome or a failure. Multiagent then commits or
  rolls back its pending graph transition and answers the waiting tool call.
- Disable the Multiagent tool cleanly when its plugin/config is absent.

### Gate

- [ ] multiagent imports/owns none of BaseAgent, AgentRun, AgentSlot,
      FsAgentFactory, AgentRunScheduler, AgentIdAllocator, or SessionId.
- [ ] A command cannot create an Agent before SessionActor accepts and executes
      its typed effect.
- [ ] Allocation persistence failure returns a typed root/spawn failure without
      constructing or inserting an Agent.
- [ ] Spawn failure leaves no live graph child, AgentSlot, or unanswered bridge
      request; success publishes the same AgentId to graph, slot, and tool result.
- [ ] Spawn, follow-up, delete, cancel, timeout, and nested join behavior pass.
- [ ] Root and worker assembly share one code path.
- [ ] A normal root-only Session works when the Multiagent tool is disabled.
- [ ] SessionActor contains physical effect handling but no duplicated graph
      policy.
- [ ] RuntimeExecution/take_runtime compatibility wrappers are deleted.

This phase resolves the apparent Scheduler/Multiagent dependency cycle:
Multiagent never calls Scheduler. It emits intent; SessionActor owns the Agent
and submits the physical run.

## 12. Phase 7 — Activate Agent recovery checkpoints

### Goal

Connect the recovery state established in Phase 1 to the parked-run protocol
and make interrupted tool-call reasoning durable without giving BaseAgent a
persistence dependency.

### Suggested files

- claw-core/src/agent/recovery.rs
- claw-core/src/agent/base_agent.rs
- claw-core/src/agent/base_agent/
- claw-core/src/agent/iteration_loop/tool_round.rs
- claw-core/src/agent/event.rs
- claw-core/src/agent/factory/
- claw-core/src/agent/base_agent/context_adapter.rs
- claw-core/src/agent/base_agent/transcript.rs
- claw-core/src/agent/factory/transcript.rs
- claw-core/src/protocol/tool.rs
- claw-core/src/session/persistence.rs
- claw-core/src/session/actor.rs
- claw-core/src/orchestrator/engine.rs
- claw-core/src/scheduler/
- claw-memory/src/transcript_store.rs

### Recovery schema and ownership

~~~text
AgentRecoverySnapshot {
    mode: AgentMode,
    loaded_tool_groups: Vec<ToolGroupId>,
    next_tool_call_id: ToolCallId,
    unsettled_toolcalls:
        BTreeMap<ToolCallId, UnsettledToolCallRecord>,
}
~~~

- AgentId is the collection key. The stored record wraps the snapshot in an
  explicit schema version.
- `UnsettledToolCallRecord` contains stable identity and recovery facts only.
  Runtime `TrackedToolCall`, futures, Wakers, Scheduler state, and checkout
  state are never serialized.
- Two calls with equal tool names and arguments remain distinct. ToolCallId is
  monotonic within AgentId. Allocation, counter increment, and unsettled
  insertion are one in-memory transition before snapshot export.
- BaseAgent coordinates the generic snapshot export. Each component mutates and
  contributes its own live semantics; BaseAgent does not inspect concrete
  adapter state. It owns no SharedPersistence, DurableState, AgentStateStore,
  filesystem generic, or persistence callback.
- Engine owns `AgentStateStore<F>` and runtime checkpoint I/O.
  `FsAgentFactory<F, H, T>` uses that store only for initial create/restore and
  injects the restored ordinary state into `BaseAgent<H, T>`.
- SessionActor retains recovery policy with slot metadata and authorizes
  permanent record removal after the Agent returns and logical deletion is
  committed.

### Checkpoint protocol

~~~text
AgentRunUpdate::CheckpointRequired {
    agent_id,
    run_id,
    checkpoint_id,
    purpose,
    snapshot: AgentRecoverySnapshot,
}

CheckpointResult {
    run_id,
    checkpoint_id,
    result: Result<(), AgentCheckpointError>,
}
~~~

- Because the active BaseAgent is owned inside AgentRun, it calls
  `recovery_snapshot()` before yielding; Engine never borrows a running Agent.
- `checkpoint_id` is transient and scoped to one RunId. It matches one parked run
  with one acknowledgement and is not a persisted generation.
- BaseAgent releases every mutable borrow before emitting
  `CheckpointRequired`. No persistence guard exists in BaseAgent.
- Scheduler parks the run and never polls it until Engine returns the matching
  `CheckpointResult`. It forwards but never inspects the snapshot or policy.
- For PersistentRoot, success means the exported snapshot is durable. For
  EphemeralRoot and TransientWorker, Engine may acknowledge the same boundary
  without storage; they receive no restart guarantee.
- Every recovery-relevant mutation must eventually be covered by an
  acknowledgement. Ordinary mode/loaded-group updates may be coalesced, but an
  AgentRun cannot publish terminal completion while its recovery state differs
  from the last acknowledged snapshot. The pre-tool checkpoint cannot be
  coalesced past the first tool-body poll.
- A failed batched flush conservatively fails every barrier included in that
  flush. It does not fail unrelated later barriers.
- Stale RunId/checkpoint_id acknowledgements are rejected.
- Checkpoint failure resumes the AgentRun with Err; BaseAgent converts it to a
  typed outcome and the normal completion path returns the Agent.
- Scheduler implements only park/resume/fail mechanics. Engine and
  AgentStateStore own persistence policy and flush semantics. Do not add a
  PersistenceCoordinator solely for this flow.

### Persistent tool start order

~~~text
allocate ToolCallId and increment next_tool_call_id
→ insert UnsettledToolCallRecord in BaseAgent live state
→ BaseAgent calls recovery_snapshot()
→ emit CheckpointRequired carrying the snapshot
→ Scheduler parks this AgentRun
→ Engine durably stores the PersistentRoot snapshot
→ matching CheckpointResult succeeds
→ resume and first-poll the tool body
~~~

If the checkpoint fails, the tool body is never polled. EphemeralRoot and
TransientWorker perform the same state transition and yield boundary but Engine
acknowledges it without a durable Agent write.

### Tool outcome and transcript settlement

The transcript groups intermediate tool patches inside one open turn. An open
turn is not a durable settlement boundary:

~~~text
tool body completes
→ append tool outcome to the open transcript turn
→ keep the call unsettled across later LLM/tool iterations
→ terminal or explicit turn commit
→ fallibly checkpoint the committed transcript
→ clear calls represented by that committed turn in BaseAgent
→ export the new AgentRecoverySnapshot
→ checkpoint or acknowledge it through the same parked-run protocol
~~~

- Extend Transcript with a fallible checkpoint API that guarantees committed
  records through the requested transcript version are durable.
- Best-effort flush or later inspection of `last_persist_error` is insufficient
  for settlement.
- Do not force an intermediate transcript commit merely to clear a tool call.
- If the turn is cancelled/discarded after a tool body ran, its persistent
  unsettled record remains Unknown until explicit recovery/reconciliation.
- A crash after transcript durability but before the final Agent snapshot is
  durable may conservatively recover the call as unsettled; it is never replayed
  blindly.

- Keep ToolRunner internal to BaseAgent. Remove SessionActor
  `record_tool_started`, add/remove inflight bookkeeping, the Phase 1 legacy
  mirror, and turn-end settlement heuristics.
- Use “active/inflight” only for transient futures and “unsettled” for the
  recovery fact that a side-effect outcome is not durably known.
- Preserve transcript, profile, and long-term memory as separate canonical
  stores.

### Creation, migration, deletion, and rollback

- Version the persisted Agent recovery record.
- Persistent root creation uses Factory to construct a new Agent and initial
  snapshot, durably stores that snapshot, then publishes `root_agent_id` and
  canonical-store identities in SessionState.
- Factory restore loads the versioned snapshot and canonical-store identities,
  then calls the same invariant builder used by create_new.
- Read the old SessionState representation long enough to migrate PersistentRoot
  state idempotently:
  1. construct the versioned Agent recovery record from legacy fields;
  2. durably checkpoint the new record;
  3. publish `root_agent_id` and canonical-store identities in SessionState
     while removing embedded legacy Agent fields;
  4. durably checkpoint SessionState.
- Recovery removes unreferenced Agent records left by a crash during creation or
  deletion. A referenced missing/corrupt Agent record is a typed recovery error.
- Permanent Session deletion durably removes its root link before removing the
  corresponding Agent record. BaseAgent never deletes its own record.
- EphemeralRoot and TransientWorker create no durable Agent record.
- If rollback to an old binary is required, retain dual-write compatibility for
  a defined release window; otherwise mark this phase as the hard rollback
  boundary and document it in release notes.

### Gate

- [ ] Two identical tool name/argument calls receive distinct ToolCallIds and
      are independently tracked.
- [ ] BaseAgent, AgentRun, AgentSlot, and Scheduler contain no F,
      SharedPersistence, DurableState, or AgentStateStore dependency.
- [ ] ToolCallId is not reset or reused after PersistentRoot restart.
- [ ] If the pre-execution checkpoint fails, the tool body is polled zero times.
- [ ] A stale checkpoint acknowledgement cannot resume a run.
- [ ] A failed batched flush fails every participating waiter without polling
      any affected tool body.
- [ ] A crash after start checkpoint but before durable outcome recovers
      Unknown/unsettled and does not auto-replay.
- [ ] Intermediate open-turn patches do not clear unsettled calls.
- [ ] Transcript checkpoint failure leaves calls unsettled.
- [ ] A call is cleared only after its committed turn is durably checkpointed.
- [ ] A mode or loaded-group change with no following tool is checkpointed
      before terminal completion.
- [ ] Persistence failure returns the Agent and does not break global progress.
- [ ] The checkpoint API structurally releases mutable borrows before yielding,
      and Engine holds no persistence guard while polling Agent code.
- [ ] SessionState no longer owns Agent mode, loaded groups, or unsettled calls.
- [ ] Old SessionState migration is idempotent after a crash at every migration
      write boundary.
- [ ] A dangling root link is a typed error; an unreferenced Agent record is
      reconciled as garbage.
- [ ] Permanent Session deletion removes PersistentRoot Agent state.
- [ ] EphemeralRoot and TransientWorker create no durable Agent record.
- [ ] Crash-window tests cover: before start checkpoint; after start/before tool;
      after side effect/before turn commit; after transcript commit/before clear;
      and after the final Agent-state checkpoint.
- [ ] Root recovery remains compatible with the policy frozen in Phase 0.

## 13. Phase 8 — Close overview, assembly, and memory boundaries

### Goal

Expose a complete system view and freeze memory visibility before optimization.

### Agent overview

- agents_overview() reads only AgentSlot metadata.
- SessionActor updates its projection on insert, checkout, cancellation, return,
  failure, and deletion.
- Orchestrator aggregates SessionActor snapshots across sessions.
- If a future AgentDirectory cache is needed for lookup performance, it is a
  rebuildable read model only. It never owns an Agent and is updated from slot
  lifecycle events.
- CheckedOut and Cancelling Agents remain visible.
- Aggregate output has stable ordering and globally unique AgentIds.

### Memory visibility contract

Phase 0 already records the pre-refactor characterization matrix. Phase 8
promotes the intended behavior into the enforced target contract and explicitly
documents any deliberate difference:

- each Agent sees its own transcript and its own conversation-history
  projection;
- a worker does not automatically see parent or sibling transcripts;
- inherited context is an explicit immutable spawn-time input;
- profile storage may be shared, while baked worker policy disables forbidden
  replace/clear/write operations;
- long-term memory retains the current global plus Agent-kind visibility rules;
- skills and tools are filtered by the baked configuration;
- root and worker construction use the same ContextAdapter-based assembly path.
- adapter-owned ToolGroups are co-located with their adapter; `agent/tools`
  contains only pure Agent groups with no adapter domain owner, one group per
  file.

Do not optimize context size in this phase. Test both visibility and mutation
isolation: request snapshots alone cannot prove profile/LTM write permissions or
persistent-versus-ephemeral transcript durability. The output is an observable
contract that later optimization must preserve or intentionally revise.

### Gate

- [ ] Overview covers resident, checked-out, waiting, cancelling, and failed
      Agents across all Sessions.
- [ ] Overview does not borrow the Agent body or require Agent return.
- [ ] There is no second owning Agent directory.
- [ ] Request-body snapshots prove the intended context and tool visibility for
      root, parent, child, and sibling cases.
- [ ] Profile and long-term-memory read/write tests prove cross-Agent and
      cross-Session isolation and baked worker restrictions.
- [ ] PersistentRoot, EphemeralRoot, and TransientWorker transcript durability
      matches AgentPersistencePolicy.
- [ ] Checkout/return does not clone transcript or context adapters.
- [ ] Root and worker use one assembly path with explicit policy differences.

## 14. Phase 9 — Audit deletions and verify the architecture

### Deletion audit and final cleanup

The following are Phase 9 entry conditions, not work deferred to Phase 9:

- Phase 4 already deleted NextAgentEvents direct polling, every per-session
  AgentRun poller, and AgentExecution::Running(AgentRun);
- Phase 5 already removed AgentSlot, BaseAgent, and AgentRun ownership from
  MultiagentRuntime;
- Phase 6 already removed Multiagent-owned FsAgentFactory/physical scheduling,
  AgentId allocation, duplicate root/worker assembly, and all
  RuntimeExecution/take_runtime wrappers;
- Phase 7 already removed SessionActor inflight/unsettled bookkeeping and
  turn-end settlement heuristics.

Phase 9 fails immediately if any item above remains. It may delete only residual
non-owning compatibility machinery whose earlier gate explicitly allowed it to
survive:

- stale comments that describe Multiagent as the physical scheduler;
- transitional adapters, duplicate protocol aliases, and compatibility variants
  whose explicit deletion phase has arrived.

Phase 9 must not introduce or finish an ownership migration that belonged to
Phases 4–7.

### Static dependency audit

- scheduler has no session or multiagent imports;
- multiagent has no physical agent, session, scheduler, or orchestrator imports;
- BaseAgent/iteration-loop code has no session, multiagent domain,
  orchestrator, AgentStateStore, SharedPersistence, or filesystem-type imports;
- Agent Factory may depend on construction-time recovery and canonical stores;
- only scheduler code polls AgentRun;
- only SessionActor owns AgentSlots;
- no Arc<Mutex<AgentRunScheduler>> and no Scheduler-created thread/runtime.

### Final verification

- [ ] cargo fmt --check
- [ ] cargo clippy --workspace --all-targets
- [ ] cargo test --workspace
- [ ] claw-agent persistence and recovery matrices
- [ ] claw-agent session lifecycle and stress matrices
- [ ] foreground/background spawn, follow-up, delete, cancel, interrupt, timeout,
      and nested-subagent join matrices
- [ ] deterministic manual-poll Scheduler fairness tests
- [ ] injected thread/runtime audit proves Scheduler created no additional OS
      thread, task runtime, or blocking executor
- [ ] ownership/drop-probe tests for every terminal outcome
- [ ] persistence failpoint tests for every tool-call crash window
- [ ] memory-visibility request snapshots plus write/isolation tests
- [ ] target device build

## 15. Cross-phase engineering rules

- Never hold a lock, RefCell borrow, persistence guard, or slot collection
  borrow while polling or awaiting user/tool/LLM code.
- Move a BaseAgent across the ownership boundary; do not clone it.
- `recovery_snapshot()` is synchronous and I/O-free. Only its owned projection
  crosses a running Agent boundary; Engine never borrows a checked-out Agent.
- Keep cancellation explicit and ownership-preserving.
- Treat all Agent/tool/LLM/persistence failures as data returned in a run
  outcome. They must not tear down the global loop.
- Use deterministic scripted futures and manual polling in Scheduler tests;
  avoid sleep-based fairness tests.
- Keep poll and command budgets explicit so starvation behavior is testable.
- Keep synchronous work performed by each poll bounded; cooperative fairness
  cannot compensate for one poll that blocks the only OS thread.
- Do not mix unrelated cleanup into a migration phase. Remove technical debt
  encountered on the ownership path, but keep each diff reviewable.
- Do not retain two production drivers, two Agent owners, or two assembly paths
  as a “temporary” final state.
- Do not begin context/memory optimization until the Phase 9 gates pass.

## 16. Rollback boundaries

- Phases 1–3 add contracts and isolated machinery; rollback is code-only.
- Phase 4 is the execution cutover. Keep the previous driver only within the
  short cutover commit series, never active for the same Agent, and delete it
  before completing the phase.
- Phases 5–6 move ownership and domain boundaries without intentionally changing
  persisted schema.
- Phase 7 is the durable schema boundary. Rollback requires the explicitly
  chosen dual-read/dual-write window or a forward migration; it must not rely on
  restoring physical futures.
- Phases 8–9 contain no new ownership migration; they close observable
  contracts, audit earlier deletions, and remove only explicitly allowed
  non-owning compatibility code.

## 17. Definition of done

The grand refactor is complete only when all of the following are true:

1. Engine owns one global Scheduler, and all running Agents are fairly polled
   through it on one OS thread.
2. SessionActor is the sole logical owner of AgentSlots; Scheduler is only the
   temporary physical custodian of checked-out BaseAgents.
3. Every AgentRun terminal path returns its BaseAgent exactly once.
4. Multiagent is a pluggable tool/domain component with no physical Agent or
   Scheduler ownership.
5. Root and worker Agents are assembled through one SessionActor path and the
   create/restore entry points of one FsAgentFactory invariant builder.
6. Engine owns the durable process-global AgentId allocator; Multiagent never
   allocates an ID.
7. BaseAgent<H, T> coordinates a generic recovery projection from its
   authoritative components and exports a complete AgentRecoverySnapshot,
   while Factory/Engine own restore/checkpoint I/O and F does not propagate
   into AgentRun, AgentSlot, or Scheduler.
8. PersistentRoot recovery follows the durable root link, while EphemeralRoot
   and TransientWorker create no durable Agent record.
9. Transcript settlement has a fallible durability acknowledgement and never
   clears a call from an open, uncommitted turn.
10. agents_overview() reports all Agents from slot metadata without creating a
   second owner.
11. The transient/durable boundary survives crash-window and recovery tests.
12. The legacy per-session driver, duplicate assembly, and SessionActor
   tool-call persistence paths are deleted.
13. The complete final verification gate passes.

Only after this point should priority scheduling, Agent-internal parallel tool
runs, worker durable recovery, or LLM context/memory optimization begin.
