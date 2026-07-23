//! Skill tools owned by the skill context adapter.

use std::sync::{Arc, Mutex};

use claw_skill::{SkillError, SkillId, SkillSet};
use claw_tool::{
    tool_metadata, SyncToolHandler, Tool, ToolError, ToolGroup, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolSpec,
};
use serde_json::Value;

use super::lock_skill_set;

pub(super) fn skill_tools(skills: Arc<Mutex<SkillSet>>) -> ToolGroup {
    ToolGroup::new(
        "skill",
        true,
        [
            Tool::from_sync(ListSkillTool {
                skills: Arc::clone(&skills),
            }),
            Tool::from_sync(ActivateSkillTool {
                skills: Arc::clone(&skills),
            }),
            Tool::from_sync(ReloadSkillsTool { skills }),
        ],
    )
}

/// Serves the available-skills JSON catalog resolved from the agent's SkillSet.
struct ListSkillTool {
    skills: Arc<Mutex<SkillSet>>,
}

impl ToolSpec for ListSkillTool {
    tool_metadata!("skill_list");
}

impl SyncToolHandler for ListSkillTool {
    fn invoke(&self, _call: &ToolInvocation) -> Result<ToolOutput, ToolInvokeError> {
        let mut skills = lock_skill_set(&self.skills);
        let output = match skills.list_skill() {
            Ok(output) => output.to_owned(),
            Err(error) => {
                return Ok(ToolOutput {
                    content: format!("Could not list skills: {error}"),
                    ok: false,
                });
            }
        };
        Ok(ToolOutput {
            content: output,
            ok: true,
        })
    }
}

/// Activates one skill and returns its processed document as the tool result.
struct ActivateSkillTool {
    skills: Arc<Mutex<SkillSet>>,
}

impl ToolSpec for ActivateSkillTool {
    tool_metadata!("skill_activate");
}

impl SyncToolHandler for ActivateSkillTool {
    fn invoke(&self, call: &ToolInvocation) -> Result<ToolOutput, ToolInvokeError> {
        let args = call.arguments_value()?;
        let skill_id = match args.get("skill_id") {
            Some(Value::String(skill_id)) => skill_id.trim(),
            Some(_) => {
                return Err(
                    ToolError::InvalidArguments("`skill_id` must be a string".into()).into(),
                );
            }
            None => {
                return Err(ToolError::InvokeRejected(
                    "`skill_id` is required: pass the id of a skill from skill_list.".to_string(),
                )
                .into());
            }
        };
        if skill_id.is_empty() {
            return Err(ToolError::InvokeRejected(
                "`skill_id` is required: pass the id of a skill from skill_list.".to_string(),
            )
            .into());
        }

        let mut skills = lock_skill_set(&self.skills);
        match skills.activate_skill(&SkillId::new(skill_id)) {
            Ok(document) => Ok(ToolOutput {
                content: document.into_content(),
                ok: true,
            }),
            Err(SkillError::NotFound(_)) => Err(ToolError::InvokeRejected(format!(
                "unknown skill \"{skill_id}\"; call skill_list to see what is available."
            ))
            .into()),
            Err(error) => Ok(ToolOutput {
                content: format!("Could not activate skill \"{skill_id}\": {error}"),
                ok: false,
            }),
        }
    }
}

/// Re-scans the skill registry's roots and swaps in a fresh catalog.
struct ReloadSkillsTool {
    skills: Arc<Mutex<SkillSet>>,
}

impl ToolSpec for ReloadSkillsTool {
    tool_metadata!("skill_reload");
}

impl SyncToolHandler for ReloadSkillsTool {
    fn invoke(&self, _call: &ToolInvocation) -> Result<ToolOutput, ToolInvokeError> {
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
    }
}
