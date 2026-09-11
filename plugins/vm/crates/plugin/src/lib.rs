//! Plugin that publishes the Lua VM capability and Workflow Actions.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod workflow;

use alloc::rc::Rc;

use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_workflow_plugin::WorkflowActionRegistry;

pub use barracuda_lua::{
    Context, Error, Function, Lua, MetaMethod, Package, Result, Table, UserData, UserDataHandle,
    UserDataMethods,
};
pub use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
pub use barracuda_vm_runtime::{
    Vm, VmControlAccepted, VmError, VmExecutionError, VmInputRequest, VmLimits, VmListResponse,
    VmRun, VmRunCompletion, VmRunInfo, VmRunOutcome, VmRunProgress, VmRunReference, VmRunRequest,
    VmRunState, VmRunUpdate,
};

/// Registers the VM capability and its Workflow-facing operations.
#[barracuda_plugin::macros::plugin]
pub struct VmPlugin {
    runtime: Option<VmPluginRuntime>,
    package_registry: LuaPackageRegistry,
}

struct VmPluginRuntime {
    vm: Rc<Vm>,
}

impl VmPlugin {
    /// Creates the VM Plugin from the shared construction context.
    #[must_use]
    pub fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            runtime: None,
            package_registry: LuaPackageRegistry::new(),
        }
    }
}

impl Plugin for VmPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let vm =
            Rc::new(Vm::new(self.package_registry.clone()).map_err(PluginError::registration)?);
        for registration in workflow::register_actions(&actions, Rc::clone(&vm))
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        context.provide(Rc::new(self.package_registry.clone()))?;
        context.provide(Rc::clone(&vm))?;
        self.runtime = Some(VmPluginRuntime { vm });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(VmRuntimeUnavailable))?;
        runtime
            .vm
            .start(context.task_spawner()?)
            .map_err(PluginError::registration)?;
        let task = vm_lifecycle_task(runtime.vm, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

#[embassy_executor::task]
async fn vm_lifecycle_task(vm: Rc<Vm>, cancellation: PluginTaskToken) {
    cancellation.cancelled().await;
    vm.stop();
}

#[derive(Debug, thiserror::Error)]
#[error("VM capability was not prepared during Plugin registration")]
struct VmRuntimeUnavailable;
