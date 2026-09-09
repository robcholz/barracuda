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
        DetachedTool, DetachedToolFuture, DetachedToolHandler, EmptyArgs, Tool, ToolError,
        ToolFuture, ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Vm, VmError, VmInputRequest, VmRunOutcome, VmRunReference, VmRunRequest, VmRunUpdate,
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

fn vm_tool_group(vm: Rc<Vm>) -> ToolGroup {
    ToolGroup::new(
        "vm",
        false,
        [
            Tool::from_detached(VmRunTool { vm: Rc::clone(&vm) }),
            Tool::new(VmListTool { vm: Rc::clone(&vm) }),
            Tool::new(VmInputTool { vm: Rc::clone(&vm) }),
            Tool::new(VmCancelTool { vm }),
        ],
    )
}

struct VmRunTool {
    vm: Rc<Vm>,
}

impl ToolSpec for VmRunTool {
    barracuda_agent_plugin::tools::tool_metadata!("vm_run");
}

impl DetachedToolHandler for VmRunTool {
    type Args = VmRunRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> DetachedToolFuture<'a> {
        let run = self.vm.run(request);
        Box::pin(async move {
            let run = run.map_err(|error| ToolError::InvokeRejected(error.to_string()))?;
            let accepted = encode(
                &RunAccepted {
                    run_id: run.run_id(),
                },
                true,
            )?;
            Ok(DetachedTool::with_progress(accepted, |progress| {
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
            }))
        })
    }
}

#[derive(Serialize)]
struct RunAccepted {
    run_id: u32,
}

struct VmInputTool {
    vm: Rc<Vm>,
}

struct VmListTool {
    vm: Rc<Vm>,
}

impl ToolSpec for VmListTool {
    barracuda_agent_plugin::tools::tool_metadata!("vm_list");
}

impl ToolHandler for VmListTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _request: Self::Args) -> ToolFuture<'a> {
        let response = self.vm.list();
        Box::pin(async move { encode(&response, true) })
    }
}

impl ToolSpec for VmInputTool {
    barracuda_agent_plugin::tools::tool_metadata!("vm_input");
}

impl ToolHandler for VmInputTool {
    type Args = VmInputRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.vm.input(request)) })
    }
}

struct VmCancelTool {
    vm: Rc<Vm>,
}

impl ToolSpec for VmCancelTool {
    barracuda_agent_plugin::tools::tool_metadata!("vm_cancel");
}

impl ToolHandler for VmCancelTool {
    type Args = VmRunReference;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.vm.cancel(request)) })
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

    use barracuda_agent_tool::{ToolDetachUpdate, ToolInvocation, ToolRunner, ToolSet};
    use barracuda_vm_plugin::{LuaPackageRegistry, Vm, VmInputRequest, VmRunRequest};
    use embassy_executor::{Executor, Spawner};
    use futures_lite::StreamExt as _;

    use super::vm_tool_group;

    #[embassy_executor::task]
    async fn exercise_detached_tool(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
        let result = async {
            let vm =
                Rc::new(Vm::new(LuaPackageRegistry::new()).map_err(|error| error.to_string())?);
            vm.start(spawner).map_err(|error| error.to_string())?;

            let mut tools = ToolSet::empty();
            tools
                .add_group(vm_tool_group(Rc::clone(&vm)))
                .map_err(|error| error.to_string())?;
            let handle = tools.begin().map_err(|error| error.to_string())?;
            let invocation = ToolInvocation::try_new(
                Some("vm-call"),
                "vm_run",
                &serde_json::to_string(&VmRunRequest {
                    source: concat!("local value = io.read(); ", "print('detached', value)").into(),
                })
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let (mut joined, detached) = ToolRunner::new(&handle).run(alloc::vec![invocation]);
            let mut detached = detached.ok_or("vm_run did not detach")?;

            let accepted = joined
                .next()
                .await
                .ok_or("vm_run did not return an accepted settlement")?
                .1;
            let accepted: serde_json::Value =
                serde_json::from_str(&accepted.content).map_err(|error| error.to_string())?;
            let accepted_run_id = accepted
                .get("run_id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("invalid accepted settlement: {accepted}"))?;

            let progress = detached
                .next()
                .await
                .ok_or("vm_run did not report input_required")?
                .1;
            let ToolDetachUpdate::Progress(progress) = progress else {
                return Err(format!("expected progress update, got: {progress:?}"));
            };
            let progress: serde_json::Value =
                serde_json::from_str(&progress.content).map_err(|error| error.to_string())?;
            if progress.get("kind").and_then(serde_json::Value::as_str) != Some("input_required")
                || progress.get("run_id") != accepted.get("run_id")
            {
                return Err(format!("invalid progress update: {progress}"));
            }

            let list = ToolInvocation::try_new(Some("vm-list-call"), "vm_list", "{}")
                .map_err(|error| error.to_string())?;
            let (mut listed, list_detached) = ToolRunner::new(&handle).run(alloc::vec![list]);
            if list_detached.is_some() {
                return Err("vm_list unexpectedly detached".into());
            }
            let listed = listed
                .next()
                .await
                .ok_or("vm_list did not return a result")?
                .1;
            let listed: serde_json::Value =
                serde_json::from_str(&listed.content).map_err(|error| error.to_string())?;
            if listed
                != serde_json::json!({
                    "runs": [{"run_id": accepted_run_id, "state": "input_required"}]
                })
            {
                return Err(format!("invalid vm_list result: {listed}"));
            }

            vm.input(VmInputRequest {
                run_id: u32::try_from(accepted_run_id)
                    .ok()
                    .ok_or("accepted settlement did not contain a valid run_id")?,
                input: Some("result".into()),
                eof: None,
            })
            .map_err(|error| error.to_string())?;

            let completion = detached
                .next()
                .await
                .ok_or("vm_run did not return a completion settlement")?
                .1;
            let ToolDetachUpdate::Completed(completion) = completion else {
                return Err(format!("expected completion update, got: {completion:?}"));
            };
            let completion: serde_json::Value =
                serde_json::from_str(&completion.content).map_err(|error| error.to_string())?;
            if completion
                .get("outcome")
                .and_then(serde_json::Value::as_str)
                != Some("success")
                || completion.get("output") != Some(&serde_json::json!(["detached\tresult"]))
            {
                return Err(format!("invalid completion settlement: {completion}"));
            }
            Ok(())
        }
        .await;
        let _ignored = completed.send(result);
    }

    #[test]
    fn vm_run_is_a_dynamic_detached_tool() {
        let (completed, result) = sync_channel(1);
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(exercise_detached_tool(spawner, completed))
                    .expect("spawn detached VM Tool test");
            });
        });

        result
            .recv_timeout(Duration::from_secs(10))
            .expect("detached VM Tool test timed out")
            .expect("detached VM Tool test failed");
    }
}
