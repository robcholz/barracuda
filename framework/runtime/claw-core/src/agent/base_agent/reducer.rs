use std::collections::HashSet;

use claw_interface::{ClawHttp, ClawTimer};
use serde_json::Value;

use crate::agent::effect::AgentEffect;
use crate::agent::iteration_loop::{
    CompletedKind, CompletedOutcome, IterationOutcome, IterationResult, PreemptedOutcome, ToolRun,
    ToolsOutcome,
};
use crate::protocol::Message;

use super::command::{
    AgentCommand, AgentCommandError, AgentRunError, ApprovalDecision, TickOutcome,
};
use super::control::AgentAbortHandle;
use super::pending_tool_round::PendingToolRound;
use super::state::ToolBlockVerdict;
use super::task_state::TaskAction;
use super::{AssistantCommit, BaseAgent, IterationIdAllocator, TurnLifecycle};

pub(super) struct ApprovalResume {
    pub(super) decision: ApprovalDecision,
    pub(super) pending_tools: PendingToolRound,
}

pub(super) enum AgentEffectDisposition {
    /// The tool round is paused for approval; keep already emitted effects.
    Retain,
    /// The complete tool round may now atomically reduce its effects.
    Reduce,
    /// The iteration did not complete a tool round; drop any emitted effects.
    Discard,
}

impl<H: ClawHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    /// Queue a command. The single inbound entry point.
    pub(crate) fn send_command(&mut self, command: AgentCommand) -> Result<(), AgentCommandError> {
        self.state.task_mut().enqueue_command(command)
    }

    /// Activate an input that the orchestrator already deferred until this
    /// agent returned to an idle boundary.
    pub(crate) fn activate_deferred_message(&mut self, message: Message) {
        self.state.task_mut().enqueue_task_input(message);
    }

    pub(crate) fn abort_handle(&self) -> AgentAbortHandle {
        self.interruption.handle()
    }

    pub(super) fn drain_inbox(&mut self) -> Option<ApprovalResume> {
        let mut approval_resume = None;
        loop {
            let action = match self.state.task_mut().pop_action() {
                Ok(action) => action,
                Err(error) => {
                    tracing::error!(
                        name: "task_mailbox_invariant_failed",
                        detail = %error,
                    );
                    self.fail_with(AgentRunError::TaskStateInvariant);
                    break;
                }
            };
            let Some(action) = action else {
                break;
            };
            if let Some(resume) = self.apply_action(action) {
                debug_assert!(approval_resume.is_none());
                approval_resume = Some(resume);
            }
        }
        approval_resume
    }

    pub(super) fn drain_agent_effects(&mut self) {
        let mut effects = Vec::new();
        self.effect_inbox.drain_into(&mut effects);
        if effects.len() > 1 {
            let count = effects.len();
            tracing::error!(name: "agent_effect_conflict", count = count as u64);
            self.fail_with(AgentRunError::ConflictingEffects { count });
            return;
        }
        if let Some(effect) = effects.pop() {
            self.state.task_mut().enqueue_effect(effect);
        }
    }

    pub(super) fn discard_agent_effects(&mut self) {
        self.effect_inbox.clear();
    }

    fn notify_turn_lifecycle(&mut self, lifecycle: TurnLifecycle) {
        for adapter in &mut self.context_adapters {
            adapter.on_turn_lifecycle(lifecycle);
        }
    }

    fn apply_action(&mut self, action: TaskAction) -> Option<ApprovalResume> {
        match action {
            TaskAction::TaskInput {
                message,
                starts_task,
            } => {
                self.append_task_input(&message, starts_task);
                None
            }
            TaskAction::Cancel => {
                self.discard_agent_effects();
                self.transcript.discard_open_turn();
                self.interruption.clear();
                self.notify_turn_lifecycle(TurnLifecycle::Ended);
                self.outcome = Some(TickOutcome::Cancelled);
                None
            }
            TaskAction::ApprovalResult {
                decision,
                pending_tools,
            } => Some(ApprovalResume {
                decision,
                pending_tools,
            }),
            TaskAction::Effect(AgentEffect::Finish { final_message }) => {
                self.transcript.commit_ended(&final_message);
                self.notify_turn_lifecycle(TurnLifecycle::Ended);
                self.outcome = Some(TickOutcome::Ended { final_message });
                None
            }
            TaskAction::Effect(AgentEffect::Yield { message }) => {
                self.transcript
                    .commit_assistant(AssistantCommit::PlainText(&message));
                self.outcome = Some(TickOutcome::YieldedByTool { text: message });
                None
            }
        }
    }

    pub(super) fn reduce_outcome(&mut self, outcome: IterationResult) -> AgentEffectDisposition {
        match outcome {
            Ok(IterationOutcome::Completed(CompletedOutcome { kind, .. })) => match kind {
                CompletedKind::PlainText(answer) => {
                    let commit = match answer.raw_message_json.as_deref() {
                        Some(raw) => AssistantCommit::RawJson(raw),
                        None => AssistantCommit::PlainText(&answer.text),
                    };
                    self.transcript.commit_assistant(commit);
                    self.state.task_mut().finish_task();
                    self.outcome = Some(TickOutcome::Yielded { text: answer.text });
                    AgentEffectDisposition::Discard
                }
                CompletedKind::Tools(tools) => self.reduce_tool_round(tools),
            },
            Ok(IterationOutcome::Preempted(outcome)) => {
                self.merge_preempt_patch(outcome);
                AgentEffectDisposition::Discard
            }
            Err(error) => {
                self.fail_with(error.into());
                AgentEffectDisposition::Discard
            }
        }
    }

    fn reduce_tool_round(&mut self, tools: ToolsOutcome) -> AgentEffectDisposition {
        let awaits_approval = tools.next_approval().is_some();
        if awaits_approval {
            self.apply_tool_block_policy(&tools.runs);
            if self.outcome.is_none() {
                match PendingToolRound::from_tools(tools) {
                    Some(pending) => self.park_tool_round(pending),
                    None => self.fail_with(AgentRunError::TaskStateInvariant),
                }
            }
            if matches!(self.outcome.as_ref(), Some(TickOutcome::Failed(_))) {
                AgentEffectDisposition::Discard
            } else {
                AgentEffectDisposition::Retain
            }
        } else {
            let ToolsOutcome { appended, runs } = tools;
            self.transcript.append_patch(&appended.into_json_array());
            self.apply_tool_block_policy(&runs);
            if matches!(self.outcome.as_ref(), Some(TickOutcome::Failed(_))) {
                AgentEffectDisposition::Discard
            } else {
                AgentEffectDisposition::Reduce
            }
        }
    }

    pub(super) fn reduce_resolved_tool_round(
        &mut self,
        pending: PendingToolRound,
    ) -> AgentEffectDisposition {
        let awaits_approval = pending.next().is_some();
        if awaits_approval {
            self.park_tool_round(pending);
            if matches!(self.outcome.as_ref(), Some(TickOutcome::Failed(_))) {
                AgentEffectDisposition::Discard
            } else {
                AgentEffectDisposition::Retain
            }
        } else {
            // The resumed round is now complete. Only now can the assistant and
            // all matched tool messages become visible to the next iteration.
            let (appended, blocked) = pending.into_completed();
            self.transcript.append_patch(&appended.into_json_array());
            if !blocked.is_empty() {
                let blocked = blocked.iter().map(String::as_str).collect::<Vec<_>>();
                self.apply_blocked_tool_policy(&blocked);
            }
            if matches!(self.outcome.as_ref(), Some(TickOutcome::Failed(_))) {
                AgentEffectDisposition::Discard
            } else {
                AgentEffectDisposition::Reduce
            }
        }
    }

    fn apply_tool_block_policy(&mut self, runs: &[ToolRun]) {
        let blocked: Vec<&str> = runs
            .iter()
            .filter(|run| run.is_blocked())
            .map(|run| run.name.as_str())
            .collect();
        self.apply_blocked_tool_policy(&blocked);
    }

    fn apply_blocked_tool_policy(&mut self, blocked: &[&str]) {
        if !blocked.is_empty() {
            tracing::warn!(name: "tool_gate_blocked", count = blocked.len() as u64);
        }
        if let ToolBlockVerdict::Exhausted { name } = self.state.block_policy.record_round(blocked)
        {
            self.fail_with(AgentRunError::ToolNotPermitted { name });
        }
    }

    fn park_tool_round(&mut self, pending: PendingToolRound) {
        let Some(summary) = pending.next().map(|approval| approval.summary.clone()) else {
            self.fail_with(AgentRunError::TaskStateInvariant);
            return;
        };
        if let Err(error) = self.state.task_mut().await_approval(pending) {
            tracing::error!(
                name: "task_phase_transition_failed",
                transition = "await_approval",
                detail = %error,
            );
            self.fail_with(AgentRunError::TaskStateInvariant);
            return;
        }
        self.outcome = Some(TickOutcome::AwaitingApproval { summary });
    }

    pub(super) fn fail_with(&mut self, error: AgentRunError) {
        self.discard_agent_effects();
        self.state.task_mut().finish_task();
        self.notify_turn_lifecycle(TurnLifecycle::Ended);
        self.outcome = Some(TickOutcome::Failed(error));
    }

    fn merge_preempt_patch(&mut self, outcome: PreemptedOutcome) {
        if outcome.produced.is_empty() {
            return;
        }
        if has_dangling_tool_calls(outcome.produced.as_slice()) {
            let tool_call_count = outcome
                .produced
                .as_slice()
                .iter()
                .filter_map(|message| message.get("tool_calls").and_then(Value::as_array))
                .map(|calls| calls.len())
                .sum::<usize>();
            tracing::warn!(
                name: "preempt_patch_dropped",
                tool_call_count = tool_call_count as u64,
            );
            return;
        }
        // Preemption ends the iteration but the turn continues in a fresh
        // iteration, so keep the salvaged work in the open turn rather than
        // closing it into its own user-less group.
        self.transcript
            .append_patch(&outcome.produced.into_json_array());
    }

    fn append_task_input(&mut self, message: &Message, starts_task: bool) {
        if starts_task {
            let state = &mut self.state;
            state.id_allocator = IterationIdAllocator::new();
            self.outcome = None;
        }
        self.transcript.append_user(message.as_str(), starts_task);
    }
}

fn has_dangling_tool_calls(items: &[Value]) -> bool {
    let mut expected: Vec<&str> = Vec::new();
    let mut satisfied: HashSet<&str> = HashSet::new();
    for message in items {
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    expected.push(id);
                }
            }
        }
        if let Some(id) = message.get("tool_call_id").and_then(Value::as_str) {
            satisfied.insert(id);
        }
    }
    expected.iter().any(|id| !satisfied.contains(id))
}
