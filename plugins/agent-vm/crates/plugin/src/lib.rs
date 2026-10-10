//! Native Agent Tool adapter for the VM capability.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{
        BackgroundTool, BackgroundToolControl, BackgroundToolFuture, BackgroundToolHandler, Tool,
        ToolError, ToolFuture, ToolGroup, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Vm, VmError, VmInputRequest, VmRunOutcome, VmRunRequest, VmRunState, VmRunUpdate,
};
use serde::Serialize;

/// Plugin registering VM operations with the Agent runtime.
#[barracuda_plugin::macros::plugin]
pub struct AgentVmPlugin;

impl AgentVmPlugin {
    /// Creates the stateless Agent adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentVmPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let vm = context.require::<Vm>("vm")?;
        tools
            .register_group(vm_tool_group(vm))
            .map_err(PluginError::registration)
    }
}

/// `vm_run` is the only VM Tool: the Agent's background Tools list, feed, and
/// cancel its runs.
fn vm_tool_group(vm: Rc<Vm>) -> ToolGroup {
    ToolGroup::new("vm", false, [Tool::background(VmRunTool { vm })])
}

struct VmRunTool {
    vm: Rc<Vm>,
}

impl ToolSpec for VmRunTool {
    barracuda_agent_plugin::tools::tool_metadata!("vm_run");
}

impl BackgroundToolHandler for VmRunTool {
    type Args = VmRunRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> BackgroundToolFuture<'a> {
        let run = self.vm.run(request);
        Box::pin(async move {
            let run = run.map_err(|error| ToolError::InvokeRejected(error.to_string()))?;
            let run_id = run.run_id();
            let accepted = encode(&RunAccepted { run_id }, true)?;
            // The completion future owns the `VmRun`; dropping it cancels the run.
            let background = BackgroundTool::with_progress(accepted, |progress| {
                Box::pin(async move {
                    let mut run = run;
                    loop {
                        match run.next_update().await {
                            Ok(VmRunUpdate::Progress(update)) => {
                                progress.send(encode(&update, true)?);
                            }
                            Ok(VmRunUpdate::Completed(response)) => {
                                let ok = response.outcome == VmRunOutcome::Success;
                                return encode(&response, ok);
                            }
                            Err(error) => return encode(&ErrorResponse { error }, false),
                        }
                    }
                })
            });
            Ok(background.with_control(VmRunControl {
                vm: Rc::clone(&self.vm),
                run_id,
            }))
        })
    }
}

#[derive(Serialize)]
struct RunAccepted {
    run_id: u32,
}

/// Reports one run's state and feeds its `io.read()`.
struct VmRunControl {
    vm: Rc<Vm>,
    run_id: u32,
}

impl BackgroundToolControl for VmRunControl {
    fn status(&self) -> Option<String> {
        let run = self
            .vm
            .list()
            .runs
            .into_iter()
            .find(|run| run.run_id == self.run_id)?;
        let state = match run.state {
            VmRunState::Running => "running",
            VmRunState::InputRequired => "input_required",
        };
        Some(state.into())
    }

    fn input<'a>(&'a self, input: Option<String>) -> ToolFuture<'a> {
        let eof = input.is_none().then_some(true);
        let result = self.vm.input(VmInputRequest {
            run_id: self.run_id,
            input,
            eof,
        });
        Box::pin(async move { tool_output(result) })
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: VmError,
}

fn tool_output<Response: Serialize>(
    result: Result<Response, VmError>,
) -> Result<ToolOutput, ToolInvokeError> {
    match result {
        Ok(response) => encode(&response, true),
        Err(error) => encode(&ErrorResponse { error }, false),
    }
}

fn encode(response: &impl Serialize, ok: bool) -> Result<ToolOutput, ToolInvokeError> {
    let content = serde_json::to_string(response).map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode VM Tool response"))
    })?;
    Ok(ToolOutput { content, ok })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use std::sync::mpsc::{SyncSender, sync_channel};
    use std::time::Duration;

    use alloc::{
        boxed::Box,
        format,
        rc::Rc,
        string::{String, ToString},
    };

    use barracuda_agent_tool::{
        BackgroundToolCall, BackgroundToolPool, ToolInvocation, ToolOutput, ToolRunner, ToolSet,
    };
    use barracuda_runtime_utils::background::{BackgroundEvent, BackgroundUpdate};
    use barracuda_vm_plugin::{LuaPackageRegistry, SeedSource, Vm, VmRunRequest};
    use embassy_executor::{Executor, Spawner};
    use embassy_time::Timer;
    use futures_lite::StreamExt as _;
    use futures_lite::future::poll_fn;

    use super::vm_tool_group;

    async fn next_update(
        pool: &BackgroundToolPool,
    ) -> BackgroundEvent<BackgroundToolCall, ToolOutput> {
        poll_fn(|context| pool.poll_next(context)).await
    }

    fn json(content: &str) -> Result<serde_json::Value, String> {
        serde_json::from_str(content).map_err(|error| format!("{error}: {content}"))
    }

    #[embassy_executor::task]
    async fn exercise_background_tool(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
        let result = async {
            let vm = Rc::new(
                Vm::new(LuaPackageRegistry::new(), SeedSource::unavailable())
                    .map_err(|error| error.to_string())?,
            );
            vm.start(spawner).map_err(|error| error.to_string())?;

            let mut tools = ToolSet::empty();
            tools
                .add_group(vm_tool_group(Rc::clone(&vm)))
                .map_err(|error| error.to_string())?;
            // The VM group is hidden until loaded; enable the Tool this test calls.
            tools
                .enable_tool("vm_run".to_string())
                .map_err(|error| error.to_string())?;
            let handle = tools.begin().map_err(|error| error.to_string())?;
            let pool = BackgroundToolPool::new();
            let run = |source: &str| {
                let invocation = ToolInvocation::try_new(
                    Some("vm-call"),
                    "vm_run",
                    &serde_json::to_string(&VmRunRequest {
                        source: source.into(),
                    })
                    .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                Ok::<_, String>(
                    ToolRunner::new(&handle)
                        .with_background(&pool)
                        .run(alloc::vec![invocation]),
                )
            };

            let accepted = run(concat!(
                "local value = io.read(); ",
                "print('background', value)"
            ))?
            .next()
            .await
            .ok_or("vm_run did not return an accepted output")?
            .1;
            let Some(run_json) = accepted
                .content
                .strip_prefix("[background:accepted]\nid: 1\n")
            else {
                return Err(format!("invalid accepted output: {accepted:?}"));
            };
            let accepted_run = json(run_json)?;

            let progress = next_update(&pool).await;
            let BackgroundUpdate::Progress(progress) = progress.update else {
                return Err(format!("expected progress update, got: {progress:?}"));
            };
            let progress = json(&progress.content)?;
            if progress.get("kind").and_then(serde_json::Value::as_str) != Some("input_required")
                || progress.get("run_id") != accepted_run.get("run_id")
            {
                return Err(format!("invalid progress update: {progress}"));
            }

            let listed = pool
                .list()
                .into_iter()
                .map(|(id, call)| (id, call.status()))
                .collect::<alloc::vec::Vec<_>>();
            if listed != [(1, String::from("input_required"))] {
                return Err(format!("invalid pool listing: {listed:?}"));
            }

            let fed = pool
                .get(1)
                .map_err(|error| error.to_string())?
                .input(Some("result".into()))
                .await
                .map_err(|error| error.to_string())?;
            if !fed.ok {
                return Err(format!("input was rejected: {fed:?}"));
            }

            let completion = next_update(&pool).await;
            let BackgroundUpdate::Completed(completion) = completion.update else {
                return Err(format!("expected completion update, got: {completion:?}"));
            };
            let completion = json(&completion.content)?;
            if completion
                .get("outcome")
                .and_then(serde_json::Value::as_str)
                != Some("success")
                || completion.get("output") != Some(&serde_json::json!(["background\tresult"]))
            {
                return Err(format!("invalid completion: {completion}"));
            }

            let accepted = run("io.read()")?
                .next()
                .await
                .ok_or("second vm_run did not return an accepted output")?
                .1;
            if !accepted.ok {
                return Err(format!("second vm_run was not accepted: {accepted:?}"));
            }
            let update = next_update(&pool).await;
            if update.id != 2 || !matches!(update.update, BackgroundUpdate::Progress(_)) {
                return Err(format!("expected progress update, got: {update:?}"));
            }

            pool.remove(2)
                .map(BackgroundToolCall::cancel)
                .map_err(|error| error.to_string())?;
            for _attempt in 0..50 {
                if vm.list().runs.is_empty() {
                    return Ok(());
                }
                Timer::after_millis(10).await;
            }
            Err(format!(
                "VM run remained active after its background call was cancelled: {:?}",
                vm.list()
            ))
        }
        .await;
        let _ignored = completed.send(result);
    }

    #[test]
    fn vm_run_is_a_background_tool() {
        let (completed, result) = sync_channel(1);
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner.spawn(
                    exercise_background_tool(spawner, completed)
                        .expect("spawn background VM Tool test"),
                );
            });
        });

        result
            .recv_timeout(Duration::from_secs(10))
            .expect("background VM Tool test timed out")
            .expect("background VM Tool test failed");
    }
}
