#![allow(clippy::unwrap_used)]

use barracuda_model_api::{BackendKind, MediaAsset, MediaRequest, ModelApi, ModelApiConfig};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use barracuda_runtime_utils::Cancel;
use futures_lite::future::block_on;
use http_client::Client;

fn configured<'a>(stack: &'a ScriptedStack) -> ModelApi<'a> {
    let mut api = ModelApi::new(Client::from_network_with_buffer_sizes(
        stack, stack, 4096, 512,
    ));
    api.set_config(ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "secret",
        "vision-model",
        "http://llm.test/v1",
    ))
    .unwrap();
    api
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
