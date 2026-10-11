use alloc::string::String;
use alloc::vec::Vec;

use barracuda_workflow_plugin::Event;
use gateway::MediaKind;
use serde::{Deserialize, Serialize};

use crate::json::valid_required;
use crate::route::GatewayRoute;

/// Gateway-owned logical inbound message published by channel providers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayInboundMessage {
    /// Origin route and conversation identity.
    pub route: GatewayRoute,
    /// Provider-assigned message identifier.
    pub message_id: String,
    /// User-visible message text; empty for a message of attachments only.
    pub text: String,
    /// Files the user sent with the message, already stored by the Gateway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// Reference to one file the Gateway stored or can send.
///
/// The bytes stay in the file; only this reference travels through
/// Workflow, Events, and transcripts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Attachment {
    /// How a channel presents the file.
    pub kind: MediaKind,
    /// Path of the file in the Plugins' filesystem view, such as
    /// `/workspace/cache/gateway/telegram/…`.
    pub path: String,
    /// MIME type of the content.
    pub mime_type: String,
    /// Name the sender gave the file, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Length of the file in bytes.
    pub size: u64,
}

/// Workflow Event emitted for normalized inbound message data.
pub struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
}

pub(crate) fn valid_inbound(message: &GatewayInboundMessage) -> bool {
    valid_required(&message.route.channel)
        && valid_required(&message.route.conversation_id)
        && valid_required(&message.message_id)
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use gateway::MediaKind;
    use json_validator::{validator, Validator};
    use serde_json::json;

    use super::{Attachment, GatewayInboundMessage};
    use crate::GatewayRoute;

    const EVENT: Validator = validator!("../../schemas/event/gateway.message.received.json");

    fn message(text: &str, attachments: Vec<Attachment>) -> GatewayInboundMessage {
        GatewayInboundMessage {
            route: GatewayRoute::new("telegram", "chat-42"),
            message_id: String::from("m1"),
            text: String::from(text),
            attachments,
        }
    }

    #[test]
    fn a_text_message_omits_attachments_from_the_event() {
        let value = serde_json::to_value(message("hello", Vec::new())).ok();

        assert_eq!(
            value,
            Some(json!({
                "route": {"channel": "telegram", "conversation_id": "chat-42"},
                "message_id": "m1",
                "text": "hello"
            }))
        );
        assert!(value.is_some_and(|value| EVENT.validate(&value).is_ok()));
    }

    #[test]
    fn attachments_serialize_as_references_the_event_schema_accepts() {
        let value = serde_json::to_value(message(
            "",
            vec![
                Attachment {
                    kind: MediaKind::Image,
                    path: String::from("/workspace/cache/gateway/telegram/1a2b3c4d-1.jpg"),
                    mime_type: String::from("image/jpeg"),
                    filename: Some(String::from("photo.jpg")),
                    size: 1234,
                },
                Attachment {
                    kind: MediaKind::Audio,
                    path: String::from("/workspace/cache/gateway/telegram/1a2b3c4d-2.ogg"),
                    mime_type: String::from("audio/ogg"),
                    filename: None,
                    size: 0,
                },
            ],
        ))
        .ok();

        assert_eq!(
            value,
            Some(json!({
                "route": {"channel": "telegram", "conversation_id": "chat-42"},
                "message_id": "m1",
                "text": "",
                "attachments": [
                    {
                        "kind": "image",
                        "path": "/workspace/cache/gateway/telegram/1a2b3c4d-1.jpg",
                        "mime_type": "image/jpeg",
                        "filename": "photo.jpg",
                        "size": 1234
                    },
                    {
                        "kind": "audio",
                        "path": "/workspace/cache/gateway/telegram/1a2b3c4d-2.ogg",
                        "mime_type": "audio/ogg",
                        "size": 0
                    }
                ]
            }))
        );
        assert!(value.is_some_and(|value| EVENT.validate(&value).is_ok()));
    }

    #[test]
    fn the_event_schema_rejects_an_attachment_without_its_path() {
        let event = json!({
            "route": {"channel": "telegram", "conversation_id": "chat-42"},
            "message_id": "m1",
            "text": "",
            "attachments": [{"kind": "image", "mime_type": "image/jpeg", "size": 1}]
        });

        assert!(EVENT.validate(&event).is_err());
    }

    #[test]
    fn a_message_without_attachments_decodes_with_none() {
        let decoded: Result<GatewayInboundMessage, _> = serde_json::from_value(json!({
            "route": {"channel": "telegram", "conversation_id": "chat-42"},
            "message_id": "m1",
            "text": "hello"
        }));

        assert_eq!(decoded.ok(), Some(message("hello", Vec::new())));
    }
}
