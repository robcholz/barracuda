#![allow(clippy::unwrap_used)]

use barracuda_model_api::{
    BackendKind, ChatRequest, Error, ModelApi, ModelApiConfig, RetryPolicy, StaticOutputSchema,
};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use barracuda_runtime_utils::{Cancel, CancellationFlag};
use embedded_io::ErrorKind;
use futures_lite::future::{block_on, poll_once};
use http_client::ClientFactory;
use serde_json::{json, Value};

fn configured<'a>(
    stack: &'a ScriptedStack,
    backend: BackendKind,
) -> ModelApi<'a, ScriptedStack, ScriptedStack> {
    let mut api = ModelApi::new(ClientFactory::from_network(stack, stack));
    api.set_config(ModelApiConfig::new(
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
    let mut api = ModelApi::new(ClientFactory::from_network(&stack, &stack));
    let messages = [json!({"role":"user","content":"hello"})];
    let error =
        block_on(api.chat(&ChatRequest::new("system", &messages), Cancel::never())).unwrap_err();
    assert!(matches!(error, Error::NotConfigured));
}

#[test]
fn openai_chat_uses_expected_wire_request() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#,
    )]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = [json!({"role":"user","content":"ping"})];
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
    let first_messages = [json!({"role":"user","content":"first"})];
    let second_messages = [json!({"role":"user","content":"second"})];

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
    let messages = [json!({"role":"user","content":"hello"})];
    let response =
        block_on(api.chat(&ChatRequest::new("system", &messages), Cancel::never())).unwrap();
    assert_eq!(response.text.as_deref(), Some("ok"));
    let request = stack.requests().pop().unwrap();
    assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
    assert!(request.contains("x-api-key: secret\r\n"));
    assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
}

#[test]
fn anthropic_chat_preserves_reasoning_tools_and_cross_segment_tool_results() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"content":[{"type":"thinking","thinking":"check"},{"type":"text","text":"result "},{"type":"text","text":"ready"},{"type":"tool_use","id":"call-out","name":"finish","input":{"ok":true}}]}"#,
    )]);
    let mut api = configured(&stack, BackendKind::AnthropicCompatible);
    let messages = [
        json!({"role":"system","content":"ignored message role"}),
        json!({"role":"user","content":"start"}),
        json!({
            "role":"assistant",
            "content":[
                {"type":"text","text":"working"},
                {"type":"unknown","value":"drop me"}
            ],
            "reasoning_content":"private thought",
            "tool_calls":[{
                "id":"call-1",
                "function":{"name":"lookup","arguments":"{\"key\":7}"}
            }]
        }),
        json!({"role":"tool","tool_call_id":"call-1","content":"first","is_error":true}),
    ];
    let reminders = [
        json!({"role":"tool","tool_call_id":"call-2","content":"second"}),
        json!({"role":"assistant","content":""}),
    ];
    let tools = serde_json::to_string(&json!([
        {
            "type":"function",
            "function":{
                "name":"lookup",
                "description":"find a value",
                "parameters":{"type":"object"}
            }
        },
        {"name":"finish","input_schema":{"type":"object"}},
        {"description":"missing name"}
    ]))
    .unwrap();
    let request = ChatRequest::new("system prompt", &messages)
        .with_reminders(&reminders)
        .with_tools(&tools);

    let response = block_on(api.chat(&request, Cancel::never())).unwrap();

    assert_eq!(response.text.as_deref(), Some("result ready"));
    assert_eq!(response.reasoning_content.as_deref(), Some("check"));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call-out");
    assert_eq!(response.tool_calls[0].name, "finish");
    assert_eq!(response.tool_calls[0].arguments_json, r#"{"ok":true}"#);

    let body = request_body(&stack);
    assert_eq!(body["system"], "system prompt");
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(body["messages"][1]["content"][2]["type"], "tool_use");
    assert_eq!(body["messages"][1]["content"][2]["input"]["key"], 7);
    assert_eq!(body["messages"][2]["content"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][2]["content"][0]["is_error"], true);
    assert_eq!(body["messages"][2]["content"][1]["is_error"], false);
    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(body["tool_choice"]["type"], "auto");
}

#[test]
fn anthropic_structured_chat_uses_native_schema_and_strict_tools() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"content":[{"type":"text","text":"{\"answer\":42}"}]}"#,
    )]);
    let mut api = configured(&stack, BackendKind::AnthropicCompatible);
    let messages = [json!({"role":"user","content":"answer"})];
    let tools = r#"[{"name":"calculator","description":"calculate"}]"#;
    let request = ChatRequest::new("", &messages).with_tools(tools);
    let schema = StaticOutputSchema {
        name: "answer",
        json: r#"{"type":"object","properties":{"answer":{"type":"integer"}}}"#,
    };

    let response: barracuda_model_api::ChatJsonResponse<Value> =
        block_on(api.chat_json(&request, schema, Cancel::never())).unwrap();

    assert_eq!(response.output.unwrap()["answer"], 42);
    let body = request_body(&stack);
    assert!(body.get("system").is_none());
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(
        body["output_config"]["format"]["schema"]["properties"]["answer"]["type"],
        "integer"
    );
    assert_eq!(body["tools"][0]["strict"], true);
    assert_eq!(body["tools"][0]["input_schema"], json!({}));
}

#[test]
fn anthropic_rejects_malformed_history_before_network_io() {
    let malformed_calls = [
        json!({"role":"assistant","tool_calls":[null]}),
        json!({"role":"assistant","tool_calls":[{"id":"","function":{"name":"x"}}]}),
        json!({"role":"assistant","tool_calls":[{"id":"id","function":{"name":""}}]}),
        json!({"role":"assistant","tool_calls":[{"id":"id","function":{"name":"x","arguments":"{"}}]}),
    ];

    for message in &malformed_calls {
        let stack = ScriptedStack::default();
        let mut api = configured(&stack, BackendKind::AnthropicCompatible);
        let error = block_on(api.chat(
            &ChatRequest::new("", core::slice::from_ref(message)),
            Cancel::never(),
        ))
        .unwrap_err();
        assert!(matches!(error, Error::Api(_)));
        assert!(stack.requests().is_empty());
    }
}

#[test]
fn anthropic_reports_distinct_malformed_response_failures() {
    let cases = [
        ("not-json", "parse"),
        (r#"{}"#, "missing"),
        (r#"{"content":[]}"#, "empty"),
        (
            r#"{"content":[{"type":"tool_use","name":"lookup","input":{}}]}"#,
            "tool",
        ),
        (
            r#"{"content":[{"type":"tool_use","id":"call","input":{}}]}"#,
            "tool",
        ),
    ];

    for (body, expected) in cases {
        let stack = ScriptedStack::new([ScriptStep::json(200, body)]);
        let mut api = configured(&stack, BackendKind::AnthropicCompatible);
        let messages = [json!({"role":"user","content":"hello"})];
        let error =
            block_on(api.chat(&ChatRequest::new("", &messages), Cancel::never())).unwrap_err();
        match expected {
            "parse" => assert!(matches!(error, Error::Parse)),
            "missing" => assert!(matches!(error, Error::MalformedResponse(_))),
            "empty" => assert!(matches!(error, Error::EmptyResponse)),
            "tool" => assert!(matches!(
                error,
                Error::MalformedResponse("malformed tool call")
            )),
            _ => unreachable!(),
        }
    }
}

#[test]
fn structured_chat_sends_schema_and_parses_output() {
    let stack = ScriptedStack::new([ScriptStep::json(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"{\"answer\":42}"}}]}"#,
    )]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = [json!({"role":"user","content":"answer"})];
    let request = ChatRequest::new("system", &messages);
    let schema = StaticOutputSchema {
        name: "answer",
        json: r#"{"type":"object"}"#,
    };
    let response: barracuda_model_api::ChatJsonResponse<Value> =
        block_on(api.chat_json(&request, schema, Cancel::never())).unwrap();
    assert_eq!(response.output.unwrap()["answer"], 42);
    assert_eq!(
        request_body(&stack)["response_format"]["type"],
        "json_schema"
    );
}

#[test]
fn invalid_tools_are_rejected_before_network_io() {
    let messages = [json!({"role":"user","content":"hello"})];
    for backend in [
        BackendKind::OpenAiCompatible,
        BackendKind::AnthropicCompatible,
    ] {
        let stack = ScriptedStack::default();
        let mut api = configured(&stack, backend);
        let error = block_on(api.chat(
            &ChatRequest::new("system", &messages).with_tools("{}"),
            Cancel::never(),
        ))
        .unwrap_err();
        assert!(matches!(error, Error::InvalidToolsJson));
        assert!(stack.requests().is_empty());
    }
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
    let messages = [json!({"role":"user","content":"hello"})];
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
    let mut api = ModelApi::new(ClientFactory::from_network(&stack, &stack));
    let mut config = ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "secret",
        "model-x",
        "http://llm.test/v1",
    );
    config.timeout_ms = 1;
    api.set_config(config).unwrap();
    let messages = [json!({"role":"user","content":"hello"})];
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::fixed(1, 0));

    let response = block_on(api.chat(&request, Cancel::never())).unwrap();

    assert_eq!(response.text.as_deref(), Some("recovered"));
    assert_eq!(stack.connect_count(), 2);
}

#[test]
fn cancelled_request_does_not_touch_network() {
    let stack = ScriptedStack::new([ScriptStep::json(200, "{}")]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = [json!({"role":"user","content":"hello"})];
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

#[test]
fn cancelling_a_pending_request_reconnects_before_the_next_call() {
    let stack = ScriptedStack::new([
        ScriptStep::pending_after_headers(200, "application/json"),
        ScriptStep::json(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"recovered"}}]}"#,
        ),
    ]);
    let mut api = configured(&stack, BackendKind::OpenAiCompatible);
    let messages = [json!({"role":"user","content":"hello"})];
    let cancelled = CancellationFlag::new();

    {
        let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::none());
        let mut pending = core::pin::pin!(api.chat(&request, Cancel::new(&cancelled)));
        assert!(block_on(poll_once(pending.as_mut())).is_none());
        cancelled.cancel();
        assert!(block_on(pending.as_mut()).unwrap_err().is_aborted());
    }

    let response =
        block_on(api.chat(&ChatRequest::new("system", &messages), Cancel::never())).unwrap();
    assert_eq!(response.text.as_deref(), Some("recovered"));
    assert_eq!(stack.connect_count(), 2);
}
