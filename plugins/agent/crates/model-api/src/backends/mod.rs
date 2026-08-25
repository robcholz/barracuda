//! Built-in backend selection and dispatch.

use alloc::string::String;

mod anthropic;
mod openai_compatible;
pub(crate) mod shared;
pub(crate) mod sse;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::{Display, EnumString, IntoStaticStr};

use super::chat_stream::ProviderStream;
use super::errors::{ChatError, InferMediaError};
use super::transport::{HttpTransport as NetClient, ResponseStream};
use super::types::{ChatJsonRequest, ChatRequest, LlmResponse, MediaRequest, ModelApiConfig};

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
        match self {
            Self::OpenAiCompatible => {
                Backend::OpenAi(openai_compatible::OpenAiCompatible::new(config))
            }
            Self::AnthropicCompatible => Backend::Anthropic(anthropic::Anthropic::new(config)),
        }
    }
}

pub(crate) enum Backend {
    OpenAi(openai_compatible::OpenAiCompatible),
    Anthropic(anthropic::Anthropic),
}

impl Backend {
    pub(crate) fn timeout_ms(&self) -> u32 {
        match self {
            Self::OpenAi(backend) => backend.timeout_ms(),
            Self::Anthropic(backend) => backend.timeout_ms(),
        }
    }

    pub(crate) async fn chat(
        &self,
        http: &mut NetClient<'_>,
        request: &ChatRequest<'_>,
    ) -> Result<LlmResponse, ChatError> {
        match self {
            Self::OpenAi(backend) => backend.chat(http, request).await,
            Self::Anthropic(backend) => backend.chat(http, request).await,
        }
    }

    pub(crate) async fn chat_json(
        &self,
        http: &mut NetClient<'_>,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
    ) -> Result<LlmResponse, ChatError> {
        match self {
            Self::OpenAi(backend) => backend.chat_json(http, request, schema_name, schema).await,
            Self::Anthropic(backend) => backend.chat_json(http, request, schema_name, schema).await,
        }
    }

    pub(crate) async fn infer_media(
        &self,
        http: &mut NetClient<'_>,
        request: &MediaRequest<'_>,
    ) -> Result<String, InferMediaError> {
        match self {
            Self::OpenAi(backend) => backend.infer_media(http, request).await,
            Self::Anthropic(backend) => backend.infer_media(http, request).await,
        }
    }

    pub(crate) async fn chat_stream<'h>(
        &self,
        http: &'h mut NetClient<'_>,
        request: &ChatRequest<'_>,
    ) -> Result<ProviderStream<ResponseStream<'h>>, ChatError> {
        match self {
            Self::OpenAi(backend) => backend.chat_stream(http, request).await,
            Self::Anthropic(backend) => backend.chat_stream(http, request).await,
        }
    }
}
