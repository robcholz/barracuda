//! Lists, launches, and supervises apps.
//!
//! An app is a Lua program with an optional compiled MicroTS view, stored as a
//! container directory in the format `docs/app-format.md` defines (see
//! [`BUNDLED_APPS`], [`INSTALLED_APPS`], and [`WORKSPACE_APPS`]). Opening an
//! app opens its view through the `ui` Plugin and runs its program on the VM
//! with the `app` Lua package. The system UI drives the lifecycle through
//! [`AppManager`]; `docs/plugin.md` describes the states.

#![no_std]

extern crate alloc;

mod bundle;
mod manager;
mod package;
mod port;
mod storage;
mod workflow;

use alloc::rc::Rc;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_ui_plugin::UiService;
use barracuda_vm_plugin::{LuaPackageRegistry, Vm};
use barracuda_workflow_plugin::WorkflowActionRegistry;
use embassy_futures::select::select;

pub use bundle::{
    AppInfo, AppList, AppLocation, BUNDLED_APPS, DEFAULT_ENTRY, FORMAT_VERSION, INSTALLED_APPS,
    InvalidApp, MANIFEST_FILE, MAX_SOURCE_BYTES, MAX_VIEW_BYTES, WORKSPACE_APPS,
};
pub use manager::{
    AppError, AppExit, AppManager, GRACE, MAX_APP_RUNS, MAX_EXIT_OUTPUT_LINES, MAX_SUSPENDED,
    RunningApp,
};
pub use port::{AppState, MIN_TICK_MS};
pub use storage::{APP_DATA, MAX_FILE_BYTES, MAX_KEY_BYTES, MAX_KV_BYTES};

/// Publishes [`AppManager`], the `app` Lua package, and the `app.*` Workflow
/// Actions.
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
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let (manager, registration) = AppManager::register(filesystem, vm, ui, &packages)
            .map_err(PluginError::registration)?;
        context.retain(registration);
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
    for app in manager.running() {
        manager.terminate(&app.app.id);
    }
}
