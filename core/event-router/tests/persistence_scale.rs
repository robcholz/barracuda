#![allow(clippy::expect_used)]
#![allow(missing_docs)]

mod support;

use core::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, JsonRpcSchema, JsonSchema,
    JsonWriter, RegisterContext, RpcError, RpcLaneStorage, RunContext, UnregisterContext,
    WorkflowClient, WorkflowControlError,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vfs::{read, remove_file};
use futures_lite::future::block_on;

const FRAME_CAPACITY: usize = 256;
const EMPTY_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{},"additionalProperties":false}"#
);
static GLOBAL_VFS_TEST_LOCK: Mutex<()> = Mutex::new(());

fn reset_global_vfs() -> MutexGuard<'static, ()> {
    let guard = GLOBAL_VFS_TEST_LOCK.lock().expect("lock global VFS test");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let _ignored = block_on(remove_file("/system/workflows.json"));
    guard
}

struct Sink;

impl JsonRpcSchema for Sink {
    const ADDRESS: &'static str = "scale.sink";
    const REQUEST_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Default)]
struct LoadState {
    done: Cell<bool>,
    elapsed: Cell<Option<Duration>>,
    error: RefCell<Option<WorkflowControlError>>,
}

struct CatalogLoader {
    documents: Vec<String>,
    state: Rc<LoadState>,
}

impl Component<FRAME_CAPACITY> for CatalogLoader {
    fn register(
        &mut self,
        context: &mut RegisterContext<'_, FRAME_CAPACITY>,
    ) -> ComponentResult<()> {
        context
            .register_json::<Sink, _>("*", |_context, _request, response: JsonWriter| async move {
                response.write("{}").await
            })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_CAPACITY>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = WorkflowClient::<FRAME_CAPACITY>::new(context.rpc().clone());
            let started = Instant::now();
            for document in &self.documents {
                if let Err(error) = client.load(document).await {
                    self.state.error.replace(Some(error));
                    break;
                }
            }
            self.state.elapsed.set(Some(started.elapsed()));
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn router() -> EventRouter<2, FRAME_CAPACITY, 2> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<2, FRAME_CAPACITY, 2>::new()));
    block_on(EventRouter::new(lanes)).expect("create Event Router")
}

#[test]
fn hundreds_of_workflows_persist_and_restore_in_load_order() {
    let _global_vfs = reset_global_vfs();
    const WORKFLOWS: usize = 256;
    let documents = (0..WORKFLOWS)
        .map(|index| {
            format!(
                r#"{{"id":"workflow-{index:04}","match":{{"event":"scale.*"}},"steps":[{{"call":"scale.sink"}}]}}"#
            )
        })
        .collect();
    let state = Rc::new(LoadState::default());
    let mut event_router = router();
    event_router
        .load(Box::new(CatalogLoader {
            documents,
            state: Rc::clone(&state),
        }))
        .expect("load catalog loader");

    let polls = support::drive_until(&mut event_router, |_router| state.done.get())
        .expect("drive catalog loader");
    assert!(state.error.borrow().is_none());
    let definitions = event_router.workflow_definitions();
    assert_eq!(definitions.len(), WORKFLOWS);
    assert_eq!(
        definitions.first().expect("first definition").id().as_str(),
        "workflow-0000"
    );
    assert_eq!(
        definitions.last().expect("last definition").id().as_str(),
        "workflow-0255"
    );
    let catalog: Vec<serde_json::Value> = serde_json::from_slice(
        &block_on(read("/system/workflows.json")).expect("read Workflow catalog"),
    )
    .expect("catalog is a JSON array");
    assert_eq!(catalog.len(), WORKFLOWS);

    let elapsed = state.elapsed.get().expect("catalog load elapsed time");
    eprintln!(
        "workflow_catalog_load: workflows={WORKFLOWS}, polls={polls}, elapsed_ns={}, workflows_per_second={:.0}",
        elapsed.as_nanos(),
        WORKFLOWS as f64 / elapsed.as_secs_f64()
    );

    drop(event_router);
    let restored_started = Instant::now();
    let restored = router();
    let restored_elapsed = restored_started.elapsed();
    assert_eq!(restored.workflow_definitions(), definitions);
    eprintln!(
        "workflow_catalog_restore: workflows={WORKFLOWS}, elapsed_ns={}, workflows_per_second={:.0}",
        restored_elapsed.as_nanos(),
        WORKFLOWS as f64 / restored_elapsed.as_secs_f64()
    );
}

#[test]
fn workflow_json_is_bounded_by_lane_frame_capacity() {
    let _global_vfs = reset_global_vfs();
    const STEPS: usize = 512;
    let steps = (0..STEPS)
        .map(|_| r#"{"call":"scale.sink"}"#)
        .collect::<Vec<_>>()
        .join(",");
    let document =
        format!(r#"{{"id":"large-workflow","match":{{"event":"scale.event"}},"steps":[{steps}]}}"#);
    assert!(document.len() > FRAME_CAPACITY);

    let state = Rc::new(LoadState::default());
    let mut event_router = router();
    event_router
        .load(Box::new(CatalogLoader {
            documents: vec![document],
            state: Rc::clone(&state),
        }))
        .expect("load large definition loader");

    support::drive_until(&mut event_router, |_router| state.done.get())
        .expect("drive large definition loader");
    assert!(matches!(
        state.error.borrow().as_ref(),
        Some(WorkflowControlError::Rpc(RpcError::FrameTooLarge { capacity, .. }))
            if *capacity == FRAME_CAPACITY
    ));
    assert!(event_router.workflow_definitions().is_empty());
}
