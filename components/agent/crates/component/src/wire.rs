/// Invalid variable-length Agent RPC payload.
#[derive(Debug, thiserror::Error)]
pub enum AgentWireError {
    /// A frame length exceeded its payload.
    #[error("invalid Agent frame")]
    InvalidFrame,
    /// A logical payload was not valid JSON.
    #[error("invalid Agent JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub(crate) fn write_payload<const N: usize>(
    payload: &mut [u8; N],
    bytes: &[u8],
) -> Result<u16, AgentWireError> {
    payload
        .get_mut(..bytes.len())
        .ok_or(AgentWireError::InvalidFrame)?
        .copy_from_slice(bytes);
    u16::try_from(bytes.len()).map_err(|_| AgentWireError::InvalidFrame)
}

pub(crate) fn read_payload<const N: usize>(
    payload: &[u8; N],
    length: u16,
) -> Result<&[u8], AgentWireError> {
    payload
        .get(..usize::from(length))
        .ok_or(AgentWireError::InvalidFrame)
}
