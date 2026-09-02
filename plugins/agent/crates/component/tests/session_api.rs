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
use barracuda_agent_component::delete_session::{DeleteSession, DeleteSessionRequest};
use barracuda_agent_component::dto::{
    FixedStr, InputRequestIdDto, PermissionLevelDto, ReasoningEffortDto, SessionIdDto,
    SessionPersistenceDto, SessionRpcError,
};
use barracuda_agent_component::list_sessions::ListSessions;
use barracuda_agent_component::new_session::{NewSession, NewSessionRequest};
use barracuda_agent_component::open_session::{
    frames_from_open_session_response, OpenSession, OpenSessionError, OpenSessionRequest,
    OpenSessionResponse, OpenSessionResponseDecoder, OpenSessionResponseField,
    SessionCloseReasonDto, SessionEventDto, ToolOutputDto,
};
use barracuda_agent_component::session;
use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, InputRequestId, InputRequestKind, IterationEvent,
    IterationId, ModelApiFactory, RuntimeStorageConfig, SessionCloseReason, SessionEvent,
    SessionEventError, SessionId, ToolCall, ToolOutput, TurnEvent, TurnId, TurnOrigin,
};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, RegisterContext,
    RpcError, RpcFrame, RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{install_global_memory_vfs, memory_vfs, ScriptStep, ScriptedStack};
use http_client::ClientFactory;
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

    *result.stage.borrow_mut() = "configure";
    success(
        client
            .call::<session::set_reasoning_effort::SetReasoningEffort>(
                session::set_reasoning_effort::SetReasoningEffortRequest {
                    session: SessionIdDto::new(session.0),
                    effort: ReasoningEffortDto::High,
                },
            )?
            .await?,
    )?;
    success(
        client
            .call::<session::set_permission_level::SetPermissionLevel>(
                session::set_permission_level::SetPermissionLevelRequest {
                    session: SessionIdDto::new(session.0),
                    level: PermissionLevelDto::Deny,
                },
            )?
            .await?,
    )?;
    success(
        client
            .call::<session::interrupt::Interrupt>(session::interrupt::InterruptRequest {
                session: SessionIdDto::new(session.0),
            })?
            .await?,
    )?;
    success(
        client
            .call::<session::cancel::Cancel>(session::cancel::CancelRequest {
                session: SessionIdDto::new(session.0),
            })?
            .await?,
    )?;
    let respond = session::respond::RespondRequestFrame {
        session: SessionIdDto::new(session.0),
        request: InputRequestIdDto::new(99),
        text: FixedStr::new("unsolicited").map_err(|_error| RpcError::InvalidFrameState)?,
    };
    let respond = RpcStream::new(futures_lite::stream::iter([Ok(respond)]));
    let response = client.call::<session::respond::Respond>(respond)?.await?;
    let Err(error) = response else {
        return Err(RpcError::InvalidFrameState);
    };
    assert_eq!(*error.view()?, SessionRpcError::NotAwaitingInput);

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
    let closed_control = client
        .call::<session::cancel::Cancel>(session::cancel::CancelRequest {
            session: SessionIdDto::new(session.0),
        })?
        .await?;
    let Err(error) = closed_control else {
        return Err(RpcError::InvalidFrameState);
    };
    assert_eq!(*error.view()?, SessionRpcError::SessionNotOpen);

    *result.stage.borrow_mut() = "reopen";
    let mut reopened = client.call::<OpenSession>(OpenSessionRequest {
        session: SessionIdDto::new(session.0),
    })?;
    let opened = reopened.next().await.ok_or(RpcError::InvalidFrameState)?;
    let opened = success(opened?)?;
    let mut reopened_decoder = OpenSessionResponseDecoder::new();
    assert!(matches!(
        reopened_decoder.push(*opened.view()?),
        Ok(Some(OpenSessionResponse::Opened { session: opened })) if opened == session
    ));

    let mut duplicate = client.call::<OpenSession>(OpenSessionRequest {
        session: SessionIdDto::new(session.0),
    })?;
    let duplicate = duplicate
        .next()
        .await
        .ok_or(RpcError::InvalidFrameState)??;
    let Err(error) = duplicate else {
        return Err(RpcError::InvalidFrameState);
    };
    assert_eq!(*error.view()?, OpenSessionError::AlreadyOpen);
    success(
        client
            .call::<session::close::Close>(session::close::CloseRequest {
                session: SessionIdDto::new(session.0),
            })?
            .await?,
    )?;

    *result.stage.borrow_mut() = "delete";
    success(
        client
            .call::<DeleteSession>(DeleteSessionRequest {
                session: SessionIdDto::new(session.0),
            })?
            .await?,
    )?;
    let mut remaining = client.call::<ListSessions>(())?;
    let empty = remaining.next().await.ok_or(RpcError::InvalidFrameState)?;
    assert_eq!(success(empty?)?.view()?.count, 0);
    assert!(remaining.next().await.is_none());

    let missing_delete = client
        .call::<DeleteSession>(DeleteSessionRequest {
            session: SessionIdDto::new(session.0),
        })?
        .await?;
    assert!(missing_delete.is_err());
    let mut missing_open = client.call::<OpenSession>(OpenSessionRequest {
        session: SessionIdDto::new(session.0),
    })?;
    let missing_open = missing_open
        .next()
        .await
        .ok_or(RpcError::InvalidFrameState)??;
    let Err(error) = missing_open else {
        return Err(RpcError::InvalidFrameState);
    };
    assert_eq!(*error.view()?, OpenSessionError::SessionNotFound);
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

#[test]
fn session_open_decoder_rejects_corrupt_or_interleaved_messages_and_recovers() {
    let base = frames_from_open_session_response(
        SessionId::new(77),
        &OpenSessionResponse::Opened {
            session: SessionId::new(77),
        },
    )
    .expect("base response encodes")[0];
    let frame = |field, value| {
        let mut frame = base;
        frame.field = field;
        frame.value = FixedStr::new(value).expect("fixture fits");
        frame
    };
    let mut decoder = OpenSessionResponseDecoder::new();

    assert_eq!(
        decoder.push(frame(OpenSessionResponseField::EventMore, "{")),
        Ok(None)
    );
    assert!(matches!(
        decoder.push(frame(OpenSessionResponseField::OpenedComplete, "}")),
        Err(barracuda_agent_component::open_session::OpenSessionDecodeError::InvalidSequence)
    ));
    assert!(matches!(
        decoder.push(frame(OpenSessionResponseField::EventComplete, "not-json")),
        Err(barracuda_agent_component::open_session::OpenSessionDecodeError::InvalidJson)
    ));

    let opened_json = serde_json::to_string(&OpenSessionResponse::Opened {
        session: SessionId::new(77),
    })
    .expect("response serializes");
    assert!(matches!(
        decoder.push(frame(OpenSessionResponseField::EventComplete, &opened_json)),
        Err(barracuda_agent_component::open_session::OpenSessionDecodeError::InvalidSequence)
    ));
    assert!(matches!(
        decoder.push(frame(OpenSessionResponseField::OpenedComplete, &opened_json)),
        Ok(Some(OpenSessionResponse::Opened { session })) if session == SessionId::new(77)
    ));

    let utf8_response = OpenSessionResponse::Event {
        session: SessionId::new(77),
        event: SessionEventDto::OutputDelta {
            text: "对话🦀".repeat(300),
        },
    };
    let frames = frames_from_open_session_response(SessionId::new(77), &utf8_response)
        .expect("UTF-8 response chunks without splitting code points");
    assert!(frames.len() > 1);
    let decoded = frames
        .into_iter()
        .find_map(|frame| decoder.push(frame).expect("chunk sequence decodes"))
        .expect("final chunk completes response");
    assert_eq!(decoded, utf8_response);
}

#[test]
fn session_event_transport_projects_the_complete_public_lifecycle_vocabulary() {
    let call = ToolCall {
        id: "call-7".into(),
        name: "search".into(),
        arguments_json: r#"{"query":"rust"}"#.into(),
    };
    let cases = [
        (
            SessionEvent::Turn(TurnEvent::Started {
                turn: TurnId::new(1),
                origin: TurnOrigin::User,
            }),
            SessionEventDto::TurnStarted {
                turn: TurnId::new(1),
                origin: TurnOrigin::User,
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::Started {
                turn: TurnId::new(2),
                origin: TurnOrigin::ToolCall { call: call.clone() },
            }),
            SessionEventDto::TurnStarted {
                turn: TurnId::new(2),
                origin: TurnOrigin::ToolCall { call: call.clone() },
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::InputRequested {
                request: InputRequestId::new(3),
                kind: InputRequestKind::PermissionApproval {
                    tool_call: call.clone(),
                    reason: "network access".into(),
                },
            }),
            SessionEventDto::InputRequested {
                request: InputRequestId::new(3),
                kind: InputRequestKind::PermissionApproval {
                    tool_call: call.clone(),
                    reason: "network access".into(),
                },
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Started {
                iteration: IterationId::new(4),
            })),
            SessionEventDto::IterationStarted {
                iteration: IterationId::new(4),
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Reasoning(
                StreamPart::Delta("thinking".into()),
            ))),
            SessionEventDto::ReasoningDelta {
                text: "thinking".into(),
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Reasoning(
                StreamPart::End,
            ))),
            SessionEventDto::ReasoningEnded,
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                StreamPart::Delta("answer".into()),
            ))),
            SessionEventDto::OutputDelta {
                text: "answer".into(),
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                StreamPart::End,
            ))),
            SessionEventDto::OutputEnded,
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                StreamPart::Delta((
                    call.clone(),
                    ToolOutput {
                        content: "found".into(),
                        ok: true,
                    },
                )),
            ))),
            SessionEventDto::ToolResult {
                call: call.clone(),
                output: ToolOutputDto {
                    content: "found".into(),
                    ok: true,
                },
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                StreamPart::End,
            ))),
            SessionEventDto::ToolResultsEnded,
        ),
        (
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Ended)),
            SessionEventDto::IterationEnded,
        ),
        (
            SessionEvent::Turn(TurnEvent::EffectOutput(StreamPart::Delta("effect".into()))),
            SessionEventDto::EffectOutputDelta {
                text: "effect".into(),
            },
        ),
        (
            SessionEvent::Turn(TurnEvent::EffectOutput(StreamPart::End)),
            SessionEventDto::EffectOutputEnded,
        ),
        (
            SessionEvent::Turn(TurnEvent::Ended {
                turn: TurnId::new(1),
            }),
            SessionEventDto::TurnEnded {
                turn: TurnId::new(1),
            },
        ),
        (
            SessionEvent::Error(SessionEventError::DeleteFailed),
            SessionEventDto::SessionError {
                message: "session deletion failed".into(),
            },
        ),
        (
            SessionEvent::Closed(SessionCloseReason::Requested),
            SessionEventDto::Closed {
                reason: SessionCloseReasonDto::Requested,
            },
        ),
        (
            SessionEvent::Closed(SessionCloseReason::Deleted),
            SessionEventDto::Closed {
                reason: SessionCloseReasonDto::Deleted,
            },
        ),
        (
            SessionEvent::Closed(SessionCloseReason::RuntimeShutdown),
            SessionEventDto::Closed {
                reason: SessionCloseReasonDto::RuntimeShutdown,
            },
        ),
    ];

    for (event, expected) in cases {
        assert_eq!(SessionEventDto::from(event), expected);
    }
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

        drive_until(&mut router, &result, || *result.stage.borrow() == "done").await;

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
