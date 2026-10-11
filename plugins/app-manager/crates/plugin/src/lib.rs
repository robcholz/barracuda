//! Lists, launches, and supervises apps.
//!
//! An app is a Lua program with an optional compiled MicroTS view, stored as a
//! bundle directory (see [`BUNDLED_APPS`] and [`INSTALLED_APPS`]). Launching an
//! app opens its view through the `ui` Plugin and runs its program on the VM;
//! the program claims the view with `require("ui").attach(APP.ui)`. One app is
//! in the foreground at a time.

#![no_std]

extern crate alloc;

mod bundle;
mod manager;
mod workflow;

use alloc::rc::Rc;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_ui_plugin::UiService;
use barracuda_vm_plugin::Vm;
use barracuda_workflow_plugin::WorkflowActionRegistry;
use embassy_futures::select::select;

pub use bundle::{
    AppInfo, AppLocation, BUNDLED_APPS, INSTALLED_APPS, MAX_SOURCE_BYTES, MAX_VIEW_BYTES,
};
pub use manager::{AppError, AppExit, AppManager, ForegroundApp, MAX_EXIT_OUTPUT_LINES};

/// Publishes [`AppManager`] and the `app.*` Workflow Actions.
#[barracuda_plugin::macros::plugin]
pub struct AppManagerPlugin {
    manager: Option<AppManager>,
}

impl AppManagerPlugin {
    /// Creates an unregistered app manager.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { manager: None }
    }
}

/// The Plugin started before it registered.
#[derive(Debug)]
struct AppManagerUnavailable;

impl core::fmt::Display for AppManagerUnavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("app manager is unavailable")
    }
}

impl core::error::Error for AppManagerUnavailable {}

impl Plugin for AppManagerPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let filesystem = context.filesystem()?.clone();
        let vm = context.require::<Vm>("vm")?;
        let ui = context.require::<UiService>("ui")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let manager = AppManager::new(filesystem, vm, ui);
        for registration in
            workflow::register_actions(&actions, &manager).map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        context.provide(Rc::new(manager.clone()))?;
        self.manager = Some(manager);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let manager = self
            .manager
            .take()
            .ok_or_else(|| PluginError::registration(AppManagerUnavailable))?;
        let task = app_task(manager, context.task_token()).map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// Supervises the foreground app until the Plugin stops.
#[embassy_executor::task]
async fn app_task(manager: AppManager, cancellation: PluginTaskToken) {
    select(cancellation.cancelled(), manager.supervise()).await;
    manager.stop();
}
