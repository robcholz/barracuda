use barracuda_model_api::{BackendKind, ChatRequest, ModelApi, ModelApiConfig};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use barracuda_runtime_utils::Cancel;
use futures_lite::future::block_on;
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#,
    )]);
    let mut api = ModelApi::new(&stack, 4096, 512);
    api.set_config(ModelApiConfig::new(
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
