//! Drive a [`TranscriptStore`] through a few turns and inspect what the model
//! would see.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p barracuda-agent-memory --example conversation --target x86_64-unknown-linux-gnu
//! ```
//!
//! The store is pure storage — no summarization, no LLM. Persistence is an
//! in-memory VFS; on device the same code runs over the plugin-private root.

use barracuda_agent_memory::{AssistantFragment, TranscriptStore};
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;

fn main() -> anyhow::Result<()> {
    block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let conversation_id = 42;
    let filesystem = memory_vfs().await?;
    let store = TranscriptStore::new(filesystem, conversation_id, "/data/conversations").await?;

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
    let messages: Vec<&barracuda_agent_memory::ChatMessage> =
        turns.iter().flat_map(|t| &t.messages).collect();
    println!(
        "conversation has {} message(s) to send to the model:\n",
        messages.len()
    );
    println!("{}", serde_json::to_string_pretty(&messages)?);

    store.flush().await?;
    Ok(())
}
