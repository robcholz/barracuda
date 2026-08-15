//! The `plan` tool group owned by [`AgentModeContextProvider`](super::AgentModeContextProvider).

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_persistence::DurableState;
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, Tool, ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolOutput,
    ToolSpec,
};
use serde::Deserialize;

use super::AgentMode;
use crate::agent::base_agent::{AgentEffect, AgentEffectEmitter};
use crate::agent::BaseAgentState;

const DEFAULT_CANCEL_MESSAGE: &str = "Planning cancelled.";

#[derive(Deserialize)]
struct ClarifyArgs {
    question: String,
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
enum ExitArgs {
    Execute {
        plan: String,
        #[serde(rename = "message")]
        _message: Option<String>,
    },
    Cancel {
        message: Option<String>,
    },
}

pub(super) fn plan_tools(
    state: DurableState<BaseAgentState>,
    effects: AgentEffectEmitter,
) -> ToolGroup {
    ToolGroup::new(
        "plan",
        true,
        [
            Tool::new(EnterPlanModeTool {
                state: state.clone(),
            }),
            Tool::new(RequestClarificationTool {
                effects: effects.clone(),
            }),
            Tool::new(ExitPlanModeTool { state, effects }),
        ],
    )
}

struct EnterPlanModeTool {
    state: DurableState<BaseAgentState>,
}

impl ToolSpec for EnterPlanModeTool {
    tool_metadata!("plan_enter");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for EnterPlanModeTool {
    type Args = EmptyArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        _args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            self.state.get_mut().set_mode(AgentMode::Plan);
            Ok(success("Plan Mode entered."))
        })
    }
}

struct RequestClarificationTool {
    effects: AgentEffectEmitter,
}

impl ToolSpec for RequestClarificationTool {
    tool_metadata!("plan_clarify");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for RequestClarificationTool {
    type Args = ClarifyArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let question = args.question.trim().to_owned();
            self.effects.emit(AgentEffect::Yield { message: question });
            Ok(success("Clarification presented to the user."))
        })
    }
}

struct ExitPlanModeTool {
    state: DurableState<BaseAgentState>,
    effects: AgentEffectEmitter,
}

impl ToolSpec for ExitPlanModeTool {
    tool_metadata!("plan_exit");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for ExitPlanModeTool {
    type Args = ExitArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let output = match args {
                ExitArgs::Execute {
                    plan: _plan,
                    _message: _,
                } => {
                    // The approved plan remains in this tool call's transcript
                    // arguments; the provider only changes the next context frame.
                    "Plan Mode exited. Begin executing the approved plan."
                }
                ExitArgs::Cancel { message } => {
                    let message = message
                        .map(|message| message.trim().to_owned())
                        .unwrap_or_else(|| DEFAULT_CANCEL_MESSAGE.to_owned());
                    self.effects.emit(AgentEffect::Yield { message });
                    "Plan Mode cancelled."
                }
            };
            self.state.get_mut().set_mode(AgentMode::Normal);
            Ok(success(output))
        })
    }
}

fn success(output: &str) -> ToolOutput {
    ToolOutput {
        content: output.to_owned(),
        ok: true,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use barracuda_agent_persistence::DurableState;
    use barracuda_agent_tool::{ToolHandler, ToolInvocation};
    use futures_lite::future::block_on;

    use super::{
        AgentEffect, AgentMode, EnterPlanModeTool, ExitPlanModeTool, RequestClarificationTool,
    };
    use crate::agent::base_agent::agent_effect_channel;
    use crate::agent::{AgentKind, BaseAgentState};

    fn invocation<'a>(name: &'a str, arguments_json: &'a str) -> ToolInvocation {
        ToolInvocation::try_new(Some("call-test"), name, arguments_json).expect("valid invocation")
    }

    fn state(mode: AgentMode) -> DurableState<BaseAgentState> {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        state.get_mut().set_mode(mode);
        state
    }

    #[test]
    fn enter_and_execute_exit_mutate_agent_mode() {
        let state = state(AgentMode::Normal);
        block_on(
            EnterPlanModeTool {
                state: state.clone(),
            }
            .invoke(
                barracuda_agent_tool::ToolContext::stateless(),
                barracuda_agent_tool::EmptyArgs {},
            ),
        )
        .expect("enter succeeds");
        assert_eq!(state.get().mode(), AgentMode::Plan);

        let (effects, _inbox) = agent_effect_channel();
        block_on(
            ExitPlanModeTool {
                state: state.clone(),
                effects,
            }
            .invoke(
                barracuda_agent_tool::ToolContext::stateless(),
                invocation("plan_exit", r#"{"outcome":"execute","plan":"ship it"}"#)
                    .arguments()
                    .expect("valid exit args"),
            ),
        )
        .expect("exit succeeds");
        assert_eq!(state.get().mode(), AgentMode::Normal);
    }

    #[test]
    fn clarification_emits_generic_yield() {
        let (effects, mut inbox) = agent_effect_channel();
        block_on(
            RequestClarificationTool { effects }.invoke(
                barracuda_agent_tool::ToolContext::stateless(),
                invocation("plan_clarify", r#"{"question":"Which board?"}"#)
                    .arguments()
                    .expect("valid clarification args"),
            ),
        )
        .expect("clarification succeeds");

        let drained = inbox.drain();
        assert_eq!(
            drained,
            vec![AgentEffect::Yield {
                message: "Which board?".to_owned(),
            }]
        );
    }

    #[test]
    fn cancel_exit_resets_mode_and_emits_generic_yield() {
        let state = state(AgentMode::Plan);
        let (effects, mut inbox) = agent_effect_channel();
        block_on(
            ExitPlanModeTool {
                state: state.clone(),
                effects,
            }
            .invoke(
                barracuda_agent_tool::ToolContext::stateless(),
                invocation(
                    "plan_exit",
                    r#"{"outcome":"cancel","message":"No changes made."}"#,
                )
                .arguments()
                .expect("valid exit args"),
            ),
        )
        .expect("cancel succeeds");

        assert_eq!(state.get().mode(), AgentMode::Normal);
        let drained = inbox.drain();
        assert_eq!(
            drained,
            vec![AgentEffect::Yield {
                message: "No changes made.".to_owned(),
            }]
        );
    }
}
use alloc::{borrow::ToOwned, string::String};
