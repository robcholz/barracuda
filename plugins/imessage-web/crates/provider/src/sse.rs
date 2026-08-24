use alloc::{format, string::String};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use gateway::{SendStreamField, StreamBoundary};

use crate::{MediaPhase, WebDelivery, WebEventData};

/// Failure while serializing a Web event as an SSE frame.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    #[error("could not serialize SSE event: {0}")]
    Json(#[from] serde_json::Error),
}

impl WebDelivery {
    /// Serialize one complete Server-Sent Events frame.
    pub fn to_sse(&self) -> Result<String, SseError> {
        match self {
            Self::Lagged { missed } => Ok(format!(
                "event: stream.lagged\ndata: {}\n\n",
                serde_json::to_string(&json!({ "missed": missed }))?
            )),
            Self::Event(event) => {
                let payload = event_payload(&event.data);
                Ok(format!(
                    "id: {}\nevent: {}\ndata: {}\n\n",
                    event.id,
                    event.data.event_name(),
                    serde_json::to_string(&payload)?
                ))
            }
        }
    }
}

fn event_payload(data: &WebEventData) -> Value {
    match data {
        WebEventData::MessageStart {
            message_id,
            reply_to,
            kind,
        } => json!({ "message_id": message_id, "reply_to": reply_to, "kind": kind }),
        WebEventData::MessageDelta { message_id, delta } => {
            json!({ "message_id": message_id, "delta": delta })
        }
        WebEventData::MessageExtra {
            message_id,
            field,
            boundary,
            content,
        } => json!({
            "message_id": message_id,
            "field": stream_field_name(*field),
            "boundary": boundary_name(*boundary),
            "content": content,
        }),
        WebEventData::MessageEnd { message_id, error } => {
            json!({ "message_id": message_id, "error": error })
        }
        WebEventData::Media {
            message_id, phase, ..
        } => match phase {
            MediaPhase::Start {
                filename,
                mime_type,
                caption,
                reply_to,
            } => json!({
                "message_id": message_id,
                "phase": "start",
                "filename": filename,
                "mime_type": mime_type,
                "caption": caption,
                "reply_to": reply_to,
            }),
            MediaPhase::Delta { bytes } => json!({
                "message_id": message_id,
                "phase": "delta",
                "data": STANDARD.encode(bytes),
            }),
            MediaPhase::End { error } => json!({
                "message_id": message_id,
                "phase": "end",
                "error": error,
            }),
        },
        WebEventData::MessageEdit { message_id, text } => {
            json!({ "message_id": message_id, "text": text })
        }
        WebEventData::MessageDelete { message_id } => json!({ "message_id": message_id }),
        WebEventData::MessageReaction {
            message_id,
            reaction,
        } => json!({ "message_id": message_id, "reaction": reaction }),
        WebEventData::ConversationTyping { typing } => json!({ "typing": typing }),
    }
}

const fn boundary_name(boundary: StreamBoundary) -> &'static str {
    match boundary {
        StreamBoundary::More => "more",
        StreamBoundary::Complete => "complete",
    }
}

const fn stream_field_name(field: SendStreamField) -> &'static str {
    match field {
        SendStreamField::Text => "text",
        SendStreamField::Reasoning => "reasoning",
        SendStreamField::EffectResult => "effect_result",
        SendStreamField::Notice => "notice",
        SendStreamField::Event => "event",
        SendStreamField::ToolResultStart => "tool_result_start",
        SendStreamField::ToolCallId => "tool_call_id",
        SendStreamField::ToolName => "tool_name",
        SendStreamField::ToolArguments => "tool_arguments",
        SendStreamField::ToolOutput => "tool_output",
        SendStreamField::ToolSucceeded => "tool_succeeded",
        SendStreamField::ToolFailed => "tool_failed",
        SendStreamField::ToolResultEnd => "tool_result_end",
    }
}
