use alloc::{
    borrow::ToOwned, collections::BTreeMap, collections::BTreeSet, collections::VecDeque,
    string::String, string::ToString, vec::Vec,
};
use core::cell::OnceCell;
use core::task::{Context, Poll};

use barracuda_agent_tool::ToolGroup;
use barracuda_runtime_utils::oneshot;
use portable_atomic_util::Arc;

use barracuda_agent::Message;
use barracuda_agent::{AgentId, AgentKind};

use super::effect::{
    DispatchOutcome, EffectId, InterruptOutcome, MultiagentEffect, MultiagentEffectResult,
    MultiagentPhysicalError,
};
use super::model::{MultiagentSnapshot, SubagentResult, SubagentStatus, SubagentTimeout};
use super::policy::SpawnPolicy;
use super::state::MultiagentState;
use super::tool_port::{
    DeleteCommand, FollowupCommand, MultiagentAction, MultiagentBridge, MultiagentCommandError,
    SpawnCommand,
};

enum DispatchPurpose {
    Initial {
        child: AgentId,
    },
    Followup {
        completed: oneshot::Sender<Result<(), MultiagentCommandError>>,
    },
}

/// Subagents one Session may keep at once, across every depth. Each one is a
/// full Agent with its own transcript, so the bound keeps a model from
/// exhausting device memory by fanning out or nesting.
const MAX_LIVE_SUBAGENTS: usize = 3;

enum PendingEffect {
    Spawn {
        requester: AgentId,
        accepted: oneshot::Sender<Result<AgentId, MultiagentCommandError>>,
        completion: oneshot::Sender<SubagentResult>,
    },
    Dispatch {
        target: AgentId,
        purpose: DispatchPurpose,
    },
    Interrupt {
        command: FollowupCommand,
    },
}

enum RemovalCause {
    Retire {
        agent: AgentId,
        parent: AgentId,
        result: SubagentResult,
    },
    Timeout {
        agent: AgentId,
        parent: AgentId,
        result: SubagentResult,
    },
    Delete {
        target: AgentId,
        completed: Option<oneshot::Sender<Result<(), MultiagentCommandError>>>,
    },
    Cleanup,
}

struct RemovalPlan {
    victims: BTreeSet<AgentId>,
    reaped: BTreeSet<AgentId>,
    removed: BTreeSet<AgentId>,
    failure: Option<MultiagentPhysicalError>,
    cause: RemovalCause,
}

/// Optional multiagent orchestration attached to one Session.
///
/// It owns topology, join state, pending result delivery, command validation,
/// and timeout policy. It never owns live Agents, Agent slots, AgentManager,
/// Session identity, timers, or persistence.
pub struct Multiagent {
    state: MultiagentState,
    /// Created when tools first need it, so an orchestration that never runs
    /// Agents allocates nothing.
    bridge: OnceCell<Arc<MultiagentBridge>>,
    effects: VecDeque<MultiagentEffect>,
    pending_effects: BTreeMap<EffectId, PendingEffect>,
    next_effect_id: u64,
    routes: BTreeMap<AgentId, oneshot::Sender<SubagentResult>>,
    pending_interrupts: BTreeMap<AgentId, FollowupCommand>,
    removals: Vec<RemovalPlan>,
}

impl Multiagent {
    pub fn new() -> Self {
        Self {
            state: MultiagentState::default(),
            bridge: OnceCell::new(),
            effects: VecDeque::new(),
            pending_effects: BTreeMap::new(),
            next_effect_id: 1,
            routes: BTreeMap::new(),
            pending_interrupts: BTreeMap::new(),
            removals: Vec::new(),
        }
    }

    pub fn tool_group(&self, caller: AgentId, kind: &AgentKind) -> Option<ToolGroup> {
        super::tools::tool_group(caller, kind, Arc::clone(self.bridge()))
    }

    pub fn register_root(&mut self, id: AgentId, kind: AgentKind) -> bool {
        let inserted = self.state.insert_root(id, kind);
        if inserted {
            self.publish_snapshot();
        }
        inserted
    }

    pub fn contains(&self, id: AgentId) -> bool {
        self.state.contains(id)
    }

    pub fn agent_ids(&self) -> Vec<AgentId> {
        self.state.agent_ids().collect()
    }

    pub fn root_children(&self) -> Vec<AgentId> {
        self.state.root_children()
    }

    pub fn poll_effect(&mut self, context: &mut Context<'_>) -> Poll<Option<MultiagentEffect>> {
        self.reap_cancelled_completion_receivers();
        if let Some(effect) = self.take_effect() {
            return Poll::Ready(Some(effect));
        }

        let Some(bridge) = self.bridge.get() else {
            return Poll::Pending;
        };
        let Poll::Ready(command) = bridge.poll_command(context) else {
            return Poll::Pending;
        };
        let (requester, action) = command.into_parts();
        match action {
            MultiagentAction::Spawn(command) => self.prepare_spawn(requester, command),
            MultiagentAction::Delete(command) => self.prepare_delete(requester, command),
            MultiagentAction::Followup(command) => self.prepare_followup(requester, command),
            MultiagentAction::AcknowledgeDelivery(child) => {
                self.acknowledge_delivery(requester, child);
            }
        }
        Poll::Ready(self.take_effect())
    }

    pub fn take_effect(&mut self) -> Option<MultiagentEffect> {
        self.effects.pop_front()
    }

    pub fn apply_result(&mut self, result: MultiagentEffectResult) -> Option<AgentId> {
        match result {
            MultiagentEffectResult::Spawned { id, spec, outcome } => {
                self.reduce_spawn(id, spec, outcome)
            }
            MultiagentEffectResult::Dispatched { id, outcome } => {
                self.reduce_dispatch(id, outcome);
                None
            }
            MultiagentEffectResult::Interrupted { id, outcome } => {
                self.reduce_interrupt(id, outcome);
                None
            }
        }
    }

    pub fn on_agent_started(&mut self, agent: AgentId) {
        if self.state.status(agent) != Some(SubagentStatus::Reaping) {
            self.state.set_status(agent, SubagentStatus::Running);
            self.publish_snapshot();
        }
    }

    pub fn on_agent_awaiting_approval(&mut self, agent: AgentId) {
        if self.state.status(agent) != Some(SubagentStatus::Reaping) {
            self.state
                .set_status(agent, SubagentStatus::AwaitingApproval);
            self.publish_snapshot();
        }
    }

    pub fn on_approval_resolved(&mut self, agent: AgentId) {
        if self.state.status(agent) == Some(SubagentStatus::AwaitingApproval) {
            self.state.set_status(agent, SubagentStatus::Running);
            self.publish_snapshot();
        }
    }

    pub fn on_agent_completed(&mut self, agent: AgentId, text: String, ok: bool) {
        if let Some(command) = self.pending_interrupts.remove(&agent) {
            self.queue_dispatch(
                agent,
                command.message,
                DispatchPurpose::Followup {
                    completed: command.completed,
                },
            );
            return;
        }
        if !self.state.contains(agent)
            || matches!(
                self.state.status(agent),
                Some(SubagentStatus::Reaping | SubagentStatus::CompletedPendingDelivery)
            )
        {
            return;
        }
        if self.state.root() == Some(agent) {
            self.state.set_status(agent, SubagentStatus::Idle);
            self.publish_snapshot();
            return;
        }

        let Some(parent) = self.state.parent(agent).flatten() else {
            return;
        };
        let result = SubagentResult::new(agent, text, ok);
        if ok && self.state.has_children(agent) {
            self.state.set_status(agent, SubagentStatus::Idle);
            self.publish_snapshot();
            return;
        }

        if ok {
            self.begin_removal(
                vec![agent],
                RemovalCause::Retire {
                    agent,
                    parent,
                    result,
                },
            );
        } else {
            let victims = self.state.subtree_ids(agent);
            self.begin_removal(
                victims,
                RemovalCause::Timeout {
                    agent,
                    parent,
                    result,
                },
            );
        }
    }

    pub fn on_agent_cancelled(&mut self, agent: AgentId) {
        if self.state.root() == Some(agent) {
            self.state.set_status(agent, SubagentStatus::Idle);
            self.reap_cancelled_completion_receivers();
            self.publish_snapshot();
            return;
        }
        if !self.state.contains(agent) || self.state.status(agent) == Some(SubagentStatus::Reaping)
        {
            return;
        }
        let Some(parent) = self.state.parent(agent).flatten() else {
            return;
        };
        self.begin_removal(
            self.state.subtree_ids(agent),
            RemovalCause::Timeout {
                agent,
                parent,
                result: SubagentResult::new(agent, "subagent was cancelled".to_owned(), false),
            },
        );
    }

    pub fn on_agent_idle(&mut self, agent: AgentId) {
        if self.state.status(agent) == Some(SubagentStatus::Running) {
            self.state.set_status(agent, SubagentStatus::Idle);
        }
        self.publish_snapshot();
    }

    pub fn timeout(&mut self, agent: AgentId) {
        if !self.state.contains(agent)
            || matches!(
                self.state.status(agent),
                Some(SubagentStatus::Reaping | SubagentStatus::CompletedPendingDelivery)
            )
        {
            return;
        }
        let Some(parent) = self.state.parent(agent).flatten() else {
            return;
        };
        let victims = self.state.subtree_ids(agent);
        let message = format!(
            "subagent timed out after {} ms; deleted its subtree of {} agent(s)",
            self.state
                .node(agent)
                .and_then(|node| node.timeout())
                .map_or(0, SubagentTimeout::millis),
            victims.len()
        );
        self.begin_removal(
            victims,
            RemovalCause::Timeout {
                agent,
                parent,
                result: SubagentResult::new(agent, message, false),
            },
        );
    }

    pub fn physical_agent_removed(
        &mut self,
        agent: AgentId,
        result: Result<(), MultiagentPhysicalError>,
    ) {
        for removal in self
            .removals
            .iter_mut()
            .filter(|removal| removal.victims.contains(&agent))
        {
            removal.reaped.insert(agent);
            match &result {
                Ok(()) => {
                    removal.removed.insert(agent);
                }
                Err(error) if removal.failure.is_none() => {
                    removal.failure = Some(error.clone());
                }
                Err(_) => {}
            }
        }
        self.finish_removals();
    }

    pub fn cleanup_subagents(&mut self) {
        for removal in &mut self.removals {
            removal.cause = RemovalCause::Cleanup;
        }
        self.retry_failed_removals();
        let mut victims = Vec::new();
        for child in self.state.root_children() {
            victims.extend(self.state.subtree_ids(child));
        }
        victims.sort_unstable();
        victims.dedup();
        victims.retain(|agent| self.state.status(*agent) != Some(SubagentStatus::Reaping));
        if !victims.is_empty() {
            self.begin_removal(victims, RemovalCause::Cleanup);
        }
    }

    pub fn clear(&mut self) {
        self.state = MultiagentState::default();
        self.effects.clear();
        self.pending_effects.clear();
        self.routes.clear();
        self.pending_interrupts.clear();
        self.removals.clear();
        if let Some(bridge) = self.bridge.get() {
            bridge.clear();
        }
    }

    fn prepare_spawn(&mut self, requester: AgentId, command: SpawnCommand) {
        if command.accepted.is_canceled() {
            return;
        }
        if self.state.status(requester) != Some(SubagentStatus::Running) {
            let _ = command
                .accepted
                .send(Err(MultiagentCommandError::RequesterMissing));
            return;
        }
        let validation = self
            .state
            .node(requester)
            .ok_or(MultiagentCommandError::RequesterMissing)
            .and_then(|node| {
                let policy = SpawnPolicy::for_agent(node.kind()).ok_or_else(|| {
                    MultiagentCommandError::ForbiddenKind(command.spec.kind().as_str().to_owned())
                })?;
                if policy.allows(command.spec.kind()) && SpawnPolicy::is_known(command.spec.kind())
                {
                    Ok(())
                } else {
                    Err(MultiagentCommandError::ForbiddenKind(
                        command.spec.kind().as_str().to_owned(),
                    ))
                }
            });
        if let Err(error) = validation {
            let _ = command.accepted.send(Err(error));
            return;
        }
        if self.live_subagents() >= MAX_LIVE_SUBAGENTS {
            let _ = command
                .accepted
                .send(Err(MultiagentCommandError::LimitReached(
                    MAX_LIVE_SUBAGENTS,
                )));
            return;
        }
        let SpawnCommand {
            spec,
            accepted,
            completion,
        } = command;
        let id = self.reserve_effect(PendingEffect::Spawn {
            requester,
            accepted,
            completion,
        });
        self.effects.push_back(MultiagentEffect::Spawn { id, spec });
    }

    fn prepare_delete(&mut self, requester: AgentId, command: DeleteCommand) {
        if command.completed.is_canceled() {
            return;
        }
        if self.state.status(requester) != Some(SubagentStatus::Running) {
            let _ = command
                .completed
                .send(Err(MultiagentCommandError::RequesterMissing));
            return;
        }
        if !self.state.is_strict_descendant(requester, command.target) {
            let _ = command
                .completed
                .send(Err(MultiagentCommandError::TargetNotControlled));
            return;
        }
        if let Some(removal) = self.removals.iter_mut().find(|removal| {
            removal.failure.is_some()
                && matches!(
                    &removal.cause,
                    RemovalCause::Delete { target, .. } if *target == command.target
                )
        }) {
            removal.failure = None;
            let RemovalCause::Delete { completed, .. } = &mut removal.cause else {
                log::error!("failed removal retry no longer has a Delete cause");
                tracing::error!("failed removal retry no longer has a Delete cause");
                return;
            };
            *completed = Some(command.completed);
            let agents = removal
                .victims
                .difference(&removal.removed)
                .copied()
                .collect();
            self.queue_remove(agents);
            self.publish_snapshot();
            return;
        }
        if matches!(
            self.state.status(command.target),
            Some(SubagentStatus::Reaping | SubagentStatus::CompletedPendingDelivery)
        ) {
            let _ = command
                .completed
                .send(Err(MultiagentCommandError::TargetNotControlled));
            return;
        }
        self.begin_removal(
            self.state.subtree_ids(command.target),
            RemovalCause::Delete {
                target: command.target,
                completed: Some(command.completed),
            },
        );
    }

    fn prepare_followup(&mut self, requester: AgentId, command: FollowupCommand) {
        if command.completed.is_canceled() {
            return;
        }
        if self.state.status(requester) != Some(SubagentStatus::Running) {
            let _ = command
                .completed
                .send(Err(MultiagentCommandError::RequesterMissing));
            return;
        }
        if !self.state.is_strict_descendant(requester, command.target) {
            let _ = command
                .completed
                .send(Err(MultiagentCommandError::TargetNotControlled));
            return;
        }
        if self.pending_interrupts.contains_key(&command.target) {
            let _ = command
                .completed
                .send(Err(MultiagentCommandError::TargetBusy));
            return;
        }
        match self.state.status(command.target) {
            Some(SubagentStatus::Idle) => {
                self.queue_dispatch(
                    command.target,
                    command.message,
                    DispatchPurpose::Followup {
                        completed: command.completed,
                    },
                );
            }
            Some(SubagentStatus::AwaitingApproval | SubagentStatus::Running) => {
                let target = command.target;
                let id = self.reserve_effect(PendingEffect::Interrupt { command });
                self.effects
                    .push_back(MultiagentEffect::Interrupt { id, target });
            }
            Some(SubagentStatus::Ready) => {
                let _ = command
                    .completed
                    .send(Err(MultiagentCommandError::TargetBusy));
            }
            Some(SubagentStatus::Reaping | SubagentStatus::CompletedPendingDelivery) | None => {
                let _ = command
                    .completed
                    .send(Err(MultiagentCommandError::TargetNotControlled));
            }
        }
    }

    fn commit_spawn(
        &mut self,
        requester: AgentId,
        accepted: oneshot::Sender<Result<AgentId, MultiagentCommandError>>,
        completion: oneshot::Sender<SubagentResult>,
        spec: super::model::SubagentSpec,
        id: AgentId,
    ) -> Option<AgentId> {
        if self.state.status(requester) != Some(SubagentStatus::Running) {
            let _ = accepted.send(Err(MultiagentCommandError::RequesterMissing));
            return Some(id);
        }
        let (kind, name, goal, timeout) = spec.into_parts();
        if !self.state.insert_child(requester, id, kind, name, timeout) {
            let _ = accepted.send(Err(MultiagentCommandError::RequesterMissing));
            return Some(id);
        }
        self.routes.insert(id, completion);
        if accepted.send(Ok(id)).is_err() {
            self.begin_removal(self.state.subtree_ids(id), RemovalCause::Cleanup);
            return None;
        }
        self.queue_timeout(id, timeout);
        self.queue_dispatch(id, goal, DispatchPurpose::Initial { child: id });
        self.publish_snapshot();
        None
    }

    fn reduce_spawn(
        &mut self,
        effect: EffectId,
        spec: super::model::SubagentSpec,
        outcome: Result<AgentId, MultiagentPhysicalError>,
    ) -> Option<AgentId> {
        let Some(PendingEffect::Spawn {
            requester,
            accepted,
            completion,
        }) = self.pending_effects.remove(&effect)
        else {
            return outcome.ok();
        };
        match outcome {
            Ok(agent) => self.commit_spawn(requester, accepted, completion, spec, agent),
            Err(error) => {
                let _ = accepted.send(Err(MultiagentCommandError::CreateFailed(error.to_string())));
                None
            }
        }
    }

    fn reduce_dispatch(&mut self, effect: EffectId, outcome: DispatchOutcome) {
        let Some(PendingEffect::Dispatch { target, purpose }) =
            self.pending_effects.remove(&effect)
        else {
            return;
        };
        match purpose {
            DispatchPurpose::Initial { child } => match outcome {
                DispatchOutcome::Accepted => {
                    self.state.set_status(child, SubagentStatus::Running);
                }
                DispatchOutcome::Busy | DispatchOutcome::Missing => {
                    self.begin_removal(self.state.subtree_ids(child), RemovalCause::Cleanup);
                }
            },
            DispatchPurpose::Followup { completed } => {
                let result = match outcome {
                    DispatchOutcome::Accepted => {
                        self.state.set_status(target, SubagentStatus::Running);
                        Ok(())
                    }
                    DispatchOutcome::Busy => Err(MultiagentCommandError::TargetBusy),
                    DispatchOutcome::Missing => Err(MultiagentCommandError::TargetNotControlled),
                };
                let _ = completed.send(result);
            }
        }
        self.publish_snapshot();
    }

    fn reduce_interrupt(&mut self, effect: EffectId, outcome: InterruptOutcome) {
        let Some(PendingEffect::Interrupt { command }) = self.pending_effects.remove(&effect)
        else {
            return;
        };
        match outcome {
            InterruptOutcome::Accepted => {
                let previous = self.pending_interrupts.insert(command.target, command);
                debug_assert!(previous.is_none());
            }
            InterruptOutcome::Inactive => {
                let _ = command
                    .completed
                    .send(Err(MultiagentCommandError::TargetBusy));
            }
            InterruptOutcome::Missing => {
                let _ = command
                    .completed
                    .send(Err(MultiagentCommandError::TargetNotControlled));
            }
        }
        self.publish_snapshot();
    }

    fn next_effect(&mut self) -> EffectId {
        let id = EffectId(self.next_effect_id);
        self.next_effect_id = self.next_effect_id.wrapping_add(1);
        id
    }

    /// Committed subagents plus spawns still waiting for their Agent, so
    /// concurrent spawn calls cannot pass the limit together.
    fn live_subagents(&self) -> usize {
        let pending = self
            .pending_effects
            .values()
            .filter(|effect| matches!(effect, PendingEffect::Spawn { .. }))
            .count();
        self.state.subagent_count().saturating_add(pending)
    }

    fn reserve_effect(&mut self, pending: PendingEffect) -> EffectId {
        let id = self.next_effect();
        let previous = self.pending_effects.insert(id, pending);
        debug_assert!(previous.is_none());
        id
    }

    fn queue_dispatch(&mut self, target: AgentId, message: Message, purpose: DispatchPurpose) {
        let id = self.reserve_effect(PendingEffect::Dispatch { target, purpose });
        self.effects.push_back(MultiagentEffect::Dispatch {
            id,
            target,
            message,
        });
    }

    fn queue_remove(&mut self, agents: Vec<AgentId>) {
        let id = self.next_effect();
        self.effects
            .push_back(MultiagentEffect::RemoveAgents { id, agents });
    }

    fn queue_timeout(&mut self, agent: AgentId, timeout: SubagentTimeout) {
        let id = self.next_effect();
        self.effects
            .push_back(MultiagentEffect::ArmTimeout { id, agent, timeout });
    }

    fn begin_removal(&mut self, victims: Vec<AgentId>, cause: RemovalCause) {
        let victims = victims
            .into_iter()
            .filter(|id| self.state.contains(*id))
            .collect::<BTreeSet<_>>();
        if victims.is_empty() {
            if let RemovalCause::Delete {
                completed: Some(completed),
                ..
            } = cause
            {
                let _ = completed.send(Err(MultiagentCommandError::TargetNotControlled));
            }
            return;
        }

        // Subtrees are either disjoint or nested. A parent may fail or time out
        // while one of its descendants is already being reaped. Supersede the
        // descendant result route with cleanup, but keep its physical removal
        // plan alive so one return acknowledges both plans.
        let mut already_reaped = BTreeSet::new();
        let mut already_removed = BTreeSet::new();
        for removal in self
            .removals
            .iter_mut()
            .filter(|removal| !removal.victims.is_disjoint(&victims))
        {
            already_reaped.extend(removal.reaped.intersection(&victims).copied());
            already_removed.extend(removal.removed.intersection(&victims).copied());
            removal.failure = None;
            removal.cause = RemovalCause::Cleanup;
        }
        for victim in &victims {
            self.state.set_status(*victim, SubagentStatus::Reaping);
        }
        let cancelled_effects = self
            .effects
            .iter()
            .filter_map(|effect| match effect {
                MultiagentEffect::Spawn { id, .. } => match self.pending_effects.get(id) {
                    Some(PendingEffect::Spawn { requester, .. }) if victims.contains(requester) => {
                        Some(*id)
                    }
                    _ => None,
                },
                MultiagentEffect::Dispatch { id, target, .. }
                | MultiagentEffect::Interrupt { id, target }
                | MultiagentEffect::ArmTimeout {
                    id, agent: target, ..
                } if victims.contains(target) => Some(*id),
                MultiagentEffect::RemoveAgents { .. }
                | MultiagentEffect::Dispatch { .. }
                | MultiagentEffect::Interrupt { .. }
                | MultiagentEffect::ArmTimeout { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        self.effects.retain(|effect| {
            let id = match effect {
                MultiagentEffect::Spawn { id, .. }
                | MultiagentEffect::Dispatch { id, .. }
                | MultiagentEffect::Interrupt { id, .. }
                | MultiagentEffect::RemoveAgents { id, .. }
                | MultiagentEffect::ArmTimeout { id, .. } => id,
            };
            !cancelled_effects.contains(id)
        });
        for effect in cancelled_effects {
            self.pending_effects.remove(&effect);
        }
        for victim in &victims {
            if let Some(command) = self.pending_interrupts.remove(victim) {
                let _ = command
                    .completed
                    .send(Err(MultiagentCommandError::TargetNotControlled));
            }
        }
        let agents = victims
            .difference(&already_removed)
            .copied()
            .collect::<Vec<_>>();
        if !agents.is_empty() {
            self.queue_remove(agents);
        }
        self.removals.push(RemovalPlan {
            victims,
            reaped: already_reaped,
            removed: already_removed,
            failure: None,
            cause,
        });
        self.publish_snapshot();
    }

    fn finish_removals(&mut self) {
        let mut pending = Vec::with_capacity(self.removals.len());
        for mut removal in core::mem::take(&mut self.removals) {
            if removal.reaped != removal.victims {
                pending.push(removal);
                continue;
            }
            if let Some(error) = removal.failure.as_ref() {
                match &mut removal.cause {
                    RemovalCause::Delete { completed, .. } => {
                        let completed = completed.take();
                        if let Some(completed) = completed {
                            let _ = completed
                                .send(Err(MultiagentCommandError::RemoveFailed(error.to_string())));
                        }
                        pending.push(removal);
                    }
                    RemovalCause::Retire { .. }
                    | RemovalCause::Timeout { .. }
                    | RemovalCause::Cleanup => {
                        log::error!(
                            "physical Agent storage cleanup failed; \
                             completing transient logical cleanup: {error}"
                        );
                        tracing::error!(
                            name: "multiagent_cleanup_failed",
                            error = %error,
                            "physical Agent storage cleanup failed; completing transient logical cleanup"
                        );
                        self.finish_removal(removal);
                    }
                }
                continue;
            }
            if removal.removed != removal.victims {
                pending.push(removal);
                continue;
            }
            self.finish_removal(removal);
        }
        self.removals = pending;
        self.publish_snapshot();
    }

    fn retry_failed_removals(&mut self) {
        let mut effects = Vec::new();
        for removal in &mut self.removals {
            if removal.failure.take().is_some() {
                effects.push(
                    removal
                        .victims
                        .difference(&removal.removed)
                        .copied()
                        .collect(),
                );
            }
        }
        for agents in effects {
            self.queue_remove(agents);
        }
    }

    fn finish_removal(&mut self, removal: RemovalPlan) {
        match removal.cause {
            RemovalCause::Retire {
                agent,
                parent,
                result,
            } => self.publish_result(agent, parent, result),
            RemovalCause::Timeout {
                agent,
                parent,
                result,
            } => {
                self.state
                    .remove_agents(&removal.victims.iter().copied().collect::<Vec<_>>());
                self.drop_routes_for_removed(&removal.victims, Some(agent));
                self.publish_result(agent, parent, result);
            }
            RemovalCause::Delete { completed, .. } => {
                self.state
                    .remove_agents(&removal.victims.iter().copied().collect::<Vec<_>>());
                self.drop_routes_for_removed(&removal.victims, None);
                if let Some(completed) = completed {
                    let _ = completed.send(Ok(()));
                }
            }
            RemovalCause::Cleanup => {
                self.state
                    .remove_agents(&removal.victims.iter().copied().collect::<Vec<_>>());
                self.drop_routes_for_removed(&removal.victims, None);
            }
        }
    }

    fn publish_result(&mut self, agent: AgentId, _parent: AgentId, result: SubagentResult) {
        self.state
            .set_status(agent, SubagentStatus::CompletedPendingDelivery);
        let delivered = self
            .routes
            .remove(&agent)
            .is_some_and(|completion| completion.send(result).is_ok());
        if !delivered {
            self.state.remove_agents(&[agent]);
        }
    }

    fn acknowledge_delivery(&mut self, parent: AgentId, child: AgentId) {
        if self.state.parent(child) == Some(Some(parent))
            && self.state.status(child) == Some(SubagentStatus::CompletedPendingDelivery)
        {
            self.routes.remove(&child);
            self.state.remove_agents(&[child]);
            self.publish_snapshot();
        }
    }

    fn drop_routes_for_removed(&mut self, victims: &BTreeSet<AgentId>, except: Option<AgentId>) {
        let removed = victims
            .iter()
            .copied()
            .filter(|id| Some(*id) != except)
            .collect::<Vec<_>>();
        for victim in removed {
            if let Some(completion) = self.routes.remove(&victim) {
                let _ = completion.send(SubagentResult::new(
                    victim,
                    "subagent was deleted before returning a result".to_owned(),
                    false,
                ));
            }
        }
    }

    fn reap_cancelled_completion_receivers(&mut self) {
        self.pending_interrupts
            .retain(|_, command| !command.completed.is_canceled());
        let cancelled = self
            .routes
            .iter()
            .filter_map(|(&agent, sender)| sender.is_canceled().then_some(agent))
            .collect::<Vec<_>>();
        for agent in cancelled {
            if self.state.status(agent) == Some(SubagentStatus::CompletedPendingDelivery) {
                self.routes.remove(&agent);
                self.state.remove_agents(&[agent]);
                self.publish_snapshot();
            } else if self.state.contains(agent)
                && self.state.status(agent) != Some(SubagentStatus::Reaping)
            {
                self.begin_removal(self.state.subtree_ids(agent), RemovalCause::Cleanup);
            }
        }
    }

    fn bridge(&self) -> &Arc<MultiagentBridge> {
        self.bridge
            .get_or_init(|| Arc::new(MultiagentBridge::new()))
    }

    fn publish_snapshot(&self) {
        self.bridge()
            .publish_snapshot(MultiagentSnapshot::new(self.state.snapshots()));
    }
}

impl Default for Multiagent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::model::SubagentSpec;
    use super::*;
    use barracuda_agent::Message;
    use futures_lite::future;

    fn timeout() -> SubagentTimeout {
        SubagentTimeout::new(core::num::NonZeroU32::new(60_000).expect("test timeout is non-zero"))
    }

    #[test]
    fn an_unused_orchestration_creates_no_bridge() {
        let mut multiagent = Multiagent::new();
        let waker = core::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(multiagent.poll_effect(&mut context).is_pending());
        multiagent.clear();
        assert!(multiagent.bridge.get().is_none());

        assert!(
            multiagent.register_root(AgentId::new(1), barracuda_agent::baked::root_kind().clone())
        );
        assert!(multiagent.bridge.get().is_some());
    }

    fn take_spawn_effect(multiagent: &mut Multiagent) -> (EffectId, SubagentSpec) {
        let effect = future::block_on(future::poll_fn(|cx| multiagent.poll_effect(cx)))
            .expect("spawn effect");
        let MultiagentEffect::Spawn { id, spec } = effect else {
            panic!("expected spawn effect");
        };
        (id, spec)
    }

    fn apply_spawned(
        multiagent: &mut Multiagent,
        effect: EffectId,
        spec: SubagentSpec,
        child: AgentId,
    ) -> Option<AgentId> {
        multiagent.apply_result(MultiagentEffectResult::Spawned {
            id: effect,
            spec,
            outcome: Ok(child),
        })
    }

    #[test]
    fn requester_bound_commands_cannot_cross_subtrees() {
        let root = AgentId(1);
        let first = AgentId(2);
        let second = AgentId(3);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            first,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            root,
            second,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        multiagent.publish_snapshot();

        let control =
            super::super::tool_port::SubagentControl::new(first, Arc::clone(multiagent.bridge()));
        assert!(control.get(second).is_none());
    }

    #[test]
    fn failed_spawn_never_commits_a_graph_child() {
        let root = AgentId(1);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        let bridge = Arc::clone(multiagent.bridge());
        let (mut accepted, _completion) = bridge.spawn(
            root,
            super::super::model::SubagentSpec::new(
                AgentKind::from_static("worker"),
                None,
                Message::text("goal"),
                timeout(),
            ),
        );
        let (effect, spec) = take_spawn_effect(&mut multiagent);
        multiagent.apply_result(MultiagentEffectResult::Spawned {
            id: effect,
            spec,
            outcome: Err(MultiagentPhysicalError::new("injected".to_owned())),
        });

        assert_eq!(multiagent.agent_ids(), vec![root]);
        assert!(matches!(
            accepted.try_recv(),
            Ok(Some(Err(MultiagentCommandError::CreateFailed(message)))) if message == "injected"
        ));
    }

    #[test]
    fn result_is_published_only_after_physical_removal() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        let bridge = Arc::clone(multiagent.bridge());
        let (mut accepted, mut completion) = bridge.spawn(
            root,
            super::super::model::SubagentSpec::new(
                AgentKind::from_static("worker"),
                Some("child".to_owned()),
                Message::text("goal"),
                timeout(),
            ),
        );
        let (effect, spec) = take_spawn_effect(&mut multiagent);
        assert_eq!(apply_spawned(&mut multiagent, effect, spec, child), None);
        assert_eq!(accepted.try_recv(), Ok(Some(Ok(child))));

        let _ = multiagent.take_effect().expect("timeout effect");
        let initial = multiagent.take_effect().expect("initial dispatch");
        let MultiagentEffect::Dispatch {
            id,
            target,
            message,
        } = initial
        else {
            panic!("expected initial dispatch");
        };
        assert_eq!(target, child);
        assert_eq!(message.as_str(), "goal");
        multiagent.apply_result(MultiagentEffectResult::Dispatched {
            id,
            outcome: DispatchOutcome::Accepted,
        });
        multiagent.on_agent_completed(child, "done".to_owned(), true);
        assert_eq!(
            multiagent.state.status(child),
            Some(SubagentStatus::Reaping)
        );
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { .. })
        ));
        assert_eq!(completion.try_recv(), Ok(None));

        multiagent.physical_agent_removed(child, Ok(()));
        assert_eq!(
            multiagent.state.status(child),
            Some(SubagentStatus::CompletedPendingDelivery)
        );
        assert!(completion
            .try_recv()
            .expect("completion is published after removal")
            .expect("completion value")
            .ok());
        multiagent.acknowledge_delivery(root, child);
        assert!(!multiagent.contains(child));
    }

    #[test]
    fn delete_commits_the_whole_subtree_after_every_physical_removal() {
        let root = AgentId(1);
        let child = AgentId(2);
        let grandchild = AgentId(3);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            child,
            grandchild,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        let (completed, mut result) = oneshot::channel();
        multiagent.prepare_delete(
            root,
            DeleteCommand {
                target: child,
                completed,
            },
        );
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. })
                if agents == vec![child, grandchild]
        ));

        multiagent.physical_agent_removed(child, Ok(()));
        assert!(multiagent.contains(child));
        assert_eq!(result.try_recv(), Ok(None));
        multiagent.physical_agent_removed(grandchild, Ok(()));
        assert!(!multiagent.contains(child));
        assert!(!multiagent.contains(grandchild));
        assert_eq!(result.try_recv(), Ok(Some(Ok(()))));
    }

    #[test]
    fn failed_physical_delete_keeps_the_graph_and_can_be_retried() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));

        let (first_completed, mut first_result) = oneshot::channel();
        multiagent.prepare_delete(
            root,
            DeleteCommand {
                target: child,
                completed: first_completed,
            },
        );
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));
        multiagent.physical_agent_removed(
            child,
            Err(MultiagentPhysicalError::new(
                "injected remove failure".to_owned(),
            )),
        );

        assert!(multiagent.contains(child));
        assert_eq!(
            multiagent.state.status(child),
            Some(SubagentStatus::Reaping)
        );
        assert!(matches!(
            first_result.try_recv(),
            Ok(Some(Err(MultiagentCommandError::RemoveFailed(detail))))
                if detail == "injected remove failure"
        ));

        let (retry_completed, mut retry_result) = oneshot::channel();
        multiagent.prepare_delete(
            root,
            DeleteCommand {
                target: child,
                completed: retry_completed,
            },
        );
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));
        multiagent.physical_agent_removed(child, Ok(()));

        assert!(!multiagent.contains(child));
        assert_eq!(retry_result.try_recv(), Ok(Some(Ok(()))));
    }

    #[test]
    fn dropped_completion_receiver_reaps_the_child_subtree() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        let (accepted_sender, mut accepted) = oneshot::channel();
        let (completion_sender, completion) = oneshot::channel();
        let command = SpawnCommand {
            spec: super::super::model::SubagentSpec::new(
                AgentKind::from_static("worker"),
                None,
                Message::text("goal"),
                timeout(),
            ),
            accepted: accepted_sender,
            completion: completion_sender,
        };
        multiagent.prepare_spawn(root, command);
        let (effect, spec) = take_spawn_effect(&mut multiagent);
        apply_spawned(&mut multiagent, effect, spec, child);
        assert_eq!(accepted.try_recv(), Ok(Some(Ok(child))));
        let _ = multiagent.take_effect();
        let _ = multiagent.take_effect();

        drop(completion);
        let removal = future::block_on(future::poll_fn(|cx| multiagent.poll_effect(cx)))
            .expect("cancelled receiver removal");
        assert!(matches!(
            removal,
            MultiagentEffect::RemoveAgents { agents, .. } if agents == vec![child]
        ));
        assert_eq!(
            multiagent.state.status(child),
            Some(SubagentStatus::Reaping)
        );
    }

    #[test]
    fn cancelling_root_preserves_live_work_with_open_completion_receivers() {
        let root = AgentId(1);
        let background = AgentId(2);
        let nested_foreground = AgentId(3);
        let cancelled_foreground = AgentId(4);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            background,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            background,
            nested_foreground,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            root,
            cancelled_foreground,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        multiagent
            .state
            .set_status(background, SubagentStatus::Running);
        multiagent
            .state
            .set_status(nested_foreground, SubagentStatus::Running);
        multiagent
            .state
            .set_status(cancelled_foreground, SubagentStatus::Running);
        let (background_sender, _background_receiver) = oneshot::channel();
        multiagent.routes.insert(background, background_sender);
        let (nested_sender, mut nested_receiver) = oneshot::channel();
        multiagent.routes.insert(nested_foreground, nested_sender);
        let (cancelled_sender, cancelled_receiver) = oneshot::channel();
        multiagent
            .routes
            .insert(cancelled_foreground, cancelled_sender);
        drop(cancelled_receiver);

        multiagent.on_agent_cancelled(root);

        assert_eq!(multiagent.state.status(root), Some(SubagentStatus::Idle));
        assert_eq!(
            multiagent.state.status(background),
            Some(SubagentStatus::Running)
        );
        assert_eq!(
            multiagent.state.status(nested_foreground),
            Some(SubagentStatus::Running)
        );
        assert_eq!(
            multiagent.state.status(cancelled_foreground),
            Some(SubagentStatus::Reaping)
        );
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. })
                if agents == vec![cancelled_foreground]
        ));
        assert_eq!(nested_receiver.try_recv(), Ok(None));
    }

    #[test]
    fn spawns_beyond_the_live_limit_are_rejected_until_one_finishes() {
        let root = AgentId(1);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        let spawn = |multiagent: &Multiagent| {
            multiagent.bridge().spawn(
                root,
                super::super::model::SubagentSpec::new(
                    AgentKind::from_static("worker"),
                    None,
                    Message::text("goal"),
                    timeout(),
                ),
            )
        };

        // Spawns still waiting for their Agent count toward the limit.
        let mut accepted = Vec::new();
        let mut pending = Vec::new();
        for _ in 0..MAX_LIVE_SUBAGENTS {
            let (receiver, completion) = spawn(&multiagent);
            accepted.push((receiver, completion));
            pending.push(take_spawn_effect(&mut multiagent));
        }
        let (mut rejected, _completion) = spawn(&multiagent);
        let waker = core::task::Waker::noop();
        assert!(matches!(
            multiagent.poll_effect(&mut Context::from_waker(waker)),
            Poll::Ready(None)
        ));
        assert_eq!(
            rejected.try_recv(),
            Ok(Some(Err(MultiagentCommandError::LimitReached(
                MAX_LIVE_SUBAGENTS
            ))))
        );

        for (index, (effect, spec)) in pending.into_iter().enumerate() {
            let child = AgentId(10 + index as u32);
            assert_eq!(apply_spawned(&mut multiagent, effect, spec, child), None);
        }
        while multiagent.take_effect().is_some() {}

        // A delivered result frees its slot.
        multiagent.on_agent_completed(AgentId(10), "done".to_owned(), true);
        while multiagent.take_effect().is_some() {}
        multiagent.physical_agent_removed(AgentId(10), Ok(()));
        multiagent.acknowledge_delivery(root, AgentId(10));
        assert!(!multiagent.contains(AgentId(10)));
        let (mut admitted, _completion) = spawn(&multiagent);
        take_spawn_effect(&mut multiagent);
        assert_eq!(admitted.try_recv(), Ok(None));
    }

    #[test]
    fn spawn_commit_rolls_back_when_requester_started_reaping() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        let (mut accepted, _completion) = multiagent.bridge().spawn(
            root,
            super::super::model::SubagentSpec::new(
                AgentKind::from_static("worker"),
                None,
                Message::text("goal"),
                timeout(),
            ),
        );
        let (effect, spec) = take_spawn_effect(&mut multiagent);

        multiagent.state.set_status(root, SubagentStatus::Reaping);
        assert_eq!(
            apply_spawned(&mut multiagent, effect, spec, child),
            Some(child)
        );

        assert_eq!(multiagent.agent_ids(), vec![root]);
        assert_eq!(
            accepted.try_recv(),
            Ok(Some(Err(MultiagentCommandError::RequesterMissing)))
        );
        assert!(multiagent.take_effect().is_none());
    }

    #[test]
    fn running_target_accepts_interrupt_dispatch() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        multiagent.state.set_status(child, SubagentStatus::Running);
        let (completed, mut result) = oneshot::channel();

        multiagent.prepare_followup(
            root,
            FollowupCommand {
                target: child,
                message: Message::text("too early"),
                completed,
            },
        );

        assert_eq!(result.try_recv(), Ok(None));
        let Some(MultiagentEffect::Interrupt { id, target }) = multiagent.take_effect() else {
            panic!("running target should emit an interrupt effect");
        };
        assert_eq!(target, child);

        multiagent.apply_result(MultiagentEffectResult::Interrupted {
            id,
            outcome: InterruptOutcome::Accepted,
        });

        assert_eq!(result.try_recv(), Ok(None));
        multiagent.on_agent_completed(child, "subagent was interrupted".to_owned(), false);
        let Some(MultiagentEffect::Dispatch {
            id,
            target,
            message,
        }) = multiagent.take_effect()
        else {
            panic!("completed interrupt should emit the deferred dispatch");
        };
        assert_eq!(target, child);
        assert_eq!(message.as_str(), "too early");

        multiagent.apply_result(MultiagentEffectResult::Dispatched {
            id,
            outcome: DispatchOutcome::Accepted,
        });

        assert_eq!(result.try_recv(), Ok(Some(Ok(()))));
        assert_eq!(
            multiagent.state.status(child),
            Some(SubagentStatus::Running)
        );
    }

    #[test]
    fn dropped_mutating_command_receivers_cancel_the_commands() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        multiagent.state.set_status(child, SubagentStatus::Idle);

        let (delete_completed, delete_result) = oneshot::channel();
        drop(delete_result);
        multiagent.prepare_delete(
            root,
            DeleteCommand {
                target: child,
                completed: delete_completed,
            },
        );

        let (followup_completed, followup_result) = oneshot::channel();
        drop(followup_result);
        multiagent.prepare_followup(
            root,
            FollowupCommand {
                target: child,
                message: Message::text("cancelled"),
                completed: followup_completed,
            },
        );

        assert_eq!(multiagent.state.status(child), Some(SubagentStatus::Idle));
        assert!(multiagent.take_effect().is_none());
    }

    #[test]
    fn automatic_timeout_cleanup_failure_still_finishes_logical_removal() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        let (completion, mut result) = oneshot::channel();
        multiagent.routes.insert(child, completion);

        multiagent.timeout(child);
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));
        multiagent.physical_agent_removed(
            child,
            Err(MultiagentPhysicalError::new(
                "injected transient cleanup failure".to_owned(),
            )),
        );

        assert!(!multiagent.contains(child));
        assert!(!result
            .try_recv()
            .expect("foreground timeout result")
            .expect("foreground timeout value")
            .ok());
        assert!(multiagent.removals.is_empty());
    }

    #[test]
    fn parent_timeout_joins_an_existing_descendant_removal() {
        let root = AgentId(1);
        let parent = AgentId(2);
        let child = AgentId(3);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            parent,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            parent,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        let (parent_completion, mut parent_result) = oneshot::channel();
        let (child_completion, _child_result) = oneshot::channel();
        multiagent.routes.insert(parent, parent_completion);
        multiagent.routes.insert(child, child_completion);

        multiagent.on_agent_completed(child, "child done".to_owned(), true);
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));

        multiagent.timeout(parent);
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. })
                if agents == vec![parent, child]
        ));
        assert_eq!(
            multiagent.state.status(parent),
            Some(SubagentStatus::Reaping)
        );

        multiagent.physical_agent_removed(child, Ok(()));
        assert!(multiagent.contains(parent));
        multiagent.physical_agent_removed(parent, Ok(()));

        assert!(!multiagent.contains(parent));
        assert!(!multiagent.contains(child));
        assert!(!parent_result
            .try_recv()
            .expect("parent timeout result")
            .expect("parent timeout value")
            .ok());
        assert!(multiagent.removals.is_empty());
    }

    #[test]
    fn lifecycle_cleanup_supersedes_failed_delete_and_cannot_hang() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        let (completed, mut result) = oneshot::channel();
        multiagent.prepare_delete(
            root,
            DeleteCommand {
                target: child,
                completed,
            },
        );
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));
        multiagent.physical_agent_removed(
            child,
            Err(MultiagentPhysicalError::new(
                "first remove failure".to_owned(),
            )),
        );
        assert!(matches!(
            result.try_recv(),
            Ok(Some(Err(MultiagentCommandError::RemoveFailed(_))))
        ));

        multiagent.cleanup_subagents();
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));
        multiagent.physical_agent_removed(
            child,
            Err(MultiagentPhysicalError::new(
                "cleanup remove failure".to_owned(),
            )),
        );

        assert!(!multiagent.contains(child));
        assert!(multiagent.removals.is_empty());
    }

    #[test]
    fn automatic_cleanup_waits_until_every_live_agent_is_reaped() {
        let root = AgentId(1);
        let parent = AgentId(2);
        let child = AgentId(3);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            parent,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            parent,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        let (completion, mut result) = oneshot::channel();
        multiagent.routes.insert(parent, completion);

        multiagent.timeout(parent);
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. })
                if agents == vec![parent, child]
        ));
        multiagent.physical_agent_removed(
            parent,
            Err(MultiagentPhysicalError::new(
                "parent storage cleanup failed".to_owned(),
            )),
        );

        assert!(multiagent.contains(parent));
        assert!(multiagent.contains(child));
        assert_eq!(result.try_recv(), Ok(None));

        multiagent.physical_agent_removed(child, Ok(()));
        assert!(!multiagent.contains(parent));
        assert!(!multiagent.contains(child));
        assert!(!result
            .try_recv()
            .expect("timeout result")
            .expect("timeout value")
            .ok());
    }

    #[test]
    fn cleanup_revokes_queued_spawn_start_effects() {
        let root = AgentId(1);
        let child = AgentId(2);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        let (mut accepted, _completion) = multiagent.bridge().spawn(
            root,
            super::super::model::SubagentSpec::new(
                AgentKind::from_static("worker"),
                None,
                Message::text("goal"),
                timeout(),
            ),
        );
        let (effect, spec) = take_spawn_effect(&mut multiagent);
        apply_spawned(&mut multiagent, effect, spec, child);
        assert_eq!(accepted.try_recv(), Ok(Some(Ok(child))));

        multiagent.cleanup_subagents();

        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. }) if agents == vec![child]
        ));
        assert!(multiagent.take_effect().is_none());
        assert_eq!(
            multiagent.state.status(child),
            Some(SubagentStatus::Reaping)
        );
    }

    #[test]
    fn delete_of_reaping_descendant_cannot_replace_parent_timeout_result() {
        let root = AgentId(1);
        let parent = AgentId(2);
        let child = AgentId(3);
        let mut multiagent = Multiagent::new();
        assert!(multiagent.register_root(root, AgentKind::from_static("conversation")));
        multiagent.on_agent_started(root);
        assert!(multiagent.state.insert_child(
            root,
            parent,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        assert!(multiagent.state.insert_child(
            parent,
            child,
            AgentKind::from_static("worker"),
            None,
            timeout()
        ));
        let (completion, mut completion_result) = oneshot::channel();
        multiagent.routes.insert(parent, completion);

        multiagent.timeout(parent);
        assert!(matches!(
            multiagent.take_effect(),
            Some(MultiagentEffect::RemoveAgents { agents, .. })
                if agents == vec![parent, child]
        ));
        multiagent.physical_agent_removed(
            parent,
            Err(MultiagentPhysicalError::new(
                "parent storage cleanup failed".to_owned(),
            )),
        );

        let (completed, mut result) = oneshot::channel();
        multiagent.prepare_delete(
            root,
            DeleteCommand {
                target: child,
                completed,
            },
        );
        assert_eq!(
            result.try_recv(),
            Ok(Some(Err(MultiagentCommandError::TargetNotControlled)))
        );
        assert!(multiagent.take_effect().is_none());

        multiagent.physical_agent_removed(child, Ok(()));
        assert!(!multiagent.contains(parent));
        assert!(!multiagent.contains(child));
        assert!(!completion_result
            .try_recv()
            .expect("timeout result")
            .expect("timeout value")
            .ok());
    }
}
