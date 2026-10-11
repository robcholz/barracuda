//! A message's attachments travel by path into the transcript and reach the
//! model as inlined images.

use barracuda_agent_runtime::{
    AgentRuntime, ApiPurpose, Attachment, BackendKind, MediaKind, Message, ModelApiConfig,
    ModelApiFactory, RuntimeStorageConfig, SessionEvent, SessionPersistence, TurnEvent,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{memory_vfs, ScriptStep, ScriptedStack};
use base64::Engine as _;
use futures_lite::future::{block_on, zip};
use futures_lite::StreamExt as _;
use http_client::ClientFactory;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\npixels";

fn reply(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n")
}

#[test]
fn an_appended_image_reaches_the_model_inline_and_the_audio_by_name() {
    // The runtime's model clients outlive this test's stack frame.
    let network: &'static ScriptedStack =
        Box::leak(Box::new(ScriptedStack::new([ScriptStep::sse(
            200,
            &[&reply("a cat")],
        )])));
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    block_on(filesystem.write_atomic("/workspace/cache/gateway/web/1.png", PNG))
        .expect("image is stored");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "key",
                "model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model configures");

    block_on(async {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("opens");
            control
                .append(Message::text("").with_attachments(vec![
                    Attachment {
                        kind: MediaKind::Image,
                        path: "/workspace/cache/gateway/web/1.png".into(),
                        mime_type: "image/png".into(),
                        filename: Some("cat.png".into()),
                        size: PNG.len() as u64,
                    },
                    Attachment {
                        kind: MediaKind::Audio,
                        path: "/workspace/cache/gateway/web/2.ogg".into(),
                        mime_type: "audio/ogg".into(),
                        filename: None,
                        size: 4,
                    },
                ]))
                .await
                .expect("an attachment-only message appends");
            while let Some(event) = events.next().await {
                if matches!(event, Ok(SessionEvent::Turn(TurnEvent::Ended { .. }))) {
                    break;
                }
            }
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });

    let request = network
        .requests()
        .into_iter()
        .next()
        .expect("model was asked");
    let body: serde_json::Value =
        serde_json::from_str(request.split_once("\r\n\r\n").expect("body").1).expect("JSON body");
    let messages = body["messages"].as_array().expect("messages");
    let user = messages
        .iter()
        .find(|message| message["role"] == "user" && message["content"].is_array())
        .expect("the user message carries its attachments");
    let encoded = base64_png();
    assert_eq!(
        user["content"],
        serde_json::json!([
            {"image_url": {"url": format!("data:image/png;base64,{encoded}")}, "type": "image_url"},
            {"text": "[attachment: 2.ogg, audio/ogg, 4 bytes, /workspace/cache/gateway/web/2.ogg, no longer available]", "type": "text"},
        ])
    );
}

fn base64_png() -> String {
    base64::engine::general_purpose::STANDARD.encode(PNG)
}
