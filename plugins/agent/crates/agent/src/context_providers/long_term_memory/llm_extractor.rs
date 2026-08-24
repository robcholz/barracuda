//! [`LlmExtractor`] — an [`Extractor`] backed by [`ModelApi`].
//!
//! It asks the model to read a conversation transcript and call the canonical
//! long-term-memory tools. Like the conversation provider's LLM compactor, it lives in
//! `barracuda_agent` (the agent wiring layer) rather than `barracuda-agent-memory`, because the
//! [`Extractor`] seam stays free of any LLM dependency; the concrete extractor is
//! injected into the long-term memory provider.

use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};

use serde_json::json;

use barracuda_model_api::{ChatRequest, ModelApiFactory};
use barracuda_runtime_utils::Cancel;
use embedded_nal_async::{Dns, TcpConnect};
use tracing::Instrument as _;

use super::super::async_llm::SharedAsyncLlm;
use crate::config::{ApiPurpose, SharedApiManager};

use super::extraction::{ExtractError, ExtractFuture, ExtractionInput, Extractor, MemorySnapshot};
use super::tools::ExtractionTools;

/// System prompt steering the extraction through canonical memory tool calls.
const EXTRACT_SYSTEM_PROMPT: &str = prompt!("memory/long_term_extraction_system.md");

/// Header prefacing the current-memory listing handed to the model.
const EXTRACT_MEMORY_HEADER: &str = "CURRENT MEMORY:";

/// Header prefacing the transcript handed to the model.
const EXTRACT_TRANSCRIPT_HEADER: &str = "CONVERSATION:";

/// An [`Extractor`] that distills facts via the LLM client.
///
/// Owns its own async LLM client. The extractor is shared across agents as an
/// `Arc<dyn Extractor>`, while [`ModelApi::chat`] needs `&mut self`, so
/// calls borrow the client exclusively without holding a mutex while the future
/// is running.
pub(super) struct LlmExtractor<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> {
    api: SharedAsyncLlm<Tcp, Resolver>,
    /// Shared per-usage config; the extraction config is applied at the start of
    /// each extraction call.
    api_manager: SharedApiManager,
}

impl<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> LlmExtractor<Tcp, Resolver> {
    /// Build an extractor with its own unconfigured LLM client.
    fn new(api_manager: SharedApiManager, llm_factory: &ModelApiFactory<Tcp, Resolver>) -> Self {
        Self {
            api: SharedAsyncLlm::new(llm_factory.create()),
            api_manager,
        }
    }

    /// A ready-to-inject [`Extractor`] using `api_manager`.
    pub(super) fn shared(
        api_manager: SharedApiManager,
        llm_factory: &ModelApiFactory<Tcp, Resolver>,
    ) -> Arc<dyn Extractor> {
        Arc::new(Self::new(api_manager, llm_factory))
    }
}

impl<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> Extractor for LlmExtractor<Tcp, Resolver> {
    fn extract<'a>(&'a self, input: ExtractionInput<'a>) -> ExtractFuture<'a> {
        Box::pin(async move {
            let mut extraction_tools = ExtractionTools::new().map_err(ExtractError::from)?;
            let prompt = format!(
                "{EXTRACT_MEMORY_HEADER}\n{}\n\n{EXTRACT_TRANSCRIPT_HEADER}\n{}",
                render_existing(input.existing),
                input.transcript
            );
            let messages = [json!({ "role": "user", "content": prompt })];

            let tool_schemas = extraction_tools.schemas().map_err(ExtractError::from)?;
            let request =
                ChatRequest::new(EXTRACT_SYSTEM_PROMPT, &messages).with_tools(&tool_schemas);
            let max_attempts = u64::from(request.retry.max_retries).saturating_add(1);
            let chat_span =
                tracing::info_span!("api.chat", purpose = "memory_extraction", max_attempts,);
            let response = async {
                let mut api = self.api.lease().await;
                // Apply this operation's config from the manager (its explicit
                // binding, else the default). None / invalid keeps the current one.
                if let Some(config) = self.api_manager.borrow().get_api(ApiPurpose::Memory) {
                    let _ = api.set_config(config);
                }
                api.chat(&request, Cancel::never()).await
            }
            .instrument(chat_span)
            .await
            .map_err(ExtractError::from)?;

            extraction_tools
                .run(response.tool_calls)
                .await
                .map_err(ExtractError::from)
        })
    }
}

/// Render the current memory as an `id: content [tags]` listing for the prompt,
/// or `(none)` when empty.
fn render_existing(existing: &[MemorySnapshot]) -> String {
    if existing.is_empty() {
        return "(none)".into();
    }
    existing
        .iter()
        .map(|item| {
            if item.tags.is_empty() {
                format!("{}: {}", item.id, item.content)
            } else {
                format!("{}: {} [{}]", item.id, item.content, item.tags.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use barracuda_model_api::ToolCall;
    use futures_lite::future::block_on;

    use super::super::extraction::MemoryOp;

    #[test]
    fn extraction_tools_come_from_the_canonical_tool_resources() {
        let mut tools = super::ExtractionTools::new().expect("extraction tools build");
        let schemas_json = tools.schemas().expect("extraction tool schemas render");
        let schemas: serde_json::Value =
            serde_json::from_str(&schemas_json).expect("extraction tools are valid JSON");
        let names = schemas
            .as_array()
            .expect("tool schemas form an array")
            .iter()
            .filter_map(|schema| schema.pointer("/function/name")?.as_str())
            .collect::<alloc::vec::Vec<_>>();

        assert_eq!(names, ["memory_forget", "memory_store", "memory_update"]);
    }

    #[test]
    fn tool_calls_decode_into_memory_operations() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                name: "memory_store".into(),
                arguments_json:
                    r#"{"content":"  likes tea  ","tags":["preference"],"keywords":["drink"]}"#
                        .into(),
            },
            ToolCall {
                id: "2".into(),
                name: "memory_update".into(),
                arguments_json: r#"{"id":" memory-1 ","content":"likes coffee"}"#.into(),
            },
            ToolCall {
                id: "3".into(),
                name: "memory_forget".into(),
                arguments_json: r#"{"id":"memory-2"}"#.into(),
            },
        ];

        let operations = block_on(run_extraction_tools(calls)).expect("valid tool calls execute");
        assert_eq!(operations.len(), 3);
        assert!(matches!(
            &operations[0],
            MemoryOp::Add(item)
                if item.content == "likes tea" && item.tags == ["preference"]
        ));
        assert!(matches!(
            &operations[1],
            MemoryOp::Update { id, patch }
                if id.as_str() == "memory-1"
                    && patch.content.as_deref() == Some("likes coffee")
                    && patch.tags.is_none()
                    && patch.keywords.is_none()
        ));
        assert!(matches!(
            &operations[2],
            MemoryOp::Forget { id } if id.as_str() == "memory-2"
        ));
    }

    #[test]
    fn canonical_validator_rejects_blank_memory_fields() {
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "memory_store".into(),
            arguments_json: r#"{"content":"fact","tags":["   "]}"#.into(),
        }];

        assert!(block_on(run_extraction_tools(calls)).is_err());
    }

    #[test]
    fn extraction_tool_calls_use_standard_tool_lookup_errors() {
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "unexpected".into(),
            arguments_json: "{}".into(),
        }];

        let error = block_on(run_extraction_tools(calls))
            .expect_err("an unregistered extraction tool must fail");

        assert!(error.to_string().contains("tool not found: unexpected"));
    }

    async fn run_extraction_tools(
        calls: Vec<ToolCall>,
    ) -> Result<Vec<MemoryOp>, barracuda_agent_tool::ToolInvokeError> {
        let mut tools = super::ExtractionTools::new()?;
        tools.run(calls).await
    }
}
