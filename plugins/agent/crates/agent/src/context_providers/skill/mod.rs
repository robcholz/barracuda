//! Skill context provider.
//!
//! This provider owns the runtime [`SkillSet`] source for an agent. It projects
//! the skill catalog into `BlockKind::SkillList` and exposes skill tools that
//! read from the same buffered source.

use alloc::sync::Arc;
use core::cell::{RefCell, RefMut};

use crate::engine::AgentStorage;
use barracuda_agent_context::{Block, BlockKind, ContextSink};
use barracuda_agent_skill::SkillSet;
use barracuda_agent_tool::{Tool, ToolGroup};

use self::tools::{ListSkillTool, ReadSkillResourceTool, ReadSkillTool, ReloadSkillsTool};
use crate::engine::{ContextProvider, ContextProviderResult};

mod tools;

pub(crate) struct SkillContextProvider {
    skills: Arc<RefCell<SkillSet>>,
}

impl SkillContextProvider {
    pub(crate) fn new(skills: SkillSet) -> Self {
        Self {
            skills: Arc::new(RefCell::new(skills)),
        }
    }
}

impl ContextProvider for SkillContextProvider {
    fn id(&self) -> &'static str {
        "skill"
    }

    fn contribute(
        &mut self,
        _storage: &AgentStorage,
        output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        let mut skills = lock_skill_set(&self.skills);
        let rendered = skills.catalog_context();
        output.block(Block::new(BlockKind::SkillList, rendered));
        Ok(())
    }

    fn tools(&self, _storage: &AgentStorage) -> Option<ToolGroup> {
        Some(ToolGroup::new(
            self.id(),
            true,
            [
                Tool::new(ListSkillTool {
                    skills: Arc::clone(&self.skills),
                }),
                Tool::new(ReadSkillTool {
                    skills: Arc::clone(&self.skills),
                }),
                Tool::new(ReadSkillResourceTool {
                    skills: Arc::clone(&self.skills),
                }),
                Tool::new(ReloadSkillsTool {
                    skills: Arc::clone(&self.skills),
                }),
            ],
        ))
    }
}

pub(super) fn lock_skill_set(skills: &RefCell<SkillSet>) -> RefMut<'_, SkillSet> {
    skills.borrow_mut()
}
