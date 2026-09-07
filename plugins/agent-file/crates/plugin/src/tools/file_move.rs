use alloc::{boxed::Box, format, string::String};

use barracuda_agent_plugin::tools::{
    Action, Resource, RiskClass, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolSpec,
};
use barracuda_vfs::ScopedVfs;
use serde::{Deserialize, Serialize};

use super::common::{Failure, encode_failure, encode_fs_failure, encode_success};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct MoveClassification {
    from: String,
    to: String,
}

struct FileMoveTool {
    filesystem: ScopedVfs,
}

pub(super) fn tool(filesystem: ScopedVfs) -> Tool {
    Tool::new(FileMoveTool { filesystem })
}

impl ToolSpec for FileMoveTool {
    barracuda_agent_plugin::tools::tool_metadata!("file_move");

    fn classify(&self, call: &ToolInvocation) -> Action {
        let action = Action::new("file_move", RiskClass::High);
        match call.arguments::<MoveClassification>() {
            Ok(args) => {
                action.with_resource(Resource::Path(format!("{} -> {}", args.from, args.to)))
            }
            Err(_error) => action,
        }
    }
}

impl ToolHandler for FileMoveTool {
    type Args = MoveArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let from = args.from.as_str();
            let to = args.to.as_str();
            if from == to {
                return encode_failure(Failure::new(
                    "invalid_input",
                    "source and destination must differ",
                ));
            }
            match self.filesystem.metadata(from).await {
                Ok(_metadata) => {}
                Err(error) => return encode_fs_failure(error),
            }
            match self.filesystem.exists(to).await {
                Ok(true) => {
                    return encode_failure(Failure::new(
                        "already_exists",
                        "destination already exists",
                    ));
                }
                Ok(false) => {}
                Err(error) => return encode_fs_failure(error),
            }
            match self.filesystem.rename(from, to).await {
                Ok(()) => encode_success(&MoveResponse { from, to }),
                Err(error) => encode_fs_failure(error),
            }
        })
    }
}

#[derive(Serialize)]
struct MoveResponse<'a> {
    from: &'a str,
    to: &'a str,
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_agent_plugin::tools::ToolHandler;
    use embassy_futures::block_on;

    use super::{FileMoveTool, MoveArgs};
    use crate::tools::common::{filesystem, json};

    #[test]
    fn refuses_to_replace_a_destination() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/from", b"source").await?;
            filesystem.write("/to", b"target").await?;
            let tool = FileMoveTool {
                filesystem: filesystem.clone(),
            };

            let refused = tool
                .invoke(MoveArgs {
                    from: "/from".into(),
                    to: "/to".into(),
                })
                .await?;
            assert!(!refused.ok);
            assert_eq!(json(&refused.content)?["error"], "already_exists");
            assert_eq!(filesystem.read("/from").await?, b"source");
            assert_eq!(filesystem.read("/to").await?, b"target");

            let moved = tool
                .invoke(MoveArgs {
                    from: "/from".into(),
                    to: "/moved".into(),
                })
                .await?;
            assert!(moved.ok);
            assert!(!filesystem.exists("/from").await?);
            assert_eq!(filesystem.read("/moved").await?, b"source");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn schema_accepts_source_and_destination() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/file_move/schema.json");

        assert!(SCHEMA.validate_str(r#"{"from":"/a","to":"/b"}"#).is_ok());
    }
}
