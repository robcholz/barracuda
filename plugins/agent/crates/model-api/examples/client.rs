use barracuda_model_api::{BackendKind, ChatMessage, ChatRequest, ModelApi, ModelApiConfig};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use barracuda_runtime_utils::Cancel;
use futures_lite::future::block_on;
use http_client::ClientFactory;
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#,
    )]);
    let mut api = ModelApi::new(ClientFactory::from_network(&stack, &stack));
    api.set_config(ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "key",
        "model",
        "http://llm.test/v1",
    ))?;
    let messages = [ChatMessage::new(&json!({"role":"user","content":"hi"}))];
    let response = block_on(api.chat(&ChatRequest::new("be concise", &messages), Cancel::never()))?;
    println!("{:?}", response.text);
    Ok(())
}
