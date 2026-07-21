//! The `plan` tool group owned by [`AgentModeContextAdapter`](super::AgentModeContextAdapter).

use std::sync::Arc;

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolError, ToolGroup, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolSpec,
};

use crate::agent::effect::{AgentEffect, AgentEffectEmitter};
use crate::agent::tools::optional_string_argument;

use super::{lock_mode, AgentModeState, SharedAgentMode};

const DEFAULT_CANCEL_MESSAGE: &str = "Planning cancelled.";

pub(super) fn plan_tools(mode: SharedAgentMode, effects: AgentEffectEmitter) -> ToolGroup {
    ToolGroup::new(
        "plan",
        true,
        [
            Tool::from_sync(EnterPlanModeTool {
                mode: Arc::clone(&mode),
            }),
            Tool::from_sync(RequestClarificationTool {
                effects: effects.clone(),
            }),
            Tool::from_sync(ExitPlanModeTool { mode, effects }),
        ],
    )
}

pub(super) struct EnterPlanModeTool {
    mode: SharedAgentMode,
}

impl ToolSpec for EnterPlanModeTool {
    tool_metadata!("plan_enter");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for EnterPlanModeTool {
    fn invoke(&self, _call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        *lock_mode(&self.mode) = AgentModeState::Plan;
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
        let question = required_string_argument(call, "question")?;
        self.effects.emit(AgentEffect::Yield { message: question });
        Ok(success("Clarification presented to the user."))
    }
}

pub(super) struct ExitPlanModeTool {
    mode: SharedAgentMode,
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
        let outcome = required_string_argument(call, "outcome")?;
        let output = match outcome.as_str() {
            "execute" => {
                // The approved plan remains in this tool call's transcript
                // arguments; the adapter only changes the next context frame.
                let _plan = required_string_argument(call, "plan")?;
                "Plan Mode exited. Begin executing the approved plan."
            }
            "cancel" => {
                if optional_string_argument(call.arguments_json(), "plan")?.is_some() {
                    return Err(ToolError::InvalidArguments(
                        "'plan' must be omitted when 'outcome' is 'cancel'".into(),
                    )
                    .into());
                }
                let message = optional_non_empty_string_argument(call, "message")?
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
        *lock_mode(&self.mode) = AgentModeState::Normal;
        Ok(success(output))
    }
}

fn required_string_argument(
    call: &ToolInvocation<'_>,
    key: &str,
) -> Result<String, ToolInvokeError> {
    let Some(value) = optional_string_argument(call.arguments_json(), key)? else {
        return Err(ToolError::InvalidArguments(format!("'{key}' is required")).into());
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(ToolError::InvalidArguments(format!("'{key}' is required")).into());
    }
    Ok(value.to_owned())
}

fn optional_non_empty_string_argument(
    call: &ToolInvocation<'_>,
    key: &str,
) -> Result<Option<String>, ToolInvokeError> {
    optional_string_argument(call.arguments_json(), key)?
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
    use std::sync::{Arc, Mutex};

    use claw_tool::{RawToolInvocation, SyncToolHandler, ToolInvocation};

    use super::{
        AgentEffect, AgentModeState, EnterPlanModeTool, ExitPlanModeTool, RequestClarificationTool,
    };
    use crate::agent::effect::agent_effect_channel;

    fn invocation<'a>(name: &'a str, arguments_json: &'a str) -> ToolInvocation<'a> {
        ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-test"),
            name,
            arguments_json,
        })
        .expect("valid invocation")
    }

    #[test]
    fn enter_and_execute_exit_mutate_only_adapter_mode() {
        let mode = Arc::new(Mutex::new(AgentModeState::Normal));
        EnterPlanModeTool {
            mode: Arc::clone(&mode),
        }
        .invoke(&invocation("plan_enter", "{}"))
        .expect("enter succeeds");
        assert_eq!(*mode.lock().expect("mode lock"), AgentModeState::Plan);

        let (effects, _inbox) = agent_effect_channel();
        ExitPlanModeTool {
            mode: Arc::clone(&mode),
            effects,
        }
        .invoke(&invocation(
            "plan_exit",
            r#"{"outcome":"execute","plan":"ship it"}"#,
        ))
        .expect("exit succeeds");
        assert_eq!(*mode.lock().expect("mode lock"), AgentModeState::Normal);
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

        let mut drained = Vec::new();
        inbox.drain_into(&mut drained);
        assert_eq!(
            drained,
            vec![AgentEffect::Yield {
                message: "Which board?".to_owned(),
            }]
        );
    }

    #[test]
    fn cancel_exit_resets_mode_and_emits_generic_yield() {
        let mode = Arc::new(Mutex::new(AgentModeState::Plan));
        let (effects, mut inbox) = agent_effect_channel();
        ExitPlanModeTool {
            mode: Arc::clone(&mode),
            effects,
        }
        .invoke(&invocation(
            "plan_exit",
            r#"{"outcome":"cancel","message":"No changes made."}"#,
        ))
        .expect("cancel succeeds");

        assert_eq!(*mode.lock().expect("mode lock"), AgentModeState::Normal);
        let mut drained = Vec::new();
        inbox.drain_into(&mut drained);
        assert_eq!(
            drained,
            vec![AgentEffect::Yield {
                message: "No changes made.".to_owned(),
            }]
        );
    }
}
