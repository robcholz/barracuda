//! The `plan` tool group owned by [`AgentModeContextAdapter`](super::AgentModeContextAdapter).

use claw_permission::{Action, RiskClass};
use claw_persistence::DurableState;
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolError, ToolGroup, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolSpec,
};

use crate::agent::base_agent::{AgentEffect, AgentEffectEmitter};
use crate::agent::state::{AgentMode, AgentState};
use crate::agent::tools::helper::{non_blank_argument, optional_string_argument};

const DEFAULT_CANCEL_MESSAGE: &str = "Planning cancelled.";

pub(super) fn plan_tools(
    state: DurableState<AgentState>,
    effects: AgentEffectEmitter,
) -> ToolGroup {
    ToolGroup::new(
        "plan",
        true,
        [
            Tool::from_sync(EnterPlanModeTool {
                state: state.clone(),
            }),
            Tool::from_sync(RequestClarificationTool {
                effects: effects.clone(),
            }),
            Tool::from_sync(ExitPlanModeTool { state, effects }),
        ],
    )
}

pub(super) struct EnterPlanModeTool {
    state: DurableState<AgentState>,
}

impl ToolSpec for EnterPlanModeTool {
    tool_metadata!("plan_enter");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for EnterPlanModeTool {
    fn invoke(&self, _call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        self.state.get_mut().set_mode(AgentMode::Plan);
        Ok(success("Plan Mode entered."))
    }
}

pub(super) struct RequestClarificationTool {
    effects: AgentEffectEmitter,
}

impl ToolSpec for RequestClarificationTool {
    tool_metadata!("plan_clarify");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for RequestClarificationTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let args = call.arguments_value()?;
        let question = non_blank_argument(&args, "question")?;
        self.effects.emit(AgentEffect::Yield { message: question });
        Ok(success("Clarification presented to the user."))
    }
}

pub(super) struct ExitPlanModeTool {
    state: DurableState<AgentState>,
    effects: AgentEffectEmitter,
}

impl ToolSpec for ExitPlanModeTool {
    tool_metadata!("plan_exit");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for ExitPlanModeTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        let args = call.arguments_value()?;
        let outcome = non_blank_argument(&args, "outcome")?;
        let output = match outcome.as_str() {
            "execute" => {
                // The approved plan remains in this tool call's transcript
                // arguments; the adapter only changes the next context frame.
                let _plan = non_blank_argument(&args, "plan")?;
                "Plan Mode exited. Begin executing the approved plan."
            }
            "cancel" => {
                if optional_string_argument(&args, "plan")?.is_some() {
                    return Err(ToolError::InvalidArguments(
                        "'plan' must be omitted when 'outcome' is 'cancel'".into(),
                    )
                    .into());
                }
                let message = optional_non_empty_string_argument(&args, "message")?
                    .unwrap_or_else(|| DEFAULT_CANCEL_MESSAGE.to_owned());
                self.effects.emit(AgentEffect::Yield { message });
                "Plan Mode cancelled."
            }
            _ => {
                return Err(ToolError::InvalidArguments(
                    "'outcome' must be either 'execute' or 'cancel'".into(),
                )
                .into());
            }
        };
        self.state.get_mut().set_mode(AgentMode::Normal);
        Ok(success(output))
    }
}

fn optional_non_empty_string_argument(
    args: &serde_json::Value,
    key: &str,
) -> Result<Option<String>, ToolInvokeError> {
    optional_string_argument(args, key)?
        .map(|value| {
            let value = value.trim();
            if value.is_empty() {
                Err(ToolError::InvalidArguments(format!("'{key}' must not be empty")).into())
            } else {
                Ok(value.to_owned())
            }
        })
        .transpose()
}

fn success(output: &str) -> ToolOutput {
    ToolOutput {
        output: output.to_owned(),
        ok: true,
    }
}

#[cfg(test)]
mod tests {
    use claw_persistence::DurableState;
    use claw_tool::{SyncToolHandler, ToolInvocation};

    use super::{AgentEffect, EnterPlanModeTool, ExitPlanModeTool, RequestClarificationTool};
    use crate::agent::base_agent::agent_effect_channel;
    use crate::agent::state::{AgentMode, AgentState};
    use crate::agent::AgentKind;

    fn invocation<'a>(name: &'a str, arguments_json: &'a str) -> ToolInvocation<'a> {
        ToolInvocation::try_new(Some("call-test"), name, arguments_json).expect("valid invocation")
    }

    fn state(mode: AgentMode) -> DurableState<AgentState> {
        let state = DurableState::new(AgentState::new(&AgentKind::from_static("worker")));
        state.get_mut().set_mode(mode);
        state
    }

    #[test]
    fn enter_and_execute_exit_mutate_agent_mode() {
        let state = state(AgentMode::Normal);
        EnterPlanModeTool {
            state: state.clone(),
        }
        .invoke(&invocation("plan_enter", "{}"))
        .expect("enter succeeds");
        assert_eq!(state.get().mode(), AgentMode::Plan);

        let (effects, _inbox) = agent_effect_channel();
        ExitPlanModeTool {
            state: state.clone(),
            effects,
        }
        .invoke(&invocation(
            "plan_exit",
            r#"{"outcome":"execute","plan":"ship it"}"#,
        ))
        .expect("exit succeeds");
        assert_eq!(state.get().mode(), AgentMode::Normal);
    }

    #[test]
    fn clarification_emits_generic_yield() {
        let (effects, mut inbox) = agent_effect_channel();
        RequestClarificationTool { effects }
            .invoke(&invocation(
                "plan_clarify",
                r#"{"question":"Which board?"}"#,
            ))
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
        ExitPlanModeTool {
            state: state.clone(),
            effects,
        }
        .invoke(&invocation(
            "plan_exit",
            r#"{"outcome":"cancel","message":"No changes made."}"#,
        ))
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
