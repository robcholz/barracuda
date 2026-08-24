#![allow(clippy::unwrap_used)]

use barracuda_model_api::{
    BackendKind, ChatError, ChatRequest, ChatStreamEvent, ModelApi, ModelApiConfig, ModelApiError,
    RetryPolicy, StatusCode,
};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use barracuda_runtime_utils::stream::StreamPart;
use barracuda_runtime_utils::Cancel;
use embedded_io::ErrorKind;
use futures_lite::future::block_on;
use futures_lite::StreamExt as _;
use serde_json::json;

fn configured<'a>(stack: &'a ScriptedStack) -> ModelApi<'a, ScriptedStack, ScriptedStack> {
    let mut api = ModelApi::new(stack, stack, 4096, 9);
    api.set_config(ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "secret",
        "model",
        "http://llm.test/v1",
    ))
    .unwrap();
    api
}

fn sse(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n")
}

#[test]
fn stream_parses_sse_across_arbitrary_network_chunks() {
    let body = sse("hello");
    let stack = ScriptedStack::new([ScriptStep::sse(200, &[&body[..11], &body[11..]])]);
    let mut api = configured(&stack);
    let messages = [json!({"role":"user","content":"hi"})];
    let events = block_on(async {
        api.chat_stream(&ChatRequest::new("system", &messages), Cancel::never())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    });
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(ChatStreamEvent::Output(StreamPart::Delta(text))) if text == "hello"
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ChatStreamEvent::Output(StreamPart::End)))));
}

#[test]
fn sequential_streams_reuse_one_connection() {
    let first_body = sse("one");
    let second_body = sse("two");
    let stack = ScriptedStack::new([
        ScriptStep::sse(200, &[&first_body]),
        ScriptStep::sse(200, &[&second_body]),
    ]);
    let mut api = configured(&stack);
    let messages = [json!({"role":"user","content":"hi"})];

    block_on(async {
        api.chat_stream(&ChatRequest::new("system", &messages), Cancel::never())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        api.chat_stream(&ChatRequest::new("system", &messages), Cancel::never())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
    });

    assert_eq!(stack.requests().len(), 2);
    assert_eq!(stack.connect_count(), 1);
}

#[test]
fn stream_retries_connect_failure_before_emitting() {
    let body = sse("recovered");
    let stack = ScriptedStack::new([
        ScriptStep::ConnectError(ErrorKind::ConnectionReset),
        ScriptStep::sse(200, &[&body]),
    ]);
    let mut api = configured(&stack);
    let messages = [json!({"role":"user","content":"hi"})];
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::fixed(1, 0));
    let events = block_on(async {
        api.chat_stream(&request, Cancel::never())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    });
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(ChatStreamEvent::Output(StreamPart::Delta(text))) if text == "recovered"
    )));
    assert_eq!(stack.remaining(), 0);
}

#[test]
fn non_success_stream_surfaces_status_and_body() {
    let stack = ScriptedStack::new([ScriptStep::sse(429, &["rate limited"])]);
    let mut api = configured(&stack);
    let messages = [json!({"role":"user","content":"hi"})];
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::none());
    let result = block_on(api.chat_stream(&request, Cancel::never()));
    let error = match result {
        Ok(_) => panic!("non-success response unexpectedly opened a stream"),
        Err(error) => error,
    };
    assert!(matches!(
        &error,
        ChatError::Api(ModelApiError::TransientHttpStatus {
            status: StatusCode(429),
            ..
        })
    ));
    assert!(error.to_string().contains("429"));
    assert!(error.to_string().contains("rate limited"));
}

#[test]
fn stalled_stream_body_times_out_after_the_response_head() {
    let stack = ScriptedStack::new([ScriptStep::pending_after_headers(200, "text/event-stream")]);
    let mut api = ModelApi::new(&stack, &stack, 4096, 9);
    let mut config = ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "secret",
        "model",
        "http://llm.test/v1",
    );
    config.timeout_ms = 1;
    api.set_config(config).unwrap();
    let messages = [json!({"role":"user","content":"hi"})];
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::none());
    let mut stream = block_on(api.chat_stream(&request, Cancel::never())).unwrap();

    let error = block_on(stream.next()).unwrap().unwrap_err();
    assert!(matches!(
        error,
        barracuda_model_api::ChatError::Api(barracuda_model_api::ModelApiError::Timeout)
    ));
}

#[test]
fn non_success_stream_truncates_the_error_body() {
    let body = format!("{}tail-marker", "x".repeat(1100));
    let stack = ScriptedStack::new([ScriptStep::sse(400, &[&body])]);
    let mut api = configured(&stack);
    let messages = [json!({"role":"user","content":"hi"})];
    let request = ChatRequest::new("system", &messages).with_retry(RetryPolicy::none());
    let error = match block_on(api.chat_stream(&request, Cancel::never())) {
        Ok(_) => panic!("non-success response unexpectedly opened a stream"),
        Err(error) => error,
    };
    let rendered = error.to_string();
    assert!(!rendered.contains("tail-marker"));
    assert!(rendered.ends_with('…'));
}
