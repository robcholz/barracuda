//! Model-callable long-term memory tools over the dual-tier store.

mod args;

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use barracuda_agent_memory::{MemoryDraft, MemoryId, MemoryItem, MemoryPatch, StoreOutcome};
use barracuda_agent_tool::{
    tool_metadata, Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvocation,
    ToolInvokeError, ToolOutput, ToolRunner, ToolSet, ToolSpec,
};
use barracuda_model_api::ToolCall;
use core::cell::RefCell;
use futures_util::StreamExt as _;

use self::args::{
    limit_or_default, optional_trimmed, optional_trimmed_strings, trimmed, trimmed_strings, IdArgs,
    ListArgs, RecallArgs, StoreArgs, UpdateArgs,
};
use super::extraction::{ExtractedItem, MemoryOp};
use super::MemoryStores;

pub(super) struct ExtractionTools {
    tools: ToolSet,
    operations: Rc<RefCell<Vec<MemoryOp>>>,
}

impl ExtractionTools {
    pub(super) fn new() -> Result<Self, ToolInvokeError> {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let mut tools = ToolSet::empty();
        tools
            .add_group(ToolGroup::new(
                "memory_extraction",
                true,
                [
                    Tool::new(MemoryStoreTool {
                        target: ExtractionTarget {
                            operations: Rc::clone(&operations),
                        },
                    }),
                    Tool::new(MemoryUpdateTool {
                        target: ExtractionTarget {
                            operations: Rc::clone(&operations),
                        },
                    }),
                    Tool::new(MemoryForgetTool {
                        target: ExtractionTarget {
                            operations: Rc::clone(&operations),
                        },
                    }),
                ],
            ))
            .map_err(extraction_runtime_error)?;
        Ok(Self { tools, operations })
    }

    pub(super) fn schemas(&mut self) -> Result<String, ToolInvokeError> {
        self.tools
            .begin()
            .map(|tools| tools.schemas().to_owned())
            .map_err(extraction_runtime_error)
    }

    pub(super) async fn run(
        &mut self,
        calls: Vec<ToolCall>,
    ) -> Result<Vec<MemoryOp>, ToolInvokeError> {
        self.operations.borrow_mut().clear();
        let invocations = calls
            .into_iter()
            .map(|call| ToolInvocation::try_new(Some(&call.id), &call.name, &call.arguments_json))
            .collect::<Result<Vec<_>, _>>()?;

        {
            let tools = self.tools.begin().map_err(extraction_runtime_error)?;
            let runner = ToolRunner::new(&tools);
            for invocation in invocations {
                // Without a background pool, background tools fail inline.
                let mut joined = runner.run(vec![invocation]);
                let Some((_, output)) = joined.next().await else {
                    return Err(ToolError::InvokeRejected(
                        "memory extraction tool returned no result".into(),
                    )
                    .into());
                };
                if !output.ok {
                    return Err(ToolError::InvokeRejected(output.content).into());
                }
            }
        }

        Ok(core::mem::take(&mut *self.operations.borrow_mut()))
    }
}

fn extraction_runtime_error(error: impl core::fmt::Display) -> ToolInvokeError {
    ToolError::InvokeRejected(error.to_string()).into()
}

trait MemoryMutationTarget {
    fn store<'a>(&'a self, args: StoreArgs) -> ToolFuture<'a>;

    fn update<'a>(&'a self, args: UpdateArgs) -> ToolFuture<'a>;

    fn forget<'a>(&'a self, args: IdArgs) -> ToolFuture<'a>;
}

struct ExtractionTarget {
    operations: Rc<RefCell<Vec<MemoryOp>>>,
}

impl MemoryMutationTarget for ExtractionTarget {
    fn store<'a>(&'a self, args: StoreArgs) -> ToolFuture<'a> {
        Box::pin(async move {
            self.operations
                .borrow_mut()
                .push(MemoryOp::Add(ExtractedItem {
                    content: trimmed(args.content),
                    tags: trimmed_strings(args.tags),
                    keywords: trimmed_strings(args.keywords),
                }));
            Ok(extraction_accepted())
        })
    }

    fn update<'a>(&'a self, args: UpdateArgs) -> ToolFuture<'a> {
        Box::pin(async move {
            self.operations.borrow_mut().push(MemoryOp::Update {
                id: MemoryId::from(trimmed(args.id).as_str()),
                patch: MemoryPatch {
                    content: optional_trimmed(args.content),
                    tags: optional_trimmed_strings(args.tags),
                    keywords: optional_trimmed_strings(args.keywords),
                },
            });
            Ok(extraction_accepted())
        })
    }

    fn forget<'a>(&'a self, args: IdArgs) -> ToolFuture<'a> {
        Box::pin(async move {
            self.operations.borrow_mut().push(MemoryOp::Forget {
                id: MemoryId::from(trimmed(args.id).as_str()),
            });
            Ok(extraction_accepted())
        })
    }
}

fn extraction_accepted() -> ToolOutput {
    ToolOutput {
        content: String::new(),
        ok: true,
    }
}

pub(super) struct StoreTarget {
    pub(super) stores: MemoryStores,
}

impl MemoryMutationTarget for StoreTarget {
    fn store<'a>(&'a self, args: StoreArgs) -> ToolFuture<'a> {
        Box::pin(async move {
            let draft = MemoryDraft::new(trimmed(args.content))
                .with_tags(trimmed_strings(args.tags))
                .with_keywords(trimmed_strings(args.keywords))
                .with_source("manual");
            let content = match self.stores.store(draft).await {
                StoreOutcome::Created(item) => format!("Stored memory {}.", item.id),
                StoreOutcome::Duplicate(item) => {
                    format!("Already remembered (as {}); nothing changed.", item.id)
                }
            };
            Ok(ToolOutput { content, ok: true })
        })
    }

    fn update<'a>(&'a self, args: UpdateArgs) -> ToolFuture<'a> {
        Box::pin(async move {
            let id = MemoryId::from(trimmed(args.id).as_str());
            let patch = MemoryPatch {
                content: optional_trimmed(args.content),
                tags: optional_trimmed_strings(args.tags),
                keywords: optional_trimmed_strings(args.keywords),
            };
            Ok(match self.stores.update(&id, patch).await {
                Ok(item) => ToolOutput {
                    content: format!("Updated memory {}.", item.id),
                    ok: true,
                },
                Err(error) => ToolOutput {
                    content: format!("Could not update {id}: {error}."),
                    ok: false,
                },
            })
        })
    }

    fn forget<'a>(&'a self, args: IdArgs) -> ToolFuture<'a> {
        Box::pin(async move {
            let id = MemoryId::from(trimmed(args.id).as_str());
            Ok(match self.stores.forget(&id).await {
                Ok(()) => ToolOutput {
                    content: format!("Forgot memory {id}."),
                    ok: true,
                },
                Err(error) => ToolOutput {
                    content: format!("Could not forget {id}: {error}."),
                    ok: false,
                },
            })
        })
    }
}

pub(super) struct MemoryStoreTool<T> {
    pub(super) target: T,
}

impl<T: MemoryMutationTarget> ToolSpec for MemoryStoreTool<T> {
    tool_metadata!("memory_store");
}

impl<T: MemoryMutationTarget + 'static> ToolHandler for MemoryStoreTool<T> {
    type Args = StoreArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        self.target.store(args)
    }
}

pub(super) struct MemoryRecallTool {
    pub(super) stores: MemoryStores,
}

impl ToolSpec for MemoryRecallTool {
    tool_metadata!("memory_recall");
}

impl ToolHandler for MemoryRecallTool {
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

pub(super) struct MemoryListTool {
    pub(super) stores: MemoryStores,
}

impl ToolSpec for MemoryListTool {
    tool_metadata!("memory_list");
}

impl ToolHandler for MemoryListTool {
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

pub(super) struct MemoryUpdateTool<T> {
    pub(super) target: T,
}

impl<T: MemoryMutationTarget> ToolSpec for MemoryUpdateTool<T> {
    tool_metadata!("memory_update");
}

impl<T: MemoryMutationTarget + 'static> ToolHandler for MemoryUpdateTool<T> {
    type Args = UpdateArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        self.target.update(args)
    }
}

pub(super) struct MemoryForgetTool<T> {
    pub(super) target: T,
}

impl<T: MemoryMutationTarget> ToolSpec for MemoryForgetTool<T> {
    tool_metadata!("memory_forget");
}

impl<T: MemoryMutationTarget + 'static> ToolHandler for MemoryForgetTool<T> {
    type Args = IdArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        self.target.forget(args)
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use barracuda_agent_memory::LongTermMemory;
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;

    use super::*;

    #[test]
    fn standard_handlers_preserve_live_memory_mutations() {
        block_on(async {
            let filesystem = memory_vfs().await.expect("memory VFS mounts");
            let stores = MemoryStores {
                global: LongTermMemory::new(filesystem.clone(), "/global", "g-")
                    .await
                    .expect("global store opens"),
                agent: LongTermMemory::new(filesystem, "/agent", "a-")
                    .await
                    .expect("agent store opens"),
            };
            let mut tools = ToolSet::empty();
            tools
                .add_group(ToolGroup::new(
                    "memory",
                    true,
                    [
                        Tool::new(MemoryStoreTool {
                            target: StoreTarget {
                                stores: stores.clone(),
                            },
                        }),
                        Tool::new(MemoryRecallTool {
                            stores: stores.clone(),
                        }),
                        Tool::new(MemoryListTool {
                            stores: stores.clone(),
                        }),
                        Tool::new(MemoryUpdateTool {
                            target: StoreTarget {
                                stores: stores.clone(),
                            },
                        }),
                        Tool::new(MemoryForgetTool {
                            target: StoreTarget {
                                stores: stores.clone(),
                            },
                        }),
                    ],
                ))
                .expect("memory tools register");

            let handle = tools.begin().expect("memory tools begin");
            let runner = ToolRunner::new(&handle);
            let store = ToolInvocation::try_new(
                Some("store"),
                "memory_store",
                r#"{"content":"likes tea"}"#,
            )
            .expect("store invocation parses");
            let mut result = runner.run(vec![store]);
            assert!(result.next().await.expect("store result").1.ok);

            let id = stores
                .agent
                .list()
                .first()
                .expect("stored memory exists")
                .id
                .clone();
            let update = ToolInvocation::try_new(
                Some("update"),
                "memory_update",
                &format!(r#"{{"id":"{id}","content":"likes coffee"}}"#),
            )
            .expect("update invocation parses");
            let mut result = runner.run(vec![update]);
            assert!(result.next().await.expect("update result").1.ok);
            assert_eq!(
                stores
                    .agent
                    .list()
                    .first()
                    .expect("updated memory exists")
                    .content,
                "likes coffee"
            );

            let forget = ToolInvocation::try_new(
                Some("forget"),
                "memory_forget",
                &format!(r#"{{"id":"{id}"}}"#),
            )
            .expect("forget invocation parses");
            let mut result = runner.run(vec![forget]);
            assert!(result.next().await.expect("forget result").1.ok);
            assert!(stores.agent.list().is_empty());
        });
    }
}
