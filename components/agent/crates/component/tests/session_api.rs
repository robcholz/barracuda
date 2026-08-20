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
use barracuda_agent_component::link_api::{frames_from_link_api_request, LinkApi, LinkApiRequest};
use barracuda_agent_component::list_sessions::ListSessions;
use barracuda_agent_component::new_session::{NewSession, NewSessionRequest};
use barracuda_agent_component::open_session::{
    open_session_response_from_frames, OpenSession, OpenSessionRequest, OpenSessionResponse,
    OpenSessionResponseFrame, SessionEventDto,
};
use barracuda_agent_component::session;
use barracuda_agent_runtime::{
    AgentRuntime, ApiPurpose, BackendKind, Message, ModelApiConfig, ModelApiFactory,
    RuntimeStorageConfig, SessionPersistence,
};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, MemFs,
    RegisterContext, RpcError, RpcFrame, RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_model_api::ModelApi;
use barracuda_net::testing::{ScriptStep, ScriptedStack};
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

impl Component<128> for SessionApiClient {
    fn register(&mut self, _context: &mut RegisterContext<'_, 128>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<128>) -> ComponentFuture<'a> {
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
    context: RunContext<128>,
    result: Rc<ResultState>,
) -> Result<(), RpcError> {
    let client = context.rpc();
    *result.stage.borrow_mut() = "link_api";
    let link = LinkApiRequest::new(
        ModelApiConfig::new(
            BackendKind::OpenAiCompatible,
            "test-key",
            "test-model",
            "http://example.invalid",
        ),
        ApiPurpose::RootAgent,
        true,
    );
    let link_frames = RpcStream::new(futures_lite::stream::iter(
        frames_from_link_api_request(&link)
            .map_err(|_error| RpcError::InvalidFrameState)?
            .into_iter()
            .map(Ok),
    ));
    success(client.call::<LinkApi>(link_frames)?.await?)?;

    *result.stage.borrow_mut() = "new_session";
    let session = success(
        client
            .call::<NewSession>(NewSessionRequest::new(SessionPersistence::Ephemeral))?
            .await?,
    )?
    .view()?
    .session();

    *result.stage.borrow_mut() = "list_sessions";
    let mut sessions = client.call::<ListSessions>(())?;
    while let Some(item) = sessions.next().await {
        result
            .sessions
            .borrow_mut()
            .push(success(item?)?.view()?.session().0);
    }

    *result.stage.borrow_mut() = "open_session";
    let mut events = client.call::<OpenSession>(OpenSessionRequest::new(session))?;
    *result.stage.borrow_mut() = "append";
    let append = session::append::AppendRequest::new(session, Message::text("hello"));
    let append_frames = RpcStream::new(futures_lite::stream::iter(
        session::append::frames_from_append_request(&append)
            .map_err(|_error| RpcError::InvalidFrameState)?
            .into_iter()
            .map(Ok),
    ));
    *result.stage.borrow_mut() = "events";
    let mut event_frames = Vec::<OpenSessionResponseFrame>::new();
    loop {
        let item = events.next().await.ok_or(RpcError::InvalidFrameState)?;
        let frame = match item? {
            Ok(frame) => *frame.view()?,
            Err(error) => panic!("open session method error: {:?}", error.view()?),
        };
        let end = frame.is_end();
        event_frames.push(frame);
        if end {
            let opened = open_session_response_from_frames(event_frames.drain(..))
                .unwrap_or_else(|error| panic!("invalid Opened frames: {error}"));
            assert!(matches!(opened, OpenSessionResponse::Opened { .. }));
            break;
        }
    }
    success(
        client
            .call::<session::append::Append>(append_frames)?
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
        let end = frame.is_end();
        event_frames.push(frame);
        if !end {
            continue;
        }
        let event = open_session_response_from_frames(event_frames.drain(..))
            .unwrap_or_else(|error| panic!("invalid Session event frames: {error}"));
        match event {
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
            .call::<session::close::Close>(session::close::CloseRequest::new(session))?
            .await?,
    )?;
    *result.stage.borrow_mut() = "done";
    Ok(())
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
        let factory = ModelApiFactory::new(move || ModelApi::new(network, 4096, 1024));
        let (runtime, service) = AgentRuntime::<MemFs, ScriptedStack>::new(
            MemFs::new(),
            RuntimeStorageConfig {
                persistence_root: "/agent".into(),
                skill_roots: Vec::new(),
            },
            factory,
        )
        .expect("build Agent runtime");

        let result = Rc::new(ResultState::default());
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 128, 8>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router =
            EventRouter::new(lanes, filesystem, "workflows").expect("build Event Router");
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
    router: &mut EventRouter<8, 128, 8>,
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
