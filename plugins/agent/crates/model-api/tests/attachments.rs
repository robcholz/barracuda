//! Attachments referenced by `media` parts are inlined or named in the
//! request body, by the configured modalities and limits.

#![allow(clippy::unwrap_used)]

use barracuda_model_api::{
    BackendKind, ChatMessage, ChatRequest, InputModalities, ModelApi, ModelApiConfig,
};
use barracuda_platform_test::{memory_vfs, ScriptStep, ScriptedStack};
use barracuda_runtime_utils::Cancel;
use barracuda_vfs::ScopedVfs;
use base64::Engine as _;
use futures_lite::future::block_on;
use http_client::ClientFactory;
use serde_json::{json, Value};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\npng-pixels";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0jpeg-pixels";
const GIF: &[u8] = b"GIF89agif-pixels";
const HEIC: &[u8] = b"\0\0\0\x18ftypheicheic-pixels";
const PDF: &[u8] = b"%PDF-1.7 report";

const OPENAI_REPLY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
const ANTHROPIC_REPLY: &str =
    r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;

fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A `media` part as the transcript stores it.
fn part(kind: &str, path: &str, mime_type: &str, size: usize) -> Value {
    json!({"kind": kind, "path": path, "mime_type": mime_type, "size": size, "type": "media"})
}

fn user(parts: Vec<Value>) -> ChatMessage {
    ChatMessage::new(&json!({"role": "user", "content": parts}))
}

fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

async fn files(entries: &[(&str, &[u8])]) -> ScopedVfs {
    let files = memory_vfs().await.unwrap();
    for (path, bytes) in entries {
        files.write_atomic(path, bytes).await.unwrap();
    }
    files
}

fn config(backend: BackendKind, configure: impl FnOnce(&mut ModelApiConfig)) -> ModelApiConfig {
    let mut config = ModelApiConfig::new(backend, "secret", "model-x", "http://llm.test/v1");
    configure(&mut config);
    config
}

/// Sends one chat and returns the exact request body.
fn sent_body(
    config: ModelApiConfig,
    messages: &[ChatMessage],
    files: Option<&ScopedVfs>,
) -> String {
    let reply = match config.backend {
        BackendKind::OpenAiCompatible => OPENAI_REPLY,
        BackendKind::AnthropicCompatible => ANTHROPIC_REPLY,
    };
    let stack = ScriptedStack::new([ScriptStep::json(200, reply)]);
    let mut api = ModelApi::new(ClientFactory::from_network(&stack, &stack));
    api.set_config(config).unwrap();
    let mut request = ChatRequest::new("sys", messages);
    request.files = files;
    block_on(api.chat(&request, Cancel::never())).unwrap();
    let request = stack.requests().pop().unwrap();
    request.split_once("\r\n\r\n").unwrap().1.to_owned()
}

#[test]
fn openai_inlines_the_newest_images_and_names_every_other_attachment() {
    block_on(async {
        let files = files(&[
            ("/workspace/cache/a.png", PNG),
            ("/workspace/cache/b.jpg", JPEG),
            ("/workspace/cache/c.ogg", b"OggS"),
            ("/workspace/cache/d.gif", GIF),
        ])
        .await;
        let messages = [
            user(vec![
                text("first"),
                part("image", "/workspace/cache/a.png", "image/png", 18),
            ]),
            user(vec![
                part("image", "/workspace/cache/b.jpg", "image/jpeg", 15),
                part("audio", "/workspace/cache/c.ogg", "audio/ogg", 4),
            ]),
            user(vec![
                text("third"),
                part("image", "/workspace/cache/d.gif", "image/gif", 16),
            ]),
        ];
        let body = sent_body(
            config(BackendKind::OpenAiCompatible, |_| {}),
            &messages,
            Some(&files),
        );
        assert_eq!(
            body,
            format!(
                concat!(
                    r#"{{"max_tokens":8192,"messages":[{{"content":"sys","role":"system"}},"#,
                    r#"{{"content":[{{"text":"first","type":"text"}},{{"text":"[attachment: a.png, image/png, 18 bytes, /workspace/cache/a.png]","type":"text"}}],"role":"user"}},"#,
                    r#"{{"content":[{{"image_url":{{"url":"data:image/jpeg;base64,{jpeg}"}},"type":"image_url"}},{{"text":"[attachment: c.ogg, audio/ogg, 4 bytes, /workspace/cache/c.ogg]","type":"text"}}],"role":"user"}},"#,
                    r#"{{"content":[{{"text":"third","type":"text"}},{{"image_url":{{"url":"data:image/gif;base64,{gif}"}},"type":"image_url"}}],"role":"user"}}],"#,
                    r#""model":"model-x"}}"#,
                ),
                jpeg = base64(JPEG),
                gif = base64(GIF),
            )
        );
    });
}

#[test]
fn oversized_missing_unrecognized_and_disabled_images_become_placeholders() {
    block_on(async {
        let big = [PNG, &[0_u8; 64]].concat();
        let files = files(&[
            ("/workspace/cache/big.png", &big),
            ("/workspace/cache/photo.heic", HEIC),
            ("/workspace/cache/ok.png", PNG),
        ])
        .await;
        let messages = [user(vec![
            part("image", "/workspace/cache/big.png", "image/png", 82),
            part("image", "/workspace/cache/gone.png", "image/png", 9),
            part("image", "/workspace/cache/photo.heic", "image/heic", 22),
        ])];
        let body: Value = serde_json::from_str(&sent_body(
            config(BackendKind::OpenAiCompatible, |config| {
                config.image_max_bytes = 64;
            }),
            &messages,
            Some(&files),
        ))
        .unwrap();
        assert_eq!(
            body["messages"][1]["content"],
            json!([
                text("[attachment: big.png, image/png, 82 bytes, /workspace/cache/big.png]"),
                text("[attachment: gone.png, image/png, 9 bytes, /workspace/cache/gone.png, no longer available]"),
                text("[attachment: photo.heic, image/heic, 22 bytes, /workspace/cache/photo.heic]"),
            ])
        );

        // With the image modality off, a readable image is named too.
        let messages = [user(vec![part(
            "image",
            "/workspace/cache/ok.png",
            "image/png",
            18,
        )])];
        let body: Value = serde_json::from_str(&sent_body(
            config(BackendKind::OpenAiCompatible, |config| {
                config.input_modalities.image = false;
            }),
            &messages,
            Some(&files),
        ))
        .unwrap();
        assert_eq!(
            body["messages"][1]["content"],
            json!([text(
                "[attachment: ok.png, image/png, 18 bytes, /workspace/cache/ok.png]"
            )])
        );

        // A request without a filesystem names every attachment.
        let body: Value = serde_json::from_str(&sent_body(
            config(BackendKind::OpenAiCompatible, |_| {}),
            &messages,
            None,
        ))
        .unwrap();
        assert_eq!(
            body["messages"][1]["content"],
            json!([text(
                "[attachment: ok.png, image/png, 18 bytes, /workspace/cache/ok.png]"
            )])
        );
    });
}

#[test]
fn only_newest_images_count_toward_the_limit() {
    block_on(async {
        let files = files(&[
            ("/workspace/cache/1.png", PNG),
            ("/workspace/cache/2.png", PNG),
            ("/workspace/cache/3.png", PNG),
        ])
        .await;
        let messages = [
            user(vec![part(
                "image",
                "/workspace/cache/1.png",
                "image/png",
                18,
            )]),
            user(vec![
                part("image", "/workspace/cache/2.png", "image/png", 18),
                part("image", "/workspace/cache/3.png", "image/png", 18),
            ]),
        ];
        let body: Value = serde_json::from_str(&sent_body(
            config(BackendKind::OpenAiCompatible, |config| {
                config.max_images_in_context = 1;
            }),
            &messages,
            Some(&files),
        ))
        .unwrap();
        let types = |message: &Value| {
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|part| part["type"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(types(&body["messages"][1]), ["text"]);
        assert_eq!(types(&body["messages"][2]), ["text", "image_url"]);
    });
}

#[test]
fn anthropic_inlines_images_and_pdfs_as_base64_blocks() {
    block_on(async {
        let files = files(&[
            ("/workspace/cache/cat.png", PNG),
            ("/workspace/media/report.pdf", PDF),
        ])
        .await;
        let messages = [
            ChatMessage::new(&json!({"role": "user", "content": "hello"})),
            ChatMessage::new(&json!({"role": "assistant", "content": "hi"})),
            user(vec![
                text("read these"),
                part("image", "/workspace/cache/cat.png", "image/png", 18),
                part("file", "/workspace/media/report.pdf", "application/pdf", 15),
            ]),
        ];
        let body = sent_body(
            config(BackendKind::AnthropicCompatible, |config| {
                config.input_modalities = InputModalities {
                    image: true,
                    pdf: true,
                };
            }),
            &messages,
            Some(&files),
        );
        assert_eq!(
            body,
            format!(
                concat!(
                    r#"{{"max_tokens":8192,"messages":["#,
                    r#"{{"content":[{{"text":"hello","type":"text"}}],"role":"user"}},"#,
                    r#"{{"content":[{{"text":"hi","type":"text"}}],"role":"assistant"}},"#,
                    r#"{{"content":[{{"text":"read these","type":"text"}},"#,
                    r#"{{"source":{{"data":"{png}","media_type":"image/png","type":"base64"}},"type":"image"}},"#,
                    r#"{{"source":{{"data":"{pdf}","media_type":"application/pdf","type":"base64"}},"type":"document"}}],"role":"user"}}],"#,
                    r#""model":"model-x","system":"sys"}}"#,
                ),
                png = base64(PNG),
                pdf = base64(PDF),
            )
        );

        // PDFs stay names while the modality is off, and on OpenAI-compatible
        // backends, which take no documents.
        for (backend, pdf) in [
            (BackendKind::AnthropicCompatible, false),
            (BackendKind::OpenAiCompatible, true),
        ] {
            let body: Value = serde_json::from_str(&sent_body(
                config(backend, |config| config.input_modalities.pdf = pdf),
                &messages,
                Some(&files),
            ))
            .unwrap();
            let messages = body["messages"].as_array().unwrap();
            let content = &messages.last().unwrap()["content"];
            assert_eq!(
                content[2],
                text("[attachment: report.pdf, application/pdf, 15 bytes, /workspace/media/report.pdf]"),
                "{backend:?}"
            );
        }
    });
}

#[test]
fn anthropic_body_without_attachments_keeps_its_shape() {
    let messages = [
        ChatMessage::new(&json!({"role": "user", "content": "what time is it?"})),
        ChatMessage::new(&json!({"role": "assistant", "content": "checking",
            "tool_calls": [{"id": "c1", "type": "function",
                            "function": {"name": "time_now", "arguments": "{\"tz\":\"utc\"}"}}]})),
        ChatMessage::new(
            &json!({"role": "tool", "tool_call_id": "c1", "content": "12:00", "is_error": false}),
        ),
    ];
    let body = sent_body(
        config(BackendKind::AnthropicCompatible, |_| {}),
        &messages,
        None,
    );
    assert_eq!(
        body,
        concat!(
            r#"{"max_tokens":8192,"messages":["#,
            r#"{"content":[{"text":"what time is it?","type":"text"}],"role":"user"},"#,
            r#"{"content":[{"text":"checking","type":"text"},{"id":"c1","input":{"tz":"utc"},"name":"time_now","type":"tool_use"}],"role":"assistant"},"#,
            r#"{"content":[{"content":"12:00","is_error":false,"tool_use_id":"c1","type":"tool_result"}],"role":"user"}],"#,
            r#""model":"model-x","system":"sys"}"#,
        )
    );
}

/// One tool round whose result returned a screenshot and a log file.
fn tool_round() -> [ChatMessage; 3] {
    [
        ChatMessage::new(&json!({"role": "user", "content": "take a screenshot"})),
        ChatMessage::new(&json!({"role": "assistant",
            "tool_calls": [{"id": "c1", "type": "function",
                            "function": {"name": "file_read", "arguments": "{}"}}]})),
        ChatMessage::new(
            &json!({"role": "tool", "tool_call_id": "c1", "is_error": false,
            "content": [
                text("{\"path\":\"/workspace/cache/shot.png\"}"),
                part("image", "/workspace/cache/shot.png", "image/png", 18),
                part("file", "/workspace/cache/run.log", "text/plain", 3),
            ]}),
        ),
    ]
}

#[test]
fn openai_tool_images_follow_the_tool_results_in_a_user_message() {
    block_on(async {
        let files = files(&[
            ("/workspace/cache/shot.png", PNG),
            ("/workspace/cache/run.log", b"log"),
        ])
        .await;
        let body = sent_body(
            config(BackendKind::OpenAiCompatible, |_| {}),
            &tool_round(),
            Some(&files),
        );
        assert_eq!(
            body,
            format!(
                concat!(
                    r#"{{"max_tokens":8192,"messages":[{{"content":"sys","role":"system"}},"#,
                    r#"{{"content":"take a screenshot","role":"user"}},"#,
                    r#"{{"role":"assistant","tool_calls":[{{"function":{{"arguments":"{{}}","name":"file_read"}},"id":"c1","type":"function"}}]}},"#,
                    r#"{{"content":"{{\"path\":\"/workspace/cache/shot.png\"}}\n[image: shot.png]\n[attachment: run.log, text/plain, 3 bytes, /workspace/cache/run.log]","is_error":false,"role":"tool","tool_call_id":"c1"}},"#,
                    r#"{{"content":[{{"text":"The tool results above returned these images.","type":"text"}},{{"text":"[image: shot.png]","type":"text"}},{{"image_url":{{"url":"data:image/png;base64,{png}"}},"type":"image_url"}}],"role":"user"}}],"#,
                    r#""model":"model-x"}}"#,
                ),
                png = base64(PNG),
            )
        );
    });
}

#[test]
fn anthropic_tool_results_carry_their_images() {
    block_on(async {
        let files = files(&[("/workspace/cache/shot.png", PNG)]).await;
        let body = sent_body(
            config(BackendKind::AnthropicCompatible, |_| {}),
            &tool_round(),
            Some(&files),
        );
        assert_eq!(
            body,
            format!(
                concat!(
                    r#"{{"max_tokens":8192,"messages":["#,
                    r#"{{"content":[{{"text":"take a screenshot","type":"text"}}],"role":"user"}},"#,
                    r#"{{"content":[{{"id":"c1","input":{{}},"name":"file_read","type":"tool_use"}}],"role":"assistant"}},"#,
                    r#"{{"content":[{{"content":[{{"text":"{{\"path\":\"/workspace/cache/shot.png\"}}","type":"text"}},"#,
                    r#"{{"source":{{"data":"{png}","media_type":"image/png","type":"base64"}},"type":"image"}},"#,
                    r#"{{"text":"[attachment: run.log, text/plain, 3 bytes, /workspace/cache/run.log, no longer available]","type":"text"}}],"#,
                    r#""is_error":false,"tool_use_id":"c1","type":"tool_result"}}],"role":"user"}}],"#,
                    r#""model":"model-x","system":"sys"}}"#,
                ),
                png = base64(PNG),
            )
        );
    });
}

#[test]
fn tool_images_count_toward_the_limit() {
    block_on(async {
        let files = files(&[
            ("/workspace/cache/old.png", PNG),
            ("/workspace/cache/shot.png", PNG),
        ])
        .await;
        let mut messages = vec![user(vec![part(
            "image",
            "/workspace/cache/old.png",
            "image/png",
            18,
        )])];
        messages.extend(tool_round());
        let body: Value = serde_json::from_str(&sent_body(
            config(BackendKind::AnthropicCompatible, |config| {
                config.max_images_in_context = 1;
            }),
            &messages,
            Some(&files),
        ))
        .unwrap();
        assert_eq!(
            body["messages"][0]["content"][0],
            text("[attachment: old.png, image/png, 18 bytes, /workspace/cache/old.png]")
        );
        assert_eq!(
            body["messages"][3]["content"][0]["content"][1]["type"],
            "image"
        );
    });
}
