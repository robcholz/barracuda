//! End-to-end streaming tests: drive [`ClawApiAsync::chat_stream`] over the
//! `ChunkedHttp` double, which serves an SSE body in small byte chunks so the
//! parser is exercised across arbitrary frame/codepoint splits.

use std::sync::atomic::{AtomicBool, Ordering};

use claw_api::{
    BackendKind, ChatError, ChatRequest, ChatStreamEvent, ClawApiAsync, ClawApiConfig, ToolCall,
};
use claw_interface::http::ChunkedHttp;
use claw_interface::{Cancel, ImmediateTimer};
use claw_utils::stream::StreamPart;
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use serde_json::json;

fn config(backend: BackendKind) -> ClawApiConfig {
    ClawApiConfig::new(backend, "key", "model", "http://example.test/v1")
}

/// Drive a `chat_stream` to completion and collect only its semantic events.
fn run(backend: BackendKind, sse_body: &str, chunk_size: usize) -> Vec<ChatStreamEvent> {
    let http = ChunkedHttp::new([sse_body.to_string()], chunk_size);
    let mut rt = ClawApiAsync::<ChunkedHttp, ImmediateTimer>::new(http, ImmediateTimer);
    rt.set_config(config(backend)).unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages);

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        let mut deltas = Vec::new();
        while let Some(event) = stream.next().await {
            deltas.push(event.expect("stream event"));
        }
        deltas
    })
}

#[test]
fn openai_streams_semantic_events_without_assembling_a_response() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"th\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"ink\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"ping\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    // 7-byte chunks split SSE frames and JSON tokens mid-way on purpose.
    let deltas = run(BackendKind::OpenAiCompatible, sse, 7);

    assert_eq!(
        deltas,
        vec![
            ChatStreamEvent::Reasoning(StreamPart::Delta("th".to_string())),
            ChatStreamEvent::Reasoning(StreamPart::Delta("ink".to_string())),
            ChatStreamEvent::Reasoning(StreamPart::End),
            ChatStreamEvent::Output(StreamPart::Delta("Hel".to_string())),
            ChatStreamEvent::Output(StreamPart::Delta("lo".to_string())),
            ChatStreamEvent::Output(StreamPart::End),
            ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                id: "call_1".to_string(),
                name: "ping".to_string(),
                arguments_json: "{}".to_string(),
            })),
            ChatStreamEvent::ToolCalls(StreamPart::End),
        ]
    );
}

#[test]
fn anthropic_streams_semantic_events_without_assembling_a_response() {
    let sse = concat!(
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"ping\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let deltas = run(BackendKind::AnthropicCompatible, sse, 9);

    assert_eq!(
        deltas,
        vec![
            ChatStreamEvent::Reasoning(StreamPart::End),
            ChatStreamEvent::Output(StreamPart::Delta("Hi".to_string())),
            ChatStreamEvent::Output(StreamPart::End),
            ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                id: "toolu_1".to_string(),
                name: "ping".to_string(),
                arguments_json: "{}".to_string(),
            })),
            ChatStreamEvent::ToolCalls(StreamPart::End),
        ]
    );
}

#[test]
fn premature_eof_is_yielded_as_a_stream_error() {
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
    let http = ChunkedHttp::new([sse], 64);
    let mut rt = ClawApiAsync::<ChunkedHttp, ImmediateTimer>::new(http, ImmediateTimer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages);

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Reasoning(StreamPart::End)))
        );
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Output(StreamPart::Delta(
                "partial".to_string()
            ))))
        );
        assert_eq!(
            stream.next().await,
            Some(Err(ChatError::truncated_stream()))
        );
        assert_eq!(stream.next().await, None);
    });
}

#[test]
fn streaming_abort_remains_active_after_headers() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"second\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let http = ChunkedHttp::new([sse], 64);
    let mut rt = ClawApiAsync::<ChunkedHttp, ImmediateTimer>::new(http, ImmediateTimer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages);

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Reasoning(StreamPart::End)))
        );
        assert!(matches!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Output(StreamPart::Delta(text)))) if text == "first"
        ));
        abort.store(true, Ordering::Relaxed);
        let error = stream
            .next()
            .await
            .expect("abort item")
            .expect_err("aborted");
        assert!(error.is_aborted(), "{error}");
    });
}
