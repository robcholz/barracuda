//! The app API: what an installable app can count on from one device to the
//! next, and the check that an app fits this device.
//!
//! Packages place themselves in the app API with [`AppApi`]: core packages
//! every app has, capabilities an app asks for by name, and device-specific
//! packages that talk to a Board's hardware. `docs/app-api.md` is the
//! contract; [`CAPABILITIES`] is the part of it this check needs, so an app
//! can be told what this device lacks even when no Plugin here provides it.

use crate::bundle::AppInfo;
use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use barracuda_uikit::DisplayInfo;
use barracuda_vm_plugin::AppApi;
use serde_json::{Value, json};

/// The app API level this system implements.
pub const API_LEVEL: u32 = 1;

/// The app API's core packages: every app has them without asking.
pub const CORE_PACKAGES: [&str; 2] = ["app", "time"];

/// The app API's capabilities: the package name, the API level it joined
/// at, and what the install screen calls it when this device lacks it.
pub const CAPABILITIES: [(&str, u32, &str); 8] = [
    ("agent", 1, "Agent"),
    ("audio", 1, "声音"),
    ("camera", 1, "相机"),
    ("filesystem", 1, "共享文件"),
    ("http", 1, "网络"),
    ("light", 1, "灯光"),
    ("motion", 1, "动作传感器"),
    ("notify", 1, "通知"),
];

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

/// Whether package `name` is an app API capability at level `api` or below.
pub(crate) fn is_capability(name: &str, api: u32) -> bool {
    CAPABILITIES
        .iter()
        .any(|(capability, since, _)| *capability == name && *since <= api)
}

/// Whether package `name` is a core package.
pub(crate) fn is_core(name: &str) -> bool {
    CORE_PACKAGES.contains(&name)
}

/// The registered packages, by name, and where each stands in the app API.
pub(crate) type Packages = BTreeMap<&'static str, Option<AppApi>>;

/// The capabilities this device provides.
fn provided(packages: &Packages) -> impl Iterator<Item = &'static str> + '_ {
    packages
        .iter()
        .filter_map(|(name, api)| matches!(api, Some(AppApi::Capability { .. })).then_some(*name))
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
    let present = |name: &str| matches!(packages.get(name), Some(Some(AppApi::Capability { .. })));
    if let Some(missing) = app.requires.iter().find(|name| !present(name)) {
        let noun = CAPABILITIES
            .iter()
            .find(|(capability, _, _)| capability == missing)
            .map_or(missing.as_str(), |(_, _, noun)| noun);
        return Some(format!("此设备没有{noun}"));
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

/// The packages a run of installable app `app` may require: the core
/// packages, the capabilities it requires and uses, and its permissions.
pub(crate) fn grant(app: &AppInfo, packages: &Packages) -> Vec<String> {
    packages
        .iter()
        .filter(|(_, api)| matches!(api, Some(AppApi::Core { .. })))
        .map(|(name, _)| (*name).to_string())
        .chain(app.requires.iter().cloned())
        .chain(app.uses.iter().cloned())
        .chain(app.permissions.iter().cloned())
        .collect()
}

/// What `app.device()` returns.
pub(crate) fn describe(device: &Device, packages: &Packages) -> Value {
    let capabilities: serde_json::Map<String, Value> = provided(packages)
        .map(|name| (String::from(name), Value::Bool(true)))
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

/// What each package lets an app do, in the words its Plugin declared, for
/// the install screen; packages outside the app API have none.
pub(crate) fn purpose(packages: &Packages, name: &str) -> Option<&'static str> {
    match packages.get(name) {
        Some(Some(AppApi::Capability { purpose, .. } | AppApi::DeviceSpecific { purpose })) => {
            Some(purpose)
        }
        _ => None,
    }
}
