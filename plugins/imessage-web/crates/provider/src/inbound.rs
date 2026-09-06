use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
};
use core::{future::Future, pin::Pin};

use barracuda_imessage_gateway_plugin::{BinaryStream, MediaKind};
use serde::Deserialize;

/// Runtime-neutral future returned by an inbound Web message sink.
pub type InboundFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, InboundError>> + 'a>>;

/// Text received through the REST-facing Web service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundMessage {
    pub conversation_id: String,
    pub message_id: String,
    pub thread_id: Option<String>,
    pub text: String,
    pub reply_to: Option<String>,
}

/// Binary body received through the REST-facing Web service.
pub enum MessageBody {
    Bytes(alloc::vec::Vec<u8>),
    Stream(BinaryStream),
}

/// File, image, audio, or video received through the REST-facing Web service.
pub struct InboundMedia {
    pub conversation_id: String,
    pub message_id: String,
    pub thread_id: Option<String>,
    pub kind: MediaKind,
    pub body: MessageBody,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub reply_to: Option<String>,
}

/// Acknowledgement returned after an inbound REST command is accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundReceipt {
    pub message_id: String,
}

/// Consumer of messages received from Web clients. Event Router can implement this later.
pub trait InboundMessageSink: 'static {
    fn receive_message(&self, request: InboundMessage) -> InboundFuture<'_, ()>;

    fn receive_media(&self, _request: InboundMedia) -> InboundFuture<'_, ()> {
        Box::pin(async {
            Err(InboundError::Unsupported {
                operation: "receive_media",
            })
        })
    }
}

/// Protocol or sink failure while receiving a Web message.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InboundError {
    #[error("invalid JSON body: {message}")]
    InvalidJson { message: String },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("operation is not supported: {operation}")]
    Unsupported { operation: &'static str },
    #[error("inbound sink rejected the request: {message}")]
    Rejected { message: String },
}

/// HTTP-framework-neutral command service used by REST adapters.
pub struct WebService {
    sink: Rc<dyn InboundMessageSink>,
}

impl WebService {
    pub fn new(sink: Rc<dyn InboundMessageSink>) -> Self {
        Self { sink }
    }

    pub async fn receive_message_json(
        &self,
        conversation_id: &str,
        body: &[u8],
    ) -> Result<InboundReceipt, InboundError> {
        log::debug!(
            "IMessage Web received message request for conversation `{conversation_id}` ({} bytes)",
            body.len()
        );
        let dto: InboundMessageDto = match serde_json::from_slice(body) {
            Ok(dto) => dto,
            Err(error) => {
                log::warn!(
                    "IMessage Web rejected malformed message for conversation `{conversation_id}`: {error}"
                );
                return Err(InboundError::InvalidJson {
                    message: error.to_string(),
                });
            }
        };
        if let Err(error) = validate_required("conversation_id", conversation_id)
            .and_then(|()| validate_required("message_id", &dto.message_id))
            .and_then(|()| validate_required("text", &dto.text))
        {
            log::warn!(
                "IMessage Web rejected invalid message for conversation `{conversation_id}`: {error}"
            );
            return Err(error);
        }

        let message_id = dto.message_id.clone();
        if let Err(error) = self
            .sink
            .receive_message(InboundMessage {
                conversation_id: String::from(conversation_id),
                message_id: dto.message_id,
                thread_id: dto.thread_id,
                text: dto.text,
                reply_to: dto.reply_to,
            })
            .await
        {
            log::warn!(
                "IMessage Web failed to deliver message `{message_id}` for conversation `{conversation_id}`: {error}"
            );
            return Err(error);
        }
        log::info!(
            "IMessage Web accepted message `{message_id}` for conversation `{conversation_id}`"
        );
        Ok(InboundReceipt { message_id })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn receive_media(
        &self,
        conversation_id: &str,
        kind: MediaKind,
        message_id: &str,
        body: MessageBody,
        filename: Option<&str>,
        mime_type: Option<&str>,
        thread_id: Option<&str>,
        reply_to: Option<&str>,
    ) -> Result<InboundReceipt, InboundError> {
        if let Err(error) = validate_required("conversation_id", conversation_id)
            .and_then(|()| validate_required("message_id", message_id))
        {
            log::warn!(
                "IMessage Web rejected invalid {kind:?} media `{message_id}` for conversation `{conversation_id}`: {error}"
            );
            return Err(error);
        }
        if let Err(error) = self
            .sink
            .receive_media(InboundMedia {
                conversation_id: String::from(conversation_id),
                message_id: String::from(message_id),
                thread_id: thread_id.map(String::from),
                kind,
                body,
                filename: filename.map(String::from),
                mime_type: mime_type.map(String::from),
                reply_to: reply_to.map(String::from),
            })
            .await
        {
            log::warn!(
                "IMessage Web failed to deliver {kind:?} media `{message_id}` for conversation `{conversation_id}`: {error}"
            );
            return Err(error);
        }
        log::info!(
            "IMessage Web accepted {kind:?} media `{message_id}` for conversation `{conversation_id}`"
        );
        Ok(InboundReceipt {
            message_id: String::from(message_id),
        })
    }
}

#[derive(Deserialize)]
struct InboundMessageDto {
    message_id: String,
    thread_id: Option<String>,
    text: String,
    reply_to: Option<String>,
}

fn validate_required(name: &str, value: &str) -> Result<(), InboundError> {
    if value.trim().is_empty() {
        return Err(InboundError::InvalidRequest {
            message: alloc::format!("{name} must not be empty"),
        });
    }
    Ok(())
}
