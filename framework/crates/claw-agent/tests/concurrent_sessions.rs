#![allow(clippy::unwrap_used)]

mod support;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use claw_agent::{
    stream::StreamPart, AgentSystem, ApiPurpose, IterationEvent, Message, SessionEvent,
    SessionPersistence, TurnEvent,
};
use claw_interface::{
    Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpRequestFailure, HttpResponse,
    HttpResponseFuture, HttpStatusCode, ImmediateTimer, MemFs, StdThread, TokioExecutor,
};

use support::{assistant_text, drain_until_turn_ended, llm_config, mem_root, persistence, Sse};

const SESSION_A_INPUT: &str = "host-concurrent-session-a";
const SESSION_B_INPUT: &str = "host-concurrent-session-b";
const SESSION_A_OUTPUT: &str = "reply-for-session-a";
const SESSION_B_OUTPUT: &str = "reply-for-session-b";
const CONCURRENCY_TIMEOUT: Duration = Duration::from_secs(2);

type ConcurrentAgentSystem = AgentSystem<MemFs, Sse<ConcurrentRetryHttp>, ImmediateTimer>;

static ATTEMPTS: Mutex<[u32; 2]> = Mutex::new([0; 2]);
static FIRST_ATTEMPT_BARRIER: OnceLock<Arc<tokio::sync::Barrier>> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
enum SessionCase {
    A,
    B,
}

impl SessionCase {
    const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    const fn output(self) -> &'static str {
        match self {
            Self::A => SESSION_A_OUTPUT,
            Self::B => SESSION_B_OUTPUT,
        }
    }
}

#[derive(Default)]
struct ConcurrentRetryHttp;

impl ClawHttp for ConcurrentRetryHttp {
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        let prepared =
            classify_session(request.body).map(|session| (session, record_attempt(session)));
        let barrier = first_attempt_barrier();

        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(HttpError::Aborted);
            }

            let (session, attempt) = prepared?;
            match attempt {
                1 => {
                    tokio::time::timeout(CONCURRENCY_TIMEOUT, barrier.wait())
                        .await
                        .map_err(|_| {
                            HttpError::RequestFailed(HttpRequestFailure::driver(
                                "host concurrent session test",
                                "Sessions did not overlap before the barrier timeout",
                            ))
                        })?;
                    Err(HttpError::RequestFailed(HttpRequestFailure::transport(
                        "injected concurrent connection failure",
                    )))
                }
                2 => Ok(HttpResponse {
                    status_code: HttpStatusCode::OK,
                    body: assistant_text(session.output()),
                }),
                unexpected => Err(HttpError::RequestFailed(HttpRequestFailure::driver(
                    "host concurrent session test",
                    format!("unexpected attempt {unexpected} for {session:?}"),
                ))),
            }
        })
    }
}

#[test]
fn concurrent_sessions_retry_connection_failures_without_cross_talk() {
    *attempts() = [0; 2];

    let root = mem_root("concurrent-session-retry");
    let system =
        ConcurrentAgentSystem::new::<StdThread, TokioExecutor>(MemFs::new(), persistence(&root))
            .unwrap();
    system
        .link_api(llm_config(), ApiPurpose::RootAgent, true)
        .unwrap();

    let session_a = system.new_session(SessionPersistence::Ephemeral).unwrap();
    let session_b = system.new_session(SessionPersistence::Ephemeral).unwrap();
    let (control_a, mut events_a) = system.open_session(session_a).unwrap();
    let (control_b, mut events_b) = system.open_session(session_b).unwrap();

    futures_lite::future::block_on(async {
        control_a
            .append(Message::text(SESSION_A_INPUT))
            .await
            .unwrap();
        control_b
            .append(Message::text(SESSION_B_INPUT))
            .await
            .unwrap();
    });

    let turn_a = drain_until_turn_ended(&mut events_a);
    let turn_b = drain_until_turn_ended(&mut events_b);

    assert_eq!(output_fragments(&turn_a), vec![SESSION_A_OUTPUT]);
    assert_eq!(output_fragments(&turn_b), vec![SESSION_B_OUTPUT]);
    assert_eq!(
        *attempts(),
        [2, 2],
        "each Session should make one failed attempt and one successful retry"
    );
}

fn classify_session(body: &str) -> Result<SessionCase, HttpError> {
    if body.contains(SESSION_A_INPUT) {
        Ok(SessionCase::A)
    } else if body.contains(SESSION_B_INPUT) {
        Ok(SessionCase::B)
    } else {
        Err(HttpError::RequestFailed(HttpRequestFailure::driver(
            "host concurrent session test",
            "request did not contain a known Session marker",
        )))
    }
}

fn record_attempt(session: SessionCase) -> u32 {
    let mut attempts = attempts();
    let index = session.index();
    attempts[index] = attempts[index].saturating_add(1);
    attempts[index]
}

fn first_attempt_barrier() -> Arc<tokio::sync::Barrier> {
    Arc::clone(FIRST_ATTEMPT_BARRIER.get_or_init(|| Arc::new(tokio::sync::Barrier::new(2))))
}

fn output_fragments(events: &[SessionEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Turn(
                TurnEvent::EffectOutput(StreamPart::Delta(text))
                | TurnEvent::Iteration(IterationEvent::Output(StreamPart::Delta(text))),
            ) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn attempts() -> MutexGuard<'static, [u32; 2]> {
    ATTEMPTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
