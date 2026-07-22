//! One LLM/tool round-trip per [`IterationLoop`].
//!
//! Tool authorization is statically injected through [`ToolPermissionPolicy`].
//! This layer schedules allowed calls and waits for pending decisions; approval
//! transport belongs entirely to the injected implementation.
//!
//! On preemption this layer only detects the signal and ends the iteration.
//! It does not read, format, or return interrupt message content — upper layers
//! own pending input and context rebuild.

mod inflight_tool_call;
mod run;
mod stream;
mod types;

use core::future::Future;
use core::pin::Pin;

use claw_permission::Action;

use claw_api::{ClawApiAsync, RetryPolicy, ToolCall};
use claw_interface::{ClawHttp, ClawTimer};

use super::stream::RunControl;

pub(crate) use inflight_tool_call::InflightToolCall;
pub(crate) use stream::{IterationEmitter, IterationStream};
pub(crate) use types::{IterationEvent, IterationLoopError, IterationLoopEvent, LlmStep};

crate::define_prefixed_id!(IterationId, "iteration-", "iteration");
crate::define_prefixed_id!(ToolCallId, "tool-call-", "tool call");
crate::define_id_allocator!(
    /// Reset for each agent task.
    pub(super) IterationIdAllocator(IterationId),
    IterationId(0)
);
crate::define_id_allocator!(
    /// Reset for each iteration; never persisted.
    pub(super) ToolCallIdAllocator(ToolCallId),
    ToolCallId(0)
);

pub(super) struct ToolPermissionRequest<'a> {
    pub(super) tool_call_id: ToolCallId,
    pub(super) tool_call: &'a ToolCall,
    pub(super) action: &'a Action,
}

pub(super) enum ToolPermission {
    Allow,
    Deny(String),
    Interrupted,
    Cancelled,
}

pub(super) type PendingToolPermission<'a> = Pin<Box<dyn Future<Output = ToolPermission> + 'a>>;

pub(super) enum ToolAuthorization<'a> {
    Allow,
    Deny(String),
    Pending(PendingToolPermission<'a>),
}

/// Statically dispatched permission seam for one prepared tool call.
///
/// Immediate allow/deny decisions are returned synchronously so the iteration
/// can start allowed tools without waiting for unrelated approvals. Only a
/// genuinely pending permission carries a future.
pub(super) trait ToolPermissionPolicy {
    fn authorize<'a>(
        &'a self,
        request: ToolPermissionRequest<'_>,
        events: &IterationEmitter,
    ) -> ToolAuthorization<'a>;
}

/// [`claw_permission::AllowAll`] is the YOLO policy for callers that
/// intentionally run every valid tool call without an approval boundary.
impl ToolPermissionPolicy for claw_permission::AllowAll {
    fn authorize(
        &self,
        _request: ToolPermissionRequest<'_>,
        _events: &IterationEmitter,
    ) -> ToolAuthorization<'_> {
        ToolAuthorization::Allow
    }
}

/// One LLM response followed by its complete tool-call round.
///
/// Generic over the HTTP transport `H` so the LLM call stays statically
/// dispatched. The loop borrows the agent's [`ClawApiAsync`] mutably for exactly one
/// `chat` round, so it is consumed by [`run`](Self::run).
pub(crate) struct IterationLoop<'a, H: ClawHttp, Timer: ClawTimer, P> {
    pub llm: &'a mut ClawApiAsync<H, Timer>,
    pub control: &'a RunControl,
    pub permission: &'a P,
    /// Retry policy applied to this iteration's LLM call (see [`RetryPolicy`]).
    pub retry: RetryPolicy,
}
