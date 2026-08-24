#![allow(clippy::expect_used)]
#![allow(missing_docs)]

mod support;

use core::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;
use std::time::{Duration, Instant};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, FileSystem, RegisterContext,
    RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary, UnregisterContext, WorkflowClient,
    WorkflowControlError,
};
use barracuda_platform_test::MemFs;

const FRAME_CAPACITY: usize = 64;

struct Sink;

impl RpcMethod for Sink {
    const ADDRESS: &'static str = "scale.sink";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = ();
    type Input = Unary;
    type Output = Unary;
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
        // Stub endpoint so load-time link validation can resolve the steps
        // the catalog persists. Response mirrors Request so chained Direct
        // links of the same method stay type-identical.
        context.register_rpc::<Sink, _>(|_context, request: RpcFrame<[u8; 8]>| async move {
            Ok(Ok(*request.view()?))
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

fn router(filesystem: &MemFs, directory: &'static str) -> EventRouter<2, FRAME_CAPACITY, 2> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<2, FRAME_CAPACITY, 2>::new()));
    EventRouter::new(lanes, filesystem.clone(), directory).expect("create Event Router")
}

#[test]
fn hundreds_of_workflows_persist_and_restore_in_load_order() {
    const WORKFLOWS: usize = 256;
    const DIRECTORY: &str = "catalog-scale";

    let documents = (0..WORKFLOWS)
        .map(|index| {
            format!(
                r#"{{"id":"workflow-{index:04}","match":{{"event":"scale.*"}},"steps":[{{"call":"scale.sink"}}]}}"#
            )
        })
        .collect();
    let filesystem = MemFs::new();
    let state = Rc::new(LoadState::default());
    let mut event_router = router(&filesystem, DIRECTORY);
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
    let index = filesystem
        .read("catalog-scale/index")
        .expect("read Workflow index");
    assert_eq!(
        index.iter().filter(|byte| **byte == b'\n').count(),
        WORKFLOWS
    );

    let elapsed = state.elapsed.get().expect("catalog load elapsed time");
    eprintln!(
        "workflow_catalog_load: workflows={WORKFLOWS}, polls={polls}, elapsed_ns={}, workflows_per_second={:.0}",
        elapsed.as_nanos(),
        WORKFLOWS as f64 / elapsed.as_secs_f64()
    );

    drop(event_router);
    let restored_started = Instant::now();
    let restored = router(&filesystem, DIRECTORY);
    let restored_elapsed = restored_started.elapsed();
    assert_eq!(restored.workflow_definitions(), definitions);
    eprintln!(
        "workflow_catalog_restore: workflows={WORKFLOWS}, elapsed_ns={}, workflows_per_second={:.0}",
        restored_elapsed.as_nanos(),
        WORKFLOWS as f64 / restored_elapsed.as_secs_f64()
    );
}

#[test]
fn workflow_json_and_step_count_are_not_bounded_by_lane_frame_capacity() {
    const STEPS: usize = 512;
    const DIRECTORY: &str = "large-definition";

    let steps = (0..STEPS)
        .map(|_| r#"{"call":"scale.sink"}"#)
        .collect::<Vec<_>>()
        .join(",");
    let document =
        format!(r#"{{"id":"large-workflow","match":{{"event":"scale.event"}},"steps":[{steps}]}}"#);
    assert!(document.len() > FRAME_CAPACITY);

    let filesystem = MemFs::new();
    let state = Rc::new(LoadState::default());
    let mut event_router = router(&filesystem, DIRECTORY);
    event_router
        .load(Box::new(CatalogLoader {
            documents: vec![document],
            state: Rc::clone(&state),
        }))
        .expect("load large definition loader");

    support::drive_until(&mut event_router, |_router| state.done.get())
        .expect("drive large definition loader");
    assert!(state.error.borrow().is_none());
    let definitions = event_router.workflow_definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions.first().expect("large definition").steps().len(),
        STEPS
    );
}
