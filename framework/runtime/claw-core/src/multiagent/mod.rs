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
//! Borrow safety: Multiagent-owned tools submit semantic commands
//! through their private port. The instance starts ready agents as owned
//! futures, then — with no agent borrowed — drains and applies those commands
//! before routing completed outcomes.

mod model;
mod policy;
mod state;
mod tool_port;
mod tools;

pub(crate) use self::model::{
    MultiagentSnapshot, SubagentResult, SubagentSnapshot, SubagentStatus, SubagentTimeout,
    TranscriptText,
};
pub(crate) use self::state::{MultiagentState, MultiagentWork};
pub(crate) use self::tool_port::{MultiagentAction, MultiagentBridge, SpawnCommand};
pub(crate) use self::tools::tool_group;
