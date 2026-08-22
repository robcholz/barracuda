//! Domain-to-DTO conversions for the Agent Component.

use barracuda_agent_runtime::{
    ApiPurpose, BackendKind, InputRequestId, PermissionLevel, ReasoningEffort,
    SessionControlError, SessionId, SessionPersistence,
};

use crate::dto;

pub(crate) fn session_to_wire(id: SessionId) -> dto::SessionIdDto {
    dto::SessionIdDto::new(id.0)
}

pub(crate) fn session_from_wire(id: dto::SessionIdDto) -> SessionId {
    SessionId::new(id.get())
}

pub(crate) fn input_request_from_wire(id: dto::InputRequestIdDto) -> InputRequestId {
    InputRequestId::new(id.get())
}

pub(crate) fn persistence_from_wire(value: dto::SessionPersistenceDto) -> SessionPersistence {
    match value {
        dto::SessionPersistenceDto::Persistent => SessionPersistence::Persistent,
        dto::SessionPersistenceDto::Ephemeral => SessionPersistence::Ephemeral,
    }
}

pub(crate) fn permission_level_from_wire(value: dto::PermissionLevelDto) -> PermissionLevel {
    match value {
        dto::PermissionLevelDto::Deny => PermissionLevel::Deny,
        dto::PermissionLevelDto::Ask => PermissionLevel::Ask,
        dto::PermissionLevelDto::AllowAll => PermissionLevel::AllowAll,
    }
}

pub(crate) fn reasoning_effort_from_wire(value: dto::ReasoningEffortDto) -> ReasoningEffort {
    match value {
        dto::ReasoningEffortDto::Low => ReasoningEffort::Low,
        dto::ReasoningEffortDto::Medium => ReasoningEffort::Medium,
        dto::ReasoningEffortDto::High => ReasoningEffort::High,
        dto::ReasoningEffortDto::Ultra => ReasoningEffort::Ultra,
    }
}

pub(crate) fn backend_from_wire(value: dto::BackendKindDto) -> BackendKind {
    match value {
        dto::BackendKindDto::OpenAiCompatible => BackendKind::OpenAiCompatible,
        dto::BackendKindDto::AnthropicCompatible => BackendKind::AnthropicCompatible,
    }
}

pub(crate) fn purpose_from_wire(value: dto::ApiPurposeDto) -> ApiPurpose {
    match value {
        dto::ApiPurposeDto::RootAgent => ApiPurpose::RootAgent,
        dto::ApiPurposeDto::SubAgent => ApiPurpose::SubAgent,
        dto::ApiPurposeDto::Memory => ApiPurpose::Memory,
        dto::ApiPurposeDto::Compaction => ApiPurpose::Compaction,
    }
}

pub(crate) fn session_error_from_control(error: SessionControlError) -> dto::SessionRpcError {
    match error {
        SessionControlError::SessionClosed(_) => dto::SessionRpcError::SessionClosed,
        SessionControlError::NotAwaitingInput(_) => dto::SessionRpcError::NotAwaitingInput,
        SessionControlError::InputRequestMismatch { .. } => {
            dto::SessionRpcError::InputRequestMismatch
        }
        SessionControlError::WorkerStopped => dto::SessionRpcError::WorkerStopped,
    }
}
