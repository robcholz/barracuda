//! The `AppManager` capability: the app lifecycle and the tasks that
//! supervise app runs.
//!
//! The system UI drives the lifecycle from gestures: opening an app,
//! returning home, covering the app with an overlay, and closing an app in
//! the switcher. This module turns those into app states, lifecycle events
//! for the program, and VM and memory budgets.

use crate::bundle::{self, AppInfo, AppList};
use crate::package::AppPackage;
use crate::port::{AppState, Lifecycle, Port, Ports};
use crate::storage::Storage;
use alloc::{format, rc::Rc, string::String, vec::Vec};
use barracuda_ui_plugin::{UiService, UiSession};
use barracuda_vfs::ScopedVfs;
use barracuda_vm_plugin::{
    LuaPackageRegistration, LuaPackageRegistry, LuaPackageRegistryError, Vm, VmError,
    VmExecutionError, VmInputRequest, VmRun, VmRunCompletion, VmRunOutcome, VmRunProgress,
    VmRunRequest, VmRunUpdate,
};
use core::cell::{Cell, RefCell};
use embassy_futures::select::{Either3, select_array, select3};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic_util::Arc;
use serde::Serialize;

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
    /// No app with that ID is installed, bundled, or in the Workspace.
    NotFound,
    /// The container breaks the app format.
    InvalidBundle,
    /// The filesystem failed while reading the container.
    Storage,
    /// The view was rejected by the UI runtime.
    ViewRejected,
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
            AppError::InvalidBundle => "invalid app container",
            AppError::Storage => "app storage failed",
            AppError::ViewRejected => "app view rejected",
            AppError::NoForegroundApp => "no app is in the foreground",
            AppError::NotInteractive => "the foreground app is not taking input",
            AppError::RuntimeUnavailable => "VM runtime is unavailable",
            AppError::Busy => "no app run is free",
        })
    }
}

impl core::error::Error for AppError {}

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
    /// The VM diagnostic, or why the `app` Plugin cancelled the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// The last output lines, at most [`MAX_EXIT_OUTPUT_LINES`].
    pub output: Vec<String>,
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
}

/// What the deadline of a slot's run decided.
enum Deadline {
    Keep,
    Cancel,
}

struct Inner {
    filesystem: ScopedVfs,
    vm: Rc<Vm>,
    ui: Rc<UiService>,
    ports: Arc<Ports>,
    state: RefCell<State>,
    wakes: [Signal<NoopRawMutex, ()>; MAX_APP_RUNS],
    clock: Cell<u64>,
    tickets: Cell<u64>,
}

/// Runs apps through their lifecycle; published by the `app` Plugin.
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
    ///
    /// # Errors
    ///
    /// Returns the registry's error when the package cannot be registered.
    pub fn register(
        filesystem: ScopedVfs,
        vm: Rc<Vm>,
        ui: Rc<UiService>,
        packages: &LuaPackageRegistry,
    ) -> Result<(Self, LuaPackageRegistration), LuaPackageRegistryError> {
        let ports = Arc::new(Ports::new());
        let registration = packages.register(AppPackage::new(Arc::clone(&ports)))?;
        let manager = Self {
            inner: Rc::new(Inner {
                filesystem,
                vm,
                ui,
                ports,
                state: RefCell::new(State::default()),
                wakes: core::array::from_fn(|_| Signal::new()),
                clock: Cell::new(0),
                tickets: Cell::new(0),
            }),
        };
        Ok((manager, registration))
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
    }

    /// Every launchable app, sorted by ID, and every container that fails the
    /// app format with the reason.
    pub async fn list(&self) -> AppList {
        bundle::list(&self.inner.filesystem).await
    }

    /// Opens app `id`: resumes it if it is running, otherwise launches it.
    ///
    /// An app with a view becomes the active foreground app and the previous
    /// one moves to the background. A headless app starts as a service.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the container cannot be loaded, its view is
    /// rejected, or no app run is free.
    pub async fn open(&self, id: &str) -> Result<RunningApp, AppError> {
        let running = self.inner.state.borrow().find(id);
        if let Some(index) = running {
            return self.reopen(index).await;
        }
        let bundle = bundle::load(&self.inner.filesystem, id).await?;
        // Another open may have launched it while the container loaded.
        let running = self.inner.state.borrow().find(id);
        if let Some(index) = running {
            return self.reopen(index).await;
        }
        let has_view = bundle.view.is_some();
        if has_view {
            self.home();
        }
        let index = self.free_slot()?;
        let session = match &bundle.view {
            Some(view) => Some(self.inner.ui.open(view).map_err(|error| {
                log::warn!("app: {id}: {error}");
                AppError::ViewRejected
            })?),
            None => None,
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
            state,
        ));
        let ticket = self.ticket(index);
        self.inner.ports.insert(ticket.clone(), Arc::clone(&port));
        // The binding shares the first line so Lua line numbers stay correct.
        let source = format!("require(\"app\").__bind({ticket:?}); {}", bundle.source);
        let run = match self.inner.vm.run(VmRunRequest { source }) {
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
                    let assets = bundle::load(&self.inner.filesystem, &id)
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
        drop(state);
        self.wake(index);
        Some(public)
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
        Ok(running.public())
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
    /// The `app` Plugin runs this in its task. It notices exits,
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
        self.wake(index);
    }
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
