//! A read-only view of an Agent's conversation for people: what was said,
//! without reasoning, Tool results, or the runtime's own messages.

use alloc::{borrow::Cow, string::String, vec::Vec};

use barracuda_agent_memory::Turn;
use serde::Deserialize;

/// Who said a [`TranscriptEntry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptRole {
    /// The person.
    User,
    /// The Agent.
    Assistant,
}

/// One message of a conversation as a person reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// Who said it.
    pub role: TranscriptRole,
    /// Its text, cut to the length the caller asked for; empty when an
    /// Agent message only called Tools.
    pub text: String,
    /// The Tools an Agent message called, by name.
    pub tools: Vec<String>,
    /// Whether it belongs to the turn still in progress.
    pub open: bool,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    role: Cow<'a, str>,
    #[serde(borrow, default)]
    content: Option<Content<'a>>,
    #[serde(borrow, default)]
    tool_calls: Vec<Call<'a>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content<'a> {
    Text(#[serde(borrow)] Cow<'a, str>),
    Parts(#[serde(borrow)] Vec<Part<'a>>),
}

#[derive(Deserialize)]
struct Part<'a> {
    #[serde(borrow, default)]
    text: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Call<'a> {
    #[serde(borrow)]
    function: Function<'a>,
}

#[derive(Deserialize)]
struct Function<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
}

/// The runtime's own messages to the Agent, such as background updates,
/// start with this.
const RUNTIME_MESSAGE: &str = "[background:";

/// The last `limit` entries of `turns`, each cut to `max_chars` characters.
pub(super) fn entries(turns: &[Turn], limit: usize, max_chars: usize) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for turn in turns {
        let open = turn.id.is_none();
        for message in &turn.messages {
            let Ok(message) = message.parse::<Message<'_>>() else {
                continue;
            };
            let role = match message.role.as_ref() {
                "user" => TranscriptRole::User,
                "assistant" => TranscriptRole::Assistant,
                _ => continue,
            };
            let text = match &message.content {
                Some(Content::Text(text)) => String::from(text.as_ref()),
                Some(Content::Parts(parts)) => parts
                    .iter()
                    .filter_map(|part| part.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => String::new(),
            };
            if role == TranscriptRole::User && text.starts_with(RUNTIME_MESSAGE) {
                continue;
            }
            let tools: Vec<String> = message
                .tool_calls
                .iter()
                .map(|call| String::from(call.function.name.as_ref()))
                .collect();
            let text = text.trim();
            if text.is_empty() && tools.is_empty() {
                continue;
            }
            entries.push(TranscriptEntry {
                role,
                text: text.chars().take(max_chars).collect(),
                tools,
                open,
            });
        }
    }
    let skip = entries.len().saturating_sub(limit);
    entries.drain(..skip);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use barracuda_agent_memory::TurnId;
    use barracuda_agent_message::ChatMessage;
    use serde_json::json;

    fn turn(open: bool, messages: &[serde_json::Value]) -> Turn {
        Turn {
            id: (!open).then(|| TurnId::new(1)),
            messages: messages.iter().map(ChatMessage::new).collect(),
        }
    }

    #[test]
    fn people_read_what_was_said() {
        let turns = [
            turn(
                false,
                &[
                    json!({"role": "user", "content": "打开灯"}),
                    json!({"role": "assistant", "reasoning_content": "...", "tool_calls": [
                        {"id": "1", "type": "function",
                         "function": {"name": "vm_run", "arguments": "{}"}}]}),
                    json!({"role": "tool", "tool_call_id": "1", "content": "ok", "is_error": false}),
                    json!({"role": "assistant", "content": "灯已经打开了。"}),
                ],
            ),
            turn(
                false,
                &[
                    json!({"role": "user", "content": "[background:updates] done"}),
                    json!({"role": "assistant", "content": "后台任务完成了。"}),
                ],
            ),
            turn(
                true,
                &[json!({"role": "user", "content": [
                    {"type": "text", "text": "看看这张"},
                    {"type": "media", "media": {"path": "/a.jpg"}}]})],
            ),
        ];
        let entries = entries(&turns, 10, 4);
        let said: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.role,
                    entry.text.as_str(),
                    entry.tools.len(),
                    entry.open,
                )
            })
            .collect();
        assert_eq!(
            said,
            [
                (TranscriptRole::User, "打开灯", 0, false),
                (TranscriptRole::Assistant, "", 1, false),
                (TranscriptRole::Assistant, "灯已经打", 0, false),
                (TranscriptRole::Assistant, "后台任务", 0, false),
                (TranscriptRole::User, "看看这张", 0, true),
            ]
        );
        assert_eq!(
            entries.get(1).map(|entry| entry.tools.clone()),
            Some(vec![String::from("vm_run")])
        );

        // Only the latest entries are kept.
        let last = super::entries(&turns, 2, 64);
        assert_eq!(last.len(), 2);
        assert_eq!(
            last.get(1).map(|entry| entry.text.as_str()),
            Some("看看这张")
        );
    }
}
