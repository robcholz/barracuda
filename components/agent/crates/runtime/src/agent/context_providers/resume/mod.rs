//! One-shot context contributed when an Agent resumes after a restart.

use alloc::{borrow::Cow, string::String, vec::Vec};

use crate::agent::base_agent::AgentStorage;
use barracuda_agent_context::{Band, BlockKind, ContextSink, Scope};
use barracuda_model_api::ToolCall;

use crate::agent::base_agent::{ContextProvider, ContextProviderResult};
use crate::agent::BaseAgentState;

/// Contributes one warning for tool calls whose completion was unknown at restart.
pub(in crate::agent) struct ResumeContextProvider {
    inflight_toolcalls: Vec<ToolCall>,
    reminder_pending: bool,
}

impl ResumeContextProvider {
    pub(in crate::agent) fn new(state: &BaseAgentState) -> Self {
        Self {
            inflight_toolcalls: state.inflight_toolcalls().to_vec(),
            reminder_pending: true,
        }
    }
}

impl ContextProvider for ResumeContextProvider {
    fn id(&self) -> &'static str {
        "resume"
    }

    fn contribute(
        &mut self,
        _storage: &AgentStorage,
        output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        let reminder = if self.reminder_pending {
            render_resume_reminder(&self.inflight_toolcalls)
        } else {
            None
        };
        self.reminder_pending = false;
        output.reminder(resume_reminder_kind(), reminder.as_deref());
        Ok(())
    }
}

fn render_resume_reminder(inflight_toolcalls: &[ToolCall]) -> Option<String> {
    (!inflight_toolcalls.is_empty()).then(|| {
        let calls = inflight_toolcalls
            .iter()
            .map(|call| format!("{}({})", call.name, call.arguments_json))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Session resumed after a restart; tool calls with unknown completion status: {calls}. Inflight tool calls were not replayed; inspect current external state before relying on their completion."
        )
    })
}

fn resume_reminder_kind() -> BlockKind {
    BlockKind::Custom {
        band: Band::Volatile,
        scope: Scope::Agent,
        order: 1,
        label: Cow::Borrowed("resume"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use barracuda_agent_context::Context;
    use barracuda_agent_persistence::DurableState;
    use barracuda_model_api::ToolCall;

    use super::ResumeContextProvider;
    use crate::agent::base_agent::{AgentStorage, ContextProvider};
    use crate::agent::{AgentKind, BaseAgentState};

    #[test]
    fn inflight_toolcalls_are_reminded_once_without_exposing_tools() {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        state.get_mut().record_inflight_toolcalls(vec![ToolCall {
            id: "call-1".to_owned(),
            name: "profile_read".to_owned(),
            arguments_json: r#"{"document":"user"}"#.to_owned(),
        }]);
        let mut provider = ResumeContextProvider::new(&state.get());
        let storage = AgentStorage::new(&state, provider.id());
        assert!(provider.tools(&storage).is_none());

        let mut context = Context::new();
        let first = {
            let mut sink = context.sink();
            assert!(provider.contribute(&storage, &mut sink).is_ok());
            sink.into_history()
        };
        assert!(context.request(&first).reminders()[0]
            .to_string()
            .contains("tool calls with unknown completion status"));

        let second = {
            let mut sink = context.sink();
            assert!(provider.contribute(&storage, &mut sink).is_ok());
            sink.into_history()
        };
        assert!(context.request(&second).reminders().is_empty());
    }
}
