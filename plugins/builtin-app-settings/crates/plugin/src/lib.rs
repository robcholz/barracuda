//! The Settings system app, and the `settings` Lua package that is its way to
//! the device: Wi-Fi, the preferences, apps, storage, and what the device is.

#![no_std]

extern crate alloc;

mod words;

use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use barracuda_app_manager_plugin::{
    AppKind, AppManager, AppState, BuiltinApp, BuiltinBackend, BuiltinPackage, Preferences,
    WIFI_SETTINGS, icons,
};
use barracuda_platform::StationState;
use barracuda_plugin::api::{PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_vfs::ScopedVfs;
use barracuda_wifi_plugin::WifiControl;
use core::{future::Future, pin::Pin};
use serde_json::{Value, json};

/// The app's ID, and its package's name.
pub const SETTINGS: &str = "settings";
/// The system's version, shown on the device page.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Shared Workspace areas whose size the storage page shows: its name,
/// icon, and directory.
const AREAS: [(&str, &str, &str); 2] = [
    ("媒体", "image", "/workspace/media"),
    ("缓存", "memory", "/workspace/cache"),
];
/// The deepest directory the storage walk enters.
const MAX_DEPTH: usize = 8;

/// Ships the Settings app and answers its `settings` package.
#[barracuda_plugin::macros::plugin]
pub struct SettingsAppPlugin {
    identity: TargetIdentity,
}

impl SettingsAppPlugin {
    /// Copies the Target identity the device page shows.
    #[must_use]
    pub const fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            identity: context.target_identity,
        }
    }
}

impl Plugin for SettingsAppPlugin {
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
        let apps = context.require::<AppManager>("app-manager")?;
        let backend = Settings {
            identity: self.identity,
            filesystem: filesystem.clone(),
            preferences: context.require::<Preferences>("app-manager")?,
            apps: Rc::clone(&apps),
            wifi: context.require::<WifiControl>("wifi")?,
        };
        let registration = apps
            .add_builtin(BuiltinApp {
                id: SETTINGS,
                filesystem,
                opens: &[WIFI_SETTINGS],
                package: Some(BuiltinPackage {
                    name: SETTINGS,
                    backend: Rc::new(backend),
                }),
            })
            .map_err(PluginError::registration)?;
        if let Some(registration) = registration {
            context.retain(registration);
        }
        Ok(())
    }
}

/// Answers the `settings` package.
struct Settings {
    identity: TargetIdentity,
    filesystem: ScopedVfs,
    preferences: Rc<Preferences>,
    apps: Rc<AppManager>,
    wifi: Rc<WifiControl>,
}

impl BuiltinBackend for Settings {
    fn call<'a>(
        &'a self,
        call: &'a str,
        args: Vec<Option<String>>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            match call {
                "status" => Ok(self.status()),
                "scan" => self.scan().await,
                "set" => {
                    let change = Change::parse(args.first(), args.get(1))?;
                    self.preferences.update(|values| match change {
                        Change::Brightness(percent) => values.brightness = percent,
                        Change::Volume(percent) => values.volume = percent,
                        Change::Muted(on) => values.muted = on,
                        Change::Microphone(on) => values.microphone = on,
                        Change::Quiet(on) => values.quiet = on,
                        Change::ScreenOff(seconds) => values.screen_off = seconds,
                    });
                    Ok(self.status())
                }
                "apps" => Ok(self.installed().await),
                "storage" => Ok(self.storage().await),
                _ => Err(format!("settings.{call} does not exist")),
            }
        })
    }
}

impl Settings {
    fn status(&self) -> Value {
        let status = self.wifi.status();
        let (state, ssid) = match status.station() {
            StationState::Connected { ssid } => ("connected", ssid.clone()),
            StationState::Connecting => ("connecting", None),
            StationState::Disconnected => ("disconnected", None),
        };
        let values = self.preferences.get();
        let board = self.identity.board();
        json!({
            "wifi": { "state": state, "ssid": ssid },
            "brightness": values.brightness,
            "volume": values.volume,
            "muted": values.muted,
            "microphone": values.microphone,
            "quiet": values.quiet,
            "screen_off": values.screen_off,
            "device": {
                "version": VERSION,
                "board": board.name(),
                "chip": board.hardware().chip(),
                "platform": self.identity.platform().name(),
            },
        })
    }

    async fn scan(&self) -> Result<Value, String> {
        let mut networks = self.wifi.scan().await.map_err(|error| error.to_string())?;
        networks.sort_by_key(|network| core::cmp::Reverse(network.signal_dbm.unwrap_or(i8::MIN)));
        networks.dedup_by(|later, earlier| later.ssid == earlier.ssid);
        Ok(Value::Array(
            networks
                .into_iter()
                .filter(|network| !network.ssid.is_empty())
                .map(|network| {
                    let bars = words::bars(network.signal_dbm);
                    json!({
                        "ssid": network.ssid,
                        "bars": bars,
                        "glyphs": glyphs(Some(&format!("signal-{bars}"))),
                        "secured": network.secured,
                    })
                })
                .collect(),
        ))
    }

    /// The installed apps and their state.
    async fn installed(&self) -> Value {
        let running = self.apps.running();
        let list = self.apps.list().await;
        Value::Array(
            list.apps
                .iter()
                .filter(|app| app.kind == AppKind::Installable)
                .map(|app| {
                    let state =
                        running.iter().find(|run| run.app.id == app.id).map(|run| {
                            match run.state {
                                AppState::Background => "background",
                                AppState::Suspended => "suspended",
                                AppState::Terminating => "terminating",
                                AppState::Active | AppState::Inactive | AppState::Running => {
                                    "running"
                                }
                            }
                        });
                    json!({
                        "id": app.id,
                        "name": app.name,
                        "version": app.version,
                        "glyphs": glyphs(app.icon.as_deref()),
                        "letter": app.name.chars().next().map(String::from).unwrap_or_default(),
                        "state": state,
                    })
                })
                .collect(),
        )
    }

    async fn storage(&self) -> Value {
        let media = walk(&self.filesystem, AREAS[0].2).await;
        let apps = self.apps.used_bytes().await;
        let cache = walk(&self.filesystem, AREAS[1].2).await;
        let parts = [
            (AREAS[0].0, AREAS[0].1, media),
            ("应用", "package", apps),
            (AREAS[1].0, AREAS[1].1, cache),
        ];
        let used: u64 = parts.iter().map(|(_, _, bytes)| bytes).sum();
        json!({
            "used": used,
            "label": words::size(used),
            "parts": parts
                .iter()
                .map(|(name, icon, bytes)| json!({
                    "name": name,
                    "icon": icon,
                    "bytes": bytes,
                    "label": words::size(*bytes),
                }))
                .collect::<Vec<_>>(),
        })
    }
}

/// A preference `set` changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Brightness(u8),
    Volume(u8),
    Muted(bool),
    Microphone(bool),
    Quiet(bool),
    ScreenOff(u16),
}

impl Change {
    fn parse(key: Option<&Option<String>>, value: Option<&Option<String>>) -> Result<Self, String> {
        let key = key.and_then(Option::as_deref).unwrap_or("");
        let value = value.and_then(Option::as_deref).unwrap_or("");
        let percent = || {
            value
                .parse::<u8>()
                .ok()
                .filter(|percent| *percent <= 100)
                .ok_or_else(|| format!("{key} is a percent from 0 to 100"))
        };
        let switch = || match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(format!("{key} is true or false")),
        };
        Ok(match key {
            "brightness" => Self::Brightness(percent()?),
            "volume" => Self::Volume(percent()?),
            "muted" => Self::Muted(switch()?),
            "microphone" => Self::Microphone(switch()?),
            "quiet" => Self::Quiet(switch()?),
            "screen_off" => Self::ScreenOff(
                value
                    .parse()
                    .map_err(|_| String::from("screen_off is a number of seconds"))?,
            ),
            _ => return Err(format!("no setting \"{key}\"")),
        })
    }
}

/// The glyphs of icon `name`'s layers in the system icon font, by the role
/// their color plays; `""` for a layer it lacks or an unknown icon.
fn glyphs(name: Option<&str>) -> Value {
    let layers = name.and_then(icons::icon).unwrap_or(&[]);
    let layer = |colors: &[&str]| {
        layers
            .iter()
            .find(|layer| colors.contains(&layer.color))
            .map_or("", |layer| layer.glyph)
    };
    json!({
        "base": layer(&["#F4F4F5", "#FAFAFA"]),
        "accent": layer(&["#CCFF00"]),
        "dim": layer(&["#52525B"]),
    })
}

/// The bytes the files under `root` take up.
async fn walk(filesystem: &ScopedVfs, root: &str) -> u64 {
    let mut bytes = 0;
    let mut pending = vec![(String::from(root), 0_usize)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = filesystem.read_dir(&dir).await else {
            continue;
        };
        for entry in entries.flatten() {
            let metadata = entry.metadata();
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((format!("{dir}/{}", entry.file_name()), depth + 1));
                }
            } else {
                bytes += metadata.len();
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(text: &str) -> Option<String> {
        Some(String::from(text))
    }

    #[test]
    fn changes_parse_from_their_lua_arguments() {
        assert_eq!(
            Change::parse(Some(&some("brightness")), Some(&some("40"))),
            Ok(Change::Brightness(40))
        );
        assert_eq!(
            Change::parse(Some(&some("muted")), Some(&some("true"))),
            Ok(Change::Muted(true))
        );
        assert!(Change::parse(Some(&some("volume")), Some(&some("140"))).is_err());
        assert!(Change::parse(Some(&some("muted")), Some(&some("1"))).is_err());
        assert!(Change::parse(Some(&some("loudness")), Some(&some("3"))).is_err());
        assert!(Change::parse(None, None).is_err());
    }

    #[test]
    fn signal_icons_have_their_dim_bars() {
        let signal = glyphs(Some("signal-2"));
        assert_ne!(signal["base"], "");
        assert_ne!(signal["dim"], "");
        assert_eq!(glyphs(Some("nonexistent"))["base"], "");
    }
}
