use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};

use barracuda_agent_plugin::tools::{
    Action, RiskClass, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolSpec,
};
use barracuda_vfs::ScopedVfs;
use serde::{Deserialize, Serialize};

use super::common::{
    Failure, encode_failure, encode_fs_failure, encode_success, file_kind, path_action,
};

const DEFAULT_LIST_ENTRIES: usize = 64;
const MAX_LIST_ENTRIES: usize = 256;

fn default_list_entries() -> usize {
    DEFAULT_LIST_ENTRIES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_list_entries")]
    limit: usize,
}

struct FileListTool {
    filesystem: ScopedVfs,
}

pub(super) fn tool(filesystem: ScopedVfs) -> Tool {
    Tool::new(FileListTool { filesystem })
}

impl ToolSpec for FileListTool {
    barracuda_agent_plugin::tools::tool_metadata!("file_list");

    fn concurrent(&self) -> bool {
        true
    }

    fn classify(&self, call: &ToolInvocation) -> Action {
        path_action(call, "file_list", RiskClass::Safe)
    }
}

impl ToolHandler for FileListTool {
    type Args = ListArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = args.path.as_str();
            if args.limit == 0 || args.limit > MAX_LIST_ENTRIES {
                return encode_failure(Failure::new(
                    "invalid_input",
                    format!("limit must be between 1 and {MAX_LIST_ENTRIES}"),
                ));
            }
            let directory = match self.filesystem.read_dir(path).await {
                Ok(directory) => directory,
                Err(error) => return encode_fs_failure(error),
            };
            let mut entries = Vec::new();
            for entry in directory {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => return encode_fs_failure(error),
                };
                entries.push(EntryResponse {
                    name: entry.file_name().to_string(),
                    kind: file_kind(entry.metadata().file_type()),
                    bytes: entry.metadata().len(),
                });
            }
            entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
            let start = args.offset.min(entries.len());
            let end = start.saturating_add(args.limit).min(entries.len());
            let next_offset = (end < entries.len()).then_some(end);
            let entries = entries.drain(start..end).collect();
            encode_success(&ListResponse {
                path,
                entries,
                next_offset,
            })
        })
    }
}

#[derive(Serialize)]
struct EntryResponse {
    name: String,
    kind: &'static str,
    bytes: u64,
}

#[derive(Serialize)]
struct ListResponse<'a> {
    path: &'a str,
    entries: Vec<EntryResponse>,
    next_offset: Option<usize>,
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_agent_plugin::tools::{ToolHandler, ToolSpec};
    use embassy_futures::block_on;

    use super::{FileListTool, ListArgs};
    use crate::tools::common::{filesystem, json};

    #[test]
    fn is_sorted_paginated_concurrent_and_has_defaults() -> Result<(), Box<dyn core::error::Error>>
    {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/src/z.rs", b"z").await?;
            filesystem.write("/src/a.rs", b"abc").await?;
            filesystem.create_dir_all("/src/nested").await?;
            let tool = FileListTool {
                filesystem: filesystem.clone(),
            };

            let output = tool
                .invoke(ListArgs {
                    path: "/src".into(),
                    offset: 1,
                    limit: 2,
                })
                .await?;

            assert!(output.ok);
            assert_eq!(
                json(&output.content)?,
                serde_json::json!({
                    "path": "/src",
                    "entries": [
                        {"name": "nested", "kind": "directory", "bytes": 0},
                        {"name": "z.rs", "kind": "file", "bytes": 1}
                    ],
                    "next_offset": null
                })
            );
            let defaults: ListArgs = serde_json::from_str(r#"{"path":"/"}"#)?;
            assert_eq!(defaults.offset, 0);
            assert_eq!(defaults.limit, 64);
            assert!(tool.concurrent());
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn schema_rejects_unknown_fields() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/file_list/schema.json");

        assert!(SCHEMA.validate_str(r#"{"path":"/"}"#).is_ok());
        assert!(SCHEMA.validate_str(r#"{"path":"/","extra":true}"#).is_err());
    }
}
