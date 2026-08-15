//! Skill tools owned by the skill context provider.

use alloc::{
    borrow::ToOwned,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::cell::RefCell;

use barracuda_agent_skill::{SkillError, SkillName, SkillSet};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolOutput,
    ToolSpec,
};
use serde::Deserialize;

use super::lock_skill_set;

#[derive(Deserialize)]
struct ReadArgs {
    name: String,
}

pub(super) fn skill_tools(skills: Arc<RefCell<SkillSet>>) -> ToolGroup {
    ToolGroup::new(
        "skill",
        true,
        [
            Tool::new(ListSkillTool {
                skills: Arc::clone(&skills),
            }),
            Tool::new(ReadSkillTool {
                skills: Arc::clone(&skills),
            }),
            Tool::new(ReloadSkillsTool { skills }),
        ],
    )
}

/// Serves the available-skills JSON catalog resolved from the agent's SkillSet.
struct ListSkillTool {
    skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ListSkillTool {
    tool_metadata!("skill_list");
}

impl ToolHandler for ListSkillTool {
    type Args = EmptyArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        _args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let mut skills = lock_skill_set(&self.skills);
            let output = skills.list_skills().to_owned();
            Ok(ToolOutput {
                content: output,
                ok: true,
            })
        })
    }
}

/// Reads one skill's Markdown instructions into the current tool result.
struct ReadSkillTool {
    skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ReadSkillTool {
    tool_metadata!("skill_read");
}

impl ToolHandler for ReadSkillTool {
    type Args = ReadArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let skill_name = args.name.trim();
            let mut skills = lock_skill_set(&self.skills);
            match skills.read_skill(&SkillName::new(skill_name)) {
                Ok(document) => {
                    let (instructions, directory) = document.into_parts();
                    let content = if let Some(directory) = directory {
                        format!(
                        "Skill directory: {directory}\nResolve relative resource paths in these instructions against that directory.\n\n{instructions}"
                    )
                    } else {
                        instructions
                    };
                    Ok(ToolOutput { content, ok: true })
                }
                Err(SkillError::NotFound(_)) => Err(ToolError::InvokeRejected(format!(
                    "unknown skill \"{skill_name}\"; call skill_list to see what is available."
                ))
                .into()),
                Err(error) => Ok(ToolOutput {
                    content: format!("Could not read skill \"{skill_name}\": {error}"),
                    ok: false,
                }),
            }
        })
    }
}

/// Re-scans the skill registry's roots and swaps in a fresh catalog.
struct ReloadSkillsTool {
    skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ReloadSkillsTool {
    tool_metadata!("skill_reload");
}

impl ToolHandler for ReloadSkillsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(
        &'a self,
        _context: barracuda_agent_tool::ToolContext,
        _args: Self::Args,
    ) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let skills = lock_skill_set(&self.skills);
            if let Err(error) = skills.reload() {
                return Ok(ToolOutput {
                    content: format!("Could not refresh skills from disk: {error}"),
                    ok: false,
                });
            }
            Ok(ToolOutput {
                content: "Skills refreshed. Use skill_list to inspect the catalog.".to_string(),
                ok: true,
            })
        })
    }
}
