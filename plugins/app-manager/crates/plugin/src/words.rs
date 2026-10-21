//! What the system UI calls things on screen.

use alloc::{format, string::String};
use barracuda_appkit::{AppError, AppExit, AppState, VmRunOutcome};

/// The system icon shown beside what an app asks for: this Plugin's own
/// `notify` has its own, every other package the same one.
pub(crate) fn permission_icon(package: &str) -> &'static str {
    if package == crate::NOTIFY {
        "alert"
    } else {
        "activity"
    }
}

/// The note under what an app only uses when the device has it, on the app
/// info screen.
pub(crate) const OPTIONAL: &str = "Optional";

/// Where a running app is in its lifecycle, for the app switcher.
pub(crate) const fn state(state: AppState) -> &'static str {
    match state {
        AppState::Active | AppState::Inactive | AppState::Running => "Running",
        AppState::Background => "Background",
        AppState::Suspended => "Suspended",
        AppState::Terminating => "Quitting",
    }
}

/// Why an app that was on the display ended.
pub(crate) fn exit(exit: &AppExit) -> &'static str {
    match exit.outcome {
        VmRunOutcome::Cancelled => "Stopped responding and was closed",
        _ => "Crashed; unsaved changes were lost",
    }
}

/// Why an app could not be opened.
pub(crate) const fn open_error(error: AppError) -> &'static str {
    match error {
        AppError::InvalidBundle | AppError::ViewRejected => "The app's files are damaged",
        AppError::Busy => "Too many apps running",
        AppError::Incompatible => "Can't run on this device",
        AppError::Storage => "Couldn't read storage",
        _ => "Can't open right now",
    }
}

/// The letter an app's icon shows: the first character of its name.
pub(crate) fn glyph(name: &str) -> String {
    name.chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_default()
}

/// `HH:MM` of a UTC timestamp in milliseconds.
pub(crate) fn clock(millis: u64) -> String {
    let minutes = millis / 60_000;
    format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60)
}
