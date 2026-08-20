/// Invalid Gateway frame stream or logical JSON payload.
#[derive(Debug, thiserror::Error)]
pub enum GatewayWireError {
    /// One frame declared more bytes than its payload can hold.
    #[error("invalid Gateway chunk")]
    InvalidChunk,
    /// The logical payload was not valid JSON for the requested Gateway DTO.
    #[error("invalid Gateway JSON: {0}")]
    Json(#[from] serde_json::Error),
}
