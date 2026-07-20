use super::mode::AgentMode;
use super::task_state::TaskState;
use super::IterationIdAllocator;

pub(super) struct BlockPolicy {
    retries: u32,
    blocked_rounds: u32,
}

impl BlockPolicy {
    fn new(retries: u32) -> Self {
        Self {
            retries,
            blocked_rounds: 0,
        }
    }

    pub(super) fn record_round(&mut self, blocked: &[&str]) -> ToolBlockVerdict {
        if blocked.is_empty() {
            self.blocked_rounds = 0;
            return ToolBlockVerdict::Continue;
        }
        self.blocked_rounds = self.blocked_rounds.saturating_add(1);
        if self.blocked_rounds > self.retries {
            return ToolBlockVerdict::Exhausted {
                name: blocked[0].to_string(),
            };
        }
        ToolBlockVerdict::Continue
    }
}

pub(super) enum ToolBlockVerdict {
    Continue,
    Exhausted { name: String },
}

pub(super) struct BaseAgentState {
    pub(super) block_policy: BlockPolicy,
    pub(super) id_allocator: IterationIdAllocator,
    pub(super) mode: AgentMode,
    task: TaskState,
}

impl BaseAgentState {
    pub(super) fn new(block_retries: u32, mode: AgentMode) -> Self {
        Self {
            block_policy: BlockPolicy::new(block_retries),
            id_allocator: IterationIdAllocator::new(),
            mode,
            task: TaskState::new(),
        }
    }

    pub(super) fn task(&self) -> &TaskState {
        &self.task
    }

    pub(super) fn task_mut(&mut self) -> &mut TaskState {
        &mut self.task
    }
}
