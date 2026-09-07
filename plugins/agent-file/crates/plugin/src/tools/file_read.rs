use alloc::{boxed::Box, format, string::String, vec::Vec};

use barracuda_agent_plugin::tools::{
    Action, RiskClass, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolSpec,
};
use barracuda_vfs::ScopedVfs;
use serde::{Deserialize, Serialize};

use super::common::{Failure, encode_failure, encode_fs_failure, encode_success, path_action};

const DEFAULT_READ_BYTES: usize = 4 * 1024;
const MAX_READ_BYTES: usize = 16 * 1024;

fn default_read_bytes() -> usize {
    DEFAULT_READ_BYTES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_read_bytes")]
    limit: usize,
}

struct FileReadTool {
    filesystem: ScopedVfs,
}

pub(super) fn tool(filesystem: ScopedVfs) -> Tool {
    Tool::new(FileReadTool { filesystem })
}

impl ToolSpec for FileReadTool {
    barracuda_agent_plugin::tools::tool_metadata!("file_read");

    fn concurrent(&self) -> bool {
        true
    }

    fn classify(&self, call: &ToolInvocation) -> Action {
        path_action(call, "file_read", RiskClass::Safe)
    }
}

impl ToolHandler for FileReadTool {
    type Args = ReadArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = args.path.as_str();
            if args.limit == 0 || args.limit > MAX_READ_BYTES {
                return encode_failure(Failure::new(
                    "invalid_input",
                    format!("limit must be between 1 and {MAX_READ_BYTES}"),
                ));
            }
            let metadata = match self.filesystem.metadata(path).await {
                Ok(metadata) => metadata,
                Err(error) => return encode_fs_failure(error),
            };
            if !metadata.is_file() {
                return encode_failure(Failure::new("is_directory", "path is not a regular file"));
            }
            let length = metadata.len();
            if args.offset > length {
                return encode_failure(Failure::new(
                    "range_out_of_bounds",
                    format!("offset {} exceeds file length {length}", args.offset),
                ));
            }
            let amount_u64 = length
                .saturating_sub(args.offset)
                .min(u64::try_from(args.limit).unwrap_or(u64::MAX));
            let amount = match usize::try_from(amount_u64) {
                Ok(amount) => amount,
                Err(_error) => {
                    return encode_failure(Failure::new(
                        "filesystem",
                        "requested range cannot be represented on this target",
                    ));
                }
            };
            let bytes = if amount == 0 {
                Vec::new()
            } else {
                match self.filesystem.read_at(path, args.offset, amount).await {
                    Ok(bytes) => bytes,
                    Err(error) => return encode_fs_failure(error),
                }
            };
            let content = match String::from_utf8(bytes) {
                Ok(content) => content,
                Err(_error) => {
                    return encode_failure(Failure::new(
                        "invalid_utf8",
                        "selected byte range is not valid UTF-8",
                    ));
                }
            };
            let next = args.offset.saturating_add(amount_u64);
            encode_success(&ReadResponse {
                path,
                content,
                offset: args.offset,
                bytes: amount,
                next_offset: (next < length).then_some(next),
            })
        })
    }
}

#[derive(Serialize)]
struct ReadResponse<'a> {
    path: &'a str,
    content: String,
    offset: u64,
    bytes: usize,
    next_offset: Option<u64>,
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_agent_plugin::tools::{
        Resource, RiskClass, ToolHandler, ToolInvocation, ToolSpec,
    };
    use embassy_futures::block_on;

    use super::{FileReadTool, ReadArgs};
    use crate::tools::common::{filesystem, json};

    #[test]
    fn returns_a_bounded_byte_range() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/notes.txt", b"hello world").await?;
            let output = FileReadTool {
                filesystem: filesystem.clone(),
            }
            .invoke(ReadArgs {
                path: "/notes.txt".into(),
                offset: 6,
                limit: 5,
            })
            .await?;

            assert!(output.ok);
            assert_eq!(
                json(&output.content)?,
                serde_json::json!({
                    "path": "/notes.txt",
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
    fn reports_invalid_utf8_and_out_of_bounds_ranges() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/binary", &[0xff]).await?;
            let tool = FileReadTool {
                filesystem: filesystem.clone(),
            };

            let invalid_utf8 = tool
                .invoke(ReadArgs {
                    path: "/binary".into(),
                    offset: 0,
                    limit: 1,
                })
                .await?;
            assert!(!invalid_utf8.ok);
            assert_eq!(json(&invalid_utf8.content)?["error"], "invalid_utf8");

            let out_of_bounds = tool
                .invoke(ReadArgs {
                    path: "/binary".into(),
                    offset: 2,
                    limit: 1,
                })
                .await?;
            assert!(!out_of_bounds.ok);
            assert_eq!(
                json(&out_of_bounds.content)?["error"],
                "range_out_of_bounds"
            );
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn supports_continuation_empty_ranges_defaults_and_safe_classification()
    -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/note", b"abcdef").await?;
            let tool = FileReadTool {
                filesystem: filesystem.clone(),
            };

            let page = tool
                .invoke(ReadArgs {
                    path: "/note".into(),
                    offset: 0,
                    limit: 3,
                })
                .await?;
            assert_eq!(json(&page.content)?["next_offset"], 3);

            let empty = tool
                .invoke(ReadArgs {
                    path: "/note".into(),
                    offset: 6,
                    limit: 1,
                })
                .await?;
            assert_eq!(json(&empty.content)?["content"], "");

            let defaults: ReadArgs = serde_json::from_str(r#"{"path":"/note"}"#)?;
            assert_eq!(defaults.offset, 0);
            assert_eq!(defaults.limit, 4096);
            assert!(tool.concurrent());

            let invocation = ToolInvocation::try_new(
                None,
                "file_read",
                r#"{"path":"/note","offset":0,"limit":1}"#,
            )?;
            let action = tool.classify(&invocation);
            assert_eq!(action.risk(), RiskClass::Safe);
            assert_eq!(action.resource(), Some(&Resource::Path("/note".into())));
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn schema_enforces_read_bounds() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/file_read/schema.json");

        assert!(SCHEMA.validate_str(r#"{"path":"/a"}"#).is_ok());
        assert!(
            SCHEMA
                .validate_str(r#"{"path":"/a","limit":16385}"#)
                .is_err()
        );
    }
}
