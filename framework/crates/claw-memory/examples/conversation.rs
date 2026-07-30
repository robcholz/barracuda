//! Drive a [`TranscriptStore`] through a few turns and inspect what the model
//! would see.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p claw-memory --example conversation --target x86_64-unknown-linux-gnu
//! ```
//!
//! The store is pure storage — no summarization, no LLM. Persistence is an
//! in-memory [`MemFs`]; on device the same code runs over the DATA root.

use std::sync::Arc;

use claw_interface::MemFs;
use claw_memory::{AssistantFragment, TranscriptStore};

fn main() -> anyhow::Result<()> {
    let conversation_id = 42;
    let filesystem = Arc::new(MemFs::new());
    let store = TranscriptStore::<MemFs>::new(filesystem, conversation_id, "/data/conversations")?;

    // One handle owns the turn and commits it as one record on drop.
    {
        let turn = store.open_turn()?;
        {
            let mut user = turn.user()?;
            user.append("what's the weather in Shanghai?");
        }
        {
            let mut assistant = turn.assistant()?;
            assistant.append(AssistantFragment::Content("Let me check."));
            assistant.append(AssistantFragment::ToolCall(serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "weather",
                    "arguments": "{\"city\":\"Shanghai\"}",
                },
            })));
        }
        {
            let mut tool = turn.tool("call_1", false)?;
            tool.append(r#"{"temp_c":21,"sky":"clear"}"#);
        }
    }

    {
        let turn = store.open_turn()?;
        {
            let mut user = turn.user()?;
            user.append("and tomorrow?");
        }
        {
            let mut assistant = turn.assistant()?;
            assistant.append(AssistantFragment::Content("Sunny, "));
            assistant.append(AssistantFragment::Content("around 23C."));
        }
    }

    // `turns()` is the read surface — committed turns plus any open one. The
    // full verbatim transcript you feed to the model is its messages flattened.
    let turns = store.turns();
    let messages: Vec<&serde_json::Value> = turns.iter().flat_map(|t| &t.messages).collect();
    println!(
        "conversation has {} message(s) to send to the model:\n",
        messages.len()
    );
    println!("{}", serde_json::to_string_pretty(&messages)?);

    // Each turn was persisted when its handle dropped.
    Ok(())
}
