//! The app API: what an installable app can count on from one device to the
//! next, and the check that an app fits this device.
//!
//! The app API is the `app` package at [`API_LEVEL`] and the packages
//! registered on this device, each with what it promises
//! ([`PackageInterface`]): ambient packages every app has, stable packages an
//! app asks for by name and version, and Board-specific ones. This Plugin
//! names no other Plugin's package: it learns them from the registry, so a
//! device can only describe what its own Plugins register.

use crate::bundle::{APP_PACKAGE, AppInfo};
use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use barracuda_uikit::DisplayInfo;
use barracuda_vm_plugin::PackageInterface;
use serde_json::{Value, json};

/// The version of the `app` package and the app format this system
/// implements, which a manifest's `api` names.
pub const API_LEVEL: u32 = 1;

/// What this device offers apps besides its packages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The Board's stable name, which an app's `boards` lists.
    pub board: &'static str,
    /// The display apps' views are drawn on, if the Board has one.
    pub display: Option<DisplayInfo>,
    /// Whether the display reads touch.
    pub touch: bool,
}

impl Device {
    /// A device without a display.
    #[must_use]
    pub const fn headless(board: &'static str) -> Self {
        Self {
            board,
            display: None,
            touch: false,
        }
    }
}

/// The shape a view was built for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewShape {
    /// Only the circle inscribed in the frame shows.
    Round,
    /// The whole frame shows.
    Rect,
}

/// A `requires` or `uses` entry: the package's name and, after `@`, the
/// least version of its interface the app needs, 1 when it names none, and 0
/// when the version is not a number.
#[must_use]
pub fn requirement(entry: &str) -> (&str, u32) {
    match entry.split_once('@') {
        Some((name, version)) => (name, version.parse().unwrap_or(0)),
        None => (entry, 1),
    }
}

/// The registered packages, by name, and what each promises.
pub(crate) type Packages = BTreeMap<&'static str, Option<PackageInterface>>;

/// The version of stable package `name` on this device.
fn stable(packages: &Packages, name: &str) -> Option<u32> {
    match packages.get(name) {
        Some(Some(PackageInterface::Stable { version, .. })) => Some(*version),
        _ => None,
    }
}

/// Whether every app has package `name` without asking for it.
pub(crate) fn is_ambient(packages: &Packages, name: &str) -> bool {
    name == APP_PACKAGE
        || matches!(
            packages.get(name),
            Some(Some(PackageInterface::Ambient { .. }))
        )
}

/// Why app `app` cannot run on this device, in the words the install screen
/// shows, or `None` when it can.
pub(crate) fn incompatibility(
    app: &AppInfo,
    device: &Device,
    packages: &Packages,
) -> Option<String> {
    if app.api > API_LEVEL {
        return Some(format!(
            "需要更新系统（应用需要 API {}，本机是 {API_LEVEL}）",
            app.api
        ));
    }
    if !app.boards.is_empty() && !app.boards.iter().any(|board| board == device.board) {
        return Some(format!("只适用于 {}", app.boards.join("、")));
    }
    if app.has_view {
        let Some(display) = device.display else {
            return Some(String::from("此设备没有屏幕"));
        };
        // A view lays out at the display's size: the size it names is the
        // least it needs, and a view for the whole frame loses its corners
        // on a round display.
        if let Some([width, height]) = app.view_size
            && (display.width < width || display.height < height)
        {
            return Some(format!("屏幕太小（应用需要 {width}×{height}）"));
        }
        if app.view_shape == Some(ViewShape::Rect) && display.round {
            return Some(String::from("应用需要方屏"));
        }
    }
    for entry in &app.requires {
        let (name, needed) = requirement(entry);
        match stable(packages, name) {
            None => return Some(format!("此设备没有 {name}")),
            Some(version) if version < needed => {
                return Some(format!(
                    "需要更新系统（应用需要 {name} 第 {needed} 版，本机是第 {version} 版）"
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(missing) = app
        .permissions
        .iter()
        .find(|name| !packages.contains_key(name.as_str()))
    {
        return Some(format!("此设备不支持 {missing}"));
    }
    None
}

/// The packages a run of installable app `app` may require: the ambient
/// packages, the packages it requires and uses, and its permissions.
pub(crate) fn grant(app: &AppInfo, packages: &Packages) -> Vec<String> {
    packages
        .iter()
        .filter(|(_, interface)| matches!(interface, Some(PackageInterface::Ambient { .. })))
        .map(|(name, _)| (*name).to_string())
        .chain(
            app.requires
                .iter()
                .chain(&app.uses)
                .map(|entry| requirement(entry).0.to_string()),
        )
        .chain(app.permissions.iter().cloned())
        .collect()
}

/// What `app.device()` returns: the stable packages come with their version.
pub(crate) fn describe(device: &Device, packages: &Packages) -> Value {
    let capabilities: serde_json::Map<String, Value> = packages
        .keys()
        .filter_map(|name| {
            stable(packages, name).map(|version| (String::from(*name), Value::from(version)))
        })
        .collect();
    json!({
        "api": API_LEVEL,
        "board": device.board,
        "display": device.display.map(|display| json!({
            "width": display.width,
            "height": display.height,
            "round": display.round,
        })),
        "touch": device.touch,
        "capabilities": capabilities,
    })
}

/// What package `name` lets an app do, in the words its Plugin declared,
/// for the install screen; `None` for a package that promises nothing.
pub(crate) fn purpose(packages: &Packages, name: &str) -> Option<&'static str> {
    match packages.get(name) {
        Some(Some(
            PackageInterface::Stable { purpose, .. } | PackageInterface::BoardSpecific { purpose },
        )) => Some(purpose),
        _ => None,
    }
}
