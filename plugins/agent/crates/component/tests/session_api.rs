#![allow(missing_docs)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use std::cell::RefCell;
use std::future::{pending, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_component::session::SessionOutputEvent;
use barracuda_agent_runtime::{AgentRuntime, ModelApiFactory, RuntimeStorageConfig};
use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, Event, EventRouter, JsonRef, JsonRpcSchema,
    JsonSchema, JsonWriter, RegisterContext, RpcAddress, RpcClient, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{install_global_memory_vfs, memory_vfs, ScriptStep, ScriptedStack};
use http_client::ClientFactory;
use serde::Deserialize;
use serde_json::Value;
use static_cell::StaticCell;

const FRAME_SIZE: usize = 512;
const EMPTY_SCHEMA: JsonSchema = barracuda_event_router::json_schema_inline!(
    r#"{"type":"object","properties":{},"additionalProperties":false}"#
);
const EVENT_SCHEMA: JsonSchema =
    barracuda_event_router::json_schema_inline!(r#"{"type":"object"}"#);
const WORKFLOW_JSON: &str = r#"{
    "id":"agent-session-event-test",
    "match":{"event":"session.event"},
    "steps":[{"call":"test.record-session-event"}]
}"#;
const EXPECTED_OUTPUT: &str = "abcdefghijklmnopqrstuvwxyz😀ABCDEFGHIJKLMNO";

static NETWORK: StaticCell<ScriptedStack> = StaticCell::new();

struct RecordSessionEvent;

impl JsonRpcSchema for RecordSessionEvent {
    const ADDRESS: &'static str = "test.record-session-event";
    const REQUEST_SCHEMA: JsonSchema = EVENT_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = FRAME_SIZE;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionEventRecord {
    session: String,
    sequence: u32,
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
}

#[derive(Default)]
struct ResultState {
    stage: RefCell<&'static str>,
    events: RefCell<Vec<SessionEventRecord>>,
    finished: RefCell<bool>,
}

impl ResultState {
    fn text_for_event_type(&self, event_type: &str) -> Option<String> {
        let mut events: Vec<_> = self
            .events
            .borrow()
            .iter()
            .filter(|event| event.event_type == event_type)
            .map(|event| {
                (
                    event.sequence,
                    event
                        .payload
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect();
        if events.is_empty() {
            return None;
        }
        events.sort_by_key(|(sequence, _text)| *sequence);
        Some(
            events
                .iter()
                .map(|(_sequence, text)| text.as_str())
                .collect(),
        )
    }

    fn payload_for_event_type(&self, event_type: &str) -> Option<Value> {
        self.events
            .borrow()
            .iter()
            .find(|event| event.event_type == event_type)
            .map(|event| event.payload.clone())
    }
}

struct SessionApiClient {
    result: Rc<ResultState>,
}

impl Component<FRAME_SIZE> for SessionApiClient {
    fn name(&self) -> &'static str {
        "session-api-client"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let result = Rc::clone(&self.result);
        context.register_json::<RecordSessionEvent, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let result = Rc::clone(&result);
                async move {
                    result.events.borrow_mut().push(request.deserialize()?);
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            run_session_api(context.rpc().clone(), Rc::clone(&self.result)).await;
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

async fn run_session_api(client: RpcClient, result: Rc<ResultState>) {
    let system_rpcs = client
        .rpcs_by_visibility("system")
        .expect("discover system RPCs");
    let agent_rpcs = [
        "session.new",
        "session.list",
        "session.open",
        "session.delete",
        "session.append",
        "session.respond",
        "session.set_reasoning_effort",
        "session.set_permission_level",
        "session.interrupt",
        "session.cancel",
        "session.close",
    ];
    for address in agent_rpcs {
        assert!(
            system_rpcs.contains(&RpcAddress::try_from(address).expect("valid Agent RPC address")),
            "{address} must be visible to the system group"
        );
    }
    let public_rpcs = client
        .rpcs_by_visibility("*")
        .expect("discover public RPCs");
    for address in agent_rpcs {
        assert!(
            !public_rpcs.contains(&RpcAddress::try_from(address).expect("valid Agent RPC address")),
            "{address} must not be publicly visible"
        );
    }

    *result.stage.borrow_mut() = "load_workflow";
    WorkflowClient::<FRAME_SIZE>::new(client.clone())
        .load(WORKFLOW_JSON)
        .await
        .expect("load session Event workflow");

    *result.stage.borrow_mut() = "new";
    let created = call(&client, "session.new", r#"{"persistence":"ephemeral"}"#).await;
    let session = created
        .get("session")
        .and_then(Value::as_str)
        .expect("session.new returns a session")
        .to_owned();

    *result.stage.borrow_mut() = "list";
    let listed = call(&client, "session.list", r#"{"offset":0,"limit":1}"#).await;
    assert_eq!(
        listed
            .get("sessions")
            .and_then(Value::as_array)
            .and_then(|sessions| sessions.first())
            .and_then(Value::as_str),
        Some(session.as_str())
    );
    assert!(listed.get("next_offset").is_some_and(Value::is_null));

    *result.stage.borrow_mut() = "closed_append";
    let rejected = call(
        &client,
        "session.append",
        &format!(r#"{{"session":"{session}","text":"too early"}}"#),
    )
    .await;
    assert_eq!(
        rejected.get("error").and_then(Value::as_str),
        Some("session_not_open")
    );

    *result.stage.borrow_mut() = "open";
    let opened = call(
        &client,
        "session.open",
        &format!(r#"{{"session":"{session}"}}"#),
    )
    .await;
    assert_eq!(
        opened.get("session").and_then(Value::as_str),
        Some(session.as_str())
    );
    assert_eq!(opened.get("run").and_then(Value::as_str), Some("run-1"));

    for (address, body) in [
        (
            "session.set_reasoning_effort",
            format!(r#"{{"session":"{session}","effort":"medium"}}"#),
        ),
        (
            "session.set_permission_level",
            format!(r#"{{"session":"{session}","level":"ask"}}"#),
        ),
    ] {
        *result.stage.borrow_mut() = "control";
        assert_eq!(call(&client, address, &body).await, serde_json::json!({}));
    }

    *result.stage.borrow_mut() = "append";
    let message = "x".repeat(430);
    assert_eq!(
        call(
            &client,
            "session.append",
            &format!(r#"{{"session":"{session}","text":"{message}"}}"#),
        )
        .await,
        serde_json::json!({})
    );

    *result.stage.borrow_mut() = "events";
    wait_until(|| result.text_for_event_type("output_delta").as_deref() == Some(EXPECTED_OUTPUT))
        .await;

    *result.stage.borrow_mut() = "usage";
    wait_until(|| {
        result
            .payload_for_event_type("usage")
            .and_then(|payload| payload.get("cache_read_tokens").cloned())
            .and_then(|value| value.as_u64())
            == Some(8)
    })
    .await;
    let usage = result
        .payload_for_event_type("usage")
        .expect("usage event has a payload");
    assert_eq!(usage.get("input_tokens").and_then(Value::as_u64), Some(12));
    assert_eq!(usage.get("output_tokens").and_then(Value::as_u64), Some(3));

    *result.stage.borrow_mut() = "close";
    assert_eq!(
        call(
            &client,
            "session.close",
            &format!(r#"{{"session":"{session}"}}"#),
        )
        .await,
        serde_json::json!({})
    );
    wait_until(|| {
        result
            .events
            .borrow()
            .iter()
            .any(|event| event.event_type == "closed")
    })
    .await;

    *result.stage.borrow_mut() = "delete";
    assert_eq!(
        call(
            &client,
            "session.delete",
            &format!(r#"{{"session":"{session}"}}"#),
        )
        .await,
        serde_json::json!({})
    );
    *result.finished.borrow_mut() = true;
}

async fn call(client: &RpcClient, method: &str, request: &str) -> Value {
    let address = RpcAddress::try_from(method).expect("valid test RPC address");
    client
        .call_json(&address, request)
        .expect("start JSON RPC")
        .await
        .expect("complete JSON RPC")
        .deserialize()
        .expect("valid JSON RPC response")
}

async fn wait_until(mut ready: impl FnMut() -> bool) {
    while !ready() {
        futures_lite::future::yield_now().await;
    }
}

#[test]
fn system_json_rpcs_and_bounded_session_events_drive_the_agent() {
    futures_lite::future::block_on(async {
        let event = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"",
            "abcdefghijklmnopqrstuvwxyz😀ABCDEFGHIJKLMNO",
            "\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":8}}}\n\n",
            "data: [DONE]\n\n"
        );
        let network: &'static ScriptedStack =
            NETWORK.init(ScriptedStack::new([ScriptStep::sse(200, &[event])]));
        let factory = ModelApiFactory::new(move || {
            ModelApi::new(ClientFactory::from_network(network, network))
        });
        let (runtime, service) = AgentRuntime::new(
            memory_vfs().await.expect("memory VFS mounts"),
            RuntimeStorageConfig {
                persistence_root: "/agent".into(),
                skill_roots: Vec::new(),
            },
            factory,
        )
        .expect("build Agent runtime");
        runtime
            .set_api(
                barracuda_agent_runtime::ModelApiConfig::new(
                    barracuda_agent_runtime::BackendKind::OpenAiCompatible,
                    "test-key",
                    "test-model",
                    "http://example.invalid",
                ),
                barracuda_agent_runtime::ApiPurpose::RootAgent,
                true,
            )
            .expect("configure model API");
        runtime.start_all().expect("start Agent tools");

        let result = Rc::new(ResultState::default());
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, FRAME_SIZE, 8>::new()));
        let mut router = EventRouter::new(lanes).await.expect("build Event Router");
        router
            .load(Box::new(AgentComponent::new(runtime, service)))
            .expect("load Agent Component");
        router
            .load(Box::new(SessionApiClient {
                result: Rc::clone(&result),
            }))
            .expect("load Session API client");

        drive_until(&mut router, &result).await;

        let request = network
            .requests()
            .into_iter()
            .next()
            .expect("Agent sends one model request");
        let request_body: Value = serde_json::from_str(
            request
                .split_once("\r\n\r\n")
                .expect("HTTP request contains a body")
                .1,
        )
        .expect("model request body is JSON");
        let tool_names: Vec<_> = request_body
            .get("tools")
            .expect("model request contains tools")
            .as_array()
            .expect("model request tools are an array")
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(
            tool_names.contains(&RecordSessionEvent::ADDRESS),
            "missing RPC tool in model request: {request_body}"
        );
        assert!(!tool_names.contains(&"session.new"));

        let events = result.events.borrow();
        assert!(events.iter().all(|event| event.session == "session-1"));
        assert!(events.iter().all(|event| event.payload.is_object()));
        let mut sequences: Vec<_> = events.iter().map(|event| event.sequence).collect();
        sequences.sort_unstable();
        sequences.dedup();
        assert_eq!(sequences.len(), events.len());
        assert_eq!(sequences.first(), Some(&0));
        assert_eq!(
            sequences.last().copied(),
            u32::try_from(events.len())
                .ok()
                .and_then(|len| len.checked_sub(1))
        );
    });
}

async fn drive_until(router: &mut EventRouter<8, FRAME_SIZE, 8>, result: &ResultState) {
    core::future::poll_fn(|context| {
        if let Poll::Ready(Err(error)) = Pin::new(&mut *router).poll(context) {
            panic!(
                "Event Router failed during {}: {error}",
                result.stage.borrow()
            );
        }
        if *result.finished.borrow() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}

#[test]
fn session_event_contract_is_public_and_bounded() {
    assert_eq!(SessionOutputEvent::ID, "session.event");
    let schema = include_str!("../../../schemas/event/session_event.json");
    for field in ["session", "sequence", "type", "payload"] {
        assert!(schema.contains(&format!("\"{field}\"")));
    }
    for removed in [
        "run",
        "chunk_index",
        "field",
        "chunk",
        "field_complete",
        "event_complete",
        "terminal",
    ] {
        assert!(!schema.contains(&format!("\"{removed}\"")));
    }
    assert!(!schema.contains("maxLength"));
}
