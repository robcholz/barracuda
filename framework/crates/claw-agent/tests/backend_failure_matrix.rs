#![allow(clippy::unwrap_used)]

mod support;
use support::Sse;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use claw_agent::tools::{
    SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolResult, ToolSpec,
};
use claw_agent::{
    stream::StreamPart, AgentError, AgentSystem, IterationEvent, Message, SessionEvent,
    SessionTurnError, TurnEvent, TurnEventError,
};
use claw_interface::{
    Cancel, ClawFile, ClawFs, ClawHttp, ClawTimer, FsError, HttpError, HttpJsonRequest,
    HttpRequestFailure, HttpResponse, HttpResponseFuture, HttpStatusCode, ImmediateTimer, MemFs,
    SleepOutcome, StdThread, TimerFuture, TokioExecutor,
};
use futures_lite::future::block_on;
use support::{
    assistant_text, csv_dicts, drain_until_turn_ended, llm_config, mem_root, persistence,
};

type PermanentHttpSystem = AgentSystem<MemFs, Sse<PermanentHttp>, CountingTimer>;
type ContextProviderFailureSystem =
    AgentSystem<MemFs, Sse<TwoIterationsThenPermanentHttp>, CountingTimer>;
type MemoryExtractionFailureSystem =
    AgentSystem<MemFs, Sse<MemoryExtractionFailureHttp>, CountingTimer>;
type TransientThenSuccessSystem = AgentSystem<MemFs, Sse<TransientThenSuccessHttp>, CountingTimer>;
type TransientExhaustSystem = AgentSystem<MemFs, Sse<TransientOnlyHttp>, CountingTimer>;
type FsReadFailSystem = AgentSystem<AlwaysFailFs, Sse<PermanentHttp>, ImmediateTimer>;
type FsWriteFailSystem = AgentSystem<WriteFailFs, Sse<PermanentHttp>, ImmediateTimer>;

static BACKEND_LOCK: Mutex<()> = Mutex::new(());
static HTTP_CALLS: AtomicUsize = AtomicUsize::new(0);
static TIMER_SLEEPS: AtomicUsize = AtomicUsize::new(0);
const LARGE_TURN_BYTES: usize = 13_000;
const MEMORY_EXTRACTION_FAILURE_CALL: usize = 8;
const MEMORY_EXTRACTION_LAST_RETRY_CALL: usize = 10;

#[test]
fn backend_csv_failure_matrix_covers_fs_http_and_timer_failures() {
    let _guard = BACKEND_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    for row in csv_dicts(include_str!("fixtures/backend_failure_matrix.csv")) {
        reset_counters();
        let case = field(&row, "case");
        let actual_error = match field(&row, "backend") {
            "fs_read_fail" => fs_read_failure(),
            "fs_write_fail" => fs_persistence_write_is_deferred(),
            "fs_write_fail_session_create" => fs_session_create_write_is_deferred(),
            "http_permanent" => http_permanent_failure(),
            "http_transient_then_success" => http_transient_then_success(case),
            "http_transient_exhausts_retries" => http_transient_exhausts_retries(),
            other => panic!("unknown backend case in fixture: {other}"),
        };

        assert_error_contains(
            actual_error.as_deref(),
            field(&row, "expected_error_contains"),
            case,
        );
        assert_eq!(
            HTTP_CALLS.load(Ordering::SeqCst),
            parse_usize(&row, "expected_http_calls"),
            "case {case}: unexpected HTTP call count"
        );
        assert_eq!(
            TIMER_SLEEPS.load(Ordering::SeqCst),
            parse_usize(&row, "expected_timer_sleeps"),
            "case {case}: unexpected timer sleep count"
        );
    }
}

#[test]
fn context_provider_failure_has_a_typed_sse_error() {
    let _guard = BACKEND_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_counters();

    let system = ContextProviderFailureSystem::new::<StdThread, TokioExecutor>(
        MemFs::new(),
        persistence(&mem_root("context-provider-failure")),
    )
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();

    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    for _ in 0..2 {
        block_on(control.append(Message::text("x".repeat(LARGE_TURN_BYTES)))).unwrap();
        let seed_events = drain_until_turn_ended(&mut events);
        assert_eq!(output_fragments(&seed_events), vec!["seed".to_string()]);
    }
    block_on(control.append(Message::text("surface provider failure"))).unwrap();
    let events = drain_until_turn_ended(&mut events);
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::Turn(TurnEvent::Error(TurnEventError::Execution(
            SessionTurnError::ContextProvider(_)
        )))
    )));
}

#[test]
fn memory_extraction_failure_does_not_fail_the_turn() {
    let _guard = BACKEND_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_counters();

    let system = MemoryExtractionFailureSystem::new::<StdThread, TokioExecutor>(
        MemFs::new(),
        persistence(&mem_root("memory-extraction-failure")),
    )
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();

    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    for sequence in 0..8 {
        block_on(control.append(Message::text(format!("commit turn {sequence}")))).unwrap();
        let completed = drain_until_turn_ended(&mut events);
        assert_eq!(output_fragments(&completed), vec!["ok".to_string()]);
    }

    block_on(control.append(Message::text("extraction fails, turn continues"))).unwrap();
    let completed = drain_until_turn_ended(&mut events);
    assert_eq!(output_fragments(&completed), vec!["ok".to_string()]);
    assert!(!completed.iter().any(|event| matches!(
        event,
        SessionEvent::Error(_) | SessionEvent::Turn(TurnEvent::Error(_))
    )));
    assert_eq!(HTTP_CALLS.load(Ordering::SeqCst), 10);
}

#[derive(Default)]
struct PermanentHttp;

impl ClawHttp for PermanentHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            HTTP_CALLS.fetch_add(1, Ordering::SeqCst);
            Err(HttpError::InvalidUrl)
        })
    }
}

#[derive(Default)]
struct TwoIterationsThenPermanentHttp;

impl ClawHttp for TwoIterationsThenPermanentHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            let call = HTTP_CALLS.fetch_add(1, Ordering::SeqCst);
            if call < 2 {
                Ok(HttpResponse {
                    status_code: HttpStatusCode::OK,
                    body: assistant_text("seed"),
                })
            } else {
                Err(HttpError::InvalidUrl)
            }
        })
    }
}

#[derive(Default)]
struct MemoryExtractionFailureHttp;

impl ClawHttp for MemoryExtractionFailureHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            let call = HTTP_CALLS.fetch_add(1, Ordering::SeqCst);
            if call == MEMORY_EXTRACTION_FAILURE_CALL {
                Err(HttpError::InvalidUrl)
            } else {
                Ok(HttpResponse {
                    status_code: HttpStatusCode::OK,
                    body: assistant_text("ok"),
                })
            }
        })
    }
}

#[derive(Default)]
struct TransientThenSuccessHttp;

impl ClawHttp for TransientThenSuccessHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            let call = HTTP_CALLS.fetch_add(1, Ordering::SeqCst);
            if call == MEMORY_EXTRACTION_FAILURE_CALL {
                Err(HttpError::RequestFailed(HttpRequestFailure::transport(
                    "temporary backend outage",
                )))
            } else {
                Ok(HttpResponse {
                    status_code: HttpStatusCode::OK,
                    body: assistant_text(if call < MEMORY_EXTRACTION_FAILURE_CALL {
                        "seed"
                    } else {
                        "recovered-after-retry"
                    }),
                })
            }
        })
    }
}

#[derive(Default)]
struct TransientOnlyHttp;

impl ClawHttp for TransientOnlyHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            let call = HTTP_CALLS.fetch_add(1, Ordering::SeqCst);
            if call < MEMORY_EXTRACTION_FAILURE_CALL {
                Ok(HttpResponse {
                    status_code: HttpStatusCode::OK,
                    body: assistant_text("seed"),
                })
            } else if call <= MEMORY_EXTRACTION_LAST_RETRY_CALL {
                Err(HttpError::RequestFailed(HttpRequestFailure::transport(
                    "retry backoff should be cancelled",
                )))
            } else {
                Ok(HttpResponse {
                    status_code: HttpStatusCode::OK,
                    body: assistant_text("recovered-after-extraction-failure"),
                })
            }
        })
    }
}

#[derive(Default)]
struct CountingTimer;

impl ClawTimer for CountingTimer {
    fn sleep<'a>(&'a mut self, _duration: Duration, cancel: Cancel<'a>) -> TimerFuture<'a> {
        Box::pin(async move {
            TIMER_SLEEPS.fetch_add(1, Ordering::SeqCst);
            if cancel.is_cancelled() {
                SleepOutcome::Cancelled
            } else {
                SleepOutcome::Completed
            }
        })
    }
}

#[derive(Default)]
struct FailingFile;

impl ClawFile for FailingFile {
    fn read_to_end(&mut self) -> Result<Vec<u8>, FsError> {
        Err(FsError::io_message("read failed"))
    }

    fn read_exact_at(&mut self, _offset: u64, _len: usize) -> Result<Vec<u8>, FsError> {
        Err(FsError::io_message("read_at failed"))
    }

    fn size(&self) -> Result<u64, FsError> {
        Err(FsError::io_message("size failed"))
    }

    fn write_all(&mut self, _data: &[u8]) -> Result<(), FsError> {
        Err(FsError::io_message("write failed"))
    }
}

struct AlwaysFailFs;

impl ClawFs for AlwaysFailFs {
    type File = FailingFile;

    fn open(&self, _path: &str) -> Result<Self::File, FsError> {
        Err(FsError::io_message("open failed"))
    }

    fn create(&self, _path: &str) -> Result<Self::File, FsError> {
        Err(FsError::io_message("create failed"))
    }

    fn open_append(&self, _path: &str) -> Result<Self::File, FsError> {
        Err(FsError::io_message("append failed"))
    }

    fn rename(&self, _from: &str, _to: &str) -> Result<(), FsError> {
        Err(FsError::io_message("rename failed"))
    }

    fn create_dir_all(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::io_message("mkdir failed"))
    }

    fn exists(&self, _path: &str) -> bool {
        false
    }

    fn remove(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::io_message("remove failed"))
    }

    fn list_dir(&self, _path: &str) -> Result<Vec<String>, FsError> {
        Err(FsError::io_message("list failed"))
    }
}

struct WriteFailFs;

impl ClawFs for WriteFailFs {
    type File = FailingFile;

    fn open(&self, _path: &str) -> Result<Self::File, FsError> {
        Err(FsError::NotFound)
    }

    fn create(&self, _path: &str) -> Result<Self::File, FsError> {
        Err(FsError::io_message("persistence create failed"))
    }

    fn open_append(&self, _path: &str) -> Result<Self::File, FsError> {
        Err(FsError::io_message("append failed"))
    }

    fn rename(&self, _from: &str, _to: &str) -> Result<(), FsError> {
        Err(FsError::io_message("rename failed"))
    }

    fn create_dir_all(&self, _path: &str) -> Result<(), FsError> {
        Ok(())
    }

    fn exists(&self, _path: &str) -> bool {
        false
    }

    fn remove(&self, _path: &str) -> Result<(), FsError> {
        Ok(())
    }

    fn list_dir(&self, _path: &str) -> Result<Vec<String>, FsError> {
        Ok(Vec::new())
    }
}

struct PersistenceTool;

impl ToolSpec for PersistenceTool {
    fn name(&self) -> &str {
        "persistence_probe"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"persistence_probe"}}"#
    }
}

impl SyncToolHandler for PersistenceTool {
    fn invoke(&self, call: &ToolInvocation) -> ToolResult<ToolOutput> {
        Ok(ToolOutput {
            content: call.arguments_json().to_owned(),
            ok: true,
        })
    }
}

fn fs_read_failure() -> Option<String> {
    build_fs_read_fail_system()
        .err()
        .map(|error| error.to_string())
}

fn fs_persistence_write_is_deferred() -> Option<String> {
    let system = build_fs_write_fail_system_with_tool_groups([ToolGroup::new(
        "persistence",
        true,
        [Tool::from_sync(PersistenceTool)],
    )])
    .unwrap();
    system
        .disable_tool("persistence_probe")
        .err()
        .map(|error| error.to_string())
}

fn fs_session_create_write_is_deferred() -> Option<String> {
    let system = build_fs_write_fail_system().unwrap();
    system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .err()
        .map(|error| error.to_string())
}

fn http_permanent_failure() -> Option<String> {
    let system = PermanentHttpSystem::new::<StdThread, TokioExecutor>(
        MemFs::new(),
        persistence(&mem_root("http-permanent")),
    )
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();
    first_failure_text(drive_one_turn(&system, "permanent failure"))
}

fn http_transient_then_success(case: &str) -> Option<String> {
    let system = TransientThenSuccessSystem::new::<StdThread, TokioExecutor>(
        MemFs::new(),
        persistence(&mem_root("http-transient-success")),
    )
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    for sequence in 0..8 {
        block_on(control.append(Message::text(format!("commit turn {sequence}")))).unwrap();
        let seed_events = drain_until_turn_ended(&mut events);
        assert_eq!(output_fragments(&seed_events), vec!["seed".to_string()]);
    }
    block_on(control.append(Message::text("recover"))).unwrap();
    let events = drain_until_turn_ended(&mut events);
    assert_eq!(
        output_fragments(&events),
        vec!["recovered-after-retry".to_string()],
        "case {case}: events={events:#?}"
    );
    first_failure_text(events)
}

fn http_transient_exhausts_retries() -> Option<String> {
    let system = TransientExhaustSystem::new::<StdThread, TokioExecutor>(
        MemFs::new(),
        persistence(&mem_root("transient-exhaust")),
    )
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    for sequence in 0..8 {
        block_on(control.append(Message::text(format!("commit turn {sequence}")))).unwrap();
        let seed_events = drain_until_turn_ended(&mut events);
        assert_eq!(output_fragments(&seed_events), vec!["seed".to_string()]);
    }
    block_on(control.append(Message::text("exhaust transient retries"))).unwrap();
    let completed = drain_until_turn_ended(&mut events);
    assert_eq!(
        output_fragments(&completed),
        vec!["recovered-after-extraction-failure".to_string()]
    );
    first_failure_text(completed)
}

fn build_fs_read_fail_system() -> Result<FsReadFailSystem, AgentError> {
    let system = FsReadFailSystem::new::<StdThread, TokioExecutor>(
        AlwaysFailFs,
        persistence("/fs-read-fail"),
    )?;
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();
    Ok(system)
}

fn build_fs_write_fail_system() -> Result<FsWriteFailSystem, AgentError> {
    build_fs_write_fail_system_with_tool_groups(std::iter::empty())
}

fn build_fs_write_fail_system_with_tool_groups(
    tool_groups: impl IntoIterator<Item = ToolGroup>,
) -> Result<FsWriteFailSystem, AgentError> {
    let system = FsWriteFailSystem::with_tool_groups::<StdThread, TokioExecutor>(
        WriteFailFs,
        persistence("/fs-write-fail"),
        tool_groups,
    )?;
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();
    Ok(system)
}

fn drive_one_turn<Filesystem, Http, Timer>(
    system: &AgentSystem<Filesystem, Sse<Http>, Timer>,
    input: &str,
) -> Vec<SessionEvent>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    block_on(control.append(Message::text(input))).unwrap();
    drain_until_turn_ended(&mut events)
}

fn first_failure_text(events: Vec<SessionEvent>) -> Option<String> {
    events.into_iter().find_map(|event| match event {
        SessionEvent::Error(error) => Some(error.to_string()),
        SessionEvent::Turn(TurnEvent::Error(error)) => Some(error.to_string()),
        SessionEvent::Turn(
            TurnEvent::EffectOutput(StreamPart::Delta(text))
            | TurnEvent::Iteration(IterationEvent::Output(StreamPart::Delta(text))),
        ) if text.contains("[failed:") => Some(text),
        _ => None,
    })
}

fn output_fragments(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Turn(
                TurnEvent::EffectOutput(StreamPart::Delta(text))
                | TurnEvent::Iteration(IterationEvent::Output(StreamPart::Delta(text))),
            ) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn assert_error_contains(actual: Option<&str>, expected_contains: &str, case: &str) {
    if expected_contains.is_empty() {
        assert!(actual.is_none(), "case {case}: unexpected error {actual:?}");
    } else {
        let actual = actual.unwrap_or_else(|| panic!("case {case}: expected error"));
        assert!(
            actual.contains(expected_contains),
            "case {case}: expected {actual:?} to contain {expected_contains:?}"
        );
    }
}

fn reset_counters() {
    HTTP_CALLS.store(0, Ordering::SeqCst);
    TIMER_SLEEPS.store(0, Ordering::SeqCst);
}

fn parse_usize(row: &BTreeMap<String, String>, field_name: &str) -> usize {
    field(row, field_name).parse::<usize>().unwrap()
}

fn field<'a>(row: &'a BTreeMap<String, String>, name: &str) -> &'a str {
    row.get(name)
        .unwrap_or_else(|| panic!("missing csv column {name}"))
        .as_str()
}
