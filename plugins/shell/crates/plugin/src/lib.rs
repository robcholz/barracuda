//! The device's system UI.
//!
//! Barracuda's device UI has three layers. This Plugin is the first, the
//! system: the home screen, the launcher, the app switcher, the notification
//! and control centers, and the screens around an app's lifecycle (launching,
//! unexpected exit, install, app info). It owns the gestures that move between
//! them and is the only caller that installs, uninstalls, and switches apps
//! for the person. System apps and installable apps are the other two layers;
//! the `app` Plugin runs both, see `.agents/docs/device-ui.md`.
//!
//! The system view is one compiled MicroTS view on the `ui` Plugin's system
//! layer, read from this Plugin's `/resources/view`.

#![no_std]

extern crate alloc;

mod agent;
mod notices;
mod preferences;
mod shell;
mod words;
mod workflow;

use alloc::{format, rc::Rc, string::String, vec::Vec};
use barracuda_app_plugin::AppManager;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_time_plugin::UtcClock;
use barracuda_ui_plugin::{UiAssets, UiService};
use barracuda_vfs::{FsError, ScopedVfs};
use barracuda_wifi_plugin::WifiControl;
use barracuda_workflow_plugin::WorkflowActionRegistry;
use embassy_futures::select::{Either4, select, select4};
use embassy_time::{Duration, Timer};

pub use agent::{AgentState, AgentStatus};
pub use notices::{MAX_NOTICE_CHARS, MAX_NOTICES, Notice, Notifications};
pub use preferences::{MIN_BRIGHTNESS, PreferenceValues, Preferences, SCREEN_TIMEOUTS};
pub use shell::{Screen, Services, Shell};

/// Where the compiled system view lives in this Plugin's filesystem.
pub const VIEW_DIR: &str = "/resources/view";
/// Where the preferences are kept across restarts.
pub const PREFERENCES_FILE: &str = "/data/preferences.json";
/// How often the status bar's clock is redrawn.
const CLOCK_PERIOD: Duration = Duration::from_secs(15);

/// Publishes [`Notifications`], [`AgentStatus`], [`Preferences`], and the
/// `shell.*` Workflow Actions, and runs the system UI.
#[barracuda_plugin::macros::plugin]
pub struct ShellPlugin {
    runtime: Option<(ScopedVfs, Services)>,
}

impl ShellPlugin {
    /// Creates an unregistered system UI.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

/// The Plugin started before it registered.
#[derive(Debug)]
struct ShellUnavailable;

impl core::fmt::Display for ShellUnavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("system UI is unavailable")
    }
}

impl core::error::Error for ShellUnavailable {}

impl Plugin for ShellPlugin {
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
        let ui = context.require::<UiService>("ui")?;
        let apps = context.require::<AppManager>("app")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let clock = context.require::<UtcClock>("time")?;
        let wifi = context.require::<WifiControl>("wifi")?;
        let notices = Rc::new(Notifications::new());
        let agent = Rc::new(AgentStatus::new());
        let preferences = Rc::new(Preferences::default());
        for registration in workflow::register_actions(&actions, &notices, &agent)
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        context.provide(Rc::clone(&notices))?;
        context.provide(Rc::clone(&agent))?;
        context.provide(Rc::clone(&preferences))?;
        self.runtime = Some((
            filesystem,
            Services {
                ui,
                apps: (*apps).clone(),
                notices,
                agent,
                preferences,
                clock,
                wifi,
            },
        ));
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let (filesystem, services) = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(ShellUnavailable))?;
        let task = shell_task(filesystem, services, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// Runs the system UI until the Plugin stops.
#[embassy_executor::task]
async fn shell_task(filesystem: ScopedVfs, services: Services, cancellation: PluginTaskToken) {
    select(cancellation.cancelled(), run(&filesystem, services)).await;
}

async fn run(filesystem: &ScopedVfs, services: Services) {
    let assets = match load_view(filesystem).await {
        Ok(assets) => assets,
        Err(error) => {
            log::error!("shell: cannot read the system view under {VIEW_DIR}: {error:?}");
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
    let mut shell = match Shell::new(services, &assets).await {
        Ok(shell) => shell,
        Err(error) => {
            log::error!("shell: the system view was rejected: {error}");
            return;
        }
    };
    let channel = shell.session().channel().clone();
    loop {
        let tick = shell
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
            Either4::First(gesture) => shell.gesture(gesture).await,
            Either4::Second(Some(call)) => shell.call(call).await,
            Either4::Second(None) => {
                log::error!("shell: the system view closed");
                return;
            }
            Either4::Third(()) => shell.reconcile().await,
            Either4::Fourth(Either4::Third(())) => {
                shell.preferences_changed();
                save_preferences(filesystem, preferences.get()).await;
            }
            Either4::Fourth(_) => shell.refresh(),
        }
    }
}

/// The preferences saved by an earlier run, if they can be read.
async fn load_preferences(filesystem: &ScopedVfs) -> Option<PreferenceValues> {
    let bytes = filesystem.read(PREFERENCES_FILE).await.ok()?;
    serde_json::from_slice(&bytes)
        .inspect_err(|error| log::warn!("shell: ignoring damaged preferences: {error}"))
        .ok()
}

async fn save_preferences(filesystem: &ScopedVfs, values: PreferenceValues) {
    let saved = match serde_json::to_vec(&values) {
        Ok(json) => filesystem.write(PREFERENCES_FILE, &json).await,
        Err(_) => return,
    };
    if let Err(error) = saved {
        log::warn!("shell: cannot save the preferences: {error:?}");
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
