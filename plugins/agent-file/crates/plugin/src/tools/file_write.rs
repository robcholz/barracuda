use alloc::{boxed::Box, format, string::String};

use barracuda_agent_plugin::tools::{
    Action, Resource, RiskClass, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolSpec,
};
use barracuda_vfs::ScopedVfs;
use serde::{Deserialize, Serialize};

use super::common::{Failure, MAX_WRITE_BYTES, encode_failure, encode_fs_failure, encode_success};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
struct WriteClassification {
    path: String,
    #[serde(default)]
    overwrite: bool,
}

struct FileWriteTool {
    filesystem: ScopedVfs,
}

pub(super) fn tool(filesystem: ScopedVfs) -> Tool {
    Tool::new(FileWriteTool { filesystem })
}

impl ToolSpec for FileWriteTool {
    barracuda_agent_plugin::tools::tool_metadata!("file_write");

    fn classify(&self, call: &ToolInvocation) -> Action {
        let Ok(args) = call.arguments::<WriteClassification>() else {
            return Action::new("file_write", RiskClass::High);
        };
        Action::new(
            "file_write",
            if args.overwrite {
                RiskClass::High
            } else {
                RiskClass::Moderate
            },
        )
        .with_resource(Resource::Path(args.path))
    }
}

impl ToolHandler for FileWriteTool {
    type Args = WriteArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = args.path.as_str();
            let bytes = args.content.as_bytes();
            if bytes.len() > MAX_WRITE_BYTES {
                return encode_failure(Failure::new(
                    "file_too_large",
                    format!("content exceeds the {MAX_WRITE_BYTES}-byte write limit"),
                ));
            }
            match self.filesystem.exists(path).await {
                Ok(true) if !args.overwrite => {
                    return encode_failure(Failure::new(
                        "already_exists",
                        "destination already exists; set overwrite to true to replace it",
                    ));
                }
                Ok(_) => {}
                Err(error) => return encode_fs_failure(error),
            }
            match self.filesystem.write_atomic(path, bytes).await {
                Ok(()) => encode_success(&WriteResponse {
                    path,
                    bytes: bytes.len(),
                }),
                Err(error) => encode_fs_failure(error),
            }
        })
    }
}

#[derive(Serialize)]
struct WriteResponse<'a> {
    path: &'a str,
    bytes: usize,
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_agent_plugin::tools::{RiskClass, ToolHandler, ToolInvocation, ToolSpec};
    use embassy_futures::block_on;

    use super::{FileWriteTool, WriteArgs};
    use crate::tools::common::{filesystem, json};

    #[test]
    fn creates_by_default_and_requires_opt_in_to_overwrite()
    -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            let tool = FileWriteTool {
                filesystem: filesystem.clone(),
            };

            let created = tool
                .invoke(WriteArgs {
                    path: "data/note.txt".into(),
                    content: "first".into(),
                    overwrite: false,
                })
                .await?;
            assert!(created.ok);

            let exists = tool
                .invoke(WriteArgs {
                    path: "data/note.txt".into(),
                    content: "second".into(),
                    overwrite: false,
                })
                .await?;
            assert!(!exists.ok);
            assert_eq!(json(&exists.content)?["error"], "already_exists");

            let replaced = tool
                .invoke(WriteArgs {
                    path: "data/note.txt".into(),
                    content: "second".into(),
                    overwrite: true,
                })
                .await?;
            assert!(replaced.ok);
            assert_eq!(filesystem.read("data/note.txt").await?, b"second");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn delegates_path_policy_to_the_plugin_scope() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            let output = FileWriteTool { filesystem }
                .invoke(WriteArgs {
                    path: "/../../outside".into(),
                    content: "blocked".into(),
                    overwrite: false,
                })
                .await?;

            assert!(!output.ok);
            assert_eq!(json(&output.content)?["error"], "invalid_path");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn classification_distinguishes_create_and_overwrite() -> Result<(), Box<dyn core::error::Error>>
    {
        block_on(async {
            let tool = FileWriteTool {
                filesystem: filesystem().await?,
            };
            let create = ToolInvocation::try_new(
                None,
                "file_write",
                r#"{"path":"/notes","content":"x","overwrite":false}"#,
            )?;
            let overwrite = ToolInvocation::try_new(
                None,
                "file_write",
                r#"{"path":"/notes","content":"x","overwrite":true}"#,
            )?;

            assert_eq!(tool.classify(&create).risk(), RiskClass::Moderate);
            assert_eq!(tool.classify(&overwrite).risk(), RiskClass::High);
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn schema_accepts_scoped_path_strings() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/file_write/schema.json");

        assert!(
            SCHEMA
                .validate_str(r#"{"path":"/a","content":"x"}"#)
                .is_ok()
        );
        assert!(SCHEMA.validate_str(r#"{"path":"a","content":"x"}"#).is_ok());
    }
}
