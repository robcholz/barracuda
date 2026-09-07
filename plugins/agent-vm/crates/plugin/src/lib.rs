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
        DetachedTool, DetachedToolFuture, DetachedToolHandler, Tool, ToolError, ToolFuture,
        ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Vm, VmError, VmInputRequest, VmRunOutcome, VmRunReference, VmRunRequest,
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
        true,
        [
            Tool::from_detached(VmRunTool { vm: Rc::clone(&vm) }),
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
            let completion = Box::pin(async move {
                match run.await {
                    Ok(response) => {
                        let ok = response.outcome == VmRunOutcome::Success;
                        encode(&response, ok)
                    }
                    Err(error) => encode(&ErrorResponse { error }, false),
                }
            });
            Ok(DetachedTool::new(accepted, completion))
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
