//! What each `system` call reads or changes.

use crate::words;
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use barracuda_agent_plugin::AgentRuntime;
use barracuda_app_plugin::{AppKind, AppManager, AppState};
use barracuda_platform::StationState;
use barracuda_plugin::api::TargetIdentity;
use barracuda_shell_plugin::{AgentState, AgentStatus, Notifications, Preferences};
use barracuda_time_plugin::UtcClock;
use barracuda_ui_plugin::icons;
use barracuda_vfs::ScopedVfs;
use barracuda_wifi_plugin::WifiControl;
use barracuda_workflow_plugin::WorkflowService;
use serde_json::{Value, json};

/// The system's version, shown in Settings.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The most files [`Call::Files`] lists.
pub const MAX_FILES: usize = 64;
/// The deepest directory the storage walk enters.
const MAX_DEPTH: usize = 8;

/// The shared places the Files app shows: its name, and the directory.
pub const AREAS: [(&str, &str); 4] = [
    ("媒体", "/workspace/media"),
    ("资源", "/workspace/resources"),
    ("存储卡", "/workspace/removable"),
    ("缓存", "/workspace/cache"),
];
/// Where installed apps live, in the `app` Plugin's media tree.
const INSTALLED_APPS: &str = "/plugins/media/app/apps";

/// One `system` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// Network, preferences, and the device.
    Status,
    /// Nearby networks.
    Scan,
    /// Changes one preference.
    Set(Setting),
    /// Every launchable app and its state.
    Apps,
    /// The Agent's conversations.
    Sessions,
    /// The loaded Workflows.
    Workflows,
    /// Space used by media, apps, and cache.
    Storage,
    /// How many files each of [`AREAS`] holds, or the files in one.
    Files(Option<usize>),
    /// Posts a notice.
    Notify {
        /// What happened.
        title: String,
        /// More detail.
        body: String,
        /// A system icon.
        icon: Option<String>,
    },
    /// Asks the person to allow or deny something; answers when they do.
    Ask {
        /// The question.
        title: String,
        /// Who asks.
        body: String,
    },
    /// Shows the Agent's state on the home screen.
    Agent {
        /// Its state.
        state: AgentState,
        /// What it heard.
        question: String,
        /// What it answered.
        answer: String,
    },
}

/// A preference [`Call::Set`] changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Percent.
    Brightness(u8),
    /// Percent.
    Volume(u8),
    /// Speaker off.
    Muted(bool),
    /// Microphone on.
    Microphone(bool),
    /// Do not disturb.
    Quiet(bool),
    /// Seconds; 0 is never.
    ScreenOff(u16),
}

impl Call {
    /// Reads a call from its Lua name and arguments.
    ///
    /// # Errors
    ///
    /// Returns the message Lua receives for an unknown call or a bad
    /// argument.
    pub fn parse(
        name: &str,
        first: Option<String>,
        second: Option<String>,
        third: Option<String>,
    ) -> Result<Self, String> {
        Ok(match name {
            "status" => Self::Status,
            "scan" => Self::Scan,
            "set" => Self::Set(setting(first.as_deref(), second.as_deref())?),
            "apps" => Self::Apps,
            "sessions" => Self::Sessions,
            "workflows" => Self::Workflows,
            "storage" => Self::Storage,
            "files" => Self::Files(match first {
                None => None,
                Some(area) => Some(
                    AREAS
                        .iter()
                        .position(|(name, _)| *name == area)
                        .ok_or_else(|| format!("no file area \"{area}\""))?,
                ),
            }),
            "notify" => Self::Notify {
                title: required(first, "a notice needs a title")?,
                body: second.unwrap_or_default(),
                icon: third,
            },
            "ask" => Self::Ask {
                title: required(first, "a question needs a title")?,
                body: second.unwrap_or_default(),
            },
            "agent" => Self::Agent {
                state: match first.as_deref() {
                    Some("standby") => AgentState::Standby,
                    Some("listening") => AgentState::Listening,
                    Some("thinking") => AgentState::Thinking,
                    Some("answering") => AgentState::Answering,
                    other => return Err(format!("no Agent state \"{}\"", other.unwrap_or(""))),
                },
                question: second.unwrap_or_default(),
                answer: third.unwrap_or_default(),
            },
            _ => return Err(format!("system.{name} does not exist")),
        })
    }
}

fn required(value: Option<String>, message: &str) -> Result<String, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| String::from(message))
}

fn setting(key: Option<&str>, value: Option<&str>) -> Result<Setting, String> {
    let value = value.unwrap_or("");
    let percent = || {
        value
            .parse::<u8>()
            .ok()
            .filter(|percent| *percent <= 100)
            .ok_or_else(|| format!("{} is a percent from 0 to 100", key.unwrap_or("")))
    };
    let switch = || match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("{} is true or false", key.unwrap_or(""))),
    };
    Ok(match key {
        Some("brightness") => Setting::Brightness(percent()?),
        Some("volume") => Setting::Volume(percent()?),
        Some("muted") => Setting::Muted(switch()?),
        Some("microphone") => Setting::Microphone(switch()?),
        Some("quiet") => Setting::Quiet(switch()?),
        Some("screen_off") => Setting::ScreenOff(
            value
                .parse()
                .map_err(|_| String::from("screen_off is a number of seconds"))?,
        ),
        other => return Err(format!("no setting \"{}\"", other.unwrap_or(""))),
    })
}

/// The capabilities `system` calls read and change.
pub struct Backend {
    pub(crate) identity: TargetIdentity,
    pub(crate) filesystem: ScopedVfs,
    pub(crate) preferences: Rc<Preferences>,
    pub(crate) notices: Rc<Notifications>,
    pub(crate) agent_status: Rc<AgentStatus>,
    pub(crate) apps: AppManager,
    pub(crate) wifi: Rc<WifiControl>,
    pub(crate) agent: Rc<AgentRuntime>,
    pub(crate) workflows: Rc<WorkflowService>,
    pub(crate) clock: Rc<UtcClock>,
}

impl Backend {
    /// Answers `call` with the value Lua receives, or an error message.
    pub async fn call(&self, call: Call) -> Result<Value, String> {
        match call {
            Call::Status => Ok(self.status()),
            Call::Scan => self.scan().await,
            Call::Set(setting) => {
                self.preferences.update(|values| match setting {
                    Setting::Brightness(percent) => values.brightness = percent,
                    Setting::Volume(percent) => values.volume = percent,
                    Setting::Muted(on) => values.muted = on,
                    Setting::Microphone(on) => values.microphone = on,
                    Setting::Quiet(on) => values.quiet = on,
                    Setting::ScreenOff(seconds) => values.screen_off = seconds,
                });
                Ok(self.status())
            }
            Call::Apps => Ok(self.apps().await),
            Call::Sessions => Ok(self.sessions().await),
            Call::Workflows => Ok(Value::Array(
                self.workflows
                    .definitions()
                    .iter()
                    .map(|definition| {
                        json!({
                            "id": definition.id().as_str(),
                            "event": definition.event().as_str(),
                            "trigger": words::trigger(definition.event().as_str()),
                        })
                    })
                    .collect(),
            )),
            Call::Storage => Ok(self.storage().await),
            Call::Files(None) => Ok(self.counts().await),
            Call::Files(Some(area)) => Ok(self.files(area).await),
            Call::Notify { title, body, icon } => {
                let id = self.notices.post(&title, &body, icon.as_deref());
                Ok(json!({ "id": id }))
            }
            Call::Ask { title, body } => {
                let id = self.notices.ask(&title, &body);
                let answer = self.notices.answer(id).await;
                Ok(json!({ "allowed": answer == Some(true), "answered": answer.is_some() }))
            }
            Call::Agent {
                state,
                question,
                answer,
            } => {
                self.agent_status.set(state, &question, &answer);
                Ok(Value::Bool(true))
            }
        }
    }

    fn status(&self) -> Value {
        let status = self.wifi.status();
        let (state, ssid) = match status.station() {
            StationState::Connected { ssid } => ("connected", ssid.clone()),
            StationState::Connecting => ("connecting", None),
            StationState::Disconnected => ("disconnected", None),
        };
        let values = self.preferences.get();
        let platform = self.identity.platform();
        let board = self.identity.board();
        json!({
            "wifi": { "state": state, "ssid": ssid },
            "brightness": values.brightness,
            "volume": values.volume,
            "muted": values.muted,
            "microphone": values.microphone,
            "quiet": values.quiet,
            "screen_off": values.screen_off,
            "agent": {
                "state": match self.agent_status.state() {
                    AgentState::Standby => "standby",
                    AgentState::Listening => "listening",
                    AgentState::Thinking => "thinking",
                    AgentState::Answering => "answering",
                },
                "question": self.agent_status.question(),
                "answer": self.agent_status.answer(),
            },
            "device": {
                "version": VERSION,
                "board": board.name(),
                "chip": board.hardware().chip(),
                "platform": platform.name(),
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

    async fn apps(&self) -> Value {
        let running = self.apps.running();
        let list = self.apps.list().await;
        Value::Array(
            list.apps
                .iter()
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
                        "icon": app.icon,
                        "glyphs": glyphs(app.icon.as_deref()),
                        "letter": app.name.chars().next().map(String::from).unwrap_or_default(),
                        "system": app.kind == AppKind::System,
                        "state": state,
                    })
                })
                .collect(),
        )
    }

    async fn sessions(&self) -> Value {
        let now = self.clock.now().ok().map(u64::from);
        let mut sessions = self.agent.describe_sessions().await;
        sessions.sort_by_key(|session| core::cmp::Reverse(session.updated_at));
        Value::Array(
            sessions
                .into_iter()
                .map(|session| {
                    json!({
                        "id": format!("{}", session.session),
                        "title": session.title.unwrap_or_else(|| String::from("新对话")),
                        "when": session
                            .updated_at
                            .map(|at| words::when(at, now))
                            .unwrap_or_default(),
                    })
                })
                .collect(),
        )
    }

    async fn storage(&self) -> Value {
        let media = self.usage(AREAS[0].1).await;
        let apps = self.usage(INSTALLED_APPS).await;
        let cache = self.usage(AREAS[3].1).await;
        let used = media.bytes + apps.bytes + cache.bytes;
        json!({
            "used": used,
            "label": words::size(used),
            "parts": [
                { "name": "媒体", "icon": "image", "bytes": media.bytes, "label": words::size(media.bytes) },
                { "name": "应用", "icon": "package", "bytes": apps.bytes, "label": words::size(apps.bytes) },
                { "name": "缓存", "icon": "memory", "bytes": cache.bytes, "label": words::size(cache.bytes) },
            ],
        })
    }

    async fn files(&self, area: usize) -> Value {
        let (name, root) = AREAS[area];
        let mut files = Vec::new();
        let usage = walk(&self.filesystem, root, Some(&mut files)).await;
        json!({
            "name": name,
            "present": usage.present,
            "count": usage.files,
            "label": words::size(usage.bytes),
            "files": files
                .into_iter()
                .map(|(path, bytes)| json!({ "path": path, "label": words::size(bytes) }))
                .collect::<Vec<_>>(),
        })
    }

    /// How many files every area holds.
    async fn counts(&self) -> Value {
        let mut counts = Vec::new();
        for (name, root) in AREAS {
            let usage = walk(&self.filesystem, root, None).await;
            counts.push(json!({ "name": name, "count": usage.files, "present": usage.present }));
        }
        Value::Array(counts)
    }

    async fn usage(&self, root: &str) -> Usage {
        walk(&self.filesystem, root, None).await
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

/// What a directory tree holds.
#[derive(Default)]
struct Usage {
    present: bool,
    files: u64,
    bytes: u64,
}

/// Adds up the files under `root`, listing up to [`MAX_FILES`] of them into
/// `list` by their path under it.
async fn walk(
    filesystem: &ScopedVfs,
    root: &str,
    mut list: Option<&mut Vec<(String, u64)>>,
) -> Usage {
    let mut usage = Usage::default();
    let mut pending = vec![(String::from(root), 0_usize)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = filesystem.read_dir(&dir).await else {
            continue;
        };
        usage.present = true;
        for entry in entries.flatten() {
            let path = format!("{dir}/{}", entry.file_name());
            let metadata = entry.metadata();
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else if metadata.is_file() {
                usage.files += 1;
                usage.bytes += metadata.len();
                if let Some(list) = list.as_deref_mut()
                    && list.len() < MAX_FILES
                {
                    let shown = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .trim_start_matches('/');
                    list.push((String::from(shown), metadata.len()));
                }
            }
        }
    }
    if let Some(list) = list {
        list.sort_unstable();
    }
    usage
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn parse(name: &str, args: [Option<&str>; 3]) -> Result<Call, String> {
        let [first, second, third] = args.map(|arg| arg.map(String::from));
        Call::parse(name, first, second, third)
    }

    #[test]
    fn calls_parse_from_their_lua_arguments() {
        assert_eq!(
            parse("set", [Some("brightness"), Some("40"), None]),
            Ok(Call::Set(Setting::Brightness(40)))
        );
        assert_eq!(
            parse("set", [Some("muted"), Some("true"), None]),
            Ok(Call::Set(Setting::Muted(true)))
        );
        assert!(parse("set", [Some("volume"), Some("140"), None]).is_err());
        assert!(parse("set", [Some("muted"), Some("1"), None]).is_err());
        assert_eq!(
            parse("files", [Some("缓存"), None, None]),
            Ok(Call::Files(Some(3)))
        );
        assert_eq!(parse("files", [None, None, None]), Ok(Call::Files(None)));
        assert!(parse("files", [Some("/etc"), None, None]).is_err());
        assert!(parse("notify", [None, None, None]).is_err());
        assert_eq!(
            parse("agent", [Some("thinking"), Some("几点了？"), None]),
            Ok(Call::Agent {
                state: AgentState::Thinking,
                question: String::from("几点了？"),
                answer: String::new(),
            })
        );
        assert!(parse("reboot", [None, None, None]).is_err());
    }
}
