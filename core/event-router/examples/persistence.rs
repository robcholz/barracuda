//! Usage: durable Workflows survive Event Router reconstruction and are removed
//! durably by `WorkflowClient::unload`.

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter,
    EventRouterCreateError, FileSystem, RegisterContext, RouterError, RpcFrame, RpcLaneStorage,
    RpcMethod, RunContext, Unary, UnregisterContext, WorkflowClient, WorkflowControlError,
    WorkflowId,
};
use barracuda_platform_test::MemFs;

const FRAME_SIZE: usize = 64;
const WORKFLOW_DIRECTORY: &str = "workflows";

type TestEventRouter = EventRouter<4, FRAME_SIZE, 4>;

const WORKFLOW_JSON: &str = r#"{
    "id": "persisted-recorder",
    "match": { "event": "example.persisted" },
    "steps": [
        { "call": "example.record" }
    ]
}"#;

struct Record;

impl RpcMethod for Record {
    const ADDRESS: &'static str = "example.record";
    type Request = [u8; 4];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[derive(Default)]
struct ControlState {
    done: Cell<bool>,
    error: RefCell<Option<WorkflowControlError>>,
}

struct WorkflowInstaller {
    state: Rc<ControlState>,
}

impl Component<FRAME_SIZE> for WorkflowInstaller {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        // Register the step endpoint so `workflow.load` can validate the link.
        context.register_rpc::<Record, _>(|_context, _request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(()))
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let result = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(WORKFLOW_JSON)
                .await;
            if let Err(error) = result {
                self.state.error.replace(Some(error));
            }
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

struct WorkflowUninstaller {
    state: Rc<ControlState>,
}

impl Component<FRAME_SIZE> for WorkflowUninstaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let workflow_id =
                WorkflowId::try_from("persisted-recorder").map_err(ComponentError::lifecycle)?;
            let result = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .unload(&workflow_id)
                .await;
            if let Err(error) = result {
                self.state.error.replace(Some(error));
            }
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn new_router(filesystem: &MemFs) -> Result<TestEventRouter, EventRouterCreateError> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
    EventRouter::new(lanes, filesystem.clone(), WORKFLOW_DIRECTORY)
}

async fn drive_until(
    event_router: &mut TestEventRouter,
    mut ready: impl FnMut(&TestEventRouter) -> bool,
) -> Result<(), RouterError> {
    poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut *event_router).poll(context) {
            return Poll::Ready(result);
        }
        if ready(event_router) {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    let filesystem = MemFs::new();

    // Persist: load a Workflow, then reconstruct the Event Router.
    let install_state = Rc::new(ControlState::default());
    let mut first = new_router(&filesystem)?;
    first.load(Box::new(WorkflowInstaller {
        state: Rc::clone(&install_state),
    }))?;
    drive_until(&mut first, |_router| install_state.done.get()).await?;
    assert!(install_state.error.borrow().is_none());
    assert_eq!(first.workflow_definitions().len(), 1);
    assert_eq!(
        filesystem.read("workflows/persisted-recorder.json")?,
        WORKFLOW_JSON.as_bytes()
    );

    let definitions = first.workflow_definitions();
    drop(first);

    let mut second = new_router(&filesystem)?;
    assert_eq!(second.workflow_definitions(), definitions);

    // Unload: remove the durable definition and verify a fresh router is empty.
    let uninstall_state = Rc::new(ControlState::default());
    second.load(Box::new(WorkflowUninstaller {
        state: Rc::clone(&uninstall_state),
    }))?;
    drive_until(&mut second, |_router| uninstall_state.done.get()).await?;
    assert!(uninstall_state.error.borrow().is_none());
    assert!(second.workflow_definitions().is_empty());
    assert!(!filesystem.exists("workflows/persisted-recorder.json")?);

    drop(second);
    let third = new_router(&filesystem)?;
    assert!(third.workflow_definitions().is_empty());

    println!("Workflow persisted across restart and was durably unloaded");
    Ok(())
}
