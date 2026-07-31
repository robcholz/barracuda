use serde::Serialize;
use serde_json::Value;

use super::super::{block_order, Context};
use crate::{Band, BlockKind, Scope};

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
pub(super) struct ContextSnapshot {
    schema_version: u32,
    event: &'static str,
    sequence: u64,
    content_version: u64,
    system: ObservedSystem,
    history: ObservedHistory,
    reminders: ObservedReminders,
}

#[derive(Serialize)]
struct ObservedSystem {
    blocks: Vec<ObservedText>,
    rendered: String,
    bytes: usize,
}

#[derive(Serialize)]
struct ObservedHistory {
    value: Value,
}

#[derive(Serialize)]
struct ObservedReminders {
    rendered: Vec<Value>,
}

#[derive(Serialize)]
struct ObservedText {
    kind: ObservedKind,
    content: String,
    bytes: usize,
}

#[derive(Serialize)]
struct ObservedKind {
    name: &'static str,
    label: Option<String>,
    band: &'static str,
    scope: &'static str,
    order: u16,
}

impl ContextSnapshot {
    pub(super) fn capture(context: Context, sequence: u64, history: Value) -> Self {
        let mut blocks = context.blocks.iter().collect::<Vec<_>>();
        blocks.sort_by(|left, right| block_order(left.0, right.0));
        let blocks = blocks
            .into_iter()
            .map(|(kind, content)| observed_text(kind, content.trim()))
            .collect();

        Self {
            schema_version: SCHEMA_VERSION,
            event: "context_snapshot",
            sequence,
            content_version: context.content_version,
            system: ObservedSystem {
                blocks,
                rendered: context.rendered.clone(),
                bytes: context.rendered.len(),
            },
            history: ObservedHistory { value: history },
            reminders: ObservedReminders {
                rendered: context.reminders.as_slice().to_vec(),
            },
        }
    }
}

fn observed_text(kind: &BlockKind, content: &str) -> ObservedText {
    ObservedText {
        kind: observed_kind(kind),
        content: content.to_owned(),
        bytes: content.len(),
    }
}

fn observed_kind(kind: &BlockKind) -> ObservedKind {
    let (name, label) = match kind {
        BlockKind::AgentInstruction => ("agent_instruction", None),
        BlockKind::StaticTools => ("static_tools", None),
        BlockKind::ModePolicy => ("mode_policy", None),
        BlockKind::Soul => ("soul", None),
        BlockKind::AssistantIdentity => ("assistant_identity", None),
        BlockKind::UserProfile => ("user_profile", None),
        BlockKind::GlobalMemory => ("global_memory", None),
        BlockKind::SessionContext => ("session_context", None),
        BlockKind::SessionMemory => ("session_memory", None),
        BlockKind::AgentMemory => ("agent_memory", None),
        BlockKind::SkillList => ("skill_list", None),
        BlockKind::ReasoningEffort => ("reasoning_effort", None),
        BlockKind::ConversationSummary => ("conversation_summary", None),
        BlockKind::DeferredTools => ("deferred_tools", None),
        BlockKind::ActiveMode => ("active_mode", None),
        BlockKind::ToolReminder => ("tool_reminder", None),
        BlockKind::RecentContext => ("recent_context", None),
        BlockKind::OutputContract => ("output_contract", None),
        BlockKind::Custom { label, .. } => ("custom", Some(label.to_string())),
    };
    ObservedKind {
        name,
        label,
        band: observed_band(kind.band()),
        scope: observed_scope(kind.scope()),
        order: kind.sort_key().2,
    }
}

fn observed_band(band: Band) -> &'static str {
    match band {
        Band::Static => "static",
        Band::Durable => "durable",
        Band::Volatile => "volatile",
    }
}

fn observed_scope(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::Session => "session",
        Scope::Agent => "agent",
        Scope::Conversation => "conversation",
        Scope::Turn => "turn",
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::{json, Value};

    use super::ContextSnapshot;
    use crate::{Block, BlockKind, Context};

    #[test]
    fn snapshot_keeps_wire_order_and_raw_channels() -> Result<(), Box<dyn std::error::Error>> {
        let mut context = Context::new();
        context
            .with(Block::new(BlockKind::OutputContract, "  output  "))
            .with(Block::new(BlockKind::AgentInstruction, " instruction "));
        context.with_reminder(BlockKind::ToolReminder, Some("use one tool"));
        context.rebuild();
        context.rendered_version = context.content_version;
        context.reminders.refresh();

        let history = json!([{ "role": "user", "content": "hello" }]);
        let snapshot = ContextSnapshot::capture(context, 3, history);
        let value = serde_json::to_value(snapshot)?;

        let first_kind = value
            .pointer("/system/blocks/0/kind/name")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("missing first block kind"))?;
        let second_kind = value
            .pointer("/system/blocks/1/kind/name")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("missing second block kind"))?;
        assert_eq!(first_kind, "agent_instruction");
        assert_eq!(second_kind, "output_contract");
        assert_eq!(
            value.pointer("/system/rendered"),
            Some(&json!("instruction\n\noutput"))
        );
        assert_eq!(
            value.pointer("/reminders/rendered/0/content"),
            Some(&json!(
                "<system-reminder>\nuse one tool\n</system-reminder>"
            ))
        );
        Ok(())
    }
}
