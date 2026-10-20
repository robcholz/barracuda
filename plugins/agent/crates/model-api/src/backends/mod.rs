//! Built-in backend selection and dispatch.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use getset::CopyGetters;

mod anthropic;
mod body;
mod openai;
pub(crate) mod shared;
pub(crate) mod sse;

use embedded_nal_async::{Dns, TcpConnect};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr};

use super::attachments::{self, MediaPolicy, RequestMedia};
use super::chat_stream::ProviderStream;
use super::errors::Error;
use super::transport::Transport;
use super::types::{ChatRequest, LlmResponse, ModelApiConfig};

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
    #[serde(rename = "openai")]
    #[strum(serialize = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    #[strum(serialize = "anthropic")]
    Anthropic,
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
    media: MediaPolicy,
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
            input_modalities,
            max_images_in_context,
            ..
        } = config;
        let (path, headers) = match kind {
            BackendKind::OpenAi => (
                openai::CHAT_PATH,
                vec![("Authorization".to_string(), format!("Bearer {api_key}"))],
            ),
            BackendKind::Anthropic => (
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
            media: MediaPolicy {
                modalities: input_modalities,
                documents: kind == BackendKind::Anthropic,
                max_inline: max_images_in_context,
                max_bytes: image_max_bytes,
            },
        }
    }

    /// Reads the attachments `request` inlines under this backend's policy.
    pub(crate) async fn resolve_media(&self, request: &ChatRequest<'_>) -> RequestMedia {
        attachments::resolve(self.media, request).await
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
        media: &RequestMedia,
    ) -> Result<LlmResponse, Error> {
        match self.kind {
            BackendKind::OpenAi => openai::chat(self, http, request, media).await,
            BackendKind::Anthropic => anthropic::chat(self, http, request, media).await,
        }
    }

    pub(crate) async fn chat_json(
        &self,
        http: &mut Transport<'_, impl TcpConnect, impl Dns>,
        request: &ChatRequest<'_>,
        media: &RequestMedia,
        schema_name: &str,
        schema: &Value,
    ) -> Result<LlmResponse, Error> {
        match self.kind {
            BackendKind::OpenAi => {
                openai::chat_json(self, http, request, media, schema_name, schema).await
            }
            BackendKind::Anthropic => {
                anthropic::chat_json(self, http, request, media, schema_name, schema).await
            }
        }
    }

    pub(crate) async fn chat_stream<'h>(
        &self,
        http: &'h mut Transport<'_, impl TcpConnect, impl Dns>,
        request: &ChatRequest<'_>,
        media: &RequestMedia,
    ) -> Result<ProviderStream<'h>, Error> {
        match self.kind {
            BackendKind::OpenAi => openai::chat_stream(self, http, request, media).await,
            BackendKind::Anthropic => anthropic::chat_stream(self, http, request, media).await,
        }
    }
}
