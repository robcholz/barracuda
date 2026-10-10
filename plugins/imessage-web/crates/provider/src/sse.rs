use barracuda_bulk_memory::{BulkAllocError, BulkText};
use barracuda_json_writer::{
    write_base64_str, write_compact, write_str, write_value, Object, Sink,
};
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::{MediaPhase, WebDelivery, WebEventData};

/// Failure while serializing a Web event as an SSE frame.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    #[error("could not serialize SSE event: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not enough memory for an SSE frame")]
    OutOfMemory(#[from] BulkAllocError),
}

impl WebDelivery {
    /// Encodes one complete Server-Sent Events frame into exactly sized bulk
    /// text.
    ///
    /// Fields are written in key order, matching `serde_json`'s encoding of
    /// the equivalent object; a semantic event's payload is copied as its
    /// compact JSON text instead of being decoded and encoded again.
    pub fn to_sse(&self) -> Result<BulkText, SseError> {
        match self {
            Self::Lagged { missed } => Ok(BulkText::try_encode(|sink| {
                sink.put(b"event: stream.lagged\ndata: ");
                let mut object = Object::begin(sink);
                object.value("missed", &Value::from(*missed));
                object.end();
                sink.put(b"\n\n");
            })?),
            Self::Event(event) => {
                let scalars = Scalars::of(&event.data)?;
                let id = Value::from(event.id);
                Ok(BulkText::try_encode(|sink| {
                    sink.put(b"id: ");
                    write_value(sink, &id);
                    sink.put(b"\nevent: ");
                    sink.put(event.data.event_name().as_bytes());
                    sink.put(b"\ndata: ");
                    write_payload(sink, &event.data, &scalars);
                    sink.put(b"\n\n");
                })?)
            }
        }
    }
}

/// Non-string field values encoded once before the frame is measured.
struct Scalars {
    kind: Value,
}

impl Scalars {
    fn of(data: &WebEventData) -> Result<Self, serde_json::Error> {
        let kind = match data {
            WebEventData::MessageStart { kind, .. } => serde_json::to_value(kind)?,
            WebEventData::MessageEvent { event, .. } => {
                serde_json::from_str::<IgnoredAny>(event.payload.as_str())?;
                Value::Null
            }
            _ => Value::Null,
        };
        Ok(Self { kind })
    }
}

fn write_payload(sink: &mut dyn Sink, data: &WebEventData, scalars: &Scalars) {
    if let WebEventData::ConversationSessions { json } = data {
        return write_compact(sink, json);
    }
    let mut object = Object::begin(sink);
    match data {
        WebEventData::MessageStart {
            message_id,
            reply_to,
            ..
        } => {
            object.value("kind", &scalars.kind);
            object.str("message_id", message_id);
            optional_str(object.field("reply_to"), reply_to.as_deref());
        }
        WebEventData::MessageDelta { message_id, delta } => {
            object.str("delta", delta.as_str());
            object.str("message_id", message_id);
        }
        WebEventData::MessageEvent { message_id, event } => {
            object.str("message_id", message_id);
            write_compact(object.field("payload"), event.payload.as_str());
            object.value("sequence", &Value::from(event.sequence));
            object.str("session", &event.session);
            object.str("type", &event.event_type);
        }
        WebEventData::MessageEnd { message_id, error } => {
            optional_str(object.field("error"), error.as_deref());
            object.str("message_id", message_id);
        }
        WebEventData::Media {
            message_id, phase, ..
        } => match phase {
            MediaPhase::Start {
                filename,
                mime_type,
                caption,
                reply_to,
            } => {
                optional_str(object.field("caption"), caption.as_deref());
                optional_str(object.field("filename"), filename.as_deref());
                object.str("message_id", message_id);
                optional_str(object.field("mime_type"), mime_type.as_deref());
                object.str("phase", "start");
                optional_str(object.field("reply_to"), reply_to.as_deref());
            }
            MediaPhase::Delta { bytes } => {
                write_base64_str(object.field("data"), "", bytes);
                object.str("message_id", message_id);
                object.str("phase", "delta");
            }
            MediaPhase::End { error } => {
                optional_str(object.field("error"), error.as_deref());
                object.str("message_id", message_id);
                object.str("phase", "end");
            }
        },
        WebEventData::MessageEdit { message_id, text } => {
            object.str("message_id", message_id);
            object.str("text", text);
        }
        WebEventData::MessageDelete { message_id } => object.str("message_id", message_id),
        WebEventData::MessageReaction {
            message_id,
            reaction,
        } => {
            object.str("message_id", message_id);
            object.str("reaction", reaction);
        }
        WebEventData::ConversationTyping { typing } => {
            object.value("typing", &Value::Bool(*typing));
        }
        WebEventData::ConversationSessions { .. } => {}
    }
    object.end();
}

fn optional_str(sink: &mut dyn Sink, text: Option<&str>) {
    match text {
        Some(text) => write_str(sink, text),
        None => sink.put(b"null"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alloc::rc::Rc;
    use alloc::string::String;
    use alloc::{format, vec};

    use barracuda_imessage_gateway_plugin::{MediaKind, MessageKind, SendStreamEvent};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::json;

    use super::*;
    use crate::WebEvent;

    /// The `serde_json` encoding the direct writer replaces.
    fn reference(event: &WebEvent) -> String {
        let payload = match &event.data {
            WebEventData::MessageStart {
                message_id,
                reply_to,
                kind,
            } => json!({ "message_id": message_id, "reply_to": reply_to, "kind": kind }),
            WebEventData::MessageDelta { message_id, delta } => {
                json!({ "message_id": message_id, "delta": delta.as_str() })
            }
            WebEventData::MessageEvent { message_id, event } => json!({
                "message_id": message_id,
                "session": event.session,
                "sequence": event.sequence,
                "type": event.event_type,
                "payload": serde_json::from_str::<Value>(event.payload.as_str()).unwrap(),
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
                    "message_id": message_id, "phase": "start", "filename": filename,
                    "mime_type": mime_type, "caption": caption, "reply_to": reply_to,
                }),
                MediaPhase::Delta { bytes } => json!({
                    "message_id": message_id, "phase": "delta", "data": STANDARD.encode(bytes),
                }),
                MediaPhase::End { error } => {
                    json!({ "message_id": message_id, "phase": "end", "error": error })
                }
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
            WebEventData::ConversationSessions { json } => serde_json::from_str(json).unwrap(),
        };
        format!(
            "id: {}\nevent: {}\ndata: {}\n\n",
            event.id,
            event.data.event_name(),
            serde_json::to_string(&payload).unwrap()
        )
    }

    fn event(id: u64, data: WebEventData) -> WebEvent {
        WebEvent {
            id,
            conversation_id: "chat".into(),
            thread_id: None,
            data,
        }
    }

    #[test]
    fn frames_match_the_serde_json_encoding() {
        let tricky = String::from("quote \" slash \\ newline \n tab \t ünï 🚀 \u{1}");
        let events = [
            WebEventData::MessageStart {
                message_id: tricky.clone(),
                reply_to: None,
                kind: MessageKind::Reasoning,
            },
            WebEventData::MessageStart {
                message_id: "m".into(),
                reply_to: Some(tricky.clone()),
                kind: MessageKind::Reply,
            },
            WebEventData::MessageDelta {
                message_id: "m".into(),
                delta: tricky.as_str().into(),
            },
            WebEventData::MessageEvent {
                message_id: "m".into(),
                event: SendStreamEvent::new(
                    "session-1",
                    u64::MAX,
                    "tool_result",
                    // Producers encode payloads with `serde_json`, so keys arrive sorted.
                    r#"{"b":null,"c":"ü","z":[1,2.5,{"a":"x\ny"}]}"#,
                ),
            },
            WebEventData::MessageEnd {
                message_id: "m".into(),
                error: Some(tricky.clone()),
            },
            WebEventData::Media {
                message_id: "m".into(),
                kind: MediaKind::Image,
                phase: MediaPhase::Start {
                    filename: Some("a.png".into()),
                    mime_type: None,
                    caption: Some(tricky.clone()),
                    reply_to: None,
                },
            },
            WebEventData::Media {
                message_id: "m".into(),
                kind: MediaKind::File,
                phase: MediaPhase::Delta {
                    bytes: vec![0, 1, 2, 250, 251],
                },
            },
            WebEventData::Media {
                message_id: "m".into(),
                kind: MediaKind::Audio,
                phase: MediaPhase::End { error: None },
            },
            WebEventData::MessageEdit {
                message_id: "m".into(),
                text: tricky.clone(),
            },
            WebEventData::MessageDelete {
                message_id: "m".into(),
            },
            WebEventData::MessageReaction {
                message_id: "m".into(),
                reaction: "👍".into(),
            },
            WebEventData::ConversationTyping { typing: true },
            WebEventData::ConversationSessions {
                json: r#"{"current":"session-2","now":null,"sessions":[]}"#.into(),
            },
        ];
        for (id, data) in events.into_iter().enumerate() {
            let event = event(id as u64, data);
            let expected = reference(&event);
            let frame = WebDelivery::Event(Rc::new(event)).to_sse().unwrap();
            assert_eq!(frame.as_str(), expected);
        }
        let lagged = WebDelivery::Lagged { missed: 7 }.to_sse().unwrap();
        assert_eq!(
            lagged.as_str(),
            "event: stream.lagged\ndata: {\"missed\":7}\n\n"
        );
    }

    #[test]
    fn payloads_are_copied_as_compact_text() {
        let event = event(
            1,
            WebEventData::MessageEvent {
                message_id: "m".into(),
                event: SendStreamEvent::new("s", 1, "t", "{ \"z\" : 1,\n \"a\" : \"x y\" }"),
            },
        );
        let frame = WebDelivery::Event(Rc::new(event)).to_sse().unwrap();
        assert!(frame.contains(r#""payload":{"z":1,"a":"x y"}"#));
    }

    #[test]
    fn invalid_payloads_are_rejected() {
        let event = event(
            1,
            WebEventData::MessageEvent {
                message_id: "m".into(),
                event: SendStreamEvent::new("s", 1, "t", "{not json"),
            },
        );
        assert!(matches!(
            WebDelivery::Event(Rc::new(event)).to_sse(),
            Err(SseError::Json(_))
        ));
    }
}
