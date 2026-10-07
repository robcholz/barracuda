use alloc::{boxed::Box, string::String};

use barracuda_agent_plugin::tools::{
    Action, RiskClass, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolSpec,
};
use barracuda_vfs::ScopedVfs;
use serde::{Deserialize, Serialize};

use super::common::{encode_fs_failure, encode_success, file_kind, path_action};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    path: String,
}

struct FileDeleteTool {
    filesystem: ScopedVfs,
}

pub(super) fn tool(filesystem: ScopedVfs) -> Tool {
    Tool::new(FileDeleteTool { filesystem })
}

impl ToolSpec for FileDeleteTool {
    barracuda_agent_plugin::tools::tool_metadata!("file_delete");

    fn classify(&self, call: &ToolInvocation) -> Action {
        path_action(call, "file_delete", RiskClass::High)
    }
}

impl ToolHandler for FileDeleteTool {
    type Args = DeleteArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = args.path.as_str();
            let metadata = match self.filesystem.metadata(path).await {
                Ok(metadata) => metadata,
                Err(error) => return encode_fs_failure(error),
            };
            let kind = file_kind(metadata.file_type());
            let result = if metadata.is_dir() {
                self.filesystem.remove_dir(path).await
            } else {
                self.filesystem.remove_file(path).await
            };
            match result {
                Ok(()) => encode_success(&DeleteResponse { path, kind }),
                Err(error) => encode_fs_failure(error),
            }
        })
    }
}

#[derive(Serialize)]
struct DeleteResponse<'a> {
    path: &'a str,
    kind: &'static str,
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_agent_plugin::tools::ToolHandler;
    use embassy_futures::block_on;

    use super::{DeleteArgs, FileDeleteTool};
    use crate::tools::common::{filesystem, json};

    #[test]
    fn removes_files_and_empty_directories_and_delegates_root_policy()
    -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/tmp/file", b"x").await?;
            filesystem.create_dir_all("/empty").await?;
            let tool = FileDeleteTool {
                filesystem: filesystem.clone(),
            };

            assert!(
                tool.invoke(DeleteArgs {
                    path: "/tmp/file".into()
                })
                .await?
                .ok
            );
            assert!(
                tool.invoke(DeleteArgs {
                    path: "/empty".into()
                })
                .await?
                .ok
            );
            let root = tool.invoke(DeleteArgs { path: "/".into() }).await?;
            assert!(!root.ok);
            assert_eq!(json(&root.content)?["error"], "permission_denied");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn schema_delegates_root_policy() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/file_delete/schema.json");

        assert!(SCHEMA.validate_str(r#"{"path":"/a"}"#).is_ok());
        assert!(SCHEMA.validate_str(r#"{"path":"/"}"#).is_ok());
    }
}
