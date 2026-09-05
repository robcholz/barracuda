#![allow(clippy::expect_used)]
#![allow(missing_docs)]

mod support;

use core::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient, WorkflowControlError, WorkflowControlRejection, WorkflowId,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vfs::remove_file;
use futures_lite::future::block_on;
use serde::Deserialize;
use serde_json::json;

const FRAME_SIZE: usize = 512;
const VALUE_SCHEMA: JsonSchema = JsonSchema::new(
    r#"{"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}"#,
);
const EMPTY_SCHEMA: JsonSchema =
    JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);
const DECISION_SCHEMA: JsonSchema = JsonSchema::new(
    r#"{"type":"object","properties":{"forward":{"type":"boolean"},"value":{"type":"integer"}},"required":["forward","value"],"additionalProperties":false}"#,
);
static GLOBAL_VFS_TEST_LOCK: Mutex<()> = Mutex::new(());

fn reset_global_vfs() -> MutexGuard<'static, ()> {
    let guard = GLOBAL_VFS_TEST_LOCK.lock().expect("lock global VFS test");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let _ignored = block_on(remove_file("/system/workflows.json"));
    guard
}

fn new_router<const N: usize>() -> EventRouter<N, FRAME_SIZE, N> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, FRAME_SIZE, N>::new()));
    block_on(EventRouter::new(lanes)).expect("create Event Router")
}

struct GatewayMessage;

impl Event for GatewayMessage {
    const ID: &'static str = "gateway.message.received";
}

struct AddOne;

impl JsonRpcSchema for AddOne {
    const ADDRESS: &'static str = "integration.add-one";
    const REQUEST_SCHEMA: JsonSchema = VALUE_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = VALUE_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 32;
}

struct Record;

impl JsonRpcSchema for Record {
    const ADDRESS: &'static str = "integration.record";
    const REQUEST_SCHEMA: JsonSchema = VALUE_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct Audit;

impl JsonRpcSchema for Audit {
    const ADDRESS: &'static str = "integration.audit";
    const REQUEST_SCHEMA: JsonSchema = VALUE_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Deserialize)]
struct Number {
    value: u32,
}

const MAPPING_WORKFLOW: &str = r#"{
    "id":"mapping",
    "match":{"event":"gateway.message.received"},
    "steps":[
        {"call":"integration.add-one","arguments":{"value":"$event.input.value"}},
        {"call":"integration.record"}
    ]
}"#;

struct MappingComponent {
    emitted: Rc<Cell<bool>>,
    recorded: Rc<Cell<Option<u32>>>,
}

impl Component<FRAME_SIZE> for MappingComponent {
    fn name(&self) -> &'static str {
        "mapping"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_json::<AddOne, _>(
            "*",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let value = request.deserialize::<Number>()?.value.saturating_add(1);
                response.write(&json!({"value":value})).await
            },
        )?;
        let recorded = Rc::clone(&self.recorded);
        context.register_json::<Record, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let recorded = Rc::clone(&recorded);
                async move {
                    recorded.set(Some(request.deserialize::<Number>()?.value));
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(MAPPING_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessage>(r#"{"value":41}"#)
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
fn event_selector_and_direct_link_execute_json_rpc_steps() {
    let _global_vfs = reset_global_vfs();
    let emitted = Rc::new(Cell::new(false));
    let recorded = Rc::new(Cell::new(None));
    let mut event_router = new_router::<2>();
    event_router
        .load(Box::new(MappingComponent {
            emitted: Rc::clone(&emitted),
            recorded: Rc::clone(&recorded),
        }))
        .expect("load mapping Component");

    support::drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 1
    })
    .expect("drive JSON Workflow");

    assert_eq!(recorded.get(), Some(42));
    assert_eq!(event_router.workflow_info().failed_count, 0);
}

const RETURN_WORKFLOW: &str = r#"{
    "id":"return-success",
    "match":{"event":"gateway.message.received"},
    "steps":[{"return":{}}]
}"#;

struct ReturnComponent {
    emitted: Rc<Cell<bool>>,
}

impl Component<FRAME_SIZE> for ReturnComponent {
    fn name(&self) -> &'static str {
        "return"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(RETURN_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessage>(r#"{"value":41}"#)
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
fn explicit_return_completes_without_an_rpc_call() {
    let _global_vfs = reset_global_vfs();
    let emitted = Rc::new(Cell::new(false));
    let mut event_router = new_router::<1>();
    event_router
        .load(Box::new(ReturnComponent {
            emitted: Rc::clone(&emitted),
        }))
        .expect("load return Component");

    support::drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 1
    })
    .expect("drive return Workflow");

    assert_eq!(event_router.workflow_info().failed_count, 0);
}

struct Decide;

impl JsonRpcSchema for Decide {
    const ADDRESS: &'static str = "integration.decide";
    const REQUEST_SCHEMA: JsonSchema = VALUE_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = DECISION_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 48;
}

const BRANCH_WORKFLOW: &str = r#"{
    "id":"conditional-forward",
    "match":{"event":"gateway.message.received"},
    "steps":[
        {"call":"integration.decide"},
        {
            "if":"$previous.output.forward",
            "then":[
                {"call":"integration.add-one","arguments":{"value":"$previous.output.value"}}
            ],
            "else":[]
        },
        {"call":"integration.record","arguments":{"value":"$previous.output.value"}}
    ]
}"#;

struct BranchComponent {
    emitted: Rc<Cell<bool>>,
    recorded: Rc<RefCell<Vec<u32>>>,
}

impl Component<FRAME_SIZE> for BranchComponent {
    fn name(&self) -> &'static str {
        "branch"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_json::<Decide, _>(
            "*",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let value = request.deserialize::<Number>()?.value;
                response
                    .write(&json!({"forward":value > 0,"value":value}))
                    .await
            },
        )?;
        context.register_json::<AddOne, _>(
            "*",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let value = request.deserialize::<Number>()?.value.saturating_add(1);
                response.write(&json!({"value":value})).await
            },
        )?;
        let recorded = Rc::clone(&self.recorded);
        context.register_json::<Record, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let recorded = Rc::clone(&recorded);
                async move {
                    recorded
                        .borrow_mut()
                        .push(request.deserialize::<Number>()?.value);
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(BRANCH_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            let emitter = EventEmitter::<FRAME_SIZE>::new(context.rpc().clone());
            emitter
                .emit::<GatewayMessage>(r#"{"value":7}"#)
                .await
                .map_err(ComponentError::lifecycle)?;
            emitter
                .emit::<GatewayMessage>(r#"{"value":0}"#)
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
fn dynamic_if_else_continues_with_the_last_executed_rpc_output() {
    let _global_vfs = reset_global_vfs();
    let emitted = Rc::new(Cell::new(false));
    let recorded = Rc::new(RefCell::new(Vec::new()));
    let mut event_router = new_router::<3>();
    event_router
        .load(Box::new(BranchComponent {
            emitted: Rc::clone(&emitted),
            recorded: Rc::clone(&recorded),
        }))
        .expect("load branch Component");

    support::drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 2
    })
    .expect("drive branch Workflow");

    assert_eq!(recorded.borrow().as_slice(), [8, 0]);
    assert_eq!(event_router.workflow_info().failed_count, 0);
}

const EXACT_WORKFLOW: &str = r#"{
    "id":"exact",
    "match":{"event":"gateway.message.received"},
    "steps":[{"call":"integration.record"}]
}"#;
const WILDCARD_WORKFLOW: &str = r#"{
    "id":"wildcard",
    "match":{"event":"gateway.*"},
    "steps":[{"call":"integration.audit"}]
}"#;

struct FanoutComponent {
    emitted: Rc<Cell<bool>>,
    recorded: Rc<RefCell<Vec<u32>>>,
}

impl Component<FRAME_SIZE> for FanoutComponent {
    fn name(&self) -> &'static str {
        "fanout"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let exact = Rc::clone(&self.recorded);
        context.register_json::<Record, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let exact = Rc::clone(&exact);
                async move {
                    exact
                        .borrow_mut()
                        .push(request.deserialize::<Number>()?.value);
                    response.write("{}").await
                }
            },
        )?;
        let wildcard = Rc::clone(&self.recorded);
        context.register_json::<Audit, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let wildcard = Rc::clone(&wildcard);
                async move {
                    wildcard
                        .borrow_mut()
                        .push(request.deserialize::<Number>()?.value);
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let workflows = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone());
            workflows
                .load(EXACT_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            workflows
                .load(WILDCARD_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessage>(r#"{"value":7}"#)
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
    let _global_vfs = reset_global_vfs();
    let emitted = Rc::new(Cell::new(false));
    let recorded = Rc::new(RefCell::new(Vec::new()));
    let mut event_router = new_router::<2>();
    event_router
        .load(Box::new(FanoutComponent {
            emitted: Rc::clone(&emitted),
            recorded: Rc::clone(&recorded),
        }))
        .expect("load fanout Component");

    support::drive_until(&mut event_router, |router| {
        emitted.get() && router.workflow_info().completed_count == 2
    })
    .expect("drive fanout Workflow");

    assert_eq!(recorded.borrow().as_slice(), [7, 7]);
    assert_eq!(event_router.workflow_info().failed_count, 0);
}

#[derive(Default)]
struct ControlState {
    done: Cell<bool>,
    duplicate: RefCell<Option<WorkflowControlError>>,
    missing: RefCell<Option<WorkflowControlError>>,
}

struct ControlComponent {
    state: Rc<ControlState>,
}

impl Component<FRAME_SIZE> for ControlComponent {
    fn name(&self) -> &'static str {
        "control"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_json::<Record, _>(
            "*",
            |_context, _request, response: JsonWriter| async move { response.write("{}").await },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone());
            client
                .load(EXACT_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            self.state
                .duplicate
                .replace(client.load(EXACT_WORKFLOW).await.err());
            let id = WorkflowId::try_from("exact").map_err(ComponentError::lifecycle)?;
            client
                .unload(&id)
                .await
                .map_err(ComponentError::lifecycle)?;
            self.state.missing.replace(client.unload(&id).await.err());
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn json_control_preserves_duplicate_and_not_found_rejections() {
    let _global_vfs = reset_global_vfs();
    let state = Rc::new(ControlState::default());
    let mut event_router = new_router::<1>();
    event_router
        .load(Box::new(ControlComponent {
            state: Rc::clone(&state),
        }))
        .expect("load control Component");

    support::drive_until(&mut event_router, |_router| state.done.get())
        .expect("drive JSON control calls");

    assert_eq!(
        state.duplicate.borrow().as_ref(),
        Some(&WorkflowControlError::Rejected(
            WorkflowControlRejection::DuplicateId
        ))
    );
    assert_eq!(
        state.missing.borrow().as_ref(),
        Some(&WorkflowControlError::Rejected(
            WorkflowControlRejection::NotFound
        ))
    );
}
