//! The `plan` tool group owned by [`AgentModeContextProvider`](super::AgentModeContextProvider).

use alloc::{borrow::ToOwned, string::String};

use crate::engine::AgentStorage;
use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};
use serde::Deserialize;

use super::{store_mode, AgentMode};
use crate::engine::{AgentEffect, AgentEffectEmitter};

const DEFAULT_CANCEL_MESSAGE: &str = "Planning cancelled.";

#[derive(Deserialize)]
pub(super) struct ClarifyArgs {
    question: String,
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub(super) enum ExitArgs {
    Execute {
        plan: String,
        #[serde(rename = "message")]
        _message: Option<String>,
    },
    Cancel {
        message: Option<String>,
    },
}

pub(super) struct EnterPlanModeTool {
    pub(super) storage: AgentStorage,
}

impl ToolSpec for EnterPlanModeTool {
    tool_metadata!("plan_enter");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for EnterPlanModeTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            store_mode(&self.storage, AgentMode::Plan);
            Ok(success("Plan Mode entered."))
        })
    }
}

pub(super) struct RequestClarificationTool {
    pub(super) effects: AgentEffectEmitter,
}

impl ToolSpec for RequestClarificationTool {
    tool_metadata!("plan_clarify");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for RequestClarificationTool {
    type Args = ClarifyArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let question = args.question.trim().to_owned();
            self.effects.emit(AgentEffect::Yield { message: question });
            Ok(success("Clarification presented to the user."))
        })
    }
}

pub(super) struct ExitPlanModeTool {
    pub(super) effects: AgentEffectEmitter,
    pub(super) storage: AgentStorage,
}

impl ToolSpec for ExitPlanModeTool {
    tool_metadata!("plan_exit");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for ExitPlanModeTool {
    type Args = ExitArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
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
            store_mode(&self.storage, AgentMode::Normal);
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
    use barracuda_agent_tool::{ToolInvocation, ToolOutput, ToolRunner, ToolSet};
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::{AgentEffect, AgentMode};
    use crate::context_providers::agent_mode::{load_mode, AgentModeContextProvider};
    use crate::engine::{agent_effect_channel, AgentEffectEmitter, AgentStorage, ContextProvider};
    use crate::{AgentEngineState, AgentKind};

    fn invocation<'a>(name: &'a str, arguments_json: &'a str) -> ToolInvocation {
        ToolInvocation::try_new(Some("call-test"), name, arguments_json).expect("valid invocation")
    }

    fn storage(mode: AgentMode) -> AgentStorage {
        let state = DurableState::new(AgentEngineState::new(&AgentKind::from_static("worker")));
        let (effects, _inbox) = agent_effect_channel();
        let provider = AgentModeContextProvider::new(effects);
        let storage = AgentStorage::new(&state, provider.id());
        super::store_mode(&storage, mode);
        storage
    }

    fn run(
        effects: AgentEffectEmitter,
        storage: AgentStorage,
        invocation: ToolInvocation,
    ) -> ToolOutput {
        let provider = AgentModeContextProvider::new(effects.clone());
        let mut tools = ToolSet::empty();
        tools
            .add_group(provider.tools(&storage).expect("plan tools exist"))
            .expect("plan tools register");
        let tools = tools.begin().expect("tool set begins");
        let joined = ToolRunner::new(&tools).run(vec![invocation]);
        block_on(joined.collect::<Vec<_>>())
            .pop()
            .expect("tool result")
            .1
    }

    #[test]
    fn enter_and_execute_exit_mutate_agent_mode() {
        let storage = storage(AgentMode::Normal);
        let (effects, _inbox) = agent_effect_channel();
        let output = run(
            effects.clone(),
            storage.clone(),
            invocation("plan_enter", "{}"),
        );
        assert!(output.ok);
        assert_eq!(load_mode(&storage), AgentMode::Plan);

        let output = run(
            effects,
            storage.clone(),
            invocation("plan_exit", r#"{"outcome":"execute","plan":"ship it"}"#),
        );
        assert!(output.ok);
        assert_eq!(load_mode(&storage), AgentMode::Normal);
    }

    #[test]
    fn clarification_emits_generic_yield() {
        let storage = storage(AgentMode::Plan);
        let (effects, mut inbox) = agent_effect_channel();
        let output = run(
            effects,
            storage,
            invocation("plan_clarify", r#"{"question":"Which board?"}"#),
        );
        assert!(output.ok);

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
        let storage = storage(AgentMode::Plan);
        let (effects, mut inbox) = agent_effect_channel();
        let output = run(
            effects,
            storage.clone(),
            invocation(
                "plan_exit",
                r#"{"outcome":"cancel","message":"No changes made."}"#,
            ),
        );
        assert!(output.ok);
        assert_eq!(load_mode(&storage), AgentMode::Normal);
        let drained = inbox.drain();
        assert_eq!(
            drained,
            vec![AgentEffect::Yield {
                message: "No changes made.".to_owned(),
            }]
        );
    }
}
