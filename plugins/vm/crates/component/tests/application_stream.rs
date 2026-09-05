#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(missing_docs)]

use std::cell::RefCell;
use std::future::{Future, pending};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::task::Poll;
use std::time::Duration;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, JsonRef, JsonRpcSchema, JsonSchema,
    JsonWriter, RegisterContext, RpcAddress, RpcClient, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vm_component::run::{Cancel, Input, Run};
use barracuda_vm_component::{BuiltinPackages, VmComponent, VmLimits, VmRuntime};
use embassy_executor::{Executor, Spawner};
use serde_json::Value;

const FRAME_SIZE: usize = 512;
const EMPTY_SCHEMA: JsonSchema =
    JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);
const EVENT_SCHEMA: JsonSchema = JsonSchema::new(r#"{"type":"object"}"#);

const OUTPUT_WORKFLOW: &str = r#"{
    "id":"vm-output-test",
    "match":{"event":"vm.output"},
    "steps":[{"call":"test.vm-output"}]
}"#;
const INPUT_WORKFLOW: &str = r#"{
    "id":"vm-input-test",
    "match":{"event":"vm.input_required"},
    "steps":[{"call":"test.vm-input"}]
}"#;
const FINISHED_WORKFLOW: &str = r#"{
    "id":"vm-finished-test",
    "match":{"event":"vm.finished"},
    "steps":[{"call":"test.vm-finished"}]
}"#;

struct RecordOutput;

impl JsonRpcSchema for RecordOutput {
    const ADDRESS: &'static str = "test.vm-output";
    const REQUEST_SCHEMA: JsonSchema = EVENT_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = FRAME_SIZE;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct RecordInput;

impl JsonRpcSchema for RecordInput {
    const ADDRESS: &'static str = "test.vm-input";
    const REQUEST_SCHEMA: JsonSchema = EVENT_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = FRAME_SIZE;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct RecordFinished;

impl JsonRpcSchema for RecordFinished {
    const ADDRESS: &'static str = "test.vm-finished";
    const REQUEST_SCHEMA: JsonSchema = EVENT_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = FRAME_SIZE;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Default)]
struct Observed {
    outputs: RefCell<Vec<Value>>,
    input_required: RefCell<Vec<Value>>,
    finished: RefCell<Vec<Value>>,
}

struct Driver {
    observed: Rc<Observed>,
    completed: SyncSender<Result<(), String>>,
}

impl Component<FRAME_SIZE> for Driver {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let observed = Rc::clone(&self.observed);
        context.register_json::<RecordOutput, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let observed = Rc::clone(&observed);
                async move {
                    observed.outputs.borrow_mut().push(request.deserialize()?);
                    response.write("{}").await
                }
            },
        )?;
        let observed = Rc::clone(&self.observed);
        context.register_json::<RecordInput, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let observed = Rc::clone(&observed);
                async move {
                    observed
                        .input_required
                        .borrow_mut()
                        .push(request.deserialize()?);
                    response.write("{}").await
                }
            },
        )?;
        let observed = Rc::clone(&self.observed);
        context.register_json::<RecordFinished, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let observed = Rc::clone(&observed);
                async move {
                    observed.finished.borrow_mut().push(request.deserialize()?);
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let result = self.exercise(context.rpc().clone()).await;
            let _result = self.completed.send(result);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

impl Driver {
    async fn exercise(&self, rpc: RpcClient) -> Result<(), String> {
        let workflows = WorkflowClient::<FRAME_SIZE>::new(rpc.clone());
        for definition in [OUTPUT_WORKFLOW, INPUT_WORKFLOW, FINISHED_WORKFLOW] {
            workflows
                .load(definition)
                .await
                .map_err(|error| error.to_string())?;
        }
        let source = "local io=require('io'); local name=io.input(); \
                      io.print('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789',name)";
        let request = format!(r#"{{"source":"{source}"}}"#);
        let accepted = call(&rpc, Run::ADDRESS, &request).await?;
        let run_id = accepted
            .get("run_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("vm.run was not accepted: {accepted}"))?;

        wait_until(|| !self.observed.input_required.borrow().is_empty()).await;
        let response = call(
            &rpc,
            Input::ADDRESS,
            &format!(r#"{{"run_id":{run_id},"input":"barracuda"}}"#),
        )
        .await?;
        if response != Value::Object(Default::default()) {
            return Err(format!("vm.input failed: {response}"));
        }
        wait_until(|| !self.observed.finished.borrow().is_empty()).await;

        let input_event = self
            .observed
            .input_required
            .borrow()
            .first()
            .cloned()
            .ok_or_else(|| "missing input event".to_owned())?;
        if input_event.get("run_id").and_then(Value::as_u64) != Some(run_id)
            || input_event.get("sequence").and_then(Value::as_u64) != Some(0)
        {
            return Err(format!("invalid input event: {input_event}"));
        }

        let mut output_events = self.observed.outputs.borrow().clone();
        output_events.sort_by_key(|event| event.get("sequence").and_then(Value::as_u64));
        let output = output_events
            .iter()
            .filter_map(|event| event.get("chunk").and_then(Value::as_str))
            .collect::<String>();
        if output != "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\tbarracuda" {
            return Err(format!("unexpected VM output: {output:?}"));
        }
        if output_events.len() < 2
            || output_events
                .last()
                .and_then(|event| event.get("message_end"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Err(format!(
                "output was not chunked with a terminal boundary: {output_events:?}"
            ));
        }

        let finished = self
            .observed
            .finished
            .borrow()
            .first()
            .cloned()
            .ok_or_else(|| "missing terminal event".to_owned())?;
        if finished.get("run_id").and_then(Value::as_u64) != Some(run_id)
            || finished.get("outcome").and_then(Value::as_str) != Some("success")
        {
            return Err(format!("invalid terminal event: {finished}"));
        }
        embassy_time::Timer::after_millis(1).await;

        let first = call(&rpc, Run::ADDRESS, r#"{"source":"while true do end"}"#).await?;
        let first_id = first
            .get("run_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("infinite run was not accepted: {first}"))?;
        let oversized_input = "x".repeat(
            VmLimits::default()
                .max_input_bytes()
                .checked_add(1)
                .ok_or_else(|| "input limit overflow".to_owned())?,
        );
        let rejected = call(
            &rpc,
            Input::ADDRESS,
            &format!(r#"{{"run_id":{first_id},"input":"{oversized_input}"}}"#),
        )
        .await?;
        if rejected.get("error").and_then(Value::as_str) != Some("input_limit_exceeded") {
            return Err(format!("oversized input was not rejected: {rejected}"));
        }
        let queued = call(
            &rpc,
            Input::ADDRESS,
            &format!(r#"{{"run_id":{first_id},"input":"first"}}"#),
        )
        .await?;
        if queued != Value::Object(Default::default()) {
            return Err(format!("first queued input failed: {queued}"));
        }
        let backpressure = call(
            &rpc,
            Input::ADDRESS,
            &format!(r#"{{"run_id":{first_id},"input":"second"}}"#),
        )
        .await?;
        if backpressure.get("error").and_then(Value::as_str) != Some("input_backpressure") {
            return Err(format!(
                "second queued input did not apply backpressure: {backpressure}"
            ));
        }

        let mut active = vec![first_id];
        loop {
            let accepted = call(&rpc, Run::ADDRESS, r#"{"source":"while true do end"}"#).await?;
            if let Some(run_id) = accepted.get("run_id").and_then(Value::as_u64) {
                active.push(run_id);
                if active.len() >= 16 {
                    return Err("VM did not enforce a bounded run pool".to_owned());
                }
                continue;
            }
            if accepted.get("error").and_then(Value::as_str) == Some("busy") {
                break;
            }
            return Err(format!("unexpected vm.run response: {accepted}"));
        }
        let active_count = active.len();
        for run_id in active {
            let response =
                call(&rpc, Cancel::ADDRESS, &format!(r#"{{"run_id":{run_id}}}"#)).await?;
            if response != Value::Object(Default::default()) {
                return Err(format!("vm.cancel failed: {response}"));
            }
        }
        wait_until(|| {
            self.observed
                .finished
                .borrow()
                .iter()
                .filter(|event| event.get("outcome").and_then(Value::as_str) == Some("cancelled"))
                .count()
                == active_count
        })
        .await;
        Ok(())
    }
}

async fn call(rpc: &RpcClient, method: &str, request: &str) -> Result<Value, String> {
    let address = RpcAddress::try_from(method).map_err(|error| error.to_string())?;
    let response = rpc
        .call_json(&address, request)
        .map_err(|error| error.to_string())?
        .await
        .map_err(|error| error.to_string())?;
    response.deserialize().map_err(|error| error.to_string())
}

async fn wait_until(mut ready: impl FnMut() -> bool) {
    while !ready() {
        futures_lite::future::yield_now().await;
    }
}

#[embassy_executor::task]
async fn exercise_application_stream(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        install_global_memory_vfs()
            .await
            .map_err(|error| error.to_string())?;
        let runtime = VmRuntime::new().map_err(|error| error.to_string())?;
        runtime.start(spawner).map_err(|error| error.to_string())?;
        let lanes = Box::leak(Box::new(RpcLaneStorage::<16, FRAME_SIZE, 16>::new()));
        let mut router = EventRouter::new(lanes)
            .await
            .map_err(|error| error.to_string())?;
        router
            .load(Box::new(
                VmComponent::with_runtime(BuiltinPackages::all(), runtime)
                    .with_limits(VmLimits::default().with_instruction_hook_interval(100)),
            ))
            .map_err(|error| error.to_string())?;
        router
            .load(Box::new(Driver {
                observed: Rc::new(Observed::default()),
                completed,
            }))
            .map_err(|error| error.to_string())?;

        core::future::poll_fn(|context| match Pin::new(&mut router).poll(context) {
            Poll::Ready(result) => Poll::Ready(result.map_err(|error| error.to_string())),
            Poll::Pending => Poll::Pending,
        })
        .await
    }
    .await;
    if let Err(error) = result {
        // The Driver owns `completed` after a successful load, so only setup
        // failures can reach this branch before ownership moves.
        panic!("VM application stream setup failed: {error}");
    }
}

#[test]
fn vm_uses_bounded_json_events_for_output_input_and_terminal_state() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(exercise_application_stream(spawner, completed))
                .expect("spawn VM stream test");
        });
    });

    result
        .recv_timeout(Duration::from_secs(10))
        .expect("VM stream test timed out")
        .expect("VM stream test failed");
}
