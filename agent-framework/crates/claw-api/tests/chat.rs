#![allow(clippy::unwrap_used)]

use claw_api::{
    BackendKind, ChatError, ChatJsonRequest, ChatRequest, ClawApi, ClawApiConfig, ClawApiError,
    RetryPolicy,
};
use claw_net::testing::{ScriptStep, ScriptedStack};
use claw_utils::{Cancel, CancellationFlag};
use embedded_io::ErrorKind;
use futures_lite::future::block_on;
use serde_json::{json, Value};

fn configured<'a>(stack: &'a ScriptedStack, backend: BackendKind) -> ClawApi<'a, ScriptedStack> {
    let mut api = ClawApi::new(stack, 4096, 512);
    api.set_config(ClawApiConfig::new(
        backend,
        "secret",
        "model-x",
        "http://llm.test/v1",
    ))
    .unwrap();
    api
}

fn request_body(stack: &ScriptedStack) -> Value {
    let request = stack.requests().pop().expect("missing request");
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

#[test]
fn chat_requires_configuration() {
    let stack = ScriptedStack::default();
    let mut api = ClawApi::new(&stack, 4096, 512);
    let messages = json!([{"role":"user","content":"hello"}]);
    let error =
        block_on(api.chat(&ChatRequest::new("system", &messages), Cancel::never())).unwrap_err();
    assert_eq!(error, ChatError::Api(ClawApiError::NotConfigured));
}

#[test]
fn openai_chat_uses_reqwless_wire_request() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#,
    )]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = json!([{"role":"user","content":"ping"}]);
    let response =
        block_on(api.chat(&ChatRequest::new("system", &messages), Cancel::never())).unwrap();
    assert_eq!(response.text.as_deref(), Some("pong"));

    let request = stack.requests().pop().unwrap();
    assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(request.contains("Authorization: Bearer secret\r\n"));
    let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["model"], "model-x");
}

#[test]
fn sequential_chats_reuse_one_connection() {
    let stack = ScriptedStack::new([
        ScriptStep::json(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"one"}}]}"#,
        ),
        ScriptStep::json(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"two"}}]}"#,
        ),
    ]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let first_messages = json!([{"role":"user","content":"first"}]);
    let second_messages = json!([{"role":"user","content":"second"}]);

    let first = block_on(api.chat(
        &ChatRequest::new("system", &first_messages),
        Cancel::never(),
    ))
    .unwrap();
    let second = block_on(api.chat(
        &ChatRequest::new("system", &second_messages),
        Cancel::never(),
    ))
    .unwrap();

    assert_eq!(first.text.as_deref(), Some("one"));
    assert_eq!(second.text.as_deref(), Some("two"));
    assert_eq!(stack.requests().len(), 2);
    assert_eq!(stack.connect_count(), 1);
}

#[test]
fn anthropic_chat_uses_provider_headers_and_shape() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#,
    )]);
    let mut api = configured(&stack, BackendKind::AnthropicCompatible);
    let messages = json!([{"role":"user","content":"hello"}]);
    let response =
        block_on(api.chat(&ChatRequest::new("system", &messages), Cancel::never())).unwrap();
    assert_eq!(response.text.as_deref(), Some("ok"));
    let request = stack.requests().pop().unwrap();
    assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
    assert!(request.contains("x-api-key: secret\r\n"));
    assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
}

#[test]
fn structured_chat_sends_schema_and_parses_output() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"{\"answer\":42}"}}]}"#,
    )]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = json!([{"role":"user","content":"answer"}]);
    let request = ChatJsonRequest::new("system", &messages)
        .with_output_schema("answer", r#"{"type":"object"}"#);
    let response: claw_api::ChatJsonResponse<Value> =
        block_on(api.chat_json(&request, Cancel::never())).unwrap();
    assert_eq!(response.output.unwrap()["answer"], 42);
    assert_eq!(
        request_body(&stack)["response_format"]["type"],
        "json_schema"
    );
}

#[test]
fn transient_connect_failure_is_retried_on_same_client() {
    let stack = ScriptedStack::new([
        ScriptStep::ConnectError(ErrorKind::ConnectionReset),
        ScriptStep::json(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"recovered"}}]}"#,
        ),
    ]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = json!([{"role":"user","content":"hello"}]);
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::fixed(1, 0));
    let response = block_on(api.chat(&request, Cancel::never())).unwrap();
    assert_eq!(response.text.as_deref(), Some("recovered"));
    assert_eq!(stack.remaining(), 0);
}

#[test]
fn timed_out_request_reconnects_before_retry() {
    let stack = ScriptedStack::new([
        ScriptStep::pending_after_headers(200, "application/json"),
        ScriptStep::json(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"recovered"}}]}"#,
        ),
    ]);
    let mut api = ClawApi::new(&stack, 4096, 512);
    let mut config = ClawApiConfig::new(
        BackendKind::OpenAiCompatible,
        "secret",
        "model-x",
        "http://llm.test/v1",
    );
    config.timeout_ms = 1;
    api.set_config(config).unwrap();
    let messages = json!([{"role":"user","content":"hello"}]);
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::fixed(1, 0));

    let response = block_on(api.chat(&request, Cancel::never())).unwrap();

    assert_eq!(response.text.as_deref(), Some("recovered"));
    assert_eq!(stack.connect_count(), 2);
}

#[test]
fn cancelled_request_does_not_touch_network() {
    let stack = ScriptedStack::new([ScriptStep::json(200, "{}")]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = json!([{"role":"user","content":"hello"}]);
    let cancelled = CancellationFlag::new();
    cancelled.cancel();
    let error = block_on(api.chat(
        &ChatRequest::new("system", &messages).with_retry(RetryPolicy::none()),
        Cancel::new(&cancelled),
    ))
    .unwrap_err();
    assert!(error.is_aborted());
    assert_eq!(stack.remaining(), 1);
}
