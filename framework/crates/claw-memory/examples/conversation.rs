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

use claw_interface::MemFs;
use claw_memory::{AssistantFinish, TranscriptStore};

fn main() -> anyhow::Result<()> {
    let conversation_id = 42;
    MemFs::new();
    let store = TranscriptStore::<MemFs>::new(conversation_id, "/data/conversations")?;

    // One handle owns the turn and commits it as one record on drop.
    {
        let mut turn = store.open_turn()?;
        turn.append_user("what's the weather in Shanghai?")?;
        turn.finish_user()?;
        turn.finish_assistant(AssistantFinish::RawJson(
            r#"{"role":"assistant","content":"Let me check.","tool_calls":[{"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Shanghai\"}"}}]}"#,
        ))?;
        turn.record_tool_result("call_1", r#"{"temp_c":21,"sky":"clear"}"#, false)?;
    }

    {
        let mut turn = store.open_turn()?;
        turn.append_user("and tomorrow?")?;
        turn.finish_user()?;
        turn.append_assistant("Sunny, ")?;
        turn.append_assistant("around 23C.")?;
        turn.finish_assistant(AssistantFinish::PlainText("Sunny, around 23C."))?;
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

    // Persistence is automatic: the store flushes any debounced writes when it
    // is dropped at the end of `main`.
    Ok(())
}
