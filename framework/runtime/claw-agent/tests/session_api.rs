#![allow(clippy::unwrap_used)]

mod support;
use support::Sse;

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use claw_agent::{
    stream::StreamPart, SessionCloseReason, SessionEvent, SessionId, SessionStream, TurnId,
    TurnOrigin,
};
use claw_agent::{AgentError, AgentSystem, Message, OpenSessionError};
use claw_interface::{
    BlockingHttpAdapter, Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpResponseFuture,
    ImmediateTimer, MemFs, SharedScriptHttp, StdThread, TokioExecutor,
};
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use support::{
    assistant_text, build_mem_system, drain_until_turn_ended, install_script, llm_config, mem_root,
    persistence, serialize_script,
};

type SlowAgentSystem = AgentSystem<MemFs, Sse<SlowScriptHttp>, ImmediateTimer>;

#[derive(Default)]
struct SlowScriptHttp;

impl ClawHttp for SlowScriptHttp {
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            YieldTimes::new(16).await;
            if cancel.is_cancelled() {
                return Err(HttpError::Aborted);
            }
            let mut inner = BlockingHttpAdapter::new(SharedScriptHttp::default());
            inner.post_json(request, cancel).await
        })
    }
}

struct YieldTimes {
    remaining: u32,
}

impl YieldTimes {
    const fn new(remaining: u32) -> Self {
        Self { remaining }
    }
}

impl Future for YieldTimes {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.remaining == 0 {
            Poll::Ready(())
        } else {
            self.remaining -= 1;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[test]
fn list_sessions_returns_session_ids() {
    let _script = serialize_script();
    let root = mem_root("agent-list-sessions");
    let system = build_mem_system(&root, Vec::new());
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();

    assert_eq!(system.list_sessions(), vec![session]);
}

#[test]
fn session_streams_root_reply_as_output() {
    let _script = serialize_script();
    let root = mem_root("agent-stream-output");
    let system = build_mem_system(&root, vec![assistant_text("hello there")]);
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(control.append(Message::text("say hi"))).unwrap();
    let events = drain_until_turn_ended(&mut events);

    assert!(matches!(
        events.first(),
        Some(SessionEvent::TurnStarted {
            turn: TurnId(1),
            origin: TurnOrigin::User,
        })
    ));
    assert!(matches!(
        events.last(),
        Some(SessionEvent::TurnEnded { turn: TurnId(1) })
    ));
    let outputs: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Output(StreamPart::Delta(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, vec!["hello there"]);
}

#[test]
fn open_unknown_session_returns_error() {
    let _script = serialize_script();
    let root = mem_root("agent-open-unknown");
    let system = build_mem_system(&root, Vec::new());

    assert!(matches!(
        system.open_session(SessionId(9)),
        Err(AgentError::OpenSession(OpenSessionError::SessionNotFound(
            SessionId(9)
        )))
    ));
}

#[test]
fn dropping_the_stream_releases_its_session_lease() {
    let _script = serialize_script();
    let root = mem_root("agent-drop-stream");
    let system = build_mem_system(&root, Vec::new());
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();

    drop(system.open_session(session).unwrap());

    let deadline = Instant::now() + Duration::from_secs(1);
    let reopened = loop {
        match system.open_session(session) {
            Ok(stream) => break stream,
            Err(AgentError::OpenSession(OpenSessionError::AlreadyOpen(open)))
                if open == session && Instant::now() < deadline =>
            {
                thread::yield_now();
            }
            Err(error) => panic!("dropped SessionStream did not release its lease: {error}"),
        }
    };
    drop(reopened);
}

#[test]
fn append_queues_while_current_turn_runs() {
    let _script = serialize_script();
    let root = mem_root("agent-submit-busy");
    let system = build_slow_system(
        &root,
        vec![assistant_text("first"), assistant_text("second")],
    );
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(async {
        control.append(Message::text("first")).await.unwrap();
        control.append(Message::text("second")).await.unwrap();
    });
    let first_events = drain_until_turn_ended(&mut events);

    assert!(first_events.iter().any(
        |event| matches!(event, SessionEvent::Output(StreamPart::Delta(text)) if text == "first")
    ));
    let second_events = drain_until_turn_ended(&mut events);
    assert!(second_events.iter().any(
        |event| matches!(event, SessionEvent::Output(StreamPart::Delta(text)) if text == "second")
    ));
}

#[test]
fn session_control_methods_are_idempotent() {
    let _script = serialize_script();
    let root = mem_root("agent-control-idempotent");
    let system = build_slow_system(&root, vec![assistant_text("cancelled")]);
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(async {
        control.append(Message::text("cancel me")).await.unwrap();
        control.interrupt().await.unwrap();
        control.interrupt().await.unwrap();
        control.cancel().await.unwrap();
        control.cancel().await.unwrap();
    });

    let events = drain_until_turn_ended(&mut events);
    assert!(matches!(
        events.first(),
        Some(SessionEvent::TurnStarted {
            turn: TurnId(1),
            origin: TurnOrigin::User,
        })
    ));
    assert!(matches!(
        events.last(),
        Some(SessionEvent::TurnEnded { turn: TurnId(1) })
    ));
}

#[test]
fn cancel_preserves_messages_already_queued_for_later_turns() {
    let _script = serialize_script();
    let root = mem_root("agent-cancel-preserves-inbox");
    let system = build_slow_system(
        &root,
        vec![
            assistant_text("queued message ran"),
            assistant_text("queued message ran"),
        ],
    );
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(async {
        control
            .append(Message::text("cancel this turn"))
            .await
            .unwrap();
        control
            .append(Message::text("keep this queued turn"))
            .await
            .unwrap();
        control.cancel().await.unwrap();
    });

    let cancelled = drain_until_turn_ended(&mut events);
    assert!(matches!(
        cancelled.last(),
        Some(SessionEvent::TurnEnded { turn: TurnId(1) })
    ));

    let queued = drain_until_turn_ended(&mut events);
    assert!(matches!(
        queued.last(),
        Some(SessionEvent::TurnEnded { turn: TurnId(2) })
    ));
    assert!(queued.iter().any(
        |event| matches!(event, SessionEvent::Output(StreamPart::Delta(text)) if text == "queued message ran")
    ));
}

#[test]
fn close_session_cancels_active_work_and_closes_events() {
    let _script = serialize_script();
    let root = mem_root("agent-close-session");
    let system = build_slow_system(&root, vec![assistant_text("should not surface")]);
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(async {
        control.append(Message::text("close me")).await.unwrap();
        control.close().await.unwrap();
    });
    let events = drain_until_closed(&mut events);

    assert!(matches!(
        events.last(),
        Some(SessionEvent::Closed(SessionCloseReason::Requested))
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::Output(StreamPart::Delta(_)))),
        "closed stream should be cancelled without output: {events:?}"
    );
    assert!(system.list_sessions().contains(&session));
    assert!(
        block_on(control.append(Message::text("after close"))).is_err(),
        "closed control should reject new appends"
    );
}

#[test]
fn delete_session_removes_session_and_closes_open_stream() {
    let _script = serialize_script();
    let root = mem_root("agent-delete-session");
    let system = build_mem_system(&root, Vec::new());
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (_control, mut events) = system.open_session(session).unwrap();

    system.delete_session(session).unwrap();
    let events = drain_until_closed(&mut events);

    assert!(matches!(
        events.last(),
        Some(SessionEvent::Closed(SessionCloseReason::Deleted))
    ));
    assert!(!system.list_sessions().contains(&session));
    assert!(matches!(
        system.open_session(session),
        Err(AgentError::OpenSession(OpenSessionError::SessionNotFound(
            missing
        ))) if missing == session
    ));
}

fn build_slow_system(root: &str, bodies: Vec<String>) -> SlowAgentSystem {
    install_script(bodies);
    let system = SlowAgentSystem::new::<StdThread, TokioExecutor>(persistence(root)).unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiUsage::RootAgent, true)
        .unwrap();
    system
}

fn drain_until_closed(events: &mut SessionStream) -> Vec<SessionEvent> {
    block_on(async move {
        let mut collected = Vec::new();
        while let Some(event) = events.next().await {
            let event = event.expect("Session stream failed");
            let closed = matches!(event, SessionEvent::Closed(_));
            collected.push(event);
            if closed {
                break;
            }
        }
        collected
    })
}
