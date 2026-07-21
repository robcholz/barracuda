use std::borrow::Cow;

use claw_context::{Band, Block, BlockKind, Context, Scope};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use claw_tool::{RawToolInvocation, ToolGate, ToolInvocation, ToolRunner};
use serde_json::Value;
use tracing::Instrument as _;

use crate::protocol::EventSink;

use super::command::AgentRunError;
use super::control::{PermissionGate, ResolvedPermissionGate};
use super::pending_tool_round::PendingToolRound;
use super::reducer::{AgentEffectDisposition, ApprovalResume};
use super::{BaseAgent, ContextAdapter, History, TickOutcome};
use crate::agent::iteration_loop::{
    IterationLoop, IterationLoopError, IterationResult, IterationStep,
};
use crate::agent::AgentEventBoundary;
use crate::protocol::IterationId;

impl<H: ClawHttp + StreamingHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    /// Process queued commands, advance at most one iteration, and report what
    /// happened as a [`TickOutcome`].
    pub(crate) async fn tick(
        &mut self,
        events: &EventSink,
        event_boundary: &AgentEventBoundary,
    ) -> TickOutcome {
        self.outcome = None;

        let approval_resume = self.drain_inbox();

        if self.state.task().is_running() {
            let effect_disposition = match approval_resume {
                Some(resume) => match self.resume_approval(resume).await {
                    Ok(resolved) => self.reduce_resolved_tool_round(resolved),
                    Err(error) => {
                        self.fail_with(error);
                        AgentEffectDisposition::Discard
                    }
                },
                None => {
                    let iteration_id = self.state.id_allocator.next();
                    let outcome = self
                        .run_iteration(iteration_id, events, event_boundary)
                        .await;
                    self.reduce_outcome(outcome)
                }
            };
            self.tools.apply_pending_tool_loads();
            match effect_disposition {
                AgentEffectDisposition::Retain => {}
                AgentEffectDisposition::Reduce => self.drain_agent_effects(),
                AgentEffectDisposition::Discard => self.discard_agent_effects(),
            }
            let _ = self.drain_inbox();
        }

        match self.outcome.take() {
            Some(outcome) => outcome,
            None if self.state.task().is_running() => TickOutcome::Working,
            None => TickOutcome::Idle,
        }
    }

    async fn prepare_adapter_context(
        adapters: &mut [Box<dyn ContextAdapter>],
        history_view: &dyn History,
    ) {
        for adapter in adapters {
            adapter.prepare(history_view).await;
        }
    }

    fn render_adapter_context(
        adapters: &mut [Box<dyn ContextAdapter>],
        context: &mut Context,
    ) -> Value {
        let mut sink = context.sink();
        for adapter in adapters {
            adapter.contribute(&mut sink);
        }
        sink.into_history()
    }

    pub(super) async fn run_iteration(
        &mut self,
        iteration_id: IterationId,
        events: &EventSink,
        event_boundary: &AgentEventBoundary,
    ) -> IterationResult {
        // Context adapters may perform auxiliary LLM work (conversation
        // compaction and long-term-memory extraction). Keep that work in a
        // distinct bracket so it cannot disappear into the parent `agent` span
        // before the user-facing iteration begins.
        let adapter_count = self.context_adapters.len() as u64;
        let prepare_span = tracing::info_span!(
            "iteration.prepare",
            run.iteration = %iteration_id,
            adapter_count,
        );
        let tools = prepare_span.in_scope(|| self.tools.begin())?;
        let history_view = self.transcript.as_history();
        Self::prepare_adapter_context(&mut self.context_adapters, history_view)
            .instrument(prepare_span.clone())
            .await;
        prepare_span.in_scope(|| {
            self.context_cache
                .with(Block::new(BlockKind::ToolPolicy, tools.tool_context()))
                .with_reminder(BlockKind::ToolReminder, Some(tools.extra_tool_context()))
                .with_reminder(resume_reminder_kind(), self.resume_reminder.as_deref());
        });

        let render_span =
            prepare_span.in_scope(|| tracing::info_span!("context.render", adapter_count));
        let history = render_span.in_scope(|| {
            Self::render_adapter_context(&mut self.context_adapters, &mut self.context_cache)
        });
        let iteration_loop = IterationLoop {
            llm: &mut self.llm,
            interruption: &self.interruption,
            retry: self.retry_policy,
            events,
        };
        let permission_gate = PermissionGate {
            policy: self.permission_policy.as_ref(),
        };
        let gate = &permission_gate as &dyn ToolGate;
        let context = render_span.in_scope(|| self.context_cache.request(&history));
        let step = IterationStep {
            iteration_id,
            system_prompt: context.system(),
            messages: context.history(),
            reminders: context.reminders(),
            tools: &tools,
            gate,
            event_boundary: Some(event_boundary),
        };
        drop(render_span);
        drop(prepare_span);
        let result = iteration_loop.run(step).await;
        self.resume_reminder = None;
        self.context_cache
            .with_reminder(resume_reminder_kind(), None);
        result
    }

    async fn resume_approval(
        &mut self,
        resume: ApprovalResume,
    ) -> Result<PendingToolRound, AgentRunError> {
        let (pending, pending_tools) = resume.pending_tools.pop_next().map_err(|error| {
            tracing::error!(
                name: "approval_resume_invalid_round",
                detail = %error,
            );
            AgentRunError::TaskStateInvariant
        })?;
        let call = ToolInvocation::try_from(RawToolInvocation {
            id: Some(&pending.tool_call_id),
            name: &pending.name,
            arguments_json: &pending.arguments_json,
        })
        .map_err(|error| {
            tracing::error!(
                name: "approval_resume_invalid_invocation",
                detail = %error,
            );
            AgentRunError::TaskStateInvariant
        })?;
        let gate = ResolvedPermissionGate {
            policy: self.permission_policy.as_ref(),
            expected_signature: &pending.signature,
            decision: &resume.decision,
        };
        let tools = self
            .tools
            .begin()
            .map_err(IterationLoopError::from)
            .map_err(AgentRunError::from)?;
        let runner = ToolRunner::new(&tools, Some(&gate));
        let outcome = runner.run(&call).await;
        pending_tools.resolve(pending, outcome).map_err(|error| {
            tracing::error!(
                name: "approval_resume_invalid_round",
                detail = %error,
            );
            AgentRunError::TaskStateInvariant
        })
    }
}

fn resume_reminder_kind() -> BlockKind {
    BlockKind::Custom {
        band: Band::Volatile,
        scope: Scope::Agent,
        order: 1,
        label: Cow::Borrowed("resume"),
    }
}
