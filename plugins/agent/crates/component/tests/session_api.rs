#![allow(missing_docs)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(clippy::collapsible_match)]

use std::cell::RefCell;
use std::future::{pending, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_component::dto::{FixedStr, SessionIdDto, SessionPersistenceDto};
use barracuda_agent_component::list_sessions::ListSessions;
use barracuda_agent_component::new_session::{NewSession, NewSessionRequest};
use barracuda_agent_component::open_session::{
    frames_from_open_session_response, OpenSession, OpenSessionRequest, OpenSessionResponse,
    OpenSessionResponseDecoder, SessionEventDto, ToolOutputDto,
};
use barracuda_agent_component::session;
use barracuda_agent_runtime::{AgentRuntime, ModelApiFactory, RuntimeStorageConfig, SessionId};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, RegisterContext,
    RpcError, RpcFrame, RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{install_global_memory_vfs, memory_vfs, ScriptStep, ScriptedStack};
use static_cell::StaticCell;

static NETWORK: StaticCell<ScriptedStack> = StaticCell::new();

#[derive(Default)]
struct ResultState {
    stage: RefCell<&'static str>,
    sessions: RefCell<Vec<u32>>,
    output: RefCell<String>,
}

struct SessionApiClient {
    result: Rc<ResultState>,
}

impl Component<512> for SessionApiClient {
    fn register(&mut self, _context: &mut RegisterContext<'_, 512>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<512>) -> ComponentFuture<'a> {
        Box::pin(async move {
            run_session_api(context, Rc::clone(&self.result))
                .await
                .map_err(ComponentError::lifecycle)?;
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

async fn run_session_api(
    context: RunContext<512>,
    result: Rc<ResultState>,
) -> Result<(), RpcError> {
    let client = context.rpc();
    *result.stage.borrow_mut() = "new_session";
    let session = success(
        client
            .call::<NewSession>(NewSessionRequest {
                persistence: SessionPersistenceDto::Ephemeral,
            })?
            .await?,
    )?
    .view()?
    .session;
    let session = SessionId::new(session.get());

    *result.stage.borrow_mut() = "list_sessions";
    let mut sessions = client.call::<ListSessions>(())?;
    while let Some(item) = sessions.next().await {
        let frame = success(item?)?;
        let response = frame.view()?;
        for session in response.sessions.iter().take(response.count as usize) {
            result.sessions.borrow_mut().push(session.get());
        }
    }

    *result.stage.borrow_mut() = "open_session";
    let mut events = client.call::<OpenSession>(OpenSessionRequest {
        session: SessionIdDto::new(session.0),
    })?;
    *result.stage.borrow_mut() = "append";
    let append = session::append::AppendRequestFrame {
        session: SessionIdDto::new(session.0),
        text: FixedStr::new("hello").map_err(|_error| RpcError::InvalidFrameState)?,
    };
    let append_stream = RpcStream::new(futures_lite::stream::iter([Ok(append)]));
    *result.stage.borrow_mut() = "events";
    let item = events.next().await.ok_or(RpcError::InvalidFrameState)?;
    let frame = match item? {
        Ok(frame) => *frame.view()?,
        Err(error) => panic!("open session method error: {:?}", error.view()?),
    };
    let mut decoder = OpenSessionResponseDecoder::new();
    let response = decoder
        .push(frame)
        .unwrap_or_else(|error| panic!("invalid Opened event: {error}"))
        .expect("complete Opened event");
    assert!(matches!(response, OpenSessionResponse::Opened { .. }));
    success(
        client
            .call::<session::append::Append>(append_stream)?
            .await?,
    )?;

    loop {
        let Some(item) = events.next().await else {
            break;
        };
        let outcome = item.unwrap_or_else(|error| panic!("open stream transport error: {error:?}"));
        let frame = match outcome {
            Ok(frame) => *frame.view()?,
            Err(error) => panic!("open session method error: {:?}", error.view()?),
        };
        let Some(response) = decoder
            .push(frame)
            .unwrap_or_else(|error| panic!("invalid event frame: {error}"))
        else {
            continue;
        };
        match response {
            OpenSessionResponse::Event {
                event:
                    SessionEventDto::OutputDelta { text } | SessionEventDto::EffectOutputDelta { text },
                ..
            } => {
                result.output.borrow_mut().push_str(&text);
            }
            OpenSessionResponse::Event {
                event: SessionEventDto::TurnEnded { .. },
                ..
            } => break,
            _ => {}
        }
    }

    *result.stage.borrow_mut() = "close";
    success(
        client
            .call::<session::close::Close>(session::close::CloseRequest {
                session: SessionIdDto::new(session.0),
            })?
            .await?,
    )?;
    *result.stage.borrow_mut() = "done";
    Ok(())
}

#[test]
fn session_open_chunks_and_recovers_long_tool_results() {
    let session = SessionId::new(77);
    let response = OpenSessionResponse::Event {
        session,
        event: SessionEventDto::ToolResult {
            call: barracuda_agent_runtime::ToolCall {
                id: "call-1".into(),
                name: "large-tool".into(),
                arguments_json: format!(r#"{{"input":"{}"}}"#, "a".repeat(900)),
            },
            output: ToolOutputDto {
                content: "result".repeat(400),
                ok: true,
            },
        },
    };

    let frames = frames_from_open_session_response(session, &response).expect("encode event");
    assert!(frames.len() > 4);

    let mut decoder = OpenSessionResponseDecoder::new();
    let decoded = frames
        .into_iter()
        .find_map(|frame| decoder.push(frame).expect("decode frame"))
        .expect("complete event");
    assert_eq!(decoded, response);
}

fn success<T, E>(outcome: Result<RpcFrame<T>, RpcFrame<E>>) -> Result<RpcFrame<T>, RpcError> {
    outcome.map_err(|_error| RpcError::InvalidFrameState)
}

#[test]
fn public_session_rpcs_drive_the_existing_agent_api() {
    futures_lite::future::block_on(async {
        let event = r#"data: {"choices":[{"delta":{"content":"session reply"}}]}

data: [DONE]

"#;
        let network: &'static ScriptedStack =
            NETWORK.init(ScriptedStack::new([ScriptStep::sse(200, &[event])]));
        let factory = ModelApiFactory::new(move || ModelApi::new(network, network, 4096, 1024));
        let (runtime, service) = AgentRuntime::<ScriptedStack, ScriptedStack>::new(
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

        let result = Rc::new(ResultState::default());
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        let mut router = EventRouter::<8, 512, 8>::new(lanes)
            .await
            .expect("build Event Router");
        router
            .load(Box::new(AgentComponent::new(runtime, service)))
            .expect("load Agent Component");
        router
            .load(Box::new(SessionApiClient {
                result: Rc::clone(&result),
            }))
            .expect("load Session API client");

        drive_until(&mut router, &result, || !result.output.borrow().is_empty()).await;

        assert_eq!(result.sessions.borrow().as_slice(), &[1]);
        assert_eq!(result.output.borrow().as_str(), "session reply");
    });
}

async fn drive_until(
    router: &mut EventRouter<8, 512, 8>,
    result: &ResultState,
    ready: impl Fn() -> bool,
) {
    core::future::poll_fn(|context| {
        if let Poll::Ready(poll_result) = Pin::new(&mut *router).poll(context) {
            if let Err(error) = poll_result {
                panic!(
                    "Event Router failed during {}: {error}",
                    result.stage.borrow()
                );
            }
        }
        if ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}
