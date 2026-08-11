use claw_api::{BackendKind, ChatRequest, ClawApi, ClawApiConfig};
use claw_net::testing::{ScriptStep, ScriptedStack};
use claw_utils::Cancel;
use futures_lite::future::block_on;
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#,
    )]);
    let mut api = ClawApi::new(&stack, 4096, 512);
    api.set_config(ClawApiConfig::new(
        BackendKind::OpenAiCompatible,
        "key",
        "model",
        "http://llm.test/v1",
    ))?;
    let messages = [json!({"role":"user","content":"hi"})];
    let response = block_on(api.chat(&ChatRequest::new("be concise", &messages), Cancel::never()))?;
    println!("{:?}", response.text);
    Ok(())
}
