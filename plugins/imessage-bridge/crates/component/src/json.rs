use alloc::string::String;

use serde_json::{Value, json};

use crate::state::{BridgeError, GatewayCommand, ResolveResult};

pub(crate) fn resolve_response(result: ResolveResult) -> Value {
    match result {
        ResolveResult::Missing => json!({"found": false, "open_required": false}),
        ResolveResult::Found {
            session,
            open_required,
        } => json!({
            "found": true,
            "open_required": open_required,
            "session": session,
        }),
    }
}

pub(crate) fn bound_response(session: &str) -> Value {
    json!({"session": session})
}

pub(crate) fn forwarding_response(command_id: Option<&str>) -> Value {
    match command_id {
        Some(command_id) => json!({"forward": true, "command_id": command_id}),
        None => json!({"forward": false}),
    }
}

pub(crate) fn error_response(error: BridgeError) -> Value {
    json!({"error": error.code()})
}

pub(crate) fn command_response(command: GatewayCommand) -> Value {
    match command {
        GatewayCommand::Start {
            stream_id,
            route,
            reply_to,
        } => {
            let mut response = json!({
                "action": "start",
                "stream_id": stream_id,
                "sequence": 0,
                "channel": route.channel,
                "conversation_id": route.conversation_id,
                "reply_to": reply_to,
            });
            if let Some(thread_id) = route.thread_id
                && let Some(object) = response.as_object_mut()
            {
                object.insert(String::from("thread_id"), Value::String(thread_id));
            }
            response
        }
        GatewayCommand::Chunk {
            stream_id,
            sequence,
            boundary,
            text,
        } => json!({
            "action": "chunk",
            "stream_id": stream_id,
            "sequence": sequence,
            "field": "text",
            "boundary": boundary,
            "text": text,
        }),
        GatewayCommand::Finish {
            stream_id,
            sequence,
        } => json!({
            "action": "finish",
            "stream_id": stream_id,
            "sequence": sequence,
        }),
    }
}
