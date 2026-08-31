//! Built-in backend selection and dispatch.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use getset::CopyGetters;

mod anthropic;
mod openai_compatible;
pub(crate) mod shared;
pub(crate) mod sse;

use embedded_nal_async::{Dns, TcpConnect};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr};

use super::chat_stream::ProviderStream;
use super::errors::Error;
use super::transport::Transport;
use super::types::{ChatRequest, LlmResponse, MediaRequest, ModelApiConfig};

/// Failed to parse a string backend id into [`BackendKind`].
///
/// This wrapper is intentional: `strum::ParseError` does not implement
/// `core::error::Error` in our `no_std` feature set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown LLM backend type")]
pub struct ParseBackendKindError;

fn parse_backend_kind_error(_: &str) -> ParseBackendKindError {
    ParseBackendKindError
}

/// Built-in backend kind selected by [`ModelApiConfig`](crate::ModelApiConfig).
#[derive(
    Clone, Copy, Debug, Display, EnumString, IntoStaticStr, PartialEq, Eq, Serialize, Deserialize,
)]
#[strum(
    parse_err_ty = ParseBackendKindError,
    parse_err_fn = parse_backend_kind_error
)]
pub enum BackendKind {
    #[serde(rename = "openai_compatible")]
    #[strum(serialize = "openai_compatible")]
    OpenAiCompatible,
    #[serde(rename = "anthropic_compatible")]
    #[strum(serialize = "anthropic_compatible")]
    AnthropicCompatible,
}

impl BackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    pub(crate) fn make(self, config: ModelApiConfig) -> Backend {
        Backend::new(self, config)
    }
}

#[derive(CopyGetters)]
pub(crate) struct Backend {
    kind: BackendKind,
    model: String,
    endpoint: String,
    headers: Vec<(String, String)>,
    #[getset(get_copy = "pub(crate)")]
    timeout_ms: u32,
    max_tokens: u32,
    image_max_bytes: usize,
}

impl Backend {
    fn new(kind: BackendKind, config: ModelApiConfig) -> Self {
        let ModelApiConfig {
            api_key,
            model,
            base_url,
            timeout_ms,
            max_tokens,
            image_max_bytes,
            ..
        } = config;
        let (path, headers) = match kind {
            BackendKind::OpenAiCompatible => (
                openai_compatible::CHAT_PATH,
                vec![("Authorization".to_string(), format!("Bearer {api_key}"))],
            ),
            BackendKind::AnthropicCompatible => (
                anthropic::CHAT_PATH,
                vec![
                    ("x-api-key".to_string(), api_key),
                    (
                        "anthropic-version".to_string(),
                        anthropic::ANTHROPIC_VERSION.to_string(),
                    ),
                ],
            ),
        };
        Self {
            kind,
            model,
            endpoint: shared::join_url(&base_url, path),
            headers,
            timeout_ms,
            max_tokens,
            image_max_bytes,
        }
    }

    pub(super) fn request_body(&self) -> Map<String, Value> {
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert("max_tokens".to_string(), Value::from(self.max_tokens));
        body
    }

    pub(crate) async fn chat(
        &self,
        http: &mut Transport<'_, impl TcpConnect, impl Dns>,
        request: &ChatRequest<'_>,
    ) -> Result<LlmResponse, Error> {
        match self.kind {
            BackendKind::OpenAiCompatible => openai_compatible::chat(self, http, request).await,
            BackendKind::AnthropicCompatible => anthropic::chat(self, http, request).await,
        }
    }

    pub(crate) async fn chat_json(
        &self,
        http: &mut Transport<'_, impl TcpConnect, impl Dns>,
        request: &ChatRequest<'_>,
        schema_name: &str,
        schema: &Value,
    ) -> Result<LlmResponse, Error> {
        match self.kind {
            BackendKind::OpenAiCompatible => {
                openai_compatible::chat_json(self, http, request, schema_name, schema).await
            }
            BackendKind::AnthropicCompatible => {
                anthropic::chat_json(self, http, request, schema_name, schema).await
            }
        }
    }

    pub(crate) async fn infer_media(
        &self,
        http: &mut Transport<'_, impl TcpConnect, impl Dns>,
        request: &MediaRequest<'_>,
    ) -> Result<String, Error> {
        match self.kind {
            BackendKind::OpenAiCompatible => {
                openai_compatible::infer_media(self, http, request).await
            }
            BackendKind::AnthropicCompatible => anthropic::infer_media(self, http, request).await,
        }
    }

    pub(crate) async fn chat_stream<'h>(
        &self,
        http: &'h mut Transport<'_, impl TcpConnect, impl Dns>,
        request: &ChatRequest<'_>,
    ) -> Result<ProviderStream<'h>, Error> {
        match self.kind {
            BackendKind::OpenAiCompatible => {
                openai_compatible::chat_stream(self, http, request).await
            }
            BackendKind::AnthropicCompatible => anthropic::chat_stream(self, http, request).await,
        }
    }
}
