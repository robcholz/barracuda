//! Skill tools owned by the skill context provider.

use alloc::{
    borrow::ToOwned,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::cell::RefCell;

use barracuda_agent_skill::{
    SkillError, SkillName, SkillResourcePage, SkillSet, DEFAULT_RESOURCE_READ_BYTES,
};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, ToolError, ToolFuture, ToolHandler, ToolOutput, ToolSpec,
};
use serde::{Deserialize, Serialize};

use super::lock_skill_set;

#[derive(Deserialize)]
pub(super) struct ReadArgs {
    name: String,
}

fn default_resource_read_bytes() -> usize {
    DEFAULT_RESOURCE_READ_BYTES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadResourceArgs {
    name: String,
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_resource_read_bytes")]
    limit: usize,
}

/// Serves the available-skills JSON catalog resolved from the agent's SkillSet.
pub(super) struct ListSkillTool {
    pub(super) skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ListSkillTool {
    tool_metadata!("skill_list");
}

impl ToolHandler for ListSkillTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
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
pub(super) struct ReadSkillTool {
    pub(super) skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ReadSkillTool {
    tool_metadata!("skill_read");
}

impl ToolHandler for ReadSkillTool {
    type Args = ReadArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let skill_name = args.name.trim();
            let mut skills = { lock_skill_set(&self.skills).clone() };
            match skills.read_skill(&SkillName::new(skill_name)).await {
                Ok(document) => Ok(ToolOutput {
                    content: document.into_content(),
                    ok: true,
                }),
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

/// Reads one bounded text resource relative to a registered skill.
pub(super) struct ReadSkillResourceTool {
    pub(super) skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ReadSkillResourceTool {
    tool_metadata!("skill_resource_read");
}

impl ToolHandler for ReadSkillResourceTool {
    type Args = ReadResourceArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let skill_name = args.name.trim();
            let path = args.path.trim();
            let skills = { lock_skill_set(&self.skills).clone() };
            match skills
                .read_resource(&SkillName::new(skill_name), path, args.offset, args.limit)
                .await
            {
                Ok(page) => encode_resource_page(skill_name, page),
                Err(SkillError::NotFound(_)) => Err(ToolError::InvokeRejected(format!(
                    "unknown skill \"{skill_name}\"; call skill_list to see what is available."
                ))
                .into()),
                Err(error) => Ok(ToolOutput {
                    content: format!("Could not read resource for skill \"{skill_name}\": {error}"),
                    ok: false,
                }),
            }
        })
    }
}

#[derive(Serialize)]
struct ResourceReadResponse<'a> {
    name: &'a str,
    path: &'a str,
    content: &'a str,
    offset: u64,
    bytes: usize,
    next_offset: Option<u64>,
}

fn encode_resource_page(
    name: &str,
    page: SkillResourcePage,
) -> Result<ToolOutput, barracuda_agent_tool::ToolInvokeError> {
    let content = serde_json::to_string(&ResourceReadResponse {
        name,
        path: page.path(),
        content: page.content(),
        offset: page.offset(),
        bytes: page.bytes(),
        next_offset: page.next_offset(),
    })
    .map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode skill resource response"))
    })?;
    Ok(ToolOutput { content, ok: true })
}

/// Re-scans the skill registry's roots and swaps in a fresh catalog.
pub(super) struct ReloadSkillsTool {
    pub(super) skills: Arc<RefCell<SkillSet>>,
}

impl ToolSpec for ReloadSkillsTool {
    tool_metadata!("skill_reload");
}

impl ToolHandler for ReloadSkillsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let skills = { lock_skill_set(&self.skills).clone() };
            if let Err(error) = skills.reload().await {
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

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::cell::RefCell;

    use barracuda_agent_skill::FsSkillRegistry;
    use barracuda_agent_tool::ToolHandler;
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;

    use super::{ReadArgs, ReadResourceArgs, ReadSkillResourceTool, ReadSkillTool};

    #[test]
    fn skill_read_hides_the_backend_directory() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = memory_vfs().await?;
            filesystem
                .write_atomic(
                    "skills/example/SKILL.md",
                    b"---\nname: example\ndescription: Use for examples.\n---\nRead references/guide.md.",
                )
                .await?;
            let registry = Arc::new(FsSkillRegistry::new(filesystem).add_root("skills").await?);
            let output = ReadSkillTool {
                skills: Arc::new(RefCell::new(registry.skill_set())),
            }
            .invoke(ReadArgs {
                name: "example".into(),
            })
            .await?;

            assert!(output.ok);
            assert_eq!(output.content, "Read references/guide.md.");
            assert!(!output.content.contains("skills/example"));
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn resource_tool_reads_a_bounded_relative_file() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = memory_vfs().await?;
            filesystem
                .write_atomic(
                    "skills/example/SKILL.md",
                    b"---\nname: example\ndescription: Use for examples.\n---\nbody",
                )
                .await?;
            filesystem
                .write_atomic("skills/example/references/guide.md", b"hello world")
                .await?;
            let registry = Arc::new(FsSkillRegistry::new(filesystem).add_root("skills").await?);
            let tool = ReadSkillResourceTool {
                skills: Arc::new(RefCell::new(registry.skill_set())),
            };
            let output = tool
                .invoke(ReadResourceArgs {
                    name: "example".into(),
                    path: "references/guide.md".into(),
                    offset: 6,
                    limit: 5,
                })
                .await?;

            assert!(output.ok);
            let value: serde_json::Value = serde_json::from_str(&output.content)?;
            assert_eq!(
                value,
                serde_json::json!({
                    "name": "example",
                    "path": "references/guide.md",
                    "content": "world",
                    "offset": 6,
                    "bytes": 5,
                    "next_offset": null
                })
            );
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn resource_tool_rejects_paths_outside_the_skill() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = memory_vfs().await?;
            filesystem
                .write_atomic(
                    "skills/example/SKILL.md",
                    b"---\nname: example\ndescription: Use for examples.\n---\nbody",
                )
                .await?;
            let registry = Arc::new(FsSkillRegistry::new(filesystem).add_root("skills").await?);
            let output = ReadSkillResourceTool {
                skills: Arc::new(RefCell::new(registry.skill_set())),
            }
            .invoke(ReadResourceArgs {
                name: "example".into(),
                path: "../secret.md".into(),
                offset: 0,
                limit: 16,
            })
            .await?;

            assert!(!output.ok);
            assert!(output.content.contains("invalid resource path"));
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn resource_schema_enforces_read_bounds() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/skill_resource_read/schema.json");

        assert!(SCHEMA
            .validate_str(r#"{"name":"example","path":"references/guide.md"}"#)
            .is_ok());
        assert!(SCHEMA
            .validate_str(r#"{"name":"example","path":"references/guide.md","limit":16385}"#)
            .is_err());
    }
}
