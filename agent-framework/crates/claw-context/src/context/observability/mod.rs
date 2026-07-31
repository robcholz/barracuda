//! Intrusive observation at the final [`Context::request`] boundary.
//!
//! This module intentionally sees raw prompt and history content. It is behind
//! an opt-in feature and performs no socket I/O on the request path. The first
//! `Context` starts the private WebSocket server once. When a client is
//! connected, `Context::request` clones its final context and history into a
//! bounded process-wide broadcast channel.

mod hub;
mod server;
mod snapshot;

use serde_json::Value;

use super::Context;

pub(super) fn ensure_server() {
    server::ensure_started();
}

pub(super) fn is_active() -> bool {
    hub::is_active()
}

pub(super) fn publish(context: Context, history: Value) {
    let snapshot = snapshot::ContextSnapshot::capture(context, hub::next_sequence(), history);
    match serde_json::to_string(&snapshot) {
        Ok(payload) => hub::publish(payload),
        Err(_error) => hub::record_serialization_failure(),
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::{json, Value};

    use super::hub;
    use crate::{Block, BlockKind, Context};

    #[test]
    fn request_boundary_publishes_cloned_context() -> Result<(), Box<dyn std::error::Error>> {
        const UNIQUE_SYSTEM: &str = "request-boundary-cloned-context";

        let mut snapshots = hub::subscribe();
        let mut context = Context::new();
        context.with(Block::new(BlockKind::AgentInstruction, UNIQUE_SYSTEM));
        let history = json!([{ "role": "user", "content": "hello" }]);

        let request = context.request(&history);
        assert_eq!(request.history(), &history);

        let snapshot = loop {
            let payload = snapshots.try_recv()?;
            let snapshot: Value = serde_json::from_str(&payload)?;
            if snapshot.pointer("/system/rendered").and_then(Value::as_str) == Some(UNIQUE_SYSTEM) {
                break snapshot;
            }
        };
        let event = snapshot
            .pointer("/event")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("missing snapshot event"))?;

        assert_eq!(event, "context_snapshot");
        assert_eq!(
            snapshot.pointer("/history/value"),
            Some(&json!([{ "role": "user", "content": "hello" }]))
        );
        Ok(())
    }
}
