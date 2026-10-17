//! The `AppManager` capability: the app lifecycle and the tasks that
//! supervise app runs.
//!
//! The system UI drives the lifecycle from gestures: opening an app,
//! returning home, covering the app with an overlay, and closing an app in
//! the switcher. This module turns those into app states, lifecycle events
//! for the program, and VM and memory budgets.

use crate::bapp::{self, Seal};
use crate::builtin::{Backends, BuiltinApp, BuiltinBackend};
use crate::bundle::{self, AppInfo, AppKind, AppList, Builtins, INSTALLED_APPS, Shelf};
use crate::device::{self, Device, Packages};
use crate::package::AppPackage;
use crate::port::{AppState, Lifecycle, Port, Ports};
use crate::storage::{APP_DATA, Storage};
use alloc::boxed::Box;
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use barracuda_uikit::{UiService, UiSession};
use barracuda_vfs::{FsError, ScopedVfs};
use barracuda_vm_plugin::{
    AppApi, LuaPackageRegistration, LuaPackageRegistry, LuaPackageRegistryError, LuaPackageScope,
    Vm, VmError, VmExecutionError, VmInputRequest, VmRun, VmRunCompletion, VmRunOutcome,
    VmRunProgress, VmRunRequest, VmRunUpdate,
};
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use embassy_futures::select::{Either3, select_array, select3};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic_util::Arc;
use serde::Serialize;

/// The key of the signer an app was installed from, beside its container.
const SIGNER_FILE: &str = ".signer";

/// Apps that may run at once. The VM has four execution slots; one stays
/// free for Workflow and Agent runs.
pub const MAX_APP_RUNS: usize = 3;
/// Suspended apps kept before the least recently used one is terminated.
pub const MAX_SUSPENDED: usize = 2;
/// How long an app may take to park after leaving the foreground, or to exit
/// after `app:terminate`.
pub const GRACE: Duration = Duration::from_millis(1000);
/// Output lines kept from an app that exited.
pub const MAX_EXIT_OUTPUT_LINES: usize = 16;

/// Why an app operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppError {
    /// No system or installed app has that ID.
    NotFound,
    /// The app is offered in the Workspace but not installed.
    NotInstalled,
    /// System apps cannot be installed over or uninstalled.
    SystemApp,
    /// The container breaks the app format.
    InvalidBundle,
    /// The filesystem failed while reading the container.
    Storage,
    /// The view was rejected by the UI runtime.
    ViewRejected,
    /// The app has a view and the device has no display.
    NoDisplay,
    /// The app cannot run on this device; its `incompatible` says why.
    Incompatible,
    /// The offered app changed after the person was asked to install it.
    Changed,
    /// No app is in the foreground.
    NoForegroundApp,
    /// The foreground app is not taking input.
    NotInteractive,
    /// The VM is not running yet.
    RuntimeUnavailable,
    /// Every app run is in use, or the app is still terminating.
    Busy,
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            AppError::NotFound => "app not found",
            AppError::NotInstalled => "the app is offered but not installed",
            AppError::SystemApp => "system apps cannot be installed over or uninstalled",
            AppError::InvalidBundle => "invalid app container",
            AppError::Storage => "app storage failed",
            AppError::ViewRejected => "app view rejected",
            AppError::NoDisplay => "the device has no display for the app's view",
            AppError::Incompatible => "the app cannot run on this device",
            AppError::Changed => "the offered app changed after it was approved",
            AppError::NoForegroundApp => "no app is in the foreground",
            AppError::NotInteractive => "the foreground app is not taking input",
            AppError::RuntimeUnavailable => "VM runtime is unavailable",
            AppError::Busy => "no app run is free",
        })
    }
}

impl core::error::Error for AppError {}

/// Why a builtin app was not added.
#[derive(Debug)]
pub enum BuiltinError {
    /// The ID is not an app ID.
    InvalidId,
    /// Another builtin app has the ID.
    Taken,
    /// The app's Lua package could not be registered.
    Package(LuaPackageRegistryError),
}

impl core::fmt::Display for BuiltinError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BuiltinError::InvalidId => f.write_str("the builtin app's ID is not an app ID"),
            BuiltinError::Taken => f.write_str("another builtin app has the ID"),
            BuiltinError::Package(error) => {
                write!(f, "the builtin app's package was not registered: {error}")
            }
        }
    }
}

impl core::error::Error for BuiltinError {}

impl From<VmError> for AppError {
    fn from(error: VmError) -> Self {
        match error {
            VmError::Busy => AppError::Busy,
            _ => AppError::RuntimeUnavailable,
        }
    }
}

/// A running app.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct RunningApp {
    /// The app.
    pub app: AppInfo,
    /// The VM execution running its program.
    pub run_id: u32,
    /// Where it is in its lifecycle.
    pub state: AppState,
}

/// How the most recent app run ended.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct AppExit {
    /// The app ID.
    pub id: String,
    /// The VM execution that ran it.
    pub run_id: u32,
    /// The terminal VM outcome.
    pub outcome: VmRunOutcome,
    /// The stable VM failure when `outcome` is `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<VmExecutionError>,
    /// The VM diagnostic, or why the app manager cancelled the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// The last output lines, at most [`MAX_EXIT_OUTPUT_LINES`].
    pub output: Vec<String>,
    /// Whether the app was in the foreground when it ended.
    pub foreground: bool,
}

struct Running {
    info: AppInfo,
    run_id: u32,
    ticket: String,
    port: Arc<Port>,
    session: Option<UiSession>,
    state: AppState,
    last_used: u64,
}

impl Running {
    fn public(&self) -> RunningApp {
        RunningApp {
            app: self.info.clone(),
            run_id: self.run_id,
            state: self.state,
        }
    }
}

#[derive(Default)]
struct Slot {
    /// Bumped whenever the slot's run is replaced or cancelled.
    generation: u32,
    app: Option<Running>,
    pending: Option<VmRun>,
    deadline: Option<Instant>,
}

#[derive(Default)]
struct State {
    slots: [Slot; MAX_APP_RUNS],
    foreground: Option<usize>,
    last_exit: Option<AppExit>,
    /// The app an install was requested for, awaiting the person's answer.
    install_request: Option<AppInfo>,
}

impl State {
    fn find(&self, id: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.app.as_ref().is_some_and(|app| app.info.id == id))
    }

    fn running(&mut self, index: usize) -> Option<&mut Running> {
        self.slots.get_mut(index)?.app.as_mut()
    }

    /// Puts the foreground app's view on the display, taking touches while
    /// the app is active, or blanks the display when no app is in front.
    fn present(&self) {
        let foreground = self
            .foreground
            .and_then(|index| self.slots[index].app.as_ref())
            .and_then(|running| running.session.as_ref().map(|session| (running, session)));
        match foreground {
            Some((running, session)) => session.show(running.state == AppState::Active),
            None => self
                .slots
                .iter()
                .filter_map(|slot| slot.app.as_ref()?.session.as_ref())
                .for_each(UiSession::hide),
        }
    }
}

/// What the deadline of a slot's run decided.
enum Deadline {
    Keep,
    Cancel,
}

struct Inner {
    filesystem: ScopedVfs,
    vm: Rc<Vm>,
    /// Shows apps' views; `None` on a device without a display.
    ui: Option<Rc<UiService>>,
    device: Device,
    packages: LuaPackageRegistry,
    builtins: Builtins,
    backends: Backends,
    ports: Arc<Ports>,
    state: RefCell<State>,
    wakes: [Signal<NoopRawMutex, ()>; MAX_APP_RUNS],
    changed: Signal<NoopRawMutex, ()>,
    clock: Cell<u64>,
    tickets: Cell<u64>,
}

/// Runs apps through their lifecycle; published by the `app-manager` Plugin.
///
/// At most one app with a view is in the foreground. Leaving the foreground
/// suspends an app rather than ending it, until the suspended or run budgets
/// need its place.
#[derive(Clone)]
pub struct AppManager {
    inner: Rc<Inner>,
}

impl AppManager {
    /// Creates a manager over a Plugin filesystem and registers the `app` Lua
    /// package its programs use. Keep the registration for the manager's life.
    /// Apps' views show through `ui`; without one, only headless apps open.
    /// Installable apps are checked against `device`.
    ///
    /// # Errors
    ///
    /// Returns the registry's error when the package cannot be registered.
    pub fn register(
        filesystem: ScopedVfs,
        vm: Rc<Vm>,
        ui: Option<Rc<UiService>>,
        device: Device,
        packages: &LuaPackageRegistry,
    ) -> Result<(Self, LuaPackageRegistration), LuaPackageRegistryError> {
        let ports = Arc::new(Ports::new());
        let registration = packages.register(AppPackage::new(Arc::clone(&ports)))?;
        let manager = Self {
            inner: Rc::new(Inner {
                filesystem,
                vm,
                ui,
                device,
                packages: packages.clone(),
                builtins: Builtins::default(),
                backends: Backends::new(),
                ports,
                state: RefCell::new(State::default()),
                wakes: core::array::from_fn(|_| Signal::new()),
                changed: Signal::new(),
                clock: Cell::new(0),
                tickets: Cell::new(0),
            }),
        };
        Ok((manager, registration))
    }

    fn shelf(&self) -> Shelf<'_> {
        Shelf {
            filesystem: &self.inner.filesystem,
            builtins: &self.inner.builtins,
            device: &self.inner.device,
            packages: self.packages(),
        }
    }

    fn packages(&self) -> Packages {
        self.inner.packages.packages().into_iter().collect()
    }

    /// What installable apps are checked against.
    #[must_use]
    pub fn device(&self) -> &Device {
        &self.inner.device
    }

    /// What package `name` lets an app do, in the words its Plugin declared
    /// for the install screen; `None` for a package outside the app API or
    /// one this device lacks.
    #[must_use]
    pub fn purpose(&self, name: &str) -> Option<&'static str> {
        device::purpose(&self.packages(), name)
    }

    /// What app `info` asks to be allowed, as the install screen and the
    /// portal show it: one line per package it names, in the words the
    /// package's Plugin declared, or for a capability this device lacks in
    /// the app API's name for it, with packages it only uses marked
    /// optional. Each line comes with its package.
    #[must_use]
    pub fn asks(&self, info: &AppInfo) -> Vec<(String, String)> {
        info.requires
            .iter()
            .chain(&info.uses)
            .chain(&info.permissions)
            .map(|package| {
                let purpose = self.purpose(package).map_or_else(
                    || {
                        device::CAPABILITIES
                            .iter()
                            .find(|(name, _, _)| name == package)
                            .map_or_else(
                                || format!("使用 {package}"),
                                |(_, _, noun)| {
                                    let gap = if noun.is_ascii() { " " } else { "" };
                                    format!("使用{gap}{noun}")
                                },
                            )
                    },
                    String::from,
                );
                let line = if info.uses.contains(package) {
                    format!("{purpose}（可选）")
                } else {
                    purpose
                };
                (package.clone(), line)
            })
            .collect()
    }

    /// Registers a Lua package of the app API answered by `backend` on the
    /// app manager's task, the way builtin apps' packages are. Keep the
    /// registration for as long as the package is offered.
    ///
    /// # Errors
    ///
    /// Returns the registry's error when the package cannot be registered.
    pub fn add_package(
        &self,
        name: &'static str,
        api: AppApi,
        backend: Rc<dyn BuiltinBackend>,
    ) -> Result<LuaPackageRegistration, LuaPackageRegistryError> {
        self.inner
            .packages
            .register(self.inner.backends.add(name, Some(api), backend))
    }

    /// Adds a builtin app, a system app another Plugin ships, and registers
    /// its Lua package, which installable apps can then never ask for. Keep
    /// the returned registration for as long as the app is offered.
    ///
    /// # Errors
    ///
    /// Fails when the ID is not an app ID or is taken, or the package cannot
    /// be registered.
    pub fn add_builtin(
        &self,
        app: BuiltinApp,
    ) -> Result<Option<LuaPackageRegistration>, BuiltinError> {
        if !bundle::valid_id(app.id) {
            return Err(BuiltinError::InvalidId);
        }
        let registration = match app.package {
            Some(package) => {
                let registration = self
                    .inner
                    .packages
                    .register(self.inner.backends.add(package.name, None, package.backend))
                    .map_err(BuiltinError::Package)?;
                self.inner.builtins.reserve(package.name);
                Some(registration)
            }
            None => None,
        };
        if !self.inner.builtins.add(app.id, app.filesystem, app.opens) {
            return Err(BuiltinError::Taken);
        }
        self.inner.changed.signal(());
        Ok(registration)
    }

    /// Bytes that installed apps and every app's data take up.
    pub async fn used_bytes(&self) -> u64 {
        let filesystem = &self.inner.filesystem;
        let mut used = 0;
        let mut pending = alloc::vec![String::from(INSTALLED_APPS), String::from(APP_DATA)];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = filesystem.read_dir(&dir).await else {
                continue;
            };
            for entry in entries.flatten() {
                let metadata = entry.metadata();
                if metadata.is_dir() {
                    pending.push(format!("{dir}/{}", entry.file_name()));
                } else {
                    used += metadata.len();
                }
            }
        }
        used
    }

    /// The builtin app that opens for `what`, such as `wifi`, if one does.
    #[must_use]
    pub fn opening(&self, what: &str) -> Option<String> {
        self.inner.builtins.opening(what)
    }

    /// Answers builtin apps' Lua package calls until the manager is dropped.
    pub async fn serve_builtins(&self) {
        self.inner.backends.serve().await;
    }

    fn tick_clock(&self) -> u64 {
        let now = self.inner.clock.get().wrapping_add(1);
        self.inner.clock.set(now);
        now
    }

    fn wake(&self, index: usize) {
        if let Some(wake) = self.inner.wakes.get(index) {
            wake.signal(());
        }
        self.inner.changed.signal(());
    }

    /// Waits until something the system UI shows may have changed: an app
    /// opened, moved, or ended, an app was installed or uninstalled, or an
    /// install was requested. One waiter, the system UI, is served.
    pub async fn changed(&self) {
        self.inner.changed.wait().await;
    }

    /// Every launchable app, sorted by ID, every Workspace container that can
    /// be installed, and every container that fails the app format with the
    /// reason.
    pub async fn list(&self) -> AppList {
        bundle::list(&self.shelf()).await
    }

    /// Asks the system UI to offer the Workspace container `id` for
    /// installation; the person installs or declines it there. A newer
    /// request replaces one still unanswered.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the Workspace offers no valid container
    /// `id`, the ID belongs to a system app, or the app cannot run on this
    /// device.
    pub async fn request_install(&self, id: &str) -> Result<AppInfo, AppError> {
        let offered = self.install_offer(id).await?;
        if offered.incompatible.is_some() {
            return Err(AppError::Incompatible);
        }
        self.inner.state.borrow_mut().install_request = Some(offered.clone());
        self.inner.changed.signal(());
        Ok(offered)
    }

    /// The Workspace container offered as app `id` as an install would take
    /// it: with the digest of its files, the signer of a signed package, and
    /// why it cannot be installed here, if it cannot. Besides not fitting
    /// the device, an app installed from a signed package takes updates
    /// only from its signer.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the Workspace offers no valid container
    /// `id`, a package's signature does not verify, or the ID belongs to a
    /// system app.
    pub async fn install_offer(&self, id: &str) -> Result<AppInfo, AppError> {
        let (offered, seal) = self.sealed(id).await?;
        let mut info = offered.info;
        if info.incompatible.is_none() && !self.trusts(id, &seal).await {
            info.incompatible = Some(String::from("签名与已安装的版本不同"));
        }
        info.digest = Some(bapp::hex(&seal.digest));
        info.signer = seal.signer.as_ref().map(bapp::fingerprint);
        Ok(info)
    }

    /// The offered container `id` and what its files hash to.
    async fn sealed(&self, id: &str) -> Result<(bundle::Container, Seal), AppError> {
        let offered = bundle::offered(&self.shelf(), id).await?;
        if self.system_app(id).await {
            return Err(AppError::SystemApp);
        }
        let seal = offered.seal().await?;
        Ok((offered, seal))
    }

    /// Whether `seal` may replace installed app `id`: one installed from a
    /// signed package takes only packages its signer signed.
    async fn trusts(&self, id: &str, seal: &Seal) -> bool {
        match self
            .inner
            .filesystem
            .read(&format!("{INSTALLED_APPS}/{id}/{SIGNER_FILE}"))
            .await
        {
            Ok(signer) => seal.signer.is_some_and(|key| key[..] == signer[..]),
            Err(_) => true,
        }
    }

    /// The install the system UI should ask the person about.
    #[must_use]
    pub fn install_request(&self) -> Option<AppInfo> {
        self.inner.state.borrow().install_request.clone()
    }

    /// Drops the install request for app `id`, if it is the pending one.
    pub fn decline_install(&self, id: &str) {
        let mut state = self.inner.state.borrow_mut();
        if state
            .install_request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            state.install_request = None;
            drop(state);
            self.inner.changed.signal(());
        }
    }

    /// The Workspace container offered as app `id`, with why it cannot run on
    /// this device, if it cannot.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the Workspace offers no valid container
    /// `id` or the ID belongs to a system app.
    pub async fn offered(&self, id: &str) -> Result<AppInfo, AppError> {
        let offered = bundle::offered(&self.shelf(), id).await?;
        if self.system_app(id).await {
            return Err(AppError::SystemApp);
        }
        Ok(offered.info)
    }

    async fn system_app(&self, id: &str) -> bool {
        bundle::info(&self.shelf(), id)
            .await
            .is_ok_and(|app| app.kind == AppKind::System)
    }

    /// Installs the Workspace container `id` as an installable app, with the
    /// permissions its manifest asks for. The system UI calls this once the
    /// person agreed; an installed app at another version is replaced, and a
    /// running copy is terminated first. The app's data is kept.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the Workspace offers no valid container
    /// `id`, the ID belongs to a system app, the app cannot run on this
    /// device, or the copy fails.
    pub async fn install(&self, id: &str) -> Result<AppInfo, AppError> {
        let (offered, seal) = self.sealed(id).await?;
        if offered.info.incompatible.is_some() || !self.trusts(id, &seal).await {
            return Err(AppError::Incompatible);
        }
        // What the person was asked about is what is installed.
        let pinned = self
            .inner
            .state
            .borrow()
            .install_request
            .as_ref()
            .filter(|request| request.id == id)
            .and_then(|request| request.digest.clone());
        if pinned.is_some_and(|digest| digest != bapp::hex(&seal.digest)) {
            return Err(AppError::Changed);
        }
        self.terminate(id);
        let filesystem = &self.inner.filesystem;
        let target = format!("{INSTALLED_APPS}/{id}");
        let staging = format!("{INSTALLED_APPS}/.{id}");
        let copied = async {
            remove_tree(filesystem, &staging).await?;
            filesystem.create_dir_all(&staging).await?;
            for file in offered.files() {
                let bytes = offered
                    .source
                    .read(file, bundle::MAX_VIEW_BYTES)
                    .await
                    .map_err(|_| FsError::Io)?;
                filesystem
                    .write(&format!("{staging}/{file}"), &bytes)
                    .await?;
            }
            if let Some(signer) = &seal.signer {
                filesystem
                    .write(&format!("{staging}/{SIGNER_FILE}"), signer)
                    .await?;
            }
            remove_tree(filesystem, &target).await?;
            filesystem.rename(&staging, &target).await
        }
        .await;
        if let Err(error) = copied {
            log::warn!("app: cannot install {id}: {error:?}");
            let _cleaned = remove_tree(filesystem, &staging).await;
            return Err(AppError::Storage);
        }
        log::info!("app: installed {id} {}", offered.info.version);
        {
            let mut state = self.inner.state.borrow_mut();
            if state
                .install_request
                .as_ref()
                .is_some_and(|request| request.id == id)
            {
                state.install_request = None;
            }
        }
        self.inner.changed.signal(());
        bundle::info(&self.shelf(), id).await
    }

    /// Uninstalls installable app `id`: terminates it and deletes its
    /// container and its data.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::SystemApp`] for a system app, [`AppError::NotFound`]
    /// when no app `id` is installed, and [`AppError::Storage`] when the
    /// filesystem fails.
    pub async fn uninstall(&self, id: &str) -> Result<(), AppError> {
        let app = bundle::info(&self.shelf(), id).await?;
        if app.kind == AppKind::System {
            return Err(AppError::SystemApp);
        }
        self.terminate(id);
        let filesystem = &self.inner.filesystem;
        for tree in [format!("{INSTALLED_APPS}/{id}"), format!("{APP_DATA}/{id}")] {
            remove_tree(filesystem, &tree).await.map_err(|error| {
                log::warn!("app: cannot remove {tree}: {error:?}");
                AppError::Storage
            })?;
        }
        log::info!("app: uninstalled {id}");
        self.inner.changed.signal(());
        Ok(())
    }

    /// Opens app `id`: resumes it if it is running, otherwise launches it.
    ///
    /// An app with a view becomes the active foreground app and the previous
    /// one moves to the background. A headless app starts as a service.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the container cannot be loaded, the app no
    /// longer fits this device, its view is rejected, or no app run is free.
    pub async fn open(&self, id: &str) -> Result<RunningApp, AppError> {
        let running = self.inner.state.borrow().find(id);
        if let Some(index) = running {
            return self.reopen(index).await;
        }
        let bundle = match bundle::load(&self.shelf(), id).await {
            Err(AppError::NotFound) if bundle::offered(&self.shelf(), id).await.is_ok() => {
                return Err(AppError::NotInstalled);
            }
            loaded => loaded?,
        };
        // Another open may have launched it while the container loaded.
        let running = self.inner.state.borrow().find(id);
        if let Some(index) = running {
            return self.reopen(index).await;
        }
        let ui = match (&bundle.view, &self.inner.ui) {
            (Some(_), None) => return Err(AppError::NoDisplay),
            (_, ui) => ui.clone(),
        };
        if bundle.info.incompatible.is_some() {
            return Err(AppError::Incompatible);
        }
        let packages = self.packages();
        let has_view = bundle.view.is_some();
        if has_view {
            self.home();
        }
        let index = self.free_slot()?;
        let session = match (&bundle.view, ui) {
            (Some(view), Some(ui)) => {
                // Views draw with the system fonts, which load at start.
                ui.ready().await;
                Some(ui.open(view).map_err(|error| {
                    log::warn!("app: {id}: {error}");
                    AppError::ViewRejected
                })?)
            }
            _ => None,
        };
        let state = if has_view {
            AppState::Active
        } else {
            AppState::Running
        };
        let port = Arc::new(Port::new(
            bundle.info.id.clone(),
            bundle.info.name.clone(),
            bundle.info.version.clone(),
            session.as_ref().map(|session| session.channel().clone()),
            Storage::new(self.inner.filesystem.clone(), &bundle.info.id),
            device::describe(&self.inner.device, &packages).to_string(),
            state,
        ));
        let ticket = self.ticket(index);
        self.inner.ports.insert(ticket.clone(), Arc::clone(&port));
        // The binding shares the first line so Lua line numbers stay correct.
        let source = format!("require(\"app\").__bind({ticket:?}); {}", bundle.source);
        // A system app may require every package; an installable app only
        // the core packages and what its manifest asked for.
        let scope = match bundle.info.kind {
            AppKind::System => LuaPackageScope::All,
            AppKind::Installable => LuaPackageScope::Only(device::grant(&bundle.info, &packages)),
        };
        let run = match self.inner.vm.run_scoped(VmRunRequest { source }, scope) {
            Ok(run) => run,
            Err(error) => {
                self.inner.ports.remove(&ticket);
                return Err(error.into());
            }
        };
        if has_view {
            port.transition(state, &[Lifecycle::Active]);
        }
        let running = Running {
            info: bundle.info,
            run_id: run.run_id(),
            ticket,
            port,
            session,
            state,
            last_used: self.tick_clock(),
        };
        let public = running.public();
        let mut state = self.inner.state.borrow_mut();
        let slot = &mut state.slots[index];
        slot.generation = slot.generation.wrapping_add(1);
        slot.app = Some(running);
        slot.pending = Some(run);
        slot.deadline = None;
        if has_view {
            state.foreground = Some(index);
        }
        state.present();
        drop(state);
        self.wake(index);
        Ok(public)
    }

    fn ticket(&self, index: usize) -> String {
        let ticket = self.inner.tickets.get().wrapping_add(1);
        self.inner.tickets.set(ticket);
        format!("app-{index}-{ticket}")
    }

    /// A free run slot, terminating the least recently used suspended app
    /// when every slot is taken.
    fn free_slot(&self) -> Result<usize, AppError> {
        let mut state = self.inner.state.borrow_mut();
        if let Some(index) = state.slots.iter().position(|slot| slot.app.is_none()) {
            return Ok(index);
        }
        let index = lru_suspended(&state).ok_or(AppError::Busy)?;
        self.remove(&mut state, index, None);
        Ok(index)
    }

    async fn reopen(&self, index: usize) -> Result<RunningApp, AppError> {
        let (id, app_state) = {
            let mut state = self.inner.state.borrow_mut();
            let running = state.running(index).ok_or(AppError::NotFound)?;
            (running.info.id.clone(), running.state)
        };
        match app_state {
            AppState::Terminating => return Err(AppError::Busy),
            AppState::Running | AppState::Active | AppState::Inactive => {}
            AppState::Background | AppState::Suspended => {
                if self.inner.state.borrow().foreground != Some(index) {
                    self.home();
                }
                if app_state == AppState::Suspended {
                    // A view is a function of its values: rebuild it from the
                    // container and the values the session kept.
                    let assets = bundle::load(&self.shelf(), &id)
                        .await
                        .ok()
                        .and_then(|bundle| bundle.view);
                    let resumed = {
                        let mut state = self.inner.state.borrow_mut();
                        let running = state.running(index).ok_or(AppError::NotFound)?;
                        match (&running.session, &assets) {
                            (Some(session), Some(assets)) => session.resume(assets).is_ok(),
                            _ => false,
                        }
                    };
                    if !resumed {
                        let mut state = self.inner.state.borrow_mut();
                        self.remove(&mut state, index, None);
                        drop(state);
                        log::warn!("app: {id}: its view could not be rebuilt");
                        return Err(AppError::ViewRejected);
                    }
                }
                let mut state = self.inner.state.borrow_mut();
                state.foreground = Some(index);
                state.slots[index].deadline = None;
                let last_used = self.tick_clock();
                let running = state.running(index).ok_or(AppError::NotFound)?;
                running.state = AppState::Active;
                running.last_used = last_used;
                running.port.transition(
                    AppState::Active,
                    &[Lifecycle::Foreground, Lifecycle::Active],
                );
                state.present();
                drop(state);
                self.wake(index);
            }
        }
        let mut state = self.inner.state.borrow_mut();
        state
            .running(index)
            .map(|running| running.public())
            .ok_or(AppError::NotFound)
    }

    /// Returns to the home screen: the foreground app moves to the
    /// background, saves its state, and is suspended after [`GRACE`].
    pub fn home(&self) -> Option<RunningApp> {
        let mut state = self.inner.state.borrow_mut();
        let index = state.foreground.take()?;
        let deadline = Instant::now() + GRACE;
        state.slots[index].deadline = Some(deadline);
        let running = state.running(index)?;
        let events: &[Lifecycle] = if running.state == AppState::Active {
            &[Lifecycle::Inactive, Lifecycle::Background]
        } else {
            &[Lifecycle::Background]
        };
        running.state = AppState::Background;
        if let Some(session) = &running.session {
            session.clear_events();
        }
        running.port.transition(AppState::Background, events);
        let public = running.public();
        state.present();
        drop(state);
        self.wake(index);
        Some(public)
    }

    /// Goes back one step: the foreground app pops a page with `app:back`
    /// when it has one open above its first (see `app.navigation`), and
    /// otherwise moves to the background as with [`AppManager::home`].
    /// Returns whether the app went back a page.
    pub fn back(&self) -> bool {
        {
            let mut state = self.inner.state.borrow_mut();
            let Some(index) = state.foreground else {
                return false;
            };
            if let Some(running) = state.running(index)
                && running.state == AppState::Active
                && running.port.depth() > 0
            {
                running
                    .port
                    .transition(AppState::Active, &[Lifecycle::Back]);
                return true;
            }
        }
        self.home();
        false
    }

    /// Lets the foreground app take input, or stops it while an overlay such
    /// as the control center covers it or a transition runs.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoForegroundApp`] when no app is in the foreground.
    pub fn set_interactive(&self, interactive: bool) -> Result<RunningApp, AppError> {
        let mut state = self.inner.state.borrow_mut();
        let index = state.foreground.ok_or(AppError::NoForegroundApp)?;
        let running = state.running(index).ok_or(AppError::NoForegroundApp)?;
        match (interactive, running.state) {
            (true, AppState::Inactive) => {
                running.state = AppState::Active;
                running
                    .port
                    .transition(AppState::Active, &[Lifecycle::Active]);
            }
            (false, AppState::Active) => {
                running.state = AppState::Inactive;
                if let Some(session) = &running.session {
                    session.clear_events();
                }
                running
                    .port
                    .transition(AppState::Inactive, &[Lifecycle::Inactive]);
            }
            _ => {}
        }
        let public = running.public();
        state.present();
        Ok(public)
    }

    /// Asks app `id` to exit with `app:terminate`; it is cancelled if it is
    /// still running after [`GRACE`].
    pub fn terminate(&self, id: &str) -> Option<RunningApp> {
        let mut state = self.inner.state.borrow_mut();
        let index = state.find(id)?;
        if state.foreground == Some(index) {
            state.foreground = None;
        }
        let running = state.slots[index].app.as_mut()?;
        if running.state != AppState::Terminating {
            running.state = AppState::Terminating;
            if let Some(session) = &running.session {
                session.clear_events();
            }
            running
                .port
                .transition(AppState::Terminating, &[Lifecycle::Terminate]);
            state.slots[index].deadline = Some(Instant::now() + GRACE);
        }
        let public = state.slots[index].app.as_ref().map(Running::public);
        state.present();
        drop(state);
        self.wake(index);
        public
    }

    /// The app in the foreground.
    #[must_use]
    pub fn foreground(&self) -> Option<RunningApp> {
        let mut state = self.inner.state.borrow_mut();
        let index = state.foreground?;
        state.running(index).map(|running| running.public())
    }

    /// Every running app, most recently used first, as the app switcher
    /// shows them.
    #[must_use]
    pub fn running(&self) -> Vec<RunningApp> {
        let state = self.inner.state.borrow();
        let mut apps: Vec<&Running> = state
            .slots
            .iter()
            .filter_map(|slot| slot.app.as_ref())
            .collect();
        apps.sort_unstable_by_key(|running| core::cmp::Reverse(running.last_used));
        apps.into_iter().map(Running::public).collect()
    }

    /// How the most recent app run ended.
    #[must_use]
    pub fn last_exit(&self) -> Option<AppExit> {
        self.inner.state.borrow().last_exit.clone()
    }

    fn with_foreground<R>(
        &self,
        interactive: bool,
        f: impl FnOnce(&UiSession) -> R,
    ) -> Result<R, AppError> {
        let mut state = self.inner.state.borrow_mut();
        let index = state.foreground.ok_or(AppError::NoForegroundApp)?;
        let running = state.running(index).ok_or(AppError::NoForegroundApp)?;
        if interactive && running.state != AppState::Active {
            return Err(AppError::NotInteractive);
        }
        running
            .session
            .as_ref()
            .map(f)
            .ok_or(AppError::NoForegroundApp)
    }

    /// Taps the foreground app's view; returns the handler it called, if any.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when no app is in the foreground or it is not
    /// taking input.
    pub fn tap(&self, x: f32, y: f32) -> Result<Option<String>, AppError> {
        self.with_foreground(true, |session| session.press_at(x, y))
    }

    /// The text shown by the foreground app's view, in document order.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoForegroundApp`] when no app is in the foreground.
    pub fn texts(&self) -> Result<Vec<String>, AppError> {
        self.with_foreground(false, UiSession::texts)
    }

    /// Supervises every app run; never returns.
    ///
    /// The `app-manager` Plugin runs this in its task. It notices exits,
    /// enforces grace periods, suspends apps that parked, and cancels runs
    /// that were replaced.
    pub async fn supervise(&self) {
        let slots: [_; MAX_APP_RUNS] = core::array::from_fn(|index| self.supervise_slot(index));
        select_array(slots).await;
    }

    async fn supervise_slot(&self, index: usize) {
        let inner = &self.inner;
        loop {
            let pending = {
                let mut state = inner.state.borrow_mut();
                let slot = &mut state.slots[index];
                slot.pending.take().map(|run| (slot.generation, run))
            };
            let Some((generation, mut run)) = pending else {
                inner.wakes[index].wait().await;
                continue;
            };
            loop {
                let deadline = {
                    let state = inner.state.borrow();
                    let slot = &state.slots[index];
                    (slot.generation == generation)
                        .then_some(slot.deadline)
                        .flatten()
                };
                let timer = async {
                    match deadline {
                        Some(deadline) => Timer::at(deadline).await,
                        None => core::future::pending().await,
                    }
                };
                match select3(
                    inner.wakes[index].wait(),
                    completion(&inner.vm, &mut run),
                    timer,
                )
                .await
                {
                    // Replaced or evicted: dropping the run cancels it.
                    Either3::First(())
                        if inner.state.borrow().slots[index].generation != generation =>
                    {
                        break;
                    }
                    Either3::First(()) => {}
                    Either3::Second(completion) => {
                        self.exited(index, generation, completion);
                        break;
                    }
                    Either3::Third(()) => {
                        if let Deadline::Cancel = self.deadline(index, generation) {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Acts on a passed grace period: an app that parked is suspended, one
    /// that did not is cancelled.
    fn deadline(&self, index: usize, generation: u32) -> Deadline {
        let mut state = self.inner.state.borrow_mut();
        if state.slots[index].generation != generation {
            return Deadline::Cancel;
        }
        state.slots[index].deadline = None;
        let Some(running) = state.slots[index].app.as_mut() else {
            return Deadline::Cancel;
        };
        let reason = match running.state {
            AppState::Background if running.port.is_waiting() => {
                if let Some(session) = &running.session {
                    session.suspend();
                }
                running.state = AppState::Suspended;
                running.port.transition(AppState::Suspended, &[]);
                while state
                    .slots
                    .iter()
                    .filter(|slot| {
                        slot.app
                            .as_ref()
                            .is_some_and(|app| app.state == AppState::Suspended)
                    })
                    .count()
                    > MAX_SUSPENDED
                {
                    let Some(oldest) = lru_suspended(&state) else {
                        break;
                    };
                    self.remove(&mut state, oldest, None);
                    if oldest == index {
                        return Deadline::Cancel;
                    }
                }
                return Deadline::Keep;
            }
            AppState::Background => "did not wait for events within the background grace period",
            AppState::Terminating => "did not exit within the termination grace period",
            _ => return Deadline::Keep,
        };
        let exit = AppExit {
            id: running.info.id.clone(),
            run_id: running.run_id,
            outcome: VmRunOutcome::Cancelled,
            error: None,
            diagnostic: Some(String::from(reason)),
            output: Vec::new(),
            foreground: state.foreground == Some(index),
        };
        log::warn!("app: {}: {reason}", exit.id);
        self.remove(&mut state, index, Some(exit));
        Deadline::Cancel
    }

    /// Records how the run of `generation` ended and frees its slot.
    fn exited(&self, index: usize, generation: u32, completion: Option<VmRunCompletion>) {
        let mut state = self.inner.state.borrow_mut();
        if state.slots[index].generation != generation {
            return;
        }
        let Some(running) = state.slots[index].app.as_ref() else {
            return;
        };
        let foreground = state.foreground == Some(index);
        let exit = completion.map(|completion| {
            match completion.outcome {
                VmRunOutcome::Error => log::warn!(
                    "app: {} failed: {}",
                    running.info.id,
                    completion.diagnostic.as_deref().unwrap_or("")
                ),
                _ => log::info!("app: {} exited", running.info.id),
            }
            let skip = completion
                .output
                .len()
                .saturating_sub(MAX_EXIT_OUTPUT_LINES);
            AppExit {
                id: running.info.id.clone(),
                run_id: completion.run_id,
                outcome: completion.outcome,
                error: completion.error,
                diagnostic: completion.diagnostic,
                output: completion.output.into_iter().skip(skip).collect(),
                foreground,
            }
        });
        self.remove(&mut state, index, exit);
    }

    /// Ends the app in `index`: its view closes, its program's event loop
    /// ends, and its run is cancelled if it is still going.
    fn remove(&self, state: &mut State, index: usize, exit: Option<AppExit>) {
        let slot = &mut state.slots[index];
        slot.generation = slot.generation.wrapping_add(1);
        slot.pending = None;
        slot.deadline = None;
        if let Some(running) = slot.app.take() {
            self.inner.ports.remove(&running.ticket);
            running.port.end();
        }
        if state.foreground == Some(index) {
            state.foreground = None;
        }
        if exit.is_some() {
            state.last_exit = exit;
        }
        state.present();
        self.wake(index);
    }
}

/// Deletes the file or directory tree at `path`; a missing one is fine.
fn remove_tree<'a>(
    filesystem: &'a ScopedVfs,
    path: &'a str,
) -> Pin<Box<dyn Future<Output = Result<(), FsError>> + 'a>> {
    Box::pin(async move {
        let metadata = match filesystem.metadata(path).await {
            Ok(metadata) => metadata,
            Err(FsError::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.is_dir() {
            return filesystem.remove_file(path).await;
        }
        for name in filesystem.list_dir(path).await? {
            remove_tree(filesystem, &format!("{path}/{name}")).await?;
        }
        filesystem.remove_dir(path).await
    })
}

fn lru_suspended(state: &State) -> Option<usize> {
    state
        .slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            slot.app
                .as_ref()
                .filter(|app| app.state == AppState::Suspended)
                .map(|app| (index, app.last_used))
        })
        .min_by_key(|(_, last_used)| *last_used)
        .map(|(index, _)| index)
}

/// Waits for `run` to finish. Apps have no input, so a read gets EOF.
async fn completion(vm: &Vm, run: &mut VmRun) -> Option<VmRunCompletion> {
    loop {
        match run.next_update().await {
            Ok(VmRunUpdate::Completed(completion)) => return Some(completion),
            Ok(VmRunUpdate::Progress(VmRunProgress::InputRequired { run_id })) => {
                let _closed = vm.input(VmInputRequest {
                    run_id,
                    input: None,
                    eof: Some(true),
                });
            }
            Err(error) => {
                log::warn!("app: lost an app run: {error}");
                return None;
            }
        }
    }
}
