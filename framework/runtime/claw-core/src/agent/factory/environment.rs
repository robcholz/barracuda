use std::sync::Arc;

use claw_context::Block;
use claw_permission::PermissionPolicy;
use claw_tool::ToolGroup;

use crate::agent::{AgentState, InflightToolCall};
use crate::config::{ApiUsage, ReasoningEffort};

pub(crate) struct AgentResume {
    state: AgentState,
    inflight_toolcalls: Vec<InflightToolCall>,
}

impl AgentResume {
    pub(crate) fn new(state: AgentState, inflight_toolcalls: Vec<InflightToolCall>) -> Self {
        Self {
            state,
            inflight_toolcalls,
        }
    }

    pub(super) fn into_parts(self) -> (AgentState, Vec<InflightToolCall>) {
        (self.state, self.inflight_toolcalls)
    }
}

/// Transcript identity and storage for one independently-built agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TranscriptTarget {
    InMemory(u32),
    Persistent(u32),
}

impl TranscriptTarget {
    pub(super) fn id(self) -> u32 {
        match self {
            Self::InMemory(id) | Self::Persistent(id) => id,
        }
    }

    pub(super) fn persists(self) -> bool {
        matches!(self, Self::Persistent(_))
    }
}

/// Runtime capabilities supplied by the owner constructing one agent.
///
/// The agent factory does not interpret its owner's orchestration model.
/// Extension tools are ordinary tool groups by the time they cross this
/// boundary.
pub(crate) struct AgentEnvironment {
    pub(super) transcript: TranscriptTarget,
    pub(super) api_usage: ApiUsage,
    pub(super) permission_policy: Arc<dyn PermissionPolicy>,
    pub(super) extension_tools: Vec<ToolGroup>,
    pub(super) inherited_context: Vec<Block<'static>>,
    pub(super) reasoning_effort: ReasoningEffort,
    pub(super) resume: Option<AgentResume>,
}

impl AgentEnvironment {
    pub(crate) fn new(
        transcript: TranscriptTarget,
        api_usage: ApiUsage,
        permission_policy: Arc<dyn PermissionPolicy>,
        extension_tools: Vec<ToolGroup>,
        inherited_context: Vec<Block<'static>>,
        reasoning_effort: ReasoningEffort,
        resume: Option<AgentResume>,
    ) -> Self {
        Self {
            transcript,
            api_usage,
            permission_policy,
            extension_tools,
            inherited_context,
            reasoning_effort,
            resume,
        }
    }
}
