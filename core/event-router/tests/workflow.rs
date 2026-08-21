#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    FileSystem, MemFs, RegisterContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcStream, RunContext,
    Streaming, Unary, UnregisterContext, WorkflowClient, WorkflowControlError,
    WorkflowControlRejection, WorkflowId,
};
use futures_lite::future::block_on;
use futures_util::stream;

const FRAME_SIZE: usize = 64;
const LANE_COUNT: usize = 8;
const WAITER_COUNT: usize = 8;
const WORKFLOW_DIRECTORY: &str = "workflows";

type TestEventRouter = EventRouter<LANE_COUNT, FRAME_SIZE, WAITER_COUNT>;

const ALPHA_JSON: &str = r#"{
    "id": "gateway-message-to-recorder-with-a-long-workflow-id",
    "match": { "event": "gateway.message.received" },
    "steps": [
        { "call": "integration.add-one" },
        { "call": "integration.record" }
    ]
}"#;

const BETA_JSON: &str = r#"{
    "id": "gateway-message-audit",
    "match": { "event": "gateway.*" },
    "steps": [
        { "call": "integration.audit" }
    ]
}"#;

static BOTH_WORKFLOWS: &[&str] = &[ALPHA_JSON, BETA_JSON];
static DUPLICATE_ALPHA: &[&str] = &[ALPHA_JSON, ALPHA_JSON];
static INVALID_WORKFLOW: &[&str] = &["{"];

fn new_router(filesystem: &'static MemFs) -> TestEventRouter {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<
        LANE_COUNT,
        FRAME_SIZE,
        WAITER_COUNT,
    >::new()));
    EventRouter::new(lanes, filesystem, WORKFLOW_DIRECTORY).expect("create Event Router")
}

fn drive_until(event_router: &mut TestEventRouter, ready: impl Fn(&TestEventRouter) -> bool) {
    block_on(poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut *event_router).poll(context) {
            return Poll::Ready(result);
        }
        if ready(event_router) {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }))
    .expect("Event Router remains operational");
}

#[derive(Default)]
struct ControlState {
    done: Cell<bool>,
    error: RefCell<Option<WorkflowControlError>>,
}

struct WorkflowInstaller {
    json: &'static [&'static str],
    state: Rc<ControlState>,
    register_stubs: bool,
}

impl Component<FRAME_SIZE> for WorkflowInstaller {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        if !self.register_stubs {
            return Ok(());
        }
        // Stub endpoints so load-time link validation can resolve the steps
        // these workflows persist.
        context.register_rpc::<AddOne, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(*request.view()?))
        })?;
        context.register_rpc::<Record, _>(|_context, _request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(()))
        })?;
        context.register_rpc::<Audit, _>(|_context, _request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(()))
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone());
            for json in self.json {
                if let Err(error) = client.load(json).await {
                    self.state.error.replace(Some(error));
                    break;
                }
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
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        // Stub endpoints so load-time link validation can resolve the steps
        // this workflow loads.
        context.register_rpc::<AddOne, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(*request.view()?))
        })?;
        context.register_rpc::<Record, _>(|_context, _request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(()))
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone());
            let result = async {
                client.load(ALPHA_JSON).await?;
                let workflow_id =
                    WorkflowId::try_from("gateway-message-to-recorder-with-a-long-workflow-id")
                        .expect("valid Workflow ID");
                client.unload(&workflow_id).await
            }
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

#[test]
fn streaming_load_persists_order_and_recovers_it_after_restart() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let state = Rc::new(ControlState::default());
    let mut event_router = new_router(filesystem);
    event_router
        .load(Box::new(WorkflowInstaller {
            json: BOTH_WORKFLOWS,
            state: Rc::clone(&state),
            register_stubs: true,
        }))
        .expect("load installer Component");

    drive_until(&mut event_router, |_router| state.done.get());

    assert!(state.error.borrow().is_none());
    let definitions = event_router.workflow_definitions();
    let ids: Vec<_> = definitions
        .iter()
        .map(|definition| definition.id().as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "gateway-message-to-recorder-with-a-long-workflow-id",
            "gateway-message-audit"
        ]
    );
    assert_eq!(
        filesystem
            .read("workflows/gateway-message-to-recorder-with-a-long-workflow-id.json")
            .expect("read first persisted Workflow"),
        ALPHA_JSON.as_bytes()
    );

    drop(event_router);
    let recovered = new_router(filesystem);
    assert_eq!(recovered.workflow_definitions(), definitions);
}

#[test]
fn streaming_unload_removes_runtime_and_durable_state() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let state = Rc::new(ControlState::default());
    let mut event_router = new_router(filesystem);
    event_router
        .load(Box::new(WorkflowUninstaller {
            state: Rc::clone(&state),
        }))
        .expect("load uninstaller Component");

    drive_until(&mut event_router, |_router| state.done.get());

    assert!(state.error.borrow().is_none());
    assert!(event_router.workflow_definitions().is_empty());
    assert!(
        !filesystem.exists("workflows/gateway-message-to-recorder-with-a-long-workflow-id.json")
    );
    drop(event_router);
    assert!(new_router(filesystem).workflow_definitions().is_empty());
}

#[test]
fn invalid_and_duplicate_workflow_json_are_rejected_without_corrupting_the_catalog() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let invalid_state = Rc::new(ControlState::default());
    let mut event_router = new_router(filesystem);
    event_router
        .load(Box::new(WorkflowInstaller {
            json: INVALID_WORKFLOW,
            state: Rc::clone(&invalid_state),
            register_stubs: true,
        }))
        .expect("load invalid installer Component");
    drive_until(&mut event_router, |_router| invalid_state.done.get());
    assert!(matches!(
        invalid_state.error.borrow().as_ref(),
        Some(WorkflowControlError::Rejected(
            WorkflowControlRejection::InvalidJson
        ))
    ));
    assert!(event_router.workflow_definitions().is_empty());

    let duplicate_state = Rc::new(ControlState::default());
    event_router
        .load(Box::new(WorkflowInstaller {
            json: DUPLICATE_ALPHA,
            state: Rc::clone(&duplicate_state),
            // The first installer already registered the stub endpoints.
            register_stubs: false,
        }))
        .expect("load duplicate installer Component");
    drive_until(&mut event_router, |_router| duplicate_state.done.get());
    assert!(matches!(
        duplicate_state.error.borrow().as_ref(),
        Some(WorkflowControlError::Rejected(
            WorkflowControlRejection::DuplicateId
        ))
    ));
    assert_eq!(event_router.workflow_definitions().len(), 1);
}

struct GatewayMessage;

impl Event for GatewayMessage {
    const ID: &'static str = "gateway.message.received";
    type Message = [u8; 4];
    type Input = Unary;
}

struct AddOne;

impl RpcMethod for AddOne {
    const ADDRESS: &'static str = "integration.add-one";
    type Request = [u8; 4];
    type Response = [u8; 4];
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct Record;

impl RpcMethod for Record {
    const ADDRESS: &'static str = "integration.record";
    type Request = [u8; 4];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct SequentialExecution {
    emitted: Rc<Cell<bool>>,
    recorded: Rc<Cell<Option<u32>>>,
}

impl Component<FRAME_SIZE> for SequentialExecution {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_rpc::<AddOne, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            let value = u32::from_le_bytes(*request.view()?);
            Ok(Ok(value.saturating_add(1).to_le_bytes()))
        })?;
        let recorded = Rc::clone(&self.recorded);
        context.register_rpc::<Record, _>(move |_context, request: RpcFrame<[u8; 4]>| {
            let recorded = Rc::clone(&recorded);
            async move {
                recorded.set(Some(u32::from_le_bytes(*request.view()?)));
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(ALPHA_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessage>(41_u32.to_le_bytes())
                .await
                .map_err(ComponentError::lifecycle)?;
            self.emitted.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn matching_event_executes_sequential_rpc_steps_and_updates_snapshot() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let emitted = Rc::new(Cell::new(false));
    let recorded = Rc::new(Cell::new(None));
    let mut event_router = new_router(filesystem);
    event_router
        .load(Box::new(SequentialExecution {
            emitted: Rc::clone(&emitted),
            recorded: Rc::clone(&recorded),
        }))
        .expect("load execution Component");

    drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 1
    });

    assert_eq!(recorded.get(), Some(42));
    let info = event_router.workflow_info();
    assert_eq!(info.completed_count, 1);
    assert_eq!(info.failed_count, 0);
    assert_eq!(info.cancelled_count, 0);
    assert!(info.last_failure.is_none());
}

struct Audit;

impl RpcMethod for Audit {
    const ADDRESS: &'static str = "integration.audit";
    type Request = [u8; 4];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct FanoutExecution {
    emitted: Rc<Cell<bool>>,
    recorded: Rc<Cell<bool>>,
    audited: Rc<Cell<bool>>,
}

impl Component<FRAME_SIZE> for FanoutExecution {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let recorded = Rc::clone(&self.recorded);
        context.register_rpc::<AddOne, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(*request.view()?))
        })?;
        context.register_rpc::<Record, _>(move |_context, request: RpcFrame<[u8; 4]>| {
            let recorded = Rc::clone(&recorded);
            async move {
                request.view()?;
                recorded.set(true);
                Ok(Ok(()))
            }
        })?;
        let audited = Rc::clone(&self.audited);
        context.register_rpc::<Audit, _>(move |_context, request: RpcFrame<[u8; 4]>| {
            let audited = Rc::clone(&audited);
            async move {
                request.view()?;
                audited.set(true);
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone());
            for json in BOTH_WORKFLOWS {
                client.load(json).await.map_err(ComponentError::lifecycle)?;
            }
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessage>([1, 2, 3, 4])
                .await
                .map_err(ComponentError::lifecycle)?;
            self.emitted.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn one_event_executes_every_exact_and_wildcard_match() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let emitted = Rc::new(Cell::new(false));
    let recorded = Rc::new(Cell::new(false));
    let audited = Rc::new(Cell::new(false));
    let mut event_router = new_router(filesystem);
    event_router
        .load(Box::new(FanoutExecution {
            emitted: Rc::clone(&emitted),
            recorded: Rc::clone(&recorded),
            audited: Rc::clone(&audited),
        }))
        .expect("load fanout Component");

    drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 2
    });

    assert!(recorded.get());
    assert!(audited.get());
    assert_eq!(event_router.workflow_info().failed_count, 0);
}

const STREAMING_JSON: &str = r#"{
    "id": "streaming-event-collector",
    "match": { "event": "sensor.samples" },
    "steps": [
        { "call": "integration.collect" }
    ]
}"#;

struct SensorSamples;

impl Event for SensorSamples {
    const ID: &'static str = "sensor.samples";
    type Message = [u8; 1];
    type Input = Streaming;
}

struct Collect;

impl RpcMethod for Collect {
    const ADDRESS: &'static str = "integration.collect";
    type Request = [u8; 1];
    type Response = ();
    type Error = ();
    type Input = Streaming;
    type Output = Unary;
}

struct StreamingExecution {
    emitted: Rc<Cell<bool>>,
    collected: Rc<RefCell<Vec<u8>>>,
}

impl Component<FRAME_SIZE> for StreamingExecution {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let collected = Rc::clone(&self.collected);
        context.register_rpc::<Collect, _>(
            move |_context, mut requests: RpcStream<RpcFrame<[u8; 1]>>| {
                let collected = Rc::clone(&collected);
                async move {
                    while let Some(request) = requests.next().await {
                        let [value] = *request?.view()?;
                        collected.borrow_mut().push(value);
                    }
                    Ok(Ok(()))
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(STREAMING_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            let samples = RpcStream::new(stream::iter([Ok([10]), Ok([11]), Ok([12])]));
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<SensorSamples>(samples)
                .await
                .map_err(ComponentError::lifecycle)?;
            self.emitted.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn streaming_event_preserves_message_order_through_workflow_ingress() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let emitted = Rc::new(Cell::new(false));
    let collected = Rc::new(RefCell::new(Vec::new()));
    let mut event_router = new_router(filesystem);
    event_router
        .load(Box::new(StreamingExecution {
            emitted: Rc::clone(&emitted),
            collected: Rc::clone(&collected),
        }))
        .expect("load streaming Component");

    drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 1
    });

    assert_eq!(collected.borrow().as_slice(), [10, 11, 12]);
    assert_eq!(event_router.workflow_info().failed_count, 0);
}
