use serde_json::{Value, json};

use crate::state::{BridgeError, GatewayTarget, ResolveResult};

pub(crate) fn resolve_response(result: ResolveResult) -> Value {
    match result {
        ResolveResult::Missing => json!({}),
        ResolveResult::Found {
            session,
            open_required,
        } => json!({
            "open_required": open_required,
            "session": session,
        }),
    }
}

pub(crate) fn bound_response(session: &str) -> Value {
    json!({"session": session})
}

pub(crate) fn gateway_target_response(target: Option<GatewayTarget>) -> Value {
    match target {
        Some(target) => {
            let mut route = json!({
                "channel": target.route.channel,
                "conversation_id": target.route.conversation_id,
            });
            if let Some(thread_id) = target.route.thread_id
                && let Some(object) = route.as_object_mut()
            {
                object.insert("thread_id".into(), Value::String(thread_id));
            }
            json!({
                "route": route,
                "reply_to": target.reply_to,
            })
        }
        None => json!({}),
    }
}

pub(crate) fn error_response(error: BridgeError) -> Value {
    json!({"error": error.code()})
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{gateway_target_response, resolve_response};
    use crate::state::ResolveResult;

    #[test]
    fn response_shapes_use_json_variants_without_control_flags() {
        assert_eq!(resolve_response(ResolveResult::Missing), json!({}));
        assert_eq!(
            resolve_response(ResolveResult::Found {
                session: "session-4".into(),
                open_required: true,
            }),
            json!({"open_required":true,"session":"session-4"})
        );
        assert_eq!(gateway_target_response(None), json!({}));
    }
}
