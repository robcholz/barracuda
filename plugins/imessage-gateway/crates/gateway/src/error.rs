use alloc::string::String;

use crate::Operation;

/// Failure while reading an outbound text or binary stream.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StreamError {
    #[error("stream failed: {message}")]
    Failed { message: String },
}

impl StreamError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }
}

/// Failure reported by a concrete message-channel provider.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ChannelError {
    #[error("operation is not supported: {operation:?}")]
    Unsupported { operation: Operation },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("channel authentication failed")]
    Authentication,
    #[error("channel rate limit reached")]
    RateLimited,
    #[error("channel transport failed: {message}")]
    Transport { message: String },
    #[error("platform rejected the request: {message}")]
    Platform {
        code: Option<String>,
        message: String,
    },
    #[error(transparent)]
    Stream(#[from] StreamError),
}

impl ChannelError {
    pub const fn unsupported(operation: Operation) -> Self {
        Self::Unsupported { operation }
    }
}

/// Registration, routing, or provider failure exposed by the Gateway facade.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GatewayError {
    #[error("channel is already registered: {channel}")]
    DuplicateChannel { channel: String },
    #[error("channel is not registered: {channel}")]
    UnknownChannel { channel: String },
    #[error("channel {channel} failed: {source}")]
    Channel {
        channel: String,
        #[source]
        source: ChannelError,
    },
}
