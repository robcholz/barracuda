//! The `AppManager` capability and the task that supervises the foreground app.

use crate::bundle::{self, AppInfo, AppList};
use alloc::{format, rc::Rc, string::String, vec::Vec};
use barracuda_ui_plugin::{UiService, UiSession};
use barracuda_vfs::ScopedVfs;
use barracuda_vm_plugin::{
    Vm, VmError, VmExecutionError, VmInputRequest, VmRun, VmRunCompletion, VmRunOutcome,
    VmRunProgress, VmRunRequest, VmRunUpdate,
};
use core::cell::RefCell;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use serde::Serialize;

/// Output lines kept from an app that exited.
pub const MAX_EXIT_OUTPUT_LINES: usize = 16;

/// Why an app operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppError {
    /// No app with that ID is installed or bundled.
    NotFound,
    /// The bundle is incomplete, malformed, or too large.
    InvalidBundle,
    /// The filesystem failed while reading the bundle.
    Storage,
    /// The view was rejected by the UI runtime.
    ViewRejected,
    /// No app is in the foreground.
    NoForegroundApp,
    /// The foreground app has no view.
    NoView,
    /// The VM is not running yet.
    RuntimeUnavailable,
    /// Every VM execution slot is in use.
    Busy,
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            AppError::NotFound => "app not found",
            AppError::InvalidBundle => "invalid app bundle",
            AppError::Storage => "app storage failed",
            AppError::ViewRejected => "app view rejected",
            AppError::NoForegroundApp => "no app is in the foreground",
            AppError::NoView => "the foreground app has no view",
            AppError::RuntimeUnavailable => "VM runtime is unavailable",
            AppError::Busy => "VM runtime is busy",
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

/// The app currently in the foreground.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct ForegroundApp {
    /// The app.
    pub app: AppInfo,
    /// The VM execution running its program.
    pub run_id: u32,
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
    /// The VM diagnostic when `outcome` is `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// The last output lines, at most [`MAX_EXIT_OUTPUT_LINES`].
    pub output: Vec<String>,
}

struct Foreground {
    app: ForegroundApp,
    session: Option<UiSession>,
}

#[derive(Default)]
struct State {
    /// Bumped by every launch and stop so the supervisor drops stale runs.
    generation: u32,
    foreground: Option<Foreground>,
    pending: Option<(u32, VmRun)>,
    last_exit: Option<AppExit>,
}

struct Inner {
    filesystem: ScopedVfs,
    vm: Rc<Vm>,
    ui: Rc<UiService>,
    state: RefCell<State>,
    wake: Signal<NoopRawMutex, ()>,
}

/// Lists, launches, and stops apps; published by the `app-manager` Plugin.
///
/// One app runs in the foreground at a time. Launching another app stops it.
#[derive(Clone)]
pub struct AppManager {
    inner: Rc<Inner>,
}

impl AppManager {
    /// Creates a manager over a Plugin filesystem.
    #[must_use]
    pub fn new(filesystem: ScopedVfs, vm: Rc<Vm>, ui: Rc<UiService>) -> Self {
        Self {
            inner: Rc::new(Inner {
                filesystem,
                vm,
                ui,
                state: RefCell::new(State::default()),
                wake: Signal::new(),
            }),
        }
    }

    /// Every launchable app, sorted by ID, and every container that fails the
    /// app format with the reason.
    pub async fn list(&self) -> AppList {
        bundle::list(&self.inner.filesystem).await
    }

    /// Stops the foreground app and launches app `id` in its place.
    ///
    /// The app's program sees a global `APP` table with its `id` and, when it
    /// has a view, the `ui` ticket to pass to `require("ui").attach`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when the bundle cannot be loaded or the VM refuses
    /// the run. The previous foreground app is stopped either way.
    pub async fn launch(&self, id: &str) -> Result<ForegroundApp, AppError> {
        let bundle = bundle::load(&self.inner.filesystem, id).await?;
        self.stop();
        let session = match &bundle.view {
            Some(view) => Some(self.inner.ui.open(view).map_err(|error| {
                log::warn!("app-manager: {id}: {error}");
                AppError::ViewRejected
            })?),
            None => None,
        };
        // The prelude shares the first line so Lua line numbers stay correct.
        let ui = session.as_ref().map_or_else(
            || String::from("nil"),
            |session| format!("{:?}", session.ticket()),
        );
        let source = format!(
            "APP = {{ id = {:?}, ui = {ui} }}; {}",
            bundle.info.id, bundle.source
        );
        let run = self.inner.vm.run(VmRunRequest { source })?;
        let app = ForegroundApp {
            app: bundle.info,
            run_id: run.run_id(),
        };
        let mut state = self.inner.state.borrow_mut();
        let generation = state.generation;
        state.foreground = Some(Foreground {
            app: app.clone(),
            session,
        });
        state.pending = Some((generation, run));
        drop(state);
        self.inner.wake.signal(());
        Ok(app)
    }

    /// Stops the foreground app, if any; its view closes and its run is cancelled.
    pub fn stop(&self) -> Option<ForegroundApp> {
        let mut state = self.inner.state.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.pending = None;
        let foreground = state.foreground.take();
        drop(state);
        self.inner.wake.signal(());
        foreground.map(|foreground| foreground.app)
    }

    /// The app in the foreground.
    #[must_use]
    pub fn foreground(&self) -> Option<ForegroundApp> {
        let state = self.inner.state.borrow();
        state
            .foreground
            .as_ref()
            .map(|foreground| foreground.app.clone())
    }

    /// How the most recent app run ended.
    #[must_use]
    pub fn last_exit(&self) -> Option<AppExit> {
        self.inner.state.borrow().last_exit.clone()
    }

    fn with_view<R>(&self, f: impl FnOnce(&UiSession) -> R) -> Result<R, AppError> {
        let state = self.inner.state.borrow();
        let foreground = state.foreground.as_ref().ok_or(AppError::NoForegroundApp)?;
        foreground.session.as_ref().map(f).ok_or(AppError::NoView)
    }

    /// Taps the foreground app's view; returns the handler it called, if any.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when no app with a view is in the foreground.
    pub fn tap(&self, x: f32, y: f32) -> Result<Option<String>, AppError> {
        self.with_view(|session| session.press_at(x, y))
    }

    /// The text shown by the foreground app's view, in document order.
    ///
    /// # Errors
    ///
    /// Returns [`AppError`] when no app with a view is in the foreground.
    pub fn texts(&self) -> Result<Vec<String>, AppError> {
        self.with_view(UiSession::texts)
    }

    /// Supervises the foreground app's run; never returns.
    ///
    /// The `app-manager` Plugin runs this in its task and stops the foreground
    /// app when the task is cancelled. It notices when an app exits, records
    /// [`AppManager::last_exit`], and cancels runs that were stopped.
    pub async fn supervise(&self) {
        let inner = &self.inner;
        loop {
            let pending = inner.state.borrow_mut().pending.take();
            let Some((generation, mut run)) = pending else {
                inner.wake.wait().await;
                continue;
            };
            let completion = loop {
                match select(inner.wake.wait(), completion(&inner.vm, &mut run)).await {
                    Either::First(()) if inner.state.borrow().generation == generation => {}
                    // Stopped or replaced: dropping the run cancels it.
                    Either::First(()) => break None,
                    Either::Second(completion) => break Some(completion),
                }
            };
            drop(run);
            if let Some(completion) = completion {
                self.exited(generation, completion);
            }
        }
    }

    /// Records how the run of `generation` ended and clears it from the foreground.
    fn exited(&self, generation: u32, completion: Option<VmRunCompletion>) {
        let mut state = self.inner.state.borrow_mut();
        if state.generation != generation {
            return;
        }
        // The view closes with its program.
        let Some(foreground) = state.foreground.take() else {
            return;
        };
        let id = foreground.app.app.id;
        let Some(completion) = completion else {
            state.last_exit = None;
            return;
        };
        match completion.outcome {
            VmRunOutcome::Error => log::warn!(
                "app-manager: {id} failed: {}",
                completion.diagnostic.as_deref().unwrap_or("")
            ),
            _ => log::info!("app-manager: {id} exited"),
        }
        let skip = completion
            .output
            .len()
            .saturating_sub(MAX_EXIT_OUTPUT_LINES);
        state.last_exit = Some(AppExit {
            id,
            run_id: completion.run_id,
            outcome: completion.outcome,
            error: completion.error,
            diagnostic: completion.diagnostic,
            output: completion.output.into_iter().skip(skip).collect(),
        });
    }
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
                log::warn!("app-manager: lost the app run: {error}");
                return None;
            }
        }
    }
}
