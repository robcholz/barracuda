//! Model-callable long-term memory tools over the dual-tier store.

mod args;

use alloc::vec::Vec;
use claw_api::ToolCall;
use claw_interface::ClawFs;
use claw_memory::{MemoryDraft, MemoryId, MemoryItem, MemoryPatch, StoreOutcome};
use claw_tool::{
    tool_metadata, tool_schemas, tool_validator, Tool, ToolError, ToolFuture, ToolGroup,
    ToolHandler, ToolInvocation, ToolInvokeError, ToolOutput, ToolSpec,
};

use self::args::{
    limit_or_default, optional_trimmed, optional_trimmed_strings, trimmed, trimmed_strings, IdArgs,
    ListArgs, RecallArgs, StoreArgs, UpdateArgs,
};
use super::extraction::{ExtractedItem, MemoryOp};
use super::MemoryStores;

const STORE_VALIDATOR: json_validator::Validator = tool_validator!("memory_store");
const UPDATE_VALIDATOR: json_validator::Validator = tool_validator!("memory_update");
const FORGET_VALIDATOR: json_validator::Validator = tool_validator!("memory_forget");

pub(super) const EXTRACTION_TOOLS_JSON: &str =
    tool_schemas!("memory_store", "memory_update", "memory_forget");

pub(super) fn decode_extraction_operations(
    calls: Vec<ToolCall>,
) -> Result<Vec<MemoryOp>, ToolInvokeError> {
    calls.into_iter().map(decode_extraction_operation).collect()
}

fn decode_extraction_operation(call: ToolCall) -> Result<MemoryOp, ToolInvokeError> {
    let invocation = ToolInvocation::try_new(Some(&call.id), &call.name, &call.arguments_json)?;
    match call.name.as_str() {
        "memory_store" => {
            validate_extraction_arguments(&invocation, &STORE_VALIDATOR)?;
            let args = invocation.arguments::<StoreArgs>()?;
            Ok(MemoryOp::Add(ExtractedItem {
                content: trimmed(args.content),
                tags: trimmed_strings(args.tags),
                keywords: trimmed_strings(args.keywords),
            }))
        }
        "memory_update" => {
            validate_extraction_arguments(&invocation, &UPDATE_VALIDATOR)?;
            let args = invocation.arguments::<UpdateArgs>()?;
            Ok(MemoryOp::Update {
                id: MemoryId::from(trimmed(args.id).as_str()),
                patch: MemoryPatch {
                    content: optional_trimmed(args.content),
                    tags: optional_trimmed_strings(args.tags),
                    keywords: optional_trimmed_strings(args.keywords),
                },
            })
        }
        "memory_forget" => {
            validate_extraction_arguments(&invocation, &FORGET_VALIDATOR)?;
            let args = invocation.arguments::<IdArgs>()?;
            Ok(MemoryOp::Forget {
                id: MemoryId::from(trimmed(args.id).as_str()),
            })
        }
        name => Err(ToolError::InvalidArguments(format!(
            "unexpected memory extraction tool '{name}'"
        ))
        .into()),
    }
}

fn validate_extraction_arguments(
    invocation: &ToolInvocation,
    validator: &json_validator::Validator,
) -> Result<(), ToolInvokeError> {
    validator
        .validate(invocation.arguments_value())
        .map_err(ToolError::from)?;
    Ok(())
}

pub(crate) fn memory_tools<F: ClawFs + 'static>(stores: MemoryStores<F>) -> ToolGroup {
    ToolGroup::new(
        "memory",
        true,
        [
            Tool::new(MemoryStoreTool {
                stores: stores.clone(),
            }),
            Tool::new(MemoryRecallTool {
                stores: stores.clone(),
            }),
            Tool::new(MemoryListTool {
                stores: stores.clone(),
            }),
            Tool::new(MemoryUpdateTool {
                stores: stores.clone(),
            }),
            Tool::new(MemoryForgetTool { stores }),
        ],
    )
}

struct MemoryStoreTool<F: ClawFs + 'static> {
    stores: MemoryStores<F>,
}

impl<F: ClawFs + 'static> ToolSpec for MemoryStoreTool<F> {
    tool_metadata!("memory_store");
}

impl<F: ClawFs + 'static> ToolHandler for MemoryStoreTool<F> {
    type Args = StoreArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let content = trimmed(args.content);
            let draft = MemoryDraft::new(content)
                .with_tags(trimmed_strings(args.tags))
                .with_keywords(trimmed_strings(args.keywords))
                .with_source("manual");

            let output = match self.stores.store(draft) {
                StoreOutcome::Created(item) => format!("Stored memory {}.", item.id),
                StoreOutcome::Duplicate(item) => {
                    format!("Already remembered (as {}); nothing changed.", item.id)
                }
            };
            Ok(ToolOutput {
                content: output,
                ok: true,
            })
        })
    }
}

struct MemoryRecallTool<F: ClawFs + 'static> {
    stores: MemoryStores<F>,
}

impl<F: ClawFs + 'static> ToolSpec for MemoryRecallTool<F> {
    tool_metadata!("memory_recall");
}

impl<F: ClawFs + 'static> ToolHandler for MemoryRecallTool<F> {
    type Args = RecallArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let labels = trimmed_strings(args.labels);
            let query = optional_trimmed(args.query);
            let limit = limit_or_default(args.limit);

            let items = self.stores.recall(&labels, query.as_deref(), limit);
            Ok(ToolOutput {
                content: render_items("Recalled memories", &items),
                ok: true,
            })
        })
    }
}

struct MemoryListTool<F: ClawFs + 'static> {
    stores: MemoryStores<F>,
}

impl<F: ClawFs + 'static> ToolSpec for MemoryListTool<F> {
    tool_metadata!("memory_list");
}

impl<F: ClawFs + 'static> ToolHandler for MemoryListTool<F> {
    type Args = ListArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let limit = limit_or_default(args.limit);
            let mut items = self.stores.list();
            items.truncate(limit);
            Ok(ToolOutput {
                content: render_items("All memories", &items),
                ok: true,
            })
        })
    }
}

struct MemoryUpdateTool<F: ClawFs + 'static> {
    stores: MemoryStores<F>,
}

impl<F: ClawFs + 'static> ToolSpec for MemoryUpdateTool<F> {
    tool_metadata!("memory_update");
}

impl<F: ClawFs + 'static> ToolHandler for MemoryUpdateTool<F> {
    type Args = UpdateArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let id = MemoryId::from(trimmed(args.id).as_str());
            let patch = MemoryPatch {
                content: optional_trimmed(args.content),
                tags: optional_trimmed_strings(args.tags),
                keywords: optional_trimmed_strings(args.keywords),
            };
            match self.stores.update(&id, patch) {
                Ok(item) => Ok(ToolOutput {
                    content: format!("Updated memory {}.", item.id),
                    ok: true,
                }),
                Err(error) => Ok(ToolOutput {
                    content: format!("Could not update {id}: {error}."),
                    ok: false,
                }),
            }
        })
    }
}

struct MemoryForgetTool<F: ClawFs + 'static> {
    stores: MemoryStores<F>,
}

impl<F: ClawFs + 'static> ToolSpec for MemoryForgetTool<F> {
    tool_metadata!("memory_forget");
}

impl<F: ClawFs + 'static> ToolHandler for MemoryForgetTool<F> {
    type Args = IdArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let id = MemoryId::from(trimmed(args.id).as_str());
            match self.stores.forget(&id) {
                Ok(()) => Ok(ToolOutput {
                    content: format!("Forgot memory {id}."),
                    ok: true,
                }),
                Err(error) => Ok(ToolOutput {
                    content: format!("Could not forget {id}: {error}."),
                    ok: false,
                }),
            }
        })
    }
}

fn render_items(header: &str, items: &[MemoryItem]) -> String {
    if items.is_empty() {
        return "No matching memories.".to_string();
    }
    let mut out = format!("{header}:\n");
    for item in items {
        out.push_str("- [");
        out.push_str(item.id.as_str());
        out.push(']');
        if !item.tags.is_empty() {
            out.push_str(" (");
            out.push_str(&item.tags.join(", "));
            out.push(')');
        }
        out.push(' ');
        out.push_str(&item.content);
        out.push('\n');
    }
    out
}
use alloc::string::{String, ToString};
