//! The pure `internal` Agent control tool group.

use alloc::string::{String, ToString};

use claw_tool::{tool_metadata, Tool, ToolFuture, ToolGroup, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;

use crate::agent::base_agent::{AgentEffect, AgentEffectEmitter};

#[derive(Deserialize)]
struct EndConversationArgs {
    final_message: String,
}

/// Build the always-visible core Agent control group.
pub(in crate::agent) fn internal_tools(effects: AgentEffectEmitter) -> ToolGroup {
    ToolGroup::new(
        "internal",
        true,
        [Tool::new(EndConversationTool { effects })],
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

impl ToolHandler for EndConversationTool {
    type Args = EndConversationArgs;

    fn invoke<'a>(&'a self, _context: claw_tool::ToolContext, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let final_message = args.final_message.trim();
            self.effects.emit(AgentEffect::Finish {
                final_message: final_message.to_string(),
            });
            Ok(ToolOutput {
                content: "Conversation ended.".to_string(),
                ok: true,
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use claw_tool::{ToolHandler, ToolInvocation};
    use futures_lite::future::block_on;

    use super::{AgentEffect, EndConversationTool};
    use crate::agent::base_agent::agent_effect_channel;

    #[test]
    fn conversation_end_emits_a_generic_finish_effect() {
        let (effects, mut inbox) = agent_effect_channel();
        let call = ToolInvocation::try_new(
            Some("call-test"),
            "conversation_end",
            r#"{"final_message":"Done."}"#,
        )
        .expect("valid invocation");

        let args = call.arguments().expect("valid conversation args");
        block_on(EndConversationTool { effects }.invoke(claw_tool::ToolContext::stateless(), args))
            .expect("conversation_end succeeds");

        let emitted = inbox.drain();
        assert_eq!(
            emitted,
            vec![AgentEffect::Finish {
                final_message: "Done.".to_owned(),
            }]
        );
    }
}
