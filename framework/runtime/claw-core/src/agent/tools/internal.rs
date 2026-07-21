//! The pure `internal` Agent control tool group.

use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolError, ToolGroup, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolSpec,
};

use crate::agent::effect::{AgentEffect, AgentEffectEmitter};
use crate::agent::tools::optional_string_argument;

/// Build the always-visible core Agent control group.
pub(crate) fn internal_tools(effects: AgentEffectEmitter) -> ToolGroup {
    ToolGroup::new(
        "internal",
        true,
        [Tool::from_sync(EndConversationTool { effects })],
    )
}

/// The self-control tool: emits a generic finish effect for BaseAgent's next
/// reduction boundary.
struct EndConversationTool {
    effects: AgentEffectEmitter,
}

impl ToolSpec for EndConversationTool {
    tool_metadata!("conversation_end");
}

impl SyncToolHandler for EndConversationTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let Some(final_message) = optional_string_argument(call.arguments_json(), "final_message")?
        else {
            return Err(ToolError::InvalidArguments(
                "conversation_end 'final_message' is required".into(),
            )
            .into());
        };
        let final_message = final_message.trim();
        if final_message.is_empty() {
            return Err(ToolError::InvalidArguments(
                "conversation_end 'final_message' is required".into(),
            )
            .into());
        }
        self.effects.emit(AgentEffect::Finish {
            final_message: final_message.to_string(),
        });
        Ok(ToolOutput {
            output: "Conversation ended.".to_string(),
            ok: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use claw_tool::{RawToolInvocation, SyncToolHandler, ToolInvocation};

    use super::{AgentEffect, EndConversationTool};
    use crate::agent::effect::agent_effect_channel;

    #[test]
    fn conversation_end_emits_a_generic_finish_effect() {
        let (effects, mut inbox) = agent_effect_channel();
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-test"),
            name: "conversation_end",
            arguments_json: r#"{"final_message":"Done."}"#,
        })
        .expect("valid invocation");

        EndConversationTool { effects }
            .invoke(&call)
            .expect("conversation_end succeeds");

        let mut emitted = Vec::new();
        inbox.drain_into(&mut emitted);
        assert_eq!(
            emitted,
            vec![AgentEffect::Finish {
                final_message: "Done.".to_owned(),
            }]
        );
    }
}
