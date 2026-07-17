//! Root-only Plan Mode control tools.

use claw_permission::{Action, RiskClass};
use claw_tool::{
    tool_metadata, SyncToolHandler, ToolError, ToolInvocation, ToolInvokeError, ToolOutput,
    ToolSpec,
};

use super::{optional_string_argument, ControlSignal, ControlSink, PlanModeExitOutcome};

const DEFAULT_CANCEL_MESSAGE: &str = "Planning cancelled.";

pub(super) struct EnterPlanModeTool {
    pub(super) sink: ControlSink,
}

impl ToolSpec for EnterPlanModeTool {
    tool_metadata!("plan_enter");

    fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl SyncToolHandler for EnterPlanModeTool {
    fn invoke(&self, _call: &ToolInvocation<'_>) -> Result<ToolOutput, ToolInvokeError> {
        push(&self.sink, ControlSignal::EnterPlanMode);
        Ok(success("Plan Mode entered."))
    }
}

pub(super) struct RequestClarificationTool {
    pub(super) sink: ControlSink,
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
        push(&self.sink, ControlSignal::RequestClarification { question });
        Ok(success("Clarification presented to the user."))
    }
}

pub(super) struct ExitPlanModeTool {
    pub(super) sink: ControlSink,
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
        let (outcome, output) = match outcome.as_str() {
            "execute" => {
                // The approved plan remains in this tool call's transcript arguments;
                // the control signal only switches the next iteration's framing.
                let _plan = required_string_argument(call, "plan")?;
                (
                    PlanModeExitOutcome::Execute,
                    "Plan Mode exited. Begin executing the approved plan.",
                )
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
                (
                    PlanModeExitOutcome::Cancel { message },
                    "Plan Mode cancelled.",
                )
            }
            _ => {
                return Err(ToolError::InvalidArguments(
                    "'outcome' must be either 'execute' or 'cancel'".into(),
                )
                .into());
            }
        };
        push(&self.sink, ControlSignal::ExitPlanMode { outcome });
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

fn push(sink: &ControlSink, signal: ControlSignal) {
    sink.lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push_back(signal);
}

fn success(output: &str) -> ToolOutput {
    ToolOutput {
        output: output.to_owned(),
        ok: true,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use claw_tool::{RawToolInvocation, SyncToolHandler, ToolInvocation};

    use super::{ExitPlanModeTool, RequestClarificationTool, ToolSpec};
    use crate::agent::tools::{ControlSignal, PlanModeExitOutcome};

    #[test]
    fn clarification_signal_yields_the_question() {
        let sink = Arc::new(Mutex::new(VecDeque::new()));
        let tool = RequestClarificationTool {
            sink: Arc::clone(&sink),
        };
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-7"),
            name: tool.name(),
            arguments_json: r#"{"question":"Which board?"}"#,
        })
        .expect("valid invocation");

        let output = tool.invoke(&call).expect("tool runs");

        assert!(output.ok);
        assert_eq!(
            sink.lock().expect("sink lock").pop_front(),
            Some(ControlSignal::RequestClarification {
                question: "Which board?".to_owned(),
            })
        );
    }

    #[test]
    fn exit_execute_emits_execute_signal() {
        let sink = Arc::new(Mutex::new(VecDeque::new()));
        let tool = ExitPlanModeTool {
            sink: Arc::clone(&sink),
        };
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-8"),
            name: tool.name(),
            arguments_json: r#"{"outcome":"execute","plan":"1. Inspect\n2. Implement"}"#,
        })
        .expect("valid invocation");

        let output = tool.invoke(&call).expect("tool runs");

        assert!(output.ok);
        assert_eq!(
            sink.lock().expect("sink lock").pop_front(),
            Some(ControlSignal::ExitPlanMode {
                outcome: PlanModeExitOutcome::Execute,
            })
        );
    }

    #[test]
    fn exit_execute_requires_a_plan() {
        let sink = Arc::new(Mutex::new(VecDeque::new()));
        let tool = ExitPlanModeTool { sink };
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-8-missing-plan"),
            name: tool.name(),
            arguments_json: r#"{"outcome":"execute"}"#,
        })
        .expect("valid invocation");

        assert!(tool.invoke(&call).is_err());
    }

    #[test]
    fn exit_cancel_uses_the_same_tool_without_a_plan() {
        let sink = Arc::new(Mutex::new(VecDeque::new()));
        let tool = ExitPlanModeTool {
            sink: Arc::clone(&sink),
        };
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-9"),
            name: tool.name(),
            arguments_json: r#"{"outcome":"cancel","message":"Understood; no changes made."}"#,
        })
        .expect("valid invocation");

        let output = tool.invoke(&call).expect("tool runs");

        assert!(output.ok);
        assert_eq!(
            sink.lock().expect("sink lock").pop_front(),
            Some(ControlSignal::ExitPlanMode {
                outcome: PlanModeExitOutcome::Cancel {
                    message: "Understood; no changes made.".to_owned(),
                },
            })
        );
    }

    #[test]
    fn exit_cancel_rejects_a_conflicting_plan() {
        let sink = Arc::new(Mutex::new(VecDeque::new()));
        let tool = ExitPlanModeTool { sink };
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some("call-10"),
            name: tool.name(),
            arguments_json: r#"{"outcome":"cancel","plan":"do work"}"#,
        })
        .expect("valid invocation");

        assert!(tool.invoke(&call).is_err());
    }
}
