#![allow(clippy::unwrap_used)]

use barracuda_model_api::{BackendKind, Error, MediaAsset, MediaRequest, ModelApi, ModelApiConfig};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use barracuda_runtime_utils::Cancel;
use futures_lite::future::block_on;
use http_client::ClientFactory;

fn configured<'a>(stack: &'a ScriptedStack) -> ModelApi<'a, ScriptedStack, ScriptedStack> {
    configured_backend(stack, BackendKind::OpenAiCompatible)
}

fn configured_backend<'a>(
    stack: &'a ScriptedStack,
    backend: BackendKind,
) -> ModelApi<'a, ScriptedStack, ScriptedStack> {
    let mut api = ModelApi::new(ClientFactory::from_network(stack, stack));
    api.set_config(ModelApiConfig::new(
        backend,
        "secret",
        "vision-model",
        "http://llm.test/v1",
    ))
    .unwrap();
    api
}

fn request_body(stack: &ScriptedStack) -> serde_json::Value {
    let request = stack.requests().pop().expect("missing request");
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

#[test]
fn remote_image_uses_image_url_wire_shape() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"a dog"}}]}"#,
    )]);
    let mut api = configured(&stack);
    let asset = MediaAsset::remote_url("https://example.test/dog.png");
    let response = block_on(api.infer_media(
        &MediaRequest::new(&asset).with_user_prompt("describe"),
        Cancel::never(),
    ))
    .unwrap();
    assert_eq!(response, "a dog");
    assert!(stack.requests()[0].contains("https://example.test/dog.png"));
}

#[test]
fn inline_image_is_encoded_as_data_url() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"inline"}}]}"#,
    )]);
    let mut api = configured(&stack);
    let asset = MediaAsset::inline_bytes(vec![1, 2, 3], "image/png");
    block_on(api.infer_media(
        &MediaRequest::new(&asset).with_user_prompt("describe"),
        Cancel::never(),
    ))
    .unwrap();
    assert!(stack.requests()[0].contains("data:image/png;base64,AQID"));
}

#[test]
fn empty_inline_image_is_rejected_without_network() {
    let stack = ScriptedStack::new([]);
    let mut api = configured(&stack);
    let asset = MediaAsset::inline_bytes(Vec::new(), "image/png");
    assert!(block_on(api.infer_media(&MediaRequest::new(&asset), Cancel::never())).is_err());
    assert!(stack.requests().is_empty());
}

#[test]
fn anthropic_media_uses_inline_source_and_optional_system_prompt() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"content":[{"type":"text","text":"an icon"}]}"#,
    )]);
    let mut api = configured_backend(&stack, BackendKind::AnthropicCompatible);
    let asset = MediaAsset::inline_bytes(vec![1, 2, 3], "image/png");

    let response = block_on(
        api.infer_media(
            &MediaRequest::new(&asset)
                .with_system_prompt("be concise")
                .with_user_prompt("describe"),
            Cancel::never(),
        ),
    )
    .unwrap();

    assert_eq!(response, "an icon");
    let body = request_body(&stack);
    assert_eq!(body["system"], "be concise");
    assert_eq!(body["messages"][0]["content"][0]["text"], "describe");
    assert_eq!(
        body["messages"][0]["content"][1]["source"],
        serde_json::json!({
            "type":"base64",
            "media_type":"image/png",
            "data":"AQID"
        })
    );
}

#[test]
fn anthropic_media_rejects_remote_assets_and_missing_prompts_before_io() {
    let remote_stack = ScriptedStack::default();
    let mut remote_api = configured_backend(&remote_stack, BackendKind::AnthropicCompatible);
    let remote = MediaAsset::remote_url("https://example.test/image.png");
    let error = block_on(remote_api.infer_media(
        &MediaRequest::new(&remote).with_user_prompt("describe"),
        Cancel::never(),
    ))
    .unwrap_err();
    assert!(matches!(error, Error::RequiresLocalImage));
    assert!(remote_stack.requests().is_empty());

    let prompt_stack = ScriptedStack::default();
    let mut prompt_api = configured_backend(&prompt_stack, BackendKind::AnthropicCompatible);
    let inline = MediaAsset::inline_bytes(vec![1], "image/png");
    let error = block_on(prompt_api.infer_media(
        &MediaRequest::new(&inline).with_user_prompt(""),
        Cancel::never(),
    ))
    .unwrap_err();
    assert!(matches!(error, Error::IncompleteMediaRequest));
    assert!(prompt_stack.requests().is_empty());
}
