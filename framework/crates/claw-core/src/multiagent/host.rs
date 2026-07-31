//! Physical host capabilities required by Multiagent effects.

use claw_tool::ToolGroup;

use crate::agent::{AgentId, AgentKind};
use crate::Message;

use super::effect::{DispatchOutcome, InterruptOutcome, MultiagentPhysicalError};
use super::model::SubagentTimeout;

/// Session-owned physical operations available to the Multiagent orchestrator.
pub(crate) trait MultiagentHost {
    fn allocate_agent_id(&mut self) -> AgentId;

    fn create_agent(
        &mut self,
        agent: AgentId,
        kind: &AgentKind,
        extension_tools: Vec<ToolGroup>,
    ) -> Result<(), MultiagentPhysicalError>;

    fn rollback_agent(&mut self, agent: AgentId);

    fn dispatch_agent(&mut self, target: AgentId, message: Message) -> DispatchOutcome;

    fn interrupt_agent(&mut self, target: AgentId) -> InterruptOutcome;

    fn begin_remove_agents(
        &mut self,
        agents: Vec<AgentId>,
    ) -> Vec<(AgentId, Result<(), MultiagentPhysicalError>)>;

    fn arm_timeout(&mut self, agent: AgentId, timeout: SubagentTimeout);
}
