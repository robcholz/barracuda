//! Skill tools owned by the skill context provider.

use alloc::{borrow::ToOwned, format, string::String, string::ToString};
use core::cell::RefCell;

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_skill::{
    SkillError, SkillName, SkillResourcePage, SkillSet, DEFAULT_RESOURCE_READ_BYTES,
};
use barracuda_agent_tool::{
    tool_metadata, EmptyArgs, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolOutput,
    ToolSpec,
};
use portable_atomic_util::Arc;
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

    /// Only reads state.
    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
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

    /// Only reads state.
    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
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

    /// Only reads state.
    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
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

    /// Re-reads the skill roots; nothing on disk changes.
    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for ReloadSkillsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let skills = { lock_skill_set(&self.skills).clone() };
            if let Err(error) = skills.reload().await {
                log::warn!("skill catalog reload failed: {error}");
                return Ok(ToolOutput {
                    content: reload_failure_content(&error),
                    ok: false,
                });
            }
            let rejected = skills.catalog().rejected().to_vec();
            for error in &rejected {
                log::warn!("skill package left out of the catalog: {error}");
            }
            Ok(ToolOutput {
                content: reload_success_content(&rejected),
                ok: true,
            })
        })
    }
}

fn reload_success_content(rejected: &[SkillError]) -> String {
    let mut content = String::from("Skills refreshed. Use skill_list to inspect the catalog.");
    if !rejected.is_empty() {
        content.push_str("\nThese skill packages were left out:");
        for error in rejected {
            content.push_str("\n- ");
            content.push_str(&rejection_reason(error));
        }
    }
    content
}

/// Why a package was left out, without the backing directories.
fn rejection_reason(error: &SkillError) -> String {
    match error {
        SkillError::DuplicateSkill { name, .. } => {
            format!("duplicate skill \"{name}\": every copy is left out until one is removed")
        }
        _ => error.to_string(),
    }
}

fn reload_failure_content(error: &SkillError) -> String {
    match error {
        SkillError::DuplicateSkill { name, .. } => format!(
            "Could not refresh skills: duplicate skill \"{name}\". Skill names must be unique."
        ),
        _ => String::from("Could not refresh skills from disk. See system logs for details."),
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use barracuda_agent_skill::SkillSetSource;
    use core::cell::RefCell;

    use barracuda_agent_skill::FsSkillRegistry;
    use barracuda_agent_tool::{EmptyArgs, ToolHandler};
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;
    use portable_atomic_util::Arc;

    use super::{
        ReadArgs, ReadResourceArgs, ReadSkillResourceTool, ReadSkillTool, ReloadSkillsTool,
    };

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
            .validate_str(r#"{"name":"example","path":"references/guide.md","limit":3}"#)
            .is_err());
        assert!(SCHEMA
            .validate_str(r#"{"name":"example","path":"references/guide.md","limit":16385}"#)
            .is_err());
    }

    #[test]
    fn reload_report_hides_backend_directories() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = memory_vfs().await?;
            filesystem
                .write_atomic(
                    "user-skills/example/SKILL.md",
                    b"---\nname: example\ndescription: Use for examples.\n---\nbody",
                )
                .await?;
            filesystem
                .write_atomic(
                    "bundled-skills/other/SKILL.md",
                    b"---\nname: other\ndescription: Use for other examples.\n---\nbody",
                )
                .await?;
            let registry = Arc::new(
                FsSkillRegistry::new(filesystem.clone())
                    .add_root("user-skills")
                    .await?
                    .add_root("bundled-skills")
                    .await?,
            );
            filesystem
                .write_atomic(
                    "bundled-skills/example/SKILL.md",
                    b"---\nname: example\ndescription: Duplicate example.\n---\nbody",
                )
                .await?;

            let output = ReloadSkillsTool {
                skills: Arc::new(RefCell::new(registry.skill_set())),
            }
            .invoke(EmptyArgs {})
            .await?;

            assert!(output.ok);
            assert!(output.content.contains("duplicate skill \"example\""));
            assert!(!output.content.contains("user-skills"));
            assert!(!output.content.contains("bundled-skills"));
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }
}
