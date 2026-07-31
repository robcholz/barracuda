//! Execute typed Multiagent effects through a physical host port.

use super::component::Multiagent;
use super::effect::{MultiagentEffect, MultiagentEffectResult};
use super::host::MultiagentHost;

impl Multiagent {
    pub(crate) fn execute_effect(
        &mut self,
        effect: MultiagentEffect,
        host: &mut impl MultiagentHost,
    ) {
        match effect {
            MultiagentEffect::Spawn { id, spec } => {
                let agent = host.allocate_agent_id();
                let extension_tools = self.tool_group(agent, spec.kind()).into_iter().collect();
                let outcome = host
                    .create_agent(agent, spec.kind(), extension_tools)
                    .map(|()| agent);
                if let Some(rollback) =
                    self.apply_result(MultiagentEffectResult::Spawned { id, spec, outcome })
                {
                    host.rollback_agent(rollback);
                }
            }
            MultiagentEffect::Dispatch {
                id,
                target,
                message,
            } => {
                let outcome = host.dispatch_agent(target, message);
                self.apply_result(MultiagentEffectResult::Dispatched { id, outcome });
            }
            MultiagentEffect::Interrupt { id, target } => {
                let outcome = host.interrupt_agent(target);
                self.apply_result(MultiagentEffectResult::Interrupted { id, outcome });
            }
            MultiagentEffect::RemoveAgents { id: _, agents } => {
                for (agent, result) in host.begin_remove_agents(agents) {
                    self.physical_agent_removed(agent, result);
                }
            }
            MultiagentEffect::ArmTimeout {
                id: _,
                agent,
                timeout,
            } => {
                if self.contains(agent) {
                    host.arm_timeout(agent, timeout);
                }
            }
        }
    }
}
