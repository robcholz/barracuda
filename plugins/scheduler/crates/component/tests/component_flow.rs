#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::string::String;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, JsonRef,
    JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcAddress, RpcError, RpcLaneStorage,
    RunContext, UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginError, PluginId, PluginManager, PluginRegisterContext,
    PluginResult,
};
use barracuda_scheduler_component::{SchedulerComponent, SchedulerConfig};
use barracuda_time_component::{SyncSample, TimeConfig, UtcClock, UtcClockUpdater, utc_clock};
use embassy_time::Instant;
use serde::Deserialize;

const FRAME_SIZE: usize = 512;
const EMPTY_SCHEMA: JsonSchema =
    JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);
const TRIGGERED_SCHEMA: JsonSchema = JsonSchema::new(
    r#"{"type":"object","properties":{"id":{"type":"string"},"run_number":{"type":"integer"}},"required":["id","run_number"],"additionalProperties":false}"#,
);
const WORKFLOW: &str = r#"{
    "id":"scheduler-test",
    "match":{"event":"scheduler.triggered","topic":"typed-time-flow"},
    "steps":[{"call":"scheduler-test.record"}]
}"#;
const RESTORED_WORKFLOW: &str = r#"{
    "id":"scheduler-restored-test",
    "match":{"event":"scheduler.triggered","topic":"restart-flow"},
    "steps":[{"call":"scheduler-test.record"}]
}"#;

struct Record;

impl JsonRpcSchema for Record {
    const ADDRESS: &'static str = "scheduler-test.record";
    const REQUEST_SCHEMA: JsonSchema = TRIGGERED_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct TestSchedulerPlugin {
    clock: Rc<UtcClock>,
}

impl PluginDeclaration for TestSchedulerPlugin {
    const ID: &'static str = "scheduler-component-test";
}

impl Plugin<FRAME_SIZE> for TestSchedulerPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let scheduler = futures_lite::future::block_on(SchedulerComponent::load(
            SchedulerConfig::new(10),
            Rc::clone(&self.clock),
            context.storage().clone(),
        ))
        .map_err(PluginError::registration)?;
        context.event_router.load(scheduler)?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Triggered<'a> {
    #[serde(borrow)]
    id: &'a str,
    run_number: u64,
}

struct TestDriver {
    recorded: Rc<RefCell<Option<(String, u64)>>>,
    scheduled: Rc<Cell<bool>>,
    clock_updater: UtcClockUpdater,
}

impl Component<FRAME_SIZE> for TestDriver {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let recorded = Rc::clone(&self.recorded);
        context.register_json::<Record, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let recorded = Rc::clone(&recorded);
                async move {
                    let event = request.deserialize::<Triggered<'_>>()?;
                    recorded.replace(Some((String::from(event.id), event.run_number)));
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            WorkflowClient::<FRAME_SIZE>::new(client.clone())
                .load(WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            WorkflowClient::<FRAME_SIZE>::new(client.clone())
                .load(RESTORED_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;

            let public = client
                .rpcs_by_visibility("*")
                .map_err(ComponentError::lifecycle)?;
            assert!(
                public
                    .iter()
                    .any(|address| address.as_ref() == "scheduler.schedule")
            );
            assert!(
                public
                    .iter()
                    .any(|address| address.as_ref() == "scheduler.cancel")
            );
            assert!(
                client
                    .rpcs_by_visibility("agent")
                    .map_err(ComponentError::lifecycle)?
                    .iter()
                    .all(|address| !address.as_ref().starts_with("scheduler."))
            );

            let schedule = RpcAddress::try_from("scheduler.schedule").map_err(RpcError::from)?;
            let cancel = RpcAddress::try_from("scheduler.cancel").map_err(RpcError::from)?;
            let future =
                r#"{"id":"cancel-me","trigger":{"type":"once","at":"2027-01-15T08:01:00.000Z"}}"#;
            let unavailable = client.call_json(&schedule, future)?.await?;
            assert_eq!(unavailable.as_str()?, r#"{"error":"time_unavailable"}"#);
            self.clock_updater
                .synchronize(SyncSample::new(1_800_000_000_000, Instant::now()));
            let accepted = client.call_json(&schedule, future)?.await?;
            assert_eq!(accepted.as_str()?, r#"{"id":"cancel-me"}"#);
            let duplicate = client.call_json(&schedule, future)?.await?;
            assert_eq!(duplicate.as_str()?, r#"{"error":"duplicate_id"}"#);

            let cancelled = client.call_json(&cancel, r#"{"id":"cancel-me"}"#)?.await?;
            assert_eq!(
                cancelled.as_str()?,
                r#"{"id":"cancel-me","completed_runs":0}"#
            );
            let missing = client.call_json(&cancel, r#"{"id":"cancel-me"}"#)?.await?;
            assert_eq!(missing.as_str()?, r#"{"error":"not_found"}"#);

            let malformed = client.call_json(&schedule, "[]")?.await;
            assert_eq!(
                malformed.expect_err("reject wrong request shape"),
                RpcError::InvalidJson
            );

            let due = r#"{"id":"typed-time-flow","trigger":{"type":"once","at":"2027-01-15T08:00:00.000Z"}}"#;
            let accepted = client.call_json(&schedule, due)?.await?;
            assert_eq!(accepted.as_str()?, r#"{"id":"typed-time-flow"}"#);
            let restart = r#"{"id":"restart-flow","trigger":{"type":"once","at":"2027-01-15T08:01:00.000Z"}}"#;
            let accepted = client.call_json(&schedule, restart)?.await?;
            assert_eq!(accepted.as_str()?, r#"{"id":"restart-flow"}"#);
            self.scheduled.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn scheduler_uses_time_capability_and_routes_json_event() {
    futures_lite::future::block_on(install_global_memory_vfs()).expect("install global test VFS");
    let partition = futures_lite::future::block_on(memory_partition(64 * 1024))
        .expect("create test KV partition");
    let mut manager = futures_lite::future::block_on(PluginManager::open(partition))
        .expect("open Plugin storage");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, FRAME_SIZE, 8>::new()));
    let mut router =
        futures_lite::future::block_on(EventRouter::new(lanes)).expect("create Event Router");
    let (clock, updater) = utc_clock(TimeConfig::new(10, 60_000, u64::MAX));
    manager
        .register(&mut router, TestSchedulerPlugin { clock })
        .expect("register scheduler Component");
    let recorded = Rc::new(RefCell::new(None));
    let scheduled = Rc::new(Cell::new(false));
    router
        .load(Box::new(TestDriver {
            recorded: Rc::clone(&recorded),
            scheduled: Rc::clone(&scheduled),
            clock_updater: updater,
        }))
        .expect("load test driver");

    futures_lite::future::block_on(poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut router).poll(context) {
            return Poll::Ready(result);
        }
        if scheduled.get() && recorded.borrow().is_some() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }))
    .expect("drive scheduler flow");

    assert_eq!(
        recorded.borrow().as_ref(),
        Some(&(String::from("typed-time-flow"), 1))
    );

    let scheduler_id = PluginId::try_from(TestSchedulerPlugin::ID).expect("valid Plugin ID");
    futures_lite::future::block_on(manager.unload(&mut router, &scheduler_id))
        .expect("unload scheduler Component");

    recorded.replace(None);
    let (clock, updater) = utc_clock(TimeConfig::new(10, 60_000, u64::MAX));
    updater.synchronize(SyncSample::new(1_800_000_060_000, Instant::now()));
    manager
        .register(&mut router, TestSchedulerPlugin { clock })
        .expect("restore scheduler Component");

    futures_lite::future::block_on(poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut router).poll(context) {
            return Poll::Ready(result);
        }
        if recorded.borrow().is_some() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }))
    .expect("drive restored scheduler flow");

    assert_eq!(
        recorded.borrow().as_ref(),
        Some(&(String::from("restart-flow"), 1))
    );
}
