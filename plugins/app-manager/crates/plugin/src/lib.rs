//! Runs Lua apps and, on a Board with a display, shows them and the system UI.
//!
//! This Plugin assembles two kits. The app kit (`barracuda-appkit`) installs,
//! launches, and supervises apps and needs no display; the UI kit
//! (`barracuda-uikit`) draws views on a display and turns touch into
//! gestures and knows nothing about apps. Here they meet: on a Board with a
//! display, apps' views open through the UI kit, and the system UI runs on
//! it: the home screen, the launcher, the app switcher, the notification and
//! control centers, and the screens around an app's lifecycle. Without a
//! display, only headless apps run.
//!
//! Builtin apps are system apps that other Plugins ship: they add themselves
//! with [`AppManager::add_builtin`]. The device UI's three layers, the system,
//! system apps, and installable apps, are in `.agents/docs/device-ui.md`.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

mod agent;
mod fonts;
mod notices;
mod notify;
mod preferences;
mod system;
mod words;
mod workflow;

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};
use barracuda_audio_plugin::AudioOutput;
use barracuda_board_hal::display::{Display, DisplayPeripheral};
use barracuda_board_hal::touch::{Touch, TouchPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_power_monitor_plugin::PowerStatus;
use barracuda_time_plugin::UtcClock;
use barracuda_uikit::{Ui, UiAssets};
use barracuda_vfs::{FsError, ScopedVfs};
use barracuda_vm_plugin::{LuaPackageRegistry, Vm};
use barracuda_wifi_plugin::WifiControl;
use barracuda_workflow_plugin::WorkflowActionRegistry;
use core::{future::Future, pin::Pin};
use embassy_futures::join::{join, join3};
use embassy_futures::select::{Either4, select, select4};
use embassy_time::{Duration, Timer};

pub use agent::{AgentState, AgentStatus};
pub use barracuda_appkit::{
    API_LEVEL, APP_DATA, APP_PACKAGE, AppError, AppExit, AppInfo, AppKind, AppList, AppLocation,
    AppManager, AppState, BUILTIN_APP, BuiltinApp, BuiltinBackend, BuiltinError, BuiltinPackage,
    Device, InvalidApp, RunningApp, ViewShape, VmRunOutcome,
};
pub use barracuda_uikit::icons;
pub use fonts::SystemFonts;
pub use notices::{
    DEFAULT_LABELS, MAX_LABEL_CHARS, MAX_NOTICE_CHARS, MAX_NOTICES, Notice, Notifications,
};
pub use notify::{NOTIFY, NOTIFY_API};
pub use preferences::{MIN_BRIGHTNESS, PreferenceValues, Preferences, SCREEN_TIMEOUTS};
pub use system::{Screen, Services, SystemUi, WIFI_SETTINGS};

/// Where the compiled system view lives in this Plugin's filesystem.
pub const VIEW_DIR: &str = "/resources/view";
/// Where the preferences are kept across restarts.
pub const PREFERENCES_FILE: &str = "/data/preferences.json";
/// How often the status bar's clock is redrawn.
const CLOCK_PERIOD: Duration = Duration::from_secs(15);

/// Publishes [`AppManager`], [`Notifications`], [`AgentStatus`], and
/// [`Preferences`], the `app` Lua package, and the `app.*` Workflow Actions;
/// runs apps, and the system UI when the Board has a display.
#[barracuda_plugin::macros::plugin]
pub struct AppManagerPlugin<Panel, Input> {
    /// The Board's stable name, which apps' `boards` list.
    board: &'static str,
    display: Option<Panel>,
    touch: Option<Input>,
    runtime: Option<Runtime<Panel, Input>>,
}

struct Runtime<Panel, Input> {
    filesystem: ScopedVfs,
    apps: AppManager,
    fonts: Rc<SystemFonts>,
    /// The display and the system UI's services, on a Board with a display.
    screen: Option<(Ui<Panel, Input>, Services)>,
}

impl<Panel, Input> AppManagerPlugin<Panel, Input> {
    /// Takes the display and touch input the Board gives this Plugin's slots.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: DisplayPeripheral<Display = Panel> + TouchPeripheral<Touch = Input>,
    {
        let board = context.target_identity.board().name();
        let peripherals = &mut context.hal.peripherals;
        Self {
            board,
            display: config::peripherals::DISPLAY_MAIN
                .and_then(|instance| peripherals.take_display(instance)),
            touch: config::peripherals::TOUCH_MAIN
                .and_then(|instance| peripherals.take_touch(instance)),
            runtime: None,
        }
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

impl<Panel, Input> Plugin for AppManagerPlugin<Panel, Input>
where
    Panel: Display + 'static,
    Input: Touch + 'static,
{
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
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let clock = context.require::<UtcClock>("time")?;
        let wifi = context.require::<WifiControl>("wifi")?;
        let power = context.require::<PowerStatus>("power-monitor")?;
        let audio = context.require::<AudioOutput>("audio")?;
        let ui = self
            .display
            .take()
            .map(|display| Ui::new(display, self.touch.take()));
        let device = Device {
            board: self.board,
            display: ui.as_ref().map(Ui::display),
            touch: ui.as_ref().is_some_and(Ui::touch),
        };
        let (apps, registration) = AppManager::register(
            filesystem.clone(),
            vm,
            ui.as_ref().map(Ui::service),
            device,
            &packages,
        )
        .map_err(PluginError::registration)?;
        context.retain(registration);
        for registration in barracuda_appkit::register_actions(&actions, &apps)
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        let notices = Rc::new(Notifications::new());
        let agent = Rc::new(AgentStatus::new());
        let preferences = Rc::new(Preferences::default());
        let registration = apps
            .add_package(
                NOTIFY,
                NOTIFY_API,
                Rc::new(notify::Notify(Rc::clone(&notices))),
            )
            .map_err(PluginError::registration)?;
        context.retain(registration);
        for registration in workflow::register_actions(&actions, &notices, &agent)
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        context.provide(Rc::new(apps.clone()))?;
        context.provide(Rc::clone(&notices))?;
        context.provide(Rc::clone(&agent))?;
        context.provide(Rc::clone(&preferences))?;
        let fonts = Rc::new(SystemFonts::default());
        context.provide(Rc::clone(&fonts))?;
        let screen = ui.map(|ui| {
            let services = Services {
                ui: ui.service(),
                apps: apps.clone(),
                notices,
                agent,
                preferences,
                clock,
                wifi,
                power,
                audio,
            };
            (ui, services)
        });
        self.runtime = Some(Runtime {
            filesystem,
            apps,
            fonts,
            screen,
        });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(AppManagerUnavailable))?;
        let task = app_manager_task(Box::pin(run(runtime, context.task_token())))
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// The Plugin's work, boxed once so one task serves every Board's display
/// type.
type Work = Pin<Box<dyn Future<Output = ()> + 'static>>;

#[embassy_executor::task]
async fn app_manager_task(work: Work) {
    work.await;
}

/// Supervises apps and answers builtin apps' calls and, with a display, draws
/// and runs the system UI, until cancelled; then ends every app.
async fn run<Panel: Display, Input: Touch>(
    runtime: Runtime<Panel, Input>,
    cancellation: PluginTaskToken,
) {
    let Runtime {
        filesystem,
        apps,
        fonts,
        screen,
    } = runtime;
    let shown = screen.as_ref().map(|(ui, _)| ui.service());
    let display = async {
        match screen {
            Some((ui, services)) => {
                let fonts = fonts.source();
                join(ui.run(fonts.as_ref()), run_system_ui(&filesystem, services)).await;
            }
            None => core::future::pending().await,
        }
    };
    select(
        cancellation.cancelled(),
        join3(apps.supervise(), apps.serve_builtins(), display),
    )
    .await;
    if let Some(service) = shown {
        service.close_all();
    }
    for app in apps.running() {
        apps.terminate(&app.app.id);
    }
}

/// Runs the system UI on the display, forever.
async fn run_system_ui(filesystem: &ScopedVfs, services: Services) {
    let assets = match load_view(filesystem).await {
        Ok(assets) => assets,
        Err(error) => {
            log::error!("system ui: cannot read the system view under {VIEW_DIR}: {error:?}");
            return;
        }
    };
    let preferences = Rc::clone(&services.preferences);
    if let Some(saved) = load_preferences(filesystem).await {
        preferences.update(|values| *values = saved);
    }
    let ui = Rc::clone(&services.ui);
    let apps = services.apps.clone();
    let notices = Rc::clone(&services.notices);
    let agent = Rc::clone(&services.agent);
    let mut system = match SystemUi::new(services, &assets).await {
        Ok(system) => system,
        Err(error) => {
            log::error!("system ui: the system view was rejected: {error}");
            return;
        }
    };
    let channel = system.session().channel().clone();
    loop {
        let tick = system
            .next_expiry()
            .map_or(CLOCK_PERIOD, |expiry| expiry.min(CLOCK_PERIOD));
        let event = select4(
            ui.next_gesture(),
            channel.next(),
            apps.changed(),
            select4(
                notices.changed(),
                agent.changed(),
                preferences.changed(),
                Timer::after(tick),
            ),
        )
        .await;
        match event {
            Either4::First(gesture) => system.gesture(gesture).await,
            Either4::Second(Some(call)) => system.call(call).await,
            Either4::Second(None) => {
                log::error!("system ui: the system view closed");
                return;
            }
            Either4::Third(()) => system.reconcile().await,
            Either4::Fourth(Either4::Third(())) => {
                system.preferences_changed();
                save_preferences(filesystem, preferences.get()).await;
            }
            Either4::Fourth(_) => system.refresh(),
        }
    }
}

/// The preferences saved by an earlier run, if they can be read.
async fn load_preferences(filesystem: &ScopedVfs) -> Option<PreferenceValues> {
    let bytes = filesystem.read(PREFERENCES_FILE).await.ok()?;
    serde_json::from_slice(&bytes)
        .inspect_err(|error| log::warn!("system ui: ignoring damaged preferences: {error}"))
        .ok()
}

async fn save_preferences(filesystem: &ScopedVfs, values: PreferenceValues) {
    let saved = match serde_json::to_vec(&values) {
        Ok(json) => filesystem.write(PREFERENCES_FILE, &json).await,
        Err(_) => return,
    };
    if let Err(error) = saved {
        log::warn!("system ui: cannot save the preferences: {error:?}");
    }
}

/// Reads the compiled system view: its program, stylesheet, and every font
/// atlas beside them.
async fn load_view(filesystem: &ScopedVfs) -> Result<UiAssets, FsError> {
    let mut fonts: Vec<String> = filesystem
        .list_dir(VIEW_DIR)
        .await?
        .into_iter()
        .filter(|name| name.starts_with("font-") && name.ends_with(".bin"))
        .collect();
    fonts.sort_unstable();
    let mut atlases = Vec::with_capacity(fonts.len());
    for font in fonts {
        atlases.push(filesystem.read(&format!("{VIEW_DIR}/{font}")).await?);
    }
    Ok(UiAssets {
        view: filesystem.read(&format!("{VIEW_DIR}/view.bvir")).await?,
        styles: filesystem.read(&format!("{VIEW_DIR}/styles.bin")).await?,
        fonts: atlases,
    })
}
