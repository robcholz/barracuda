//! A message's attachments travel by path into the transcript and reach the
//! model as inlined images.

use std::cell::Cell;
use std::rc::Rc;

use barracuda_agent_runtime::tools::{
    Action, EmptyArgs, RiskClass, Tool, ToolContext, ToolFuture, ToolGroup, ToolHandler,
    ToolInvocation, ToolOutput, ToolSpec,
};
use barracuda_agent_runtime::{
    AgentRuntime, ApiPurpose, Attachment, BackendKind, MediaKind, Message, ModelApiConfig,
    ModelApiFactory, RuntimeStorageConfig, SessionEvent, SessionId, SessionPersistence, TurnEvent,
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
            ModelApiConfig::new(BackendKind::OpenAi, "key", "model", "http://llm.test/v1"),
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

/// Returns a screenshot file, and records the session its call runs in.
struct ShotTool(Rc<Cell<Option<SessionId>>>);

impl ToolSpec for ShotTool {
    fn name(&self) -> &str {
        "shot"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"shot","parameters":{"type":"object","properties":{},"additionalProperties":false}}}"#
    }

    fn arguments_validator(&self) -> &'static json_validator::Validator {
        const VALIDATOR: json_validator::Validator =
            json_validator::validator!("examples/time_now.schema.json");
        &VALIDATOR
    }

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new("shot", RiskClass::Safe)
    }
}

impl ToolHandler for ShotTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, context: ToolContext, _args: Self::Args) -> ToolFuture<'a> {
        self.0.set(Some(context.session));
        Box::pin(async {
            Ok(
                ToolOutput::new("{\"path\":\"/workspace/cache/shot.png\"}", true).with_attachments(
                    vec![Attachment {
                        kind: MediaKind::Image,
                        path: "/workspace/cache/shot.png".into(),
                        mime_type: "image/png".into(),
                        filename: None,
                        size: PNG.len() as u64,
                    }],
                ),
            )
        })
    }
}

fn tool_call(name: &str) -> String {
    format!(
        concat!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"c1\",",
            "\"type\":\"function\",\"function\":{{\"name\":\"{name}\",\"arguments\":\"{{}}\"}}}}]}}}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        name = name
    )
}

#[test]
fn a_tool_image_reaches_the_next_request_and_the_call_knows_its_session() {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[&tool_call("shot")]),
        ScriptStep::sse(200, &[&reply("a screenshot")]),
    ])));
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    block_on(filesystem.write_atomic("/workspace/cache/shot.png", PNG)).expect("image is stored");
    let seen = Rc::new(Cell::new(None));
    let (runtime, service) = AgentRuntime::with_tool_groups(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
        [ToolGroup::new(
            "probe",
            true,
            [Tool::new(ShotTool(Rc::clone(&seen)))],
        )],
    )
    .expect("runtime builds");
    runtime.start_all().expect("tools start");
    runtime
        .set_api(
            ModelApiConfig::new(BackendKind::OpenAi, "key", "model", "http://llm.test/v1"),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model configures");

    let session = block_on(async {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("opens");
            control
                .append(Message::text("take a screenshot"))
                .await
                .expect("message appends");
            while let Some(event) = events.next().await {
                if matches!(event, Ok(SessionEvent::Turn(TurnEvent::Ended { .. }))) {
                    break;
                }
            }
            runtime.shutdown().await;
            session
        };
        let (_, session) = zip(service, scenario).await;
        session
    });

    assert_eq!(
        seen.get(),
        Some(session),
        "the call ran in the session's context"
    );
    let requests = network.requests();
    let request = requests.get(1).expect("the model was asked again");
    let body: serde_json::Value =
        serde_json::from_str(request.split_once("\r\n\r\n").expect("body").1).expect("JSON body");
    let messages = body["messages"].as_array().expect("messages");
    let tool = messages
        .iter()
        .position(|message| message["role"] == "tool")
        .expect("the tool result");
    assert_eq!(
        messages[tool]["content"],
        "{\"path\":\"/workspace/cache/shot.png\"}\n[image: shot.png]"
    );
    let encoded = base64_png();
    assert_eq!(
        messages[tool + 1],
        serde_json::json!({"role": "user", "content": [
            {"text": "The tool results above returned these images.", "type": "text"},
            {"text": "[image: shot.png]", "type": "text"},
            {"image_url": {"url": format!("data:image/png;base64,{encoded}")}, "type": "image_url"},
        ]})
    );
}
