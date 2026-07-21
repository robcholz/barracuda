# Architecture

By Finn(Ziheng) Sheng <zsheng2@ncsu.edu> or <robcholz00@gmail.com>

## Hard Constraints

SINGLE OS-THREAD ONLY.

## Overview

### Data Ownerships

Agent System
  -> Orchestrator (persists id allocators data here with claw-persistence)
  -> Session Actor (persists session data here when creating sessions, with claw-persistence)
    -> exposes agents_overview() so orchestrator can collect these info
    -> owns agent instances, Vec<AgentSlot>
    -> adds tools (Multiagent is used here. note: all the tools will be filtered by baked config here (e.g. profile write will be disabled for subagent from the baked config)), context, memories (Memory Component is used here), and inter-agent communications (use actor patterns to transfer signals between agents). (Note: persists root agent data (agent id and related transcript, agent mode, loaded tool groups, inflight toolcalls) when spwaning, and resume from here)
    -> Moves Agents to AgentRunScheduler, at the same time AgentSlot (agent part becomes CheckedOut), the enum quite like Instance(Agent), CheckedOut

AgentRunScheduler
  -> runs agents
  -> knows no Session or Multiagent semantics\
  -> fairly polls all checked-out AgentRuns across sessions
  -> exactly one process-global instance

Multiagent: behaves like a tool
  -> calls MultiagentBridge to get requested agent spawns (which connects to SessionActor. NOTE: SessionActor receives the commends from here, but never implements multiagent domain logics)
  -> applies graph/scheduling semantics to the agents
  -> calls MultiagentBrdige to let Session Actor move agent instances to AgentRunScheduler

Memory Components
  -> use ContextAdapter trait to decouple from (claw-core/agent)
  -> transcript store (transcript is a trait, the impls have durable or in-memory) (exposed in claw-core/agent, used in claw-core/memory). Use fs apis to persist.
  -> conversation history (projection of transcripts) in (claw-core/memory)
  -> long term memory, keep as current-is, uses claw-persistence to persist.
  -> identity.md, soul.md, user.md, keep as current-is, also uses ContextAdapter. Use fs apis to persist.
  -> skills (connects to claw-skill by ContextAdapter)

Tool Registry
  -> keep as current-is, uses claw-persistence from claw-agent

### Dataflows

Orchestrator Loop
  -> processes all incoming submits by one tick
  -> drives all sessions by one tick
  -> drives all agents by one tick
  -> processes all outcoming data by one tick

### Filesystem Level Organization

claw-core
  -> agent (single agent src, exposes AgentFactory to construct agents)
  -> config (crate-level configurations)
  -> memory (exposes memory as adapters, use traits to decouple from `agent`)
  -> multiagent (exposes as toolcall)
  -> scheduler (exposes AgentRunScheduler)
  -> session (exposes SessionActor)
  -> orchestrator (assembles all logics here and introduce persistence here)

### Transient vs Durable

- Checked-out `AgentRun`s and all `AgentRunScheduler` queues are transient runtime state and are never persisted.
- Durable `SessionActor` state and persisted `AgentSlot` metadata are the recovery source for agents.
- Transcripts, profiles, and long-term memory are independent canonical stores; session checkpoints reference their identities instead of embedding duplicate copies.
- After a crash or restart, agents are rebuilt from durable state and canonical stores; physical checkout state and in-flight futures are never restored.
- Agent, tool, and LLM failures are converted into run outcomes and returned to the owning `SessionActor`; they must not terminate the process-global loop or lose agent ownership.
