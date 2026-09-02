use std::cell::Cell;
use std::rc::Rc;

use barracuda_agent_runtime::{
    tools::{EmptyArgs, Tool, ToolFuture, ToolGroup, ToolHandler, ToolOutput, ToolSpec},
    AgentRuntime, ApiPurpose, BackendKind, InputRequestId, InputRequestKind, IterationEvent,
    Message, ModelApiConfig, ModelApiFactory, OpenSessionError, PermissionLevel, ReasoningEffort,
    RuntimeError, RuntimeStorageConfig, SessionCloseReason, SessionControlError, SessionEvent,
    SessionId, SessionPersistence, TurnEvent, TurnEventError,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{memory_vfs, NeverStack, ScriptStep, ScriptedStack};
use barracuda_runtime_utils::stream::StreamPart;
use futures_lite::future::{block_on, zip};
use futures_lite::StreamExt;
use http_client::ClientFactory;

static NETWORK: NeverStack = NeverStack;

fn tool_step(id: &str, name: &str, arguments: &str) -> ScriptStep {
    let arguments = serde_json::to_string(arguments).expect("tool arguments encode");
    let body = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{arguments}}}}}]}}}}]}}\n\ndata: [DONE]\n\n"
    );
    ScriptStep::sse(200, &[&body])
}

fn text_step(text: &str) -> ScriptStep {
    let text = serde_json::to_string(text).expect("response text encodes");
    let body =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{text}}}}}]}}\n\ndata: [DONE]\n\n");
    ScriptStep::sse(200, &[&body])
}

struct RiskyChangeTool {
    invocations: Rc<Cell<u32>>,
}

impl ToolSpec for RiskyChangeTool {
    fn name(&self) -> &str {
        "risky_change"
    }

    fn schema(&self) -> &str {
        include_str!("risky_change.schema.json")
    }

    fn arguments_validator(&self) -> &'static json_validator::Validator {
        const VALIDATOR: json_validator::Validator =
            json_validator::validator!("tests/risky_change.schema.json");
        &VALIDATOR
    }
}

impl ToolHandler for RiskyChangeTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        self.invocations.set(self.invocations.get() + 1);
        Box::pin(async {
            Ok(ToolOutput {
                content: "change applied".into(),
                ok: true,
            })
        })
    }
}

#[test]
fn executor_neutral_service_drives_public_session_api() {
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            assert_eq!(runtime.list_sessions().await, vec![session]);
            runtime
                .delete_session(session)
                .await
                .expect("session deletes");
            assert!(runtime.list_sessions().await.is_empty());
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn session_lease_controls_lifecycle_and_rejects_stale_callers() {
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");

    block_on(async move {
        let scenario = async {
            let missing = SessionId::new(999);
            assert!(matches!(
                runtime.open_session(missing).await,
                Err(RuntimeError::OpenSession(OpenSessionError::SessionNotFound(id))) if id == missing
            ));

            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            assert!(matches!(
                runtime.open_session(session).await,
                Err(RuntimeError::OpenSession(OpenSessionError::AlreadyOpen(id))) if id == session
            ));

            control
                .set_reasoning_effort(ReasoningEffort::High)
                .await
                .expect("reasoning effort updates");
            control
                .set_permission_level(PermissionLevel::Deny)
                .await
                .expect("permission level updates");
            control.interrupt().await.expect("idle interrupt succeeds");
            control.cancel().await.expect("idle cancel succeeds");
            assert!(matches!(
                control
                    .respond(InputRequestId::new(1), Message::text("not requested"))
                    .await,
                Err(SessionControlError::NotAwaitingInput(id)) if id == session
            ));

            control
                .append(Message::text("   "))
                .await
                .expect("blank message is accepted as an empty turn");
            assert!(matches!(
                events.next().await,
                Some(Ok(SessionEvent::Turn(TurnEvent::Started { .. })))
            ));
            assert!(matches!(
                events.next().await,
                Some(Ok(SessionEvent::Turn(TurnEvent::Ended { .. })))
            ));

            control.close().await.expect("open lease closes");
            assert!(matches!(
                events.next().await,
                Some(Ok(SessionEvent::Closed(SessionCloseReason::Requested)))
            ));
            assert!(events.next().await.is_none());
            assert!(matches!(
                control.append(Message::text("stale")).await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));
            assert!(matches!(
                control
                    .respond(InputRequestId::new(1), Message::text("stale"))
                    .await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));
            assert!(matches!(
                control.set_reasoning_effort(ReasoningEffort::Low).await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));
            assert!(matches!(
                control.set_permission_level(PermissionLevel::Ask).await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));
            assert!(matches!(
                control.interrupt().await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));
            assert!(matches!(
                control.cancel().await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));
            assert!(matches!(
                control.close().await,
                Err(SessionControlError::SessionClosed(id)) if id == session
            ));

            let (_reopened, mut reopened_events) = runtime
                .open_session(session)
                .await
                .expect("closed session can be reopened with a fresh lease");
            runtime
                .delete_session(session)
                .await
                .expect("open session deletes");
            assert!(matches!(
                reopened_events.next().await,
                Some(Ok(SessionEvent::Closed(SessionCloseReason::Deleted)))
            ));
            assert!(runtime.list_sessions().await.is_empty());

            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn queued_messages_run_in_fifo_order_on_one_persistent_root_agent() {
    let first = r#"data: {"choices":[{"delta":{"content":"first reply"}}]}

data: [DONE]

"#;
    let second = r#"data: {"choices":[{"delta":{"content":"second reply"}}]}

data: [DONE]

"#;
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[first]),
        ScriptStep::sse(200, &[second]),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
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
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .append(Message::text("first question"))
                .await
                .expect("first message queues");
            control
                .append(Message::text("second question"))
                .await
                .expect("second message queues while the first is active");

            let mut output = String::new();
            let mut ended = 0;
            while ended < 2 {
                match events.next().await.expect("event stream remains open") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Error(error))) => {
                        panic!("queued turn failed: {error}")
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => ended += 1,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }

            assert_eq!(output, "first replysecond reply");
            assert_eq!(network.connect_count(), 1);
            assert_eq!(network.requests().len(), 2);
            control.close().await.expect("session closes");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn risky_tool_waits_for_the_matching_approval_before_execution() {
    let request_tool = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-risky\",",
        "\"function\":{\"name\":\"risky_change\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let resolve_approval = concat!(
        "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":null,",
        "\"tool_calls\":[{\"id\":\"call-approval\",\"type\":\"function\",",
        "\"function\":{\"name\":\"permission_resolve_reply\",",
        "\"arguments\":\"{\\\"decision\\\":\\\"yes\\\"}\"}}]}}]}"
    );
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"approved and complete\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[request_tool]),
        ScriptStep::json(200, resolve_approval),
        ScriptStep::sse(200, &[finish]),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let invocations = Rc::new(Cell::new(0));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::with_tool_groups(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
        [ToolGroup::new(
            "test",
            true,
            [Tool::new(RiskyChangeTool {
                invocations: Rc::clone(&invocations),
            })],
        )],
    )
    .expect("runtime builds");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .set_permission_level(PermissionLevel::Ask)
                .await
                .expect("session asks before side effects");
            control
                .append(Message::text("make the risky change"))
                .await
                .expect("message queues");

            let request = loop {
                match events.next().await.expect("approval event arrives") {
                    Ok(SessionEvent::Turn(TurnEvent::InputRequested {
                        request,
                        kind: InputRequestKind::PermissionApproval { tool_call, reason },
                    })) => {
                        assert_eq!(tool_call.id, "call-risky");
                        assert_eq!(tool_call.name, "risky_change");
                        assert!(reason.contains("High"));
                        break request;
                    }
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            };
            assert_eq!(invocations.get(), 0);

            let wrong = InputRequestId::new(request.0 + 1);
            assert!(matches!(
                control.respond(wrong, Message::text("yes")).await,
                Err(SessionControlError::InputRequestMismatch {
                    session: error_session,
                    expected,
                    received,
                }) if error_session == session && expected == request && received == wrong
            ));
            control
                .respond(request, Message::text("yes, proceed"))
                .await
                .expect("matching approval is accepted");

            let mut output = String::new();
            let mut tool_result = None;
            loop {
                match events.next().await.expect("turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) => tool_result = Some((call.name, result.content, result.ok)),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }

            assert_eq!(invocations.get(), 1);
            assert_eq!(
                tool_result,
                Some(("risky_change".into(), "change applied".into(), true))
            );
            assert_eq!(output, "approved and complete");
            assert_eq!(network.requests().len(), 3);
            control.close().await.expect("session closes");
            runtime.stop_all().expect("tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn interrupt_cancels_a_pending_approval_and_the_session_accepts_a_new_turn() {
    let request_tool = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-interrupt\",",
        "\"function\":{\"name\":\"risky_change\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[request_tool]),
        text_step("recovered after interrupted approval"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let invocations = Rc::new(Cell::new(0));
    let (runtime, service) = AgentRuntime::with_tool_groups(
        block_on(memory_vfs()).expect("memory VFS mounts"),
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
        [ToolGroup::new(
            "test",
            true,
            [Tool::new(RiskyChangeTool {
                invocations: Rc::clone(&invocations),
            })],
        )],
    )
    .expect("runtime builds");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .set_permission_level(PermissionLevel::Ask)
                .await
                .expect("approval policy installs");
            control
                .append(Message::text("start a risky action"))
                .await
                .expect("turn queues");
            let request = loop {
                match events.next().await.expect("approval arrives") {
                    Ok(SessionEvent::Turn(TurnEvent::InputRequested { request, .. })) => {
                        break request
                    }
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            };

            control
                .interrupt()
                .await
                .expect("pending approval interrupts");
            while !matches!(
                events.next().await,
                Some(Ok(SessionEvent::Turn(TurnEvent::Ended { .. })))
            ) {}
            assert_eq!(invocations.get(), 0);
            assert!(matches!(
                control.respond(request, Message::text("too late")).await,
                Err(SessionControlError::NotAwaitingInput(id)) if id == session
            ));

            control
                .set_permission_level(PermissionLevel::AllowAll)
                .await
                .expect("policy changes after interrupt");
            control
                .append(Message::text("continue with a safe answer"))
                .await
                .expect("recovery turn queues");
            let mut output = String::new();
            loop {
                match events.next().await.expect("recovery turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(output, "recovered after interrupted approval");
            assert_eq!(network.requests().len(), 2);
            control.close().await.expect("session closes");
            runtime.stop_all().expect("tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn failed_approval_resolution_ends_only_that_turn_and_deny_never_invokes_the_tool() {
    let request_tool = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-ask\",",
        "\"function\":{\"name\":\"risky_change\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let request_denied_tool = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-deny\",",
        "\"function\":{\"name\":\"risky_change\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[request_tool]),
        ScriptStep::json(200, r#"{"choices":[]}"#),
        text_step("approval failure handled"),
        ScriptStep::sse(200, &[request_denied_tool]),
        text_step("denial handled"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let invocations = Rc::new(Cell::new(0));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::with_tool_groups(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
        [ToolGroup::new(
            "test",
            true,
            [Tool::new(RiskyChangeTool {
                invocations: Rc::clone(&invocations),
            })],
        )],
    )
    .expect("runtime builds");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .set_permission_level(PermissionLevel::Ask)
                .await
                .expect("approval policy installs");
            control
                .append(Message::text("ask before changing"))
                .await
                .expect("approval turn queues");
            let request = loop {
                match events.next().await.expect("approval arrives") {
                    Ok(SessionEvent::Turn(TurnEvent::InputRequested { request, .. })) => {
                        break request
                    }
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            };
            control
                .respond(request, Message::text("yes"))
                .await
                .expect("reply is accepted for resolution");
            let mut resolution_failed = false;
            loop {
                match events.next().await.expect("failed approval turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Error(
                        TurnEventError::InputResolutionFailed {
                            request: failed, ..
                        },
                    ))) => {
                        assert_eq!(failed, request);
                        resolution_failed = true;
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(resolution_failed);
            assert_eq!(invocations.get(), 0);

            control
                .set_permission_level(PermissionLevel::Deny)
                .await
                .expect("deny policy installs");
            control
                .append(Message::text("deny the next change"))
                .await
                .expect("denied turn queues");
            let mut denied = None;
            let mut output = String::new();
            loop {
                match events.next().await.expect("denied turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) if call.id == "call-deny" => denied = Some((result.content, result.ok)),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(matches!(
                denied,
                Some((content, false)) if content.contains("denies side effects")
            ));
            assert_eq!(invocations.get(), 0);
            assert_eq!(output, "denial handled");
            assert_eq!(network.requests().len(), 5);

            control.close().await.expect("session closes");
            runtime.stop_all().expect("tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn invalid_and_unknown_tool_calls_are_returned_to_the_model_and_duplicate_ids_do_not_poison_session(
) {
    let invalid_calls = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"unknown-1\",\"function\":{\"name\":\"missing_tool\",\"arguments\":\"{}\"}},",
        "{\"index\":1,\"id\":\"invalid-1\",\"function\":{\"name\":\"risky_change\",\"arguments\":\"{\\\"extra\\\":1}\"}}",
        "]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let duplicate_ids = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"duplicate\",\"function\":{\"name\":\"risky_change\",\"arguments\":\"{}\"}},",
        "{\"index\":1,\"id\":\"duplicate\",\"function\":{\"name\":\"risky_change\",\"arguments\":\"{}\"}}",
        "]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[invalid_calls]),
        text_step("tool errors handled"),
        ScriptStep::sse(200, &[duplicate_ids]),
        text_step("session recovered"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let invocations = Rc::new(Cell::new(0));
    let (runtime, service) = AgentRuntime::with_tool_groups(
        block_on(memory_vfs()).expect("memory VFS mounts"),
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
        [ToolGroup::new(
            "test",
            true,
            [Tool::new(RiskyChangeTool {
                invocations: Rc::clone(&invocations),
            })],
        )],
    )
    .expect("runtime builds");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .set_permission_level(PermissionLevel::AllowAll)
                .await
                .expect("tools may run");
            control
                .append(Message::text("call invalid tools"))
                .await
                .expect("turn queues");
            let mut failures = Vec::new();
            let mut output = String::new();
            loop {
                match events.next().await.expect("tool-error turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) => failures.push((call.id, result.content, result.ok)),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(failures.len(), 2);
            assert!(failures.iter().all(|(_, _, ok)| !ok));
            assert!(failures
                .iter()
                .any(|(id, text, _)| { id == "unknown-1" && text.contains("missing_tool") }));
            assert!(failures
                .iter()
                .any(|(id, text, _)| { id == "invalid-1" && text.contains("extra") }));
            assert_eq!(output, "tool errors handled");
            assert_eq!(invocations.get(), 0);

            control
                .append(Message::text("duplicate provider ids"))
                .await
                .expect("duplicate-id turn queues");
            let mut saw_turn_error = false;
            loop {
                match events.next().await.expect("duplicate-id turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Error(_))) => saw_turn_error = true,
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(saw_turn_error);

            control
                .append(Message::text("recover after malformed provider output"))
                .await
                .expect("recovery turn queues");
            let mut recovered = String::new();
            loop {
                match events.next().await.expect("recovery turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => recovered.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(recovered, "session recovered");
            assert_eq!(network.requests().len(), 4);

            control.close().await.expect("session closes");
            runtime.stop_all().expect("tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn persistent_sessions_survive_restart_and_open_leases_observe_shutdown() {
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let storage = RuntimeStorageConfig {
        persistence_root: "/agent".into(),
        skill_roots: Vec::new(),
    };
    let first_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
    let (first_runtime, first_service) =
        AgentRuntime::new(filesystem.clone(), storage.clone(), first_factory)
            .expect("first runtime builds");

    let (persistent, ephemeral) = block_on(async move {
        let scenario = async {
            let persistent = first_runtime
                .new_session(SessionPersistence::Persistent)
                .await
                .expect("persistent session creates");
            let ephemeral = first_runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("ephemeral session creates");
            let (stale_control, mut events) = first_runtime
                .open_session(persistent)
                .await
                .expect("persistent session opens");

            first_runtime.shutdown().await;
            assert!(matches!(
                events.next().await,
                Some(Ok(SessionEvent::Closed(
                    SessionCloseReason::RuntimeShutdown
                )))
            ));
            assert!(events.next().await.is_none());
            (persistent, ephemeral, stale_control)
        };
        let (_, (persistent, ephemeral, stale_control)) = zip(first_service, scenario).await;
        assert_eq!(
            stale_control.append(Message::text("after shutdown")).await,
            Err(SessionControlError::WorkerStopped)
        );
        (persistent, ephemeral)
    });

    let second_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
    let (second_runtime, second_service) =
        AgentRuntime::new(filesystem, storage, second_factory).expect("second runtime builds");
    block_on(async move {
        let scenario = async {
            assert_eq!(second_runtime.list_sessions().await, vec![persistent]);
            assert!(matches!(
                second_runtime.open_session(ephemeral).await,
                Err(RuntimeError::OpenSession(OpenSessionError::SessionNotFound(id)))
                    if id == ephemeral
            ));
            let (control, mut events) = second_runtime
                .open_session(persistent)
                .await
                .expect("persistent session reopens");
            control.close().await.expect("restored session closes");
            assert!(matches!(
                events.next().await,
                Some(Ok(SessionEvent::Closed(SessionCloseReason::Requested)))
            ));
            second_runtime
                .delete_session(persistent)
                .await
                .expect("restored session deletes permanently");
            assert!(second_runtime.list_sessions().await.is_empty());
            second_runtime.shutdown().await;
        };
        let (_, ()) = zip(second_service, scenario).await;
    });
}

#[test]
fn persistent_root_agent_resumes_its_transcript_and_session_policy_after_restart() {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        text_step("first answer"),
        text_step("second answer"),
    ])));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let storage = RuntimeStorageConfig {
        persistence_root: "/agent".into(),
        skill_roots: Vec::new(),
    };
    let factory = || {
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)))
    };

    let (first_runtime, first_service) =
        AgentRuntime::new(filesystem.clone(), storage.clone(), factory())
            .expect("first runtime builds");
    first_runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("first model API configures");

    let session = block_on(async move {
        let scenario = async {
            let session = first_runtime
                .new_session(SessionPersistence::Persistent)
                .await
                .expect("persistent session creates");
            let (control, mut events) = first_runtime
                .open_session(session)
                .await
                .expect("session opens");
            control
                .set_reasoning_effort(ReasoningEffort::High)
                .await
                .expect("reasoning persists");
            control
                .set_permission_level(PermissionLevel::Deny)
                .await
                .expect("permission persists");
            control
                .append(Message::text("first question"))
                .await
                .expect("first turn starts");
            while !matches!(
                events.next().await,
                Some(Ok(SessionEvent::Turn(TurnEvent::Ended { .. })))
            ) {}
            control.close().await.expect("first lease closes cleanly");
            assert!(matches!(
                events.next().await,
                Some(Ok(SessionEvent::Closed(SessionCloseReason::Requested)))
            ));
            first_runtime.shutdown().await;
            session
        };
        zip(first_service, scenario).await.1
    });

    let (second_runtime, second_service) = AgentRuntime::new(filesystem, storage, factory())
        .expect("second runtime resumes persistent state");
    second_runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("second model API configures");
    block_on(async move {
        let scenario = async {
            let (control, mut events) = second_runtime
                .open_session(session)
                .await
                .expect("restored session opens");
            control
                .set_reasoning_effort(ReasoningEffort::High)
                .await
                .expect("same effort remains accepted");
            control
                .set_permission_level(PermissionLevel::Deny)
                .await
                .expect("same permission remains accepted");
            control
                .append(Message::text("second question"))
                .await
                .expect("restored agent runs");
            let mut output = String::new();
            loop {
                match events.next().await.expect("restored turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Error(error))) => {
                        panic!("restored turn failed: {error}")
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(output, "second answer");
            let requests = network.requests();
            assert_eq!(requests.len(), 2);
            let restored_request = requests.get(1).expect("second model request");
            assert!(restored_request.contains("first question"));
            assert!(restored_request.contains("first answer"));
            second_runtime
                .delete_session(session)
                .await
                .expect("restored persistent session deletes");
            second_runtime.shutdown().await;
        };
        let (_, ()) = zip(second_service, scenario).await;
    });
}

#[test]
fn conversation_end_emits_effect_output_and_finishes_without_an_extra_model_call() {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([tool_step(
        "end",
        "conversation_end",
        r#"{"final_message":"  This conversation is closed.  "}"#,
    )])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
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
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("built-in tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .append(Message::text("end this unsafe conversation"))
                .await
                .expect("turn starts");
            let mut effect_parts = Vec::new();
            loop {
                match events.next().await.expect("turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::EffectOutput(part))) => {
                        effect_parts.push(part)
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(
                effect_parts,
                vec![
                    StreamPart::Delta("This conversation is closed.".into()),
                    StreamPart::End,
                ]
            );
            assert_eq!(network.requests().len(), 1);
            control.close().await.expect("session closes");
            runtime.stop_all().expect("built-in tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn aged_conversation_is_summarized_before_the_next_model_request() {
    let compaction = r#"{"choices":[{"message":{"role":"assistant","content":"The early discussion established the migration constraints."}}]}"#;
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        text_step("first acknowledged"),
        text_step("second acknowledged"),
        ScriptStep::json(200, compaction),
        text_step("summary applied"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");
    for purpose in [ApiPurpose::RootAgent, ApiPurpose::Compaction] {
        runtime
            .set_api(
                ModelApiConfig::new(
                    BackendKind::OpenAiCompatible,
                    "test-key",
                    "test-model",
                    "http://llm.test/v1",
                ),
                purpose,
                purpose == ApiPurpose::RootAgent,
            )
            .expect("model API configures");
    }

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            let first = format!("FIRST-AGED {}", "x".repeat(30_000));
            let second = format!("SECOND-RECENT {}", "y".repeat(9_000));
            for message in [first, second, "use the earlier context now".into()] {
                control
                    .append(Message::text(message))
                    .await
                    .expect("turn queues");
                loop {
                    match events.next().await.expect("turn completes") {
                        Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                        Ok(_) => {}
                        Err(error) => panic!("session stream failed: {error}"),
                    }
                }
            }

            let requests = network.requests();
            assert_eq!(requests.len(), 4);
            assert!(requests.get(2).is_some_and(|request| {
                request.contains("FIRST-AGED")
                    && request.contains("Summarize the following conversation")
            }));
            assert!(requests.get(3).is_some_and(|request| {
                request.contains("Summary of earlier conversation")
                    && request.contains("SECOND-RECENT")
                    && !request.contains("FIRST-AGED")
            }));

            control.close().await.expect("session closes");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn committed_conversation_batch_is_extracted_through_the_memory_model_and_live_tools() {
    let extraction = r#"{
        "choices":[{"message":{
            "role":"assistant",
            "content":null,
            "tool_calls":[{
                "id":"extract-store",
                "type":"function",
                "function":{
                    "name":"memory_store",
                    "arguments":"{\"content\":\"User prefers jasmine tea\",\"tags\":[\"preference\"],\"keywords\":[\"tea\"]}"
                }
            }]
        }}]
    }"#;
    let mut steps = (0..8)
        .map(|index| text_step(&format!("acknowledged {index}")))
        .collect::<Vec<_>>();
    steps.push(ScriptStep::json(200, extraction));
    steps.push(tool_step("memory-check", "memory_list", r#"{"limit":10}"#));
    steps.push(text_step("memory extraction complete"));
    for index in 0..7 {
        steps.push(text_step(&format!("later acknowledged {index}")));
    }
    steps.push(ScriptStep::json(
        200,
        r#"{
            "choices":[{"message":{
                "role":"assistant",
                "content":null,
                "tool_calls":[
                    {"id":"extract-update","type":"function","function":{"name":"memory_update","arguments":"{\"id\":\"g-0\",\"content\":\"User prefers green tea\"}"}},
                    {"id":"extract-store-task","type":"function","function":{"name":"memory_store","arguments":"{\"content\":\"Current task uses a canary deployment\",\"tags\":[\"task\"],\"keywords\":[\"deploy\"]}"}},
                    {"id":"extract-forget","type":"function","function":{"name":"memory_forget","arguments":"{\"id\":\"g-0\"}"}}
                ]
            }}]
        }"#,
    ));
    steps.push(tool_step(
        "memory-check-2",
        "memory_list",
        r#"{"limit":10}"#,
    ));
    steps.push(text_step("second extraction complete"));
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(steps)));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");
    for purpose in [ApiPurpose::RootAgent, ApiPurpose::Memory] {
        runtime
            .set_api(
                ModelApiConfig::new(
                    BackendKind::OpenAiCompatible,
                    "test-key",
                    "test-model",
                    "http://llm.test/v1",
                ),
                purpose,
                purpose == ApiPurpose::RootAgent,
            )
            .expect("model API configures");
    }
    runtime.start_all().expect("built-in tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            for index in 0..8 {
                control
                    .append(Message::text(format!(
                        "conversation fact batch {index}: the user discusses tea"
                    )))
                    .await
                    .expect("batch turn queues");
                loop {
                    match events.next().await.expect("batch turn completes") {
                        Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                        Ok(_) => {}
                        Err(error) => panic!("session stream failed: {error}"),
                    }
                }
            }

            control
                .append(Message::text("show what the extractor remembered"))
                .await
                .expect("extraction turn queues");
            let mut memory_result = None;
            let mut output = String::new();
            loop {
                match events.next().await.expect("extraction turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) if call.name == "memory_list" => {
                        memory_result = Some((result.content, result.ok));
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }

            assert!(matches!(
                memory_result,
                Some((content, true))
                    if content.contains("User prefers jasmine tea")
                        && content.contains("preference")
            ));
            assert_eq!(output, "memory extraction complete");
            let requests = network.requests();
            assert_eq!(requests.len(), 11);
            let extraction_request = requests.get(8).expect("memory extraction request");
            assert!(extraction_request.contains("CURRENT MEMORY:"));
            assert!(extraction_request.contains("(none)"));
            assert!(extraction_request.contains("conversation fact batch 0"));
            assert!(extraction_request.contains("conversation fact batch 7"));
            assert!(extraction_request.contains("memory_store"));
            let root_after_extraction = requests.get(9).expect("root request after extraction");
            assert!(root_after_extraction.contains("Shared long-term memory topics: preference"));

            for index in 0..7 {
                control
                    .append(Message::text(format!(
                        "later conversation batch {index}: deployment details"
                    )))
                    .await
                    .expect("later batch turn queues");
                loop {
                    match events.next().await.expect("later batch turn completes") {
                        Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                        Ok(_) => {}
                        Err(error) => panic!("session stream failed: {error}"),
                    }
                }
            }

            control
                .append(Message::text("reconcile the next memory batch"))
                .await
                .expect("second extraction turn queues");
            let mut second_memory_result = None;
            let mut second_output = String::new();
            loop {
                match events
                    .next()
                    .await
                    .expect("second extraction turn completes")
                {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) if call.name == "memory_list" => {
                        second_memory_result = Some((result.content, result.ok));
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => second_output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(matches!(
                second_memory_result,
                Some((content, true))
                    if content.contains("Current task uses a canary deployment")
                        && !content.contains("jasmine tea")
                        && !content.contains("green tea")
            ));
            assert_eq!(second_output, "second extraction complete");
            let requests = network.requests();
            assert_eq!(requests.len(), 21);
            let second_extraction = requests.get(18).expect("second memory extraction request");
            assert!(second_extraction.contains("g-0: User prefers jasmine tea [preference]"));
            assert!(second_extraction.contains("later conversation batch 0"));
            assert!(second_extraction.contains("later conversation batch 6"));
            let root_after_second = requests
                .get(19)
                .expect("root request after second extraction");
            assert!(root_after_second.contains("Your long-term memory topics: task"));
            assert!(!root_after_second.contains("Shared long-term memory topics: preference"));

            control.close().await.expect("session closes");
            runtime.stop_all().expect("built-in tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn a_failed_unconfigured_turn_does_not_poison_the_session() {
    let response = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack =
        Box::leak(Box::new(ScriptedStack::new([ScriptStep::sse(
            200,
            &[response],
        )])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .append(Message::text("fails before configuration"))
                .await
                .expect("first message queues");

            let mut saw_execution_error = false;
            loop {
                match events.next().await.expect("failed turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Error(TurnEventError::Execution(_)))) => {
                        saw_execution_error = true;
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(saw_execution_error);
            assert_eq!(network.connect_count(), 0);

            runtime
                .set_api(
                    ModelApiConfig::new(
                        BackendKind::OpenAiCompatible,
                        "test-key",
                        "test-model",
                        "http://llm.test/v1",
                    ),
                    ApiPurpose::RootAgent,
                    true,
                )
                .expect("model API configures after the failed turn");
            control
                .append(Message::text("retry after configuration"))
                .await
                .expect("second message queues");

            let mut output = String::new();
            loop {
                match events.next().await.expect("recovery turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(output, "recovered");
            assert_eq!(network.requests().len(), 1);
            control.close().await.expect("session closes");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
#[cfg(feature = "multiagent")]
fn joined_subagent_returns_to_its_parent_and_supervision_tools_report_domain_errors() {
    let run_child = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-run\",",
        "\"function\":{\"name\":\"subagent_run\",\"arguments\":",
        "\"{\\\"kind\\\":\\\"worker\\\",\\\"name\\\":\\\"researcher\\\",",
        "\\\"goal\\\":\\\"find the answer\\\",\\\"timeout_ms\\\":5000}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let child_reply = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"child result\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let parent_reply = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"parent received child\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let supervise = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call-spawnable\",\"function\":{\"name\":\"subagent_list_spawnable\",\"arguments\":\"{}\"}},",
        "{\"index\":1,\"id\":\"call-list\",\"function\":{\"name\":\"subagent_list\",\"arguments\":\"{}\"}},",
        "{\"index\":2,\"id\":\"call-watch\",\"function\":{\"name\":\"subagent_watch\",\"arguments\":\"{\\\"agent\\\":\\\"agent-999\\\"}\"}},",
        "{\"index\":3,\"id\":\"call-delete\",\"function\":{\"name\":\"subagent_delete\",\"arguments\":\"{\\\"agent\\\":\\\"agent-999\\\"}\"}},",
        "{\"index\":4,\"id\":\"call-interrupt-missing\",\"function\":{\"name\":\"subagent_interrupt\",\"arguments\":\"{\\\"agent\\\":\\\"agent-999\\\",\\\"message\\\":\\\"change direction\\\"}\"}},",
        "{\"index\":5,\"id\":\"call-interrupt-invalid\",\"function\":{\"name\":\"subagent_interrupt\",\"arguments\":\"{\\\"agent\\\":\\\"invalid\\\",\\\"message\\\":\\\"change direction\\\"}\"}}",
        "]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let supervision_reply = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"supervision complete\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[run_child]),
        ScriptStep::sse(200, &[child_reply]),
        ScriptStep::sse(200, &[parent_reply]),
        ScriptStep::sse(200, &[supervise]),
        ScriptStep::sse(200, &[supervision_reply]),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
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
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("built-in tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .append(Message::text("delegate synchronously"))
                .await
                .expect("delegation queues");

            let mut first_output = String::new();
            let mut run_result = None;
            loop {
                match events.next().await.expect("delegation turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => first_output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) if call.name == "subagent_run" => {
                        run_result = Some((result.content, result.ok));
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(matches!(
                run_result,
                Some((content, true))
                    if content.contains("agent-2") && content.contains("child result")
            ));
            assert_eq!(first_output, "parent received child");

            control
                .append(Message::text("inspect the subagent graph"))
                .await
                .expect("supervision queues");
            let mut tools = Vec::new();
            let mut supervision_output = String::new();
            loop {
                match events.next().await.expect("supervision turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => supervision_output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) => tools.push((call.name, result.content, result.ok)),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            tools.sort_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(tools.len(), 6);
            assert!(tools.iter().any(|(name, content, ok)| {
                name == "subagent_list_spawnable" && *ok && content.contains("worker")
            }));
            assert!(tools
                .iter()
                .any(|(name, content, ok)| name == "subagent_list"
                    && *ok
                    && content.contains("subagents")));
            assert!(tools.iter().any(|(name, content, ok)| {
                name == "subagent_watch" && !*ok && content.contains("agent-999")
            }));
            assert!(tools.iter().any(|(name, content, ok)| {
                name == "subagent_delete" && !*ok && content.contains("agent-999")
            }));
            assert_eq!(
                tools
                    .iter()
                    .filter(|(name, content, ok)| {
                        name == "subagent_interrupt" && !*ok && content.contains("agent-999")
                            || name == "subagent_interrupt" && !*ok && content.contains("invalid")
                    })
                    .count(),
                2
            );
            assert_eq!(supervision_output, "supervision complete");
            assert_eq!(network.requests().len(), 5);

            control.close().await.expect("session closes");
            runtime.stop_all().expect("built-in tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
#[cfg(feature = "multiagent")]
fn background_subagent_completion_opens_a_tool_origin_turn() {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        tool_step(
            "call-spawn",
            "subagent_spawn",
            r#"{"kind":"worker","name":"background","goal":"finish independently","timeout_ms":5000}"#,
        ),
        text_step("[alpha]"),
        text_step("[beta]"),
        text_step("[gamma]"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
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
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("root API configures");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::SubAgent,
            false,
        )
        .expect("subagent API configures");
    runtime.start_all().expect("built-in tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .append(Message::text("delegate in the background"))
                .await
                .expect("background delegation queues");

            let mut origins = Vec::new();
            let mut streamed = String::new();
            let mut effect = String::new();
            let mut spawn_result = None;
            let mut ended = 0;
            while ended < 2 {
                match events.next().await.expect("both turns complete") {
                    Ok(SessionEvent::Turn(TurnEvent::Started { origin, .. })) => {
                        origins.push(origin)
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => streamed.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) if call.name == "subagent_spawn" => {
                        spawn_result = Some((result.content, result.ok));
                    }
                    Ok(SessionEvent::Turn(TurnEvent::EffectOutput(StreamPart::Delta(text)))) => {
                        effect.push_str(&text);
                    }
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => ended += 1,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }

            assert!(matches!(
                spawn_result,
                Some((content, true)) if content.contains("agent-2") && content.contains("background")
            ));
            assert_eq!(
                origins.first(),
                Some(&barracuda_agent_runtime::TurnOrigin::User)
            );
            assert!(matches!(
                origins.get(1),
                Some(barracuda_agent_runtime::TurnOrigin::ToolCall { call })
                    if call.id == "call-spawn" && call.name == "subagent_spawn"
            ));
            assert_eq!(
                ["[alpha]", "[beta]", "[gamma]"]
                    .into_iter()
                    .filter(|candidate| streamed.contains(candidate))
                    .count(),
                2,
                "root receives its immediate continuation and the detached completion turn"
            );
            assert!(effect.is_empty());
            assert_eq!(network.requests().len(), 4);

            control.close().await.expect("session closes");
            runtime.stop_all().expect("built-in tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn cancel_aborts_active_io_without_poisoning_a_later_turn() {
    let after_cancel = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"after cancel\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::pending_after_headers(200, "text/event-stream"),
        ScriptStep::sse(200, &[after_cancel]),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
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
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");

            control
                .append(Message::text("cancel active request"))
                .await
                .expect("active turn queues");
            while network.requests().is_empty() {
                futures_lite::future::yield_now().await;
            }
            control.cancel().await.expect("active turn cancels");
            loop {
                match events.next().await.expect("cancelled turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }

            control
                .append(Message::text("recover after cancellation"))
                .await
                .expect("recovery turn queues");
            let mut output = String::new();
            loop {
                match events.next().await.expect("recovery turn ends") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert_eq!(output, "after cancel");

            assert_eq!(network.requests().len(), 2);
            control.close().await.expect("session closes");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn close_and_delete_abort_active_turns_with_distinct_lifecycle_results() {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::pending_after_headers(200, "text/event-stream"),
        ScriptStep::pending_after_headers(200, "text/event-stream"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
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
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");

    block_on(async move {
        let scenario = async {
            let closed_session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("close session creates");
            let (control, mut events) = runtime
                .open_session(closed_session)
                .await
                .expect("close session opens");
            control
                .append(Message::text("remain pending until close"))
                .await
                .expect("close turn queues");
            while network.requests().is_empty() {
                futures_lite::future::yield_now().await;
            }
            control.close().await.expect("active session closes");
            loop {
                match events.next().await.expect("close event arrives") {
                    Ok(SessionEvent::Closed(reason)) => {
                        assert_eq!(reason, SessionCloseReason::Requested);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(runtime.list_sessions().await.contains(&closed_session));
            runtime
                .delete_session(closed_session)
                .await
                .expect("closed session deletes");

            let deleted_session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("delete session creates");
            let (control, mut events) = runtime
                .open_session(deleted_session)
                .await
                .expect("delete session opens");
            control
                .append(Message::text("remain pending until delete"))
                .await
                .expect("delete turn queues");
            while network.requests().len() < 2 {
                futures_lite::future::yield_now().await;
            }
            runtime
                .delete_session(deleted_session)
                .await
                .expect("active session deletes");
            loop {
                match events.next().await.expect("delete event arrives") {
                    Ok(SessionEvent::Closed(reason)) => {
                        assert_eq!(reason, SessionCloseReason::Deleted);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }
            assert!(runtime.list_sessions().await.is_empty());
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn built_in_profile_memory_skill_and_todo_tools_preserve_live_state() {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        tool_step(
            "profile-missing",
            "profile_read",
            r#"{"document":"user_profile"}"#,
        ),
        tool_step(
            "profile-write",
            "profile_replace",
            r#"{"document":"user_profile","content":"Prefers concise answers."}"#,
        ),
        tool_step(
            "profile-present",
            "profile_read",
            r#"{"document":"user_profile"}"#,
        ),
        tool_step(
            "profile-empty-write",
            "profile_replace",
            r#"{"document":"assistant_identity","content":"   "}"#,
        ),
        tool_step(
            "profile-empty-read",
            "profile_read",
            r#"{"document":"assistant_identity"}"#,
        ),
        tool_step(
            "profile-clear",
            "profile_clear",
            r#"{"document":"user_profile"}"#,
        ),
        tool_step(
            "profile-cleared-read",
            "profile_read",
            r#"{"document":"user_profile"}"#,
        ),
        tool_step(
            "memory-store",
            "memory_store",
            r#"{"content":"The user prefers tea.","tags":["preference"],"keywords":["drink"]}"#,
        ),
        tool_step("memory-list", "memory_list", r#"{"limit":10}"#),
        tool_step(
            "memory-recall",
            "memory_recall",
            r#"{"labels":["preference"],"query":"tea","limit":3}"#,
        ),
        tool_step(
            "memory-update",
            "memory_update",
            r#"{"id":"g-0","content":"The user prefers green tea."}"#,
        ),
        tool_step("skill-list", "skill_list", "{}"),
        tool_step("skill-read", "skill_read", r#"{"name":"fixture-skill"}"#),
        tool_step("skill-missing", "skill_read", r#"{"name":"missing-skill"}"#),
        tool_step("skill-reload", "skill_reload", "{}"),
        tool_step(
            "todo",
            "todo_update",
            r#"{"todos":[{"content":"exercise built-ins","status":"completed"}]}"#,
        ),
        tool_step("memory-forget", "memory_forget", r#"{"id":"g-0"}"#),
        tool_step("memory-empty", "memory_list", "{}"),
        text_step("built-ins complete"),
    ])));
    let llm_factory =
        ModelApiFactory::new(move || ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    block_on(filesystem.write_atomic(
        "skills/fixture-skill/SKILL.md",
        b"---\nname: fixture-skill\ndescription: Exercises a real runtime skill.\n---\n# Fixture\n\nFollow the fixture workflow.\n",
    ))
    .expect("skill fixture writes");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: vec!["skills".into()],
        },
        llm_factory,
    )
    .expect("runtime builds with a live skill catalog");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "test-key",
                "test-model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model API configures");
    runtime.start_all().expect("built-in tools start");

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Persistent)
                .await
                .expect("session creates");
            let (control, mut events) = runtime.open_session(session).await.expect("session opens");
            control
                .set_permission_level(PermissionLevel::AllowAll)
                .await
                .expect("built-in mutations are allowed");
            control
                .append(Message::text("exercise the stateful built-in tools"))
                .await
                .expect("turn queues");

            let mut output = String::new();
            let mut results = Vec::new();
            loop {
                match events.next().await.expect("built-in turn completes") {
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                        StreamPart::Delta(text),
                    )))) => output.push_str(&text),
                    Ok(SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                        StreamPart::Delta((call, result)),
                    )))) => results.push((call.name, result.content, result.ok)),
                    Ok(SessionEvent::Turn(TurnEvent::Ended { .. })) => break,
                    Ok(_) => {}
                    Err(error) => panic!("session stream failed: {error}"),
                }
            }

            let result = |name: &str| {
                results
                    .iter()
                    .find(|(candidate, _, _)| candidate == name)
                    .unwrap_or_else(|| panic!("missing result for {name}"))
            };
            assert!(result("profile_read").1.contains("does not exist"));
            assert!(results.iter().any(|(name, content, ok)| {
                name == "profile_read" && *ok && content.contains("Prefers concise answers")
            }));
            assert!(results.iter().any(|(name, content, ok)| {
                name == "profile_read"
                    && *ok
                    && content == "Profile document assistant_identity is empty."
            }));
            assert!(results.iter().any(|(name, content, ok)| {
                name == "profile_clear"
                    && *ok
                    && content == "Cleared profile document user_profile."
            }));
            assert_eq!(
                results
                    .iter()
                    .filter(|(name, content, ok)| {
                        name == "profile_read" && *ok && content.contains("is empty")
                    })
                    .count(),
                2
            );
            assert!(result("memory_store").1.contains("g-0"));
            assert!(result("memory_recall").1.contains("prefers tea"));
            assert!(result("memory_update").1.contains("g-0"));
            assert!(result("skill_list").1.contains("fixture-skill"));
            assert!(result("skill_read")
                .1
                .contains("Follow the fixture workflow"));
            assert!(results.iter().any(|(name, content, ok)| {
                name == "skill_read" && !*ok && content.contains("unknown skill \"missing-skill\"")
            }));
            assert!(result("skill_reload").1.contains("Skills refreshed"));
            assert!(result("todo_update").2);
            assert!(result("memory_forget").1.contains("g-0"));
            assert!(results.iter().any(|(name, content, ok)| {
                name == "memory_list" && *ok && content == "No matching memories."
            }));
            assert_eq!(output, "built-ins complete");
            assert_eq!(network.requests().len(), 19);

            control.close().await.expect("session closes");
            runtime.stop_all().expect("built-in tools stop");
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}
