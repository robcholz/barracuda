use gateway::{ChannelError, GatewayError};
use serde::{Deserialize, Serialize};

/// Stable business rejection returned by Gateway operations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum GatewayOperationError {
    /// Required request data is missing or inconsistent.
    #[error("invalid Gateway request")]
    InvalidRequest,
    /// No provider owns the requested channel.
    #[error("Gateway channel is unknown")]
    UnknownChannel,
    /// The stream identifier is already active.
    #[error("Gateway stream already exists")]
    DuplicateStream,
    /// The stream identifier is not active.
    #[error("Gateway stream is unknown")]
    UnknownStream,
    /// A stream command does not follow the accepted sequence.
    #[error("Gateway stream command is out of order")]
    OutOfOrder,
    /// All workers or the selected stream queue are occupied.
    #[error("Gateway is busy")]
    Busy,
    /// The selected provider does not implement the operation.
    #[error("Gateway operation is unsupported")]
    Unsupported,
    /// The provider rejected its credentials.
    #[error("Gateway authentication failed")]
    Authentication,
    /// The provider rate limited the operation.
    #[error("Gateway rate limit reached")]
    RateLimited,
    /// Provider delivery failed.
    #[error("Gateway delivery failed")]
    Delivery,
}

pub(crate) fn map_gateway_error(error: &GatewayError) -> GatewayOperationError {
    match error {
        GatewayError::UnknownChannel { .. } => GatewayOperationError::UnknownChannel,
        GatewayError::DuplicateChannel { .. } => GatewayOperationError::Delivery,
        GatewayError::Channel { source, .. } => match source {
            ChannelError::Unsupported { .. } => GatewayOperationError::Unsupported,
            ChannelError::InvalidRequest { .. } => GatewayOperationError::InvalidRequest,
            ChannelError::Authentication => GatewayOperationError::Authentication,
            ChannelError::RateLimited => GatewayOperationError::RateLimited,
            ChannelError::Transport { .. }
            | ChannelError::Platform { .. }
            | ChannelError::Stream(_) => GatewayOperationError::Delivery,
        },
    }
}

/// Accepted ordered stream command.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct GatewayAccepted {
    /// Sequence accepted by the Gateway runtime.
    pub accepted_sequence: u64,
}

pub(crate) const fn valid_required(value: &str) -> bool {
    !value.is_empty()
}

pub(crate) fn valid_stream_id(value: &str) -> bool {
    valid_required(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
