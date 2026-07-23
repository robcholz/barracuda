//! Per-session agent runtime: the **graph + scheduler + lifecycle**.
//!
//! Each session owns one [`MultiagentRuntime`]. It holds:
//! - runtime graph state — root plus node topology and lifecycle metadata;
//! - runtime scheduler state — ready work and approvals;
//! - stable agent slots — each slot owns its resident agent or its checked-out run;
//! - one subagent host that owns tool commands and the inspection read model.
//!
//! Responsibility line: the instance decides *when* agents run, *what* their run
//! outcomes mean (bubble a subagent result to its parent vs. surface a root
//! reply), and *what happens to their lifetimes*. Agent slots only store; agents
//! only compute.
//!
//! Sessions are isolated — one session's agents never appear in another's store —
//! while a single global id allocator (shared at construction) keeps every
//! [`AgentId`] unique across the whole process. The root is built lazily on the
//! first delivered message (that message is its goal); later messages are
//! accepted only once the root has returned to an idle boundary.
//!
//! Borrow safety: orchestrator-owned multiagent tools submit semantic commands
//! through their private port. The instance starts ready agents as owned
//! futures, then — with no agent borrowed — drains and applies those commands
//! before routing completed outcomes.

mod agent_control;
mod agents;
mod approval;
mod construction;
mod drive;
mod drive_control;
mod id_allocator;
mod lifecycle;
mod model;
mod pending_deliveries;
mod policy;
mod state;
mod timeouts;
mod tool_port;
mod tools;

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_permission::PermissionPolicy;

use crate::agent::{AgentManager, PersistenceConfig};
use crate::config::ReasoningEffort;
use crate::protocol::AgentId;
use crate::protocol::ToolCall;

pub(crate) use self::agent_control::MultiagentDeliverError;
use self::agents::AgentSlots;
pub(crate) use self::approval::ApprovalResolutionError;
pub(crate) use self::drive::{DriveOutcome, DriveOutput, TurnStopMode};
pub(crate) use self::drive_control::{DriveControl, DriveStop};
pub(crate) use self::id_allocator::{AgentIdAllocator, AgentIdAllocatorState};
use self::model::SubagentResult;
use self::pending_deliveries::PendingDeliveries;
pub(crate) use self::state::{MultiagentState, MultiagentWork};
use self::timeouts::AgentTimeouts;
use self::tool_port::MultiagentBridge;

/// Graph placement translated into a generic single-agent environment during
/// construction. This orchestration type never enters `crate::agent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentPlacement {
    FreshRoot(PersistenceConfig),
    RestoredRoot,
    Child,
}

/// One session's agent store, graph, scheduler, and root.
pub(crate) struct MultiagentRuntime<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    /// Builds agents (root and children). Owned here; slots only store.
    manager: Rc<AgentManager<Filesystem, Http, Timer>>,
    /// Session-owned policy propagated unchanged to every built agent.
    permission_policy: Arc<dyn PermissionPolicy>,
    /// Session default copied into each newly assembled Agent.
    reasoning_effort: ReasoningEffort,
    /// Durable root identity waiting to be materialized by the AgentManager.
    restored_root: Option<AgentId>,
    root_deliveries_in_turn: Vec<AgentId>,
    root_background_spawns: BTreeMap<AgentId, ToolCall>,
    /// Shared, process-wide id allocator for roots and spawned children.
    id_allocator: AgentIdAllocator,
    /// Process-local graph and scheduler state. Agents are not restored.
    state: MultiagentState,
    /// Stable slots for live graph nodes. Each slot owns the agent in both its
    /// idle and running forms.
    slots: AgentSlots<Http, Timer>,
    /// Process-local deadline futures, one for every live non-root node.
    timeouts: AgentTimeouts,
    /// One-shot completions for foreground spawns. They are process-local: an
    /// active tool future owns the matching receiver.
    foreground_results: BTreeMap<AgentId, async_channel::Sender<SubagentResult>>,
    /// Inspection tombstones for background results queued in a parent inbox
    /// but not yet activated into that parent's agent context.
    pending_deliveries: PendingDeliveries,
    /// The only boundary exposed to subagent tools. It owns their pending
    /// commands and read-only inspection projection.
    multiagent: Arc<MultiagentBridge>,
}
