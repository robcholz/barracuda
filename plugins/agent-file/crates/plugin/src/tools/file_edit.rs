use alloc::{boxed::Box, format, string::String};

use barracuda_agent_plugin::tools::{
    Action, RiskClass, Tool, ToolFuture, ToolHandler, ToolInvocation, ToolSpec,
};
use barracuda_vfs::ScopedVfs;
use serde::{Deserialize, Serialize};

use super::common::{
    Failure, MAX_WRITE_BYTES, encode_failure, encode_fs_failure, encode_success, path_action,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

struct FileEditTool {
    filesystem: ScopedVfs,
}

pub(super) fn tool(filesystem: ScopedVfs) -> Tool {
    Tool::new(FileEditTool { filesystem })
}

impl ToolSpec for FileEditTool {
    barracuda_agent_plugin::tools::tool_metadata!("file_edit");

    fn classify(&self, call: &ToolInvocation) -> Action {
        path_action(call, "file_edit", RiskClass::Moderate)
    }
}

impl ToolHandler for FileEditTool {
    type Args = EditArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = args.path.as_str();
            if args.old_text.is_empty() {
                return encode_failure(Failure::new("invalid_input", "old_text cannot be empty"));
            }
            if args.old_text.len() > MAX_WRITE_BYTES || args.new_text.len() > MAX_WRITE_BYTES {
                return encode_failure(Failure::new(
                    "file_too_large",
                    format!("edit text exceeds the {MAX_WRITE_BYTES}-byte limit"),
                ));
            }
            let metadata = match self.filesystem.metadata(path).await {
                Ok(metadata) => metadata,
                Err(error) => return encode_fs_failure(error),
            };
            if !metadata.is_file() {
                return encode_failure(Failure::new("is_directory", "path is not a regular file"));
            }
            if metadata.len() > u64::try_from(MAX_WRITE_BYTES).unwrap_or(u64::MAX) {
                return encode_failure(Failure::new(
                    "file_too_large",
                    format!("file exceeds the {MAX_WRITE_BYTES}-byte edit limit"),
                ));
            }
            let bytes = match self.filesystem.read(path).await {
                Ok(bytes) => bytes,
                Err(error) => return encode_fs_failure(error),
            };
            if bytes.len() > MAX_WRITE_BYTES {
                return encode_failure(Failure::new(
                    "file_too_large",
                    format!("file grew beyond the {MAX_WRITE_BYTES}-byte edit limit"),
                ));
            }
            let content = match String::from_utf8(bytes) {
                Ok(content) => content,
                Err(_error) => {
                    return encode_failure(Failure::new("invalid_utf8", "file is not valid UTF-8"));
                }
            };
            let matches = content.match_indices(&args.old_text).count();
            if matches == 0 {
                return encode_failure(Failure::new(
                    "no_match",
                    "old_text does not occur in the file",
                ));
            }
            if matches > 1 && !args.replace_all {
                return encode_failure(Failure::new(
                    "ambiguous_match",
                    format!(
                        "old_text occurs {matches} times; provide more context or set replace_all"
                    ),
                ));
            }
            let replacement_count = if args.replace_all { matches } else { 1 };
            let updated = if args.replace_all {
                content.replace(&args.old_text, &args.new_text)
            } else {
                content.replacen(&args.old_text, &args.new_text, 1)
            };
            if updated.len() > MAX_WRITE_BYTES {
                return encode_failure(Failure::new(
                    "file_too_large",
                    format!("edited file exceeds the {MAX_WRITE_BYTES}-byte limit"),
                ));
            }
            match self.filesystem.write_atomic(path, updated.as_bytes()).await {
                Ok(()) => encode_success(&EditResponse {
                    path,
                    replacements: replacement_count,
                    bytes: updated.len(),
                }),
                Err(error) => encode_fs_failure(error),
            }
        })
    }
}

#[derive(Serialize)]
struct EditResponse<'a> {
    path: &'a str,
    replacements: usize,
    bytes: usize,
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_agent_plugin::tools::ToolHandler;
    use embassy_futures::block_on;

    use super::{EditArgs, FileEditTool};
    use crate::tools::common::{filesystem, json};

    #[test]
    fn requires_an_exact_unique_match_unless_replace_all_is_requested()
    -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/code.rs", b"old + old").await?;
            let tool = FileEditTool {
                filesystem: filesystem.clone(),
            };

            let ambiguous = tool
                .invoke(EditArgs {
                    path: "/code.rs".into(),
                    old_text: "old".into(),
                    new_text: "new".into(),
                    replace_all: false,
                })
                .await?;
            assert!(!ambiguous.ok);
            assert_eq!(json(&ambiguous.content)?["error"], "ambiguous_match");
            assert_eq!(filesystem.read("/code.rs").await?, b"old + old");

            let replaced = tool
                .invoke(EditArgs {
                    path: "/code.rs".into(),
                    old_text: "old".into(),
                    new_text: "new".into(),
                    replace_all: true,
                })
                .await?;
            assert!(replaced.ok);
            assert_eq!(json(&replaced.content)?["replacements"], 2);
            assert_eq!(filesystem.read("/code.rs").await?, b"new + new");

            let missing = tool
                .invoke(EditArgs {
                    path: "/code.rs".into(),
                    old_text: "old".into(),
                    new_text: "new".into(),
                    replace_all: false,
                })
                .await?;
            assert!(!missing.ok);
            assert_eq!(json(&missing.content)?["error"], "no_match");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn applies_a_single_incremental_edit() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem.write("/note", b"abcdef").await?;
            let edited = FileEditTool {
                filesystem: filesystem.clone(),
            }
            .invoke(EditArgs {
                path: "/note".into(),
                old_text: "abc".into(),
                new_text: "xyz".into(),
                replace_all: false,
            })
            .await?;

            assert!(edited.ok);
            assert_eq!(filesystem.read("/note").await?, b"xyzdef");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }

    #[test]
    fn schema_accepts_exact_text_edits() {
        const SCHEMA: json_validator::Validator =
            json_validator::validator!("resources/tools/file_edit/schema.json");

        assert!(
            SCHEMA
                .validate_str(r#"{"path":"/a","old_text":"x","new_text":"y"}"#)
                .is_ok()
        );
    }
}
