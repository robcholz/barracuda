//! Lists, launches, and supervises apps.
//!
//! An app is a Lua program with an optional compiled MicroTS view, stored as a
//! container directory in the format `docs/app-format.md` defines: a builtin
//! app's in the Plugin that ships it ([`BUILTIN_APP`]), others under
//! [`INSTALLED_APPS`] and [`WORKSPACE_APPS`]. Opening an app runs its program
//! on the VM with the `app` Lua package and, on a device with a display,
//! opens its view through the UI kit. The system UI drives the lifecycle
//! through [`AppManager`]; `docs/plugin.md` describes the states. This crate
//! needs no display: without one, it runs headless apps.

#![no_std]

extern crate alloc;

mod bapp;
mod builtin;
mod bundle;
mod device;
mod manager;
mod package;
mod port;
mod storage;
mod view;
mod workflow;

pub use bapp::{MAX_PACKAGE_FILES, PACKAGE_EXTENSION, PUBLIC_KEY_BYTES, Seal};
/// How an app run ended, as [`AppExit::outcome`] reports it.
pub use barracuda_vm_plugin::VmRunOutcome;
pub use builtin::{
    BuiltinApp, BuiltinBackend, BuiltinPackage, MAX_BUILTIN_ARGS, MAX_BUILTIN_CALLS,
};
pub use bundle::{
    APP_PACKAGE, AppInfo, AppKind, AppList, AppLocation, BUILTIN_APP, DEFAULT_ENTRY,
    FORMAT_VERSION, INSTALLED_APPS, InvalidApp, MANIFEST_FILE, MAX_PERMISSIONS, MAX_SOURCE_BYTES,
    MAX_VIEW_BYTES, WORKSPACE_APPS,
};
pub use device::{API_LEVEL, Device, ViewShape, requirement};
pub use manager::{
    AppError, AppExit, AppManager, BuiltinError, GRACE, MAX_APP_RUNS, MAX_EXIT_OUTPUT_LINES,
    MAX_SUSPENDED, RunningApp,
};
pub use port::{AppState, MIN_TICK_MS};
pub use storage::{APP_DATA, MAX_FILE_BYTES, MAX_KEY_BYTES, MAX_KV_BYTES};
pub use workflow::register_actions;
