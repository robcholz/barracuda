//! Streaming JSON protocol for Workflow control RPCs.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;
use serde::Deserialize;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use barracuda_rpc::{
    RpcClient, RpcError, RpcFrame, RpcMethod, RpcResult, RpcStream, Streaming, Unary,
};

use crate::{Rule, WorkflowDefinition, WorkflowId};

/// Receiver-side rejection returned by a Workflow control RPC.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum WorkflowControlRejection {
    /// A request frame was malformed.
    InvalidFrame,
    /// The request body was empty or was not valid Workflow JSON.
    InvalidJson,
    /// The JSON contained an invalid Workflow ID.
    InvalidWorkflowId,
    /// The JSON contained an invalid Event rule.
    InvalidRule,
    /// The JSON contained an invalid RPC address.
    InvalidRpcAddress,
    /// The Workflow did not contain an RPC step.
    EmptySteps,
    /// Another loaded Workflow already owns the requested ID.
    DuplicateId,
    /// No loaded Workflow owns the requested ID.
    NotFound,
    /// The persistence operation failed.
    Persistence,
}

/// Failure returned by [`WorkflowClient`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowControlError {
    /// RPC transport or runtime failure.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// Workflow Runtime rejected the control request.
    #[error("Workflow control request was rejected: {0:?}")]
    Rejected(WorkflowControlRejection),
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct WorkflowJsonFrameHeader {
    data_length: usize,
}

/// One fixed-layout frame in a streaming Workflow JSON request.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct WorkflowJsonFrame<const M: usize> {
    bytes: [u8; M],
}

impl<const M: usize> WorkflowJsonFrame<M> {
    const fn data_capacity() -> usize {
        M.saturating_sub(size_of::<WorkflowJsonFrameHeader>())
    }

    fn new(data: &[u8]) -> Result<Self, RpcError> {
        if data.is_empty() || data.len() > Self::data_capacity() {
            return Err(RpcError::InvalidFrameState);
        }
        let mut frame = Self { bytes: [0; M] };
        let header_size = size_of::<WorkflowJsonFrameHeader>();
        let header = WorkflowJsonFrameHeader {
            data_length: data.len(),
        };
        frame
            .bytes
            .get_mut(..header_size)
            .ok_or(RpcError::InvalidFrameState)?
            .copy_from_slice(header.as_bytes());
        let data_end = header_size
            .checked_add(data.len())
            .ok_or(RpcError::InvalidFrameState)?;
        frame
            .bytes
            .get_mut(header_size..data_end)
            .ok_or(RpcError::InvalidFrameState)?
            .copy_from_slice(data);
        Ok(frame)
    }

    fn data(&self) -> Result<&[u8], WorkflowControlRejection> {
        let (header, remaining) = WorkflowJsonFrameHeader::try_read_from_prefix(&self.bytes)
            .map_err(|_error| WorkflowControlRejection::InvalidFrame)?;
        if header.data_length == 0 || header.data_length > remaining.len() {
            return Err(WorkflowControlRejection::InvalidFrame);
        }
        let data = remaining
            .get(..header.data_length)
            .ok_or(WorkflowControlRejection::InvalidFrame)?;
        if remaining
            .get(header.data_length..)
            .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
        {
            return Err(WorkflowControlRejection::InvalidFrame);
        }
        Ok(data)
    }
}

/// Streaming RPC that validates and durably loads one Workflow JSON document.
pub struct WorkflowLoad<const M: usize>;

impl<const M: usize> RpcMethod for WorkflowLoad<M> {
    const ADDRESS: &'static str = "workflow.load";
    type Request = WorkflowJsonFrame<M>;
    type Response = ();
    type Error = WorkflowControlRejection;
    type Input = Streaming;
    type Output = Unary;
}

/// Streaming RPC that durably unloads one Workflow selected by JSON ID.
pub struct WorkflowUnload<const M: usize>;

impl<const M: usize> RpcMethod for WorkflowUnload<M> {
    const ADDRESS: &'static str = "workflow.unload";
    type Request = WorkflowJsonFrame<M>;
    type Response = ();
    type Error = WorkflowControlRejection;
    type Input = Streaming;
    type Output = Unary;
}

/// Client for Workflow Runtime's durable control RPCs.
#[derive(Clone)]
pub struct WorkflowClient<const M: usize> {
    rpc: RpcClient,
}

impl<const M: usize> WorkflowClient<M> {
    /// Wraps an existing RPC client.
    #[must_use]
    pub const fn new(rpc: RpcClient) -> Self {
        const { assert_frame_capacity::<M>() }
        Self { rpc }
    }

    /// Streams one complete Workflow JSON document to `workflow.load`.
    ///
    /// # Errors
    ///
    /// Returns a transport error or the Runtime's validation, duplicate-ID, or
    /// persistence rejection.
    pub async fn load(&self, json: &str) -> Result<(), WorkflowControlError> {
        self.call::<WorkflowLoad<M>>(json.as_bytes()).await
    }

    /// Streams a JSON Workflow ID to `workflow.unload`.
    ///
    /// # Errors
    ///
    /// Returns a transport error, a not-found rejection, or a persistence
    /// rejection.
    pub async fn unload(&self, workflow_id: &WorkflowId) -> Result<(), WorkflowControlError> {
        let json = format!(r#"{{"id":"{}"}}"#, workflow_id.as_str());
        self.call::<WorkflowUnload<M>>(json.as_bytes()).await
    }

    async fn call<Method>(&self, json: &[u8]) -> Result<(), WorkflowControlError>
    where
        Method: RpcMethod<
            Request = WorkflowJsonFrame<M>,
            Response = (),
            Error = WorkflowControlRejection,
            Input = Streaming,
            Output = Unary,
        >,
    {
        let frames = RpcStream::new(WorkflowJsonStream::<M>::new(json.to_vec()));
        let outcome = self.rpc.call::<Method>(frames)?.await?;
        match outcome {
            Ok(response) => {
                response.view()?;
                Ok(())
            }
            Err(rejection) => Err(WorkflowControlError::Rejected(*rejection.view()?)),
        }
    }
}

/// Fully collected JSON request accepted from the streaming control wire.
pub struct WorkflowJsonRequest {
    bytes: Vec<u8>,
}

impl WorkflowJsonRequest {
    /// Wraps one complete JSON document read from persistent storage.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, WorkflowControlRejection> {
        if bytes.is_empty() {
            return Err(WorkflowControlRejection::InvalidJson);
        }
        Ok(Self { bytes })
    }

    /// Collects and validates all request frames through EOF.
    pub async fn accept<const M: usize>(
        mut frames: RpcStream<RpcFrame<WorkflowJsonFrame<M>>>,
    ) -> RpcResult<Result<Self, WorkflowControlRejection>> {
        const { assert_frame_capacity::<M>() }
        let mut bytes = Vec::new();
        while let Some(frame) = frames.next().await {
            let frame = frame?;
            let frame = frame.view()?;
            let data = match frame.data() {
                Ok(data) => data,
                Err(rejection) => return Ok(Err(rejection)),
            };
            bytes.extend_from_slice(data);
        }
        Ok(Self::from_bytes(bytes))
    }

    /// Returns the original JSON bytes for durable storage.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Parses and validates this request as one Workflow definition.
    pub fn definition(&self) -> Result<WorkflowDefinition, WorkflowControlRejection> {
        let document: WorkflowDocument = serde_json::from_slice(&self.bytes)
            .map_err(|_error| WorkflowControlRejection::InvalidJson)?;
        document.try_into()
    }

    /// Parses and validates this request as one Workflow ID document.
    pub fn workflow_id(&self) -> Result<WorkflowId, WorkflowControlRejection> {
        let document: WorkflowIdDocument = serde_json::from_slice(&self.bytes)
            .map_err(|_error| WorkflowControlRejection::InvalidJson)?;
        WorkflowId::try_from(document.id)
            .map_err(|_error| WorkflowControlRejection::InvalidWorkflowId)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowDocument {
    id: String,
    #[serde(rename = "match")]
    matcher: WorkflowMatchDocument,
    steps: Vec<WorkflowStepDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowMatchDocument {
    event: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowStepDocument {
    call: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowIdDocument {
    id: String,
}

impl TryFrom<WorkflowDocument> for WorkflowDefinition {
    type Error = WorkflowControlRejection;

    fn try_from(document: WorkflowDocument) -> Result<Self, Self::Error> {
        let id = WorkflowId::try_from(document.id)
            .map_err(|_error| WorkflowControlRejection::InvalidWorkflowId)?;
        let event = Rule::try_from(document.matcher.event)
            .map_err(|_error| WorkflowControlRejection::InvalidRule)?;
        let steps = document
            .steps
            .into_iter()
            .map(|step| {
                barracuda_rpc::RpcAddress::try_from(step.call.as_str())
                    .map_err(|_error| WorkflowControlRejection::InvalidRpcAddress)
            })
            .collect::<Result<Vec<_>, _>>()?;
        WorkflowDefinition::new(id, event, steps)
            .map_err(|_error| WorkflowControlRejection::EmptySteps)
    }
}

struct WorkflowJsonStream<const M: usize> {
    bytes: Vec<u8>,
    offset: usize,
}

impl<const M: usize> WorkflowJsonStream<M> {
    fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, offset: 0 }
    }
}

impl<const M: usize> Stream for WorkflowJsonStream<M> {
    type Item = RpcResult<WorkflowJsonFrame<M>>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(remaining) = this.bytes.get(this.offset..) else {
            return Poll::Ready(Some(Err(RpcError::InvalidFrameState)));
        };
        if remaining.is_empty() {
            return Poll::Ready(None);
        }
        let length = remaining.len().min(WorkflowJsonFrame::<M>::data_capacity());
        let Some(chunk) = remaining.get(..length) else {
            return Poll::Ready(Some(Err(RpcError::InvalidFrameState)));
        };
        let frame = WorkflowJsonFrame::new(chunk);
        this.offset = match this.offset.checked_add(length) {
            Some(offset) => offset,
            None => return Poll::Ready(Some(Err(RpcError::InvalidFrameState))),
        };
        Poll::Ready(Some(frame))
    }
}

const fn assert_frame_capacity<const M: usize>() {
    assert!(
        M > size_of::<WorkflowJsonFrameHeader>(),
        "Workflow control requires room for frame metadata and at least one JSON byte"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::vec::Vec;

    use super::{WorkflowControlRejection, WorkflowJsonRequest};

    #[test]
    fn workflow_json_uses_the_documented_match_and_step_shape() {
        let request = WorkflowJsonRequest::from_bytes(
            br#"{
                "id":"gateway-to-agent",
                "match":{"event":"gateway.*"},
                "steps":[{"call":"adapter.gateway"},{"call":"agent.run"}]
            }"#
            .to_vec(),
        )
        .expect("non-empty JSON");

        let definition = request.definition().expect("valid Workflow JSON");

        assert_eq!(definition.id().as_str(), "gateway-to-agent");
        assert_eq!(definition.event().as_str(), "gateway.*");
        assert_eq!(
            definition
                .steps()
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>(),
            ["adapter.gateway", "agent.run"]
        );
    }

    #[test]
    fn workflow_json_rejects_invalid_json_and_empty_steps() {
        let malformed = WorkflowJsonRequest::from_bytes(b"{".to_vec())
            .expect("non-empty malformed request")
            .definition();
        assert!(matches!(
            malformed,
            Err(WorkflowControlRejection::InvalidJson)
        ));

        let empty_steps = WorkflowJsonRequest::from_bytes(
            br#"{"id":"empty","match":{"event":"gateway.*"},"steps":[]}"#.to_vec(),
        )
        .expect("non-empty JSON")
        .definition();
        assert!(matches!(
            empty_steps,
            Err(WorkflowControlRejection::EmptySteps)
        ));
    }
}
