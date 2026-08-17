//! Typed physical effects emitted by the Multiagent domain.

use barracuda_agent::AgentId;
use barracuda_agent::Message;

use super::model::{SubagentSpec, SubagentTimeout};

/// Ephemeral correlation key for one physical Multiagent effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EffectId(pub u64);

/// Physical work requested by the Multiagent domain.
pub enum MultiagentEffect {
    Spawn {
        id: EffectId,
        spec: SubagentSpec,
    },
    Dispatch {
        id: EffectId,
        target: AgentId,
        message: Message,
    },
    Interrupt {
        id: EffectId,
        target: AgentId,
    },
    RemoveAgents {
        id: EffectId,
        agents: Vec<AgentId>,
    },
    ArmTimeout {
        id: EffectId,
        agent: AgentId,
        timeout: SubagentTimeout,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Accepted,
    Busy,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptOutcome {
    Accepted,
    Inactive,
    Missing,
}

/// Correlated result of one physical Multiagent effect.
pub enum MultiagentEffectResult {
    Spawned {
        id: EffectId,
        spec: SubagentSpec,
        outcome: Result<AgentId, MultiagentPhysicalError>,
    },
    Dispatched {
        id: EffectId,
        outcome: DispatchOutcome,
    },
    Interrupted {
        id: EffectId,
        outcome: InterruptOutcome,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct MultiagentPhysicalError {
    detail: String,
}

impl MultiagentPhysicalError {
    pub fn new(detail: String) -> Self {
        Self { detail }
    }
}
use alloc::{string::String, vec::Vec};
