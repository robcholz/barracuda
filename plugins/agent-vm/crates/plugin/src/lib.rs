//! Native Agent Tool adapter for the VM capability.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{
        Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{Vm, VmError, VmInputRequest, VmRunReference, VmRunRequest};
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
            .register_group(ToolGroup::new(
                "vm",
                true,
                [
                    Tool::new(VmRunTool { vm: Rc::clone(&vm) }),
                    Tool::new(VmInputTool { vm: Rc::clone(&vm) }),
                    Tool::new(VmCancelTool { vm }),
                ],
            ))
            .map_err(PluginError::registration)
    }
}

struct VmRunTool {
    vm: Rc<Vm>,
}

impl ToolSpec for VmRunTool {
    barracuda_agent_plugin::tools::tool_metadata!("vm_run");
}

impl ToolHandler for VmRunTool {
    type Args = VmRunRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.vm.run(request)) })
    }
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
    let (content, ok) = match result {
        Ok(response) => (serde_json::to_string(&response), true),
        Err(error) => (serde_json::to_string(&ErrorResponse { error }), false),
    };
    let content = content.map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode VM Tool response"))
    })?;
    Ok(ToolOutput { content, ok })
}
