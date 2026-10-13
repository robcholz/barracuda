//! The system UI: which screen is up, what it shows, and what gestures and
//! presses do.
//!
//! The shell owns one view on the system layer. While an app is in the
//! foreground and no system screen covers it, that view is hidden and the
//! app's view shows; every system screen covers the app and stops it from
//! taking input until the screen closes.

use crate::notices::Notifications;
use crate::words;
use alloc::{format, rc::Rc, string::String, vec, vec::Vec};
use barracuda_app_plugin::{AppError, AppInfo, AppKind, AppManager, RunningApp, VmRunOutcome};
use barracuda_platform::StationState;
use barracuda_time_plugin::UtcClock;
use barracuda_ui_plugin::{
    Direction, Edge, Gesture, Layer, UiAssets, UiError, UiService, UiSession, ViewCall, ViewValue,
};
use barracuda_wifi_plugin::WifiControl;

/// Brightness steps the control center cycles through, on the `0..=255`
/// scale, with their labels.
const BRIGHTNESS: [(u8, &str); 3] = [(255, "100%"), (179, "70%"), (102, "40%")];

/// The control center's tiles, by their IDs in the view.
const TILE_WIFI: i32 = 0;
const TILE_QUIET: i32 = 1;
const TILE_BRIGHTNESS: i32 = 2;

/// A system screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// The home screen: status, and the one thing that most needs attention.
    Home,
    /// Every app, launchable or offered for installation.
    Apps,
    /// The app switcher.
    Running,
    /// The notification center, pulled down from the top edge.
    Notices,
    /// The control center, pulled up from the bottom edge.
    Control,
    /// An app is starting.
    Launching,
    /// The app on the display ended unexpectedly.
    Exited,
    /// Asks whether to install an offered app, listing its permissions.
    Install,
    /// An app's details, and uninstalling it.
    Info,
}

impl Screen {
    /// The screen's number in the view contract.
    const fn code(self) -> i32 {
        match self {
            Screen::Home => 0,
            Screen::Apps => 1,
            Screen::Running => 2,
            Screen::Notices => 3,
            Screen::Control => 4,
            Screen::Launching => 5,
            Screen::Exited => 6,
            Screen::Install => 7,
            Screen::Info => 8,
        }
    }

    /// Panels are pulled over whatever is below and pushed back to it.
    const fn is_panel(self) -> bool {
        matches!(self, Screen::Notices | Screen::Control)
    }
}

/// One app in the launcher.
struct Entry {
    info: AppInfo,
    /// Offered in the Workspace, not installed.
    offered: bool,
}

/// The services the system UI drives.
pub struct Services {
    /// Views and gestures.
    pub ui: Rc<UiService>,
    /// The app lifecycle.
    pub apps: AppManager,
    /// The notification center's notices.
    pub notices: Rc<Notifications>,
    /// The time on the status bar.
    pub clock: Rc<UtcClock>,
    /// Network status.
    pub wifi: Rc<WifiControl>,
}

/// The system UI's state.
pub struct Shell {
    services: Services,
    session: UiSession,
    /// The screen on the display, or `None` while the foreground app shows.
    screen: Option<Screen>,
    /// What closing a panel returns to; `None` is the foreground app.
    below: Option<Screen>,
    entries: Vec<Entry>,
    running: Vec<RunningApp>,
    /// The app a launching, exited, install, or info screen is about.
    subject: Option<AppInfo>,
    /// Why the subject exited, on the exited screen.
    exit_reason: &'static str,
    /// The last exit already acted on.
    exit_seen: Option<u32>,
    /// The foreground app as last seen.
    foreground: Option<String>,
    quiet: bool,
    brightness: usize,
}

impl Shell {
    /// Opens the system view and shows the home screen.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the view is rejected.
    pub async fn new(services: Services, assets: &UiAssets) -> Result<Self, UiError> {
        let session = services.ui.open_on(Layer::System, assets)?;
        let mut shell = Self {
            services,
            session,
            screen: None,
            below: None,
            entries: Vec::new(),
            running: Vec::new(),
            subject: None,
            exit_reason: "",
            exit_seen: None,
            foreground: None,
            quiet: false,
            brightness: 0,
        };
        shell.reload().await;
        shell.reconcile().await;
        if shell.screen.is_none() && shell.foreground.is_none() {
            shell.show(Some(Screen::Home));
        }
        Ok(shell)
    }

    /// The screen on the display, or `None` while the foreground app shows.
    #[must_use]
    pub const fn screen(&self) -> Option<Screen> {
        self.screen
    }

    /// The system view.
    #[must_use]
    pub const fn session(&self) -> &UiSession {
        &self.session
    }

    /// Re-reads the app list.
    async fn reload(&mut self) {
        let list = self.services.apps.list().await;
        self.entries = list
            .apps
            .into_iter()
            .map(|info| Entry {
                info,
                offered: false,
            })
            .chain(list.installable.into_iter().map(|info| Entry {
                info,
                offered: true,
            }))
            .collect();
    }

    /// Puts `screen` on the display, or uncovers the foreground app.
    fn show(&mut self, screen: Option<Screen>) {
        let apps = &self.services.apps;
        let screen = screen.or_else(|| apps.foreground().is_none().then_some(Screen::Home));
        self.screen = screen;
        if screen.is_some() {
            // Covered apps take no input; the error only says none is up.
            let _covered = apps.set_interactive(false);
            self.render();
            self.session.show(true);
        } else {
            self.session.hide();
            let _uncovered = apps.set_interactive(true);
        }
    }

    async fn go(&mut self, screen: Screen) {
        if matches!(screen, Screen::Apps | Screen::Running) {
            self.reload().await;
            self.running = self.services.apps.running();
        }
        self.below = None;
        self.show(Some(screen));
    }

    fn open_panel(&mut self, panel: Screen) {
        if !self.screen.is_some_and(Screen::is_panel) {
            self.below = self.screen;
        }
        if panel == Screen::Notices {
            self.services.notices.mark_read();
        }
        self.show(Some(panel));
    }

    fn close_panel(&mut self) {
        let below = self.below.take();
        self.show(below);
    }

    /// Acts on a gesture on the display.
    pub async fn gesture(&mut self, gesture: Gesture) {
        let screen = self.screen;
        match gesture {
            Gesture::Swipe {
                direction: Direction::Down,
                edge: Some(Edge::Top),
                ..
            } if screen != Some(Screen::Notices) => self.open_panel(Screen::Notices),
            Gesture::Swipe {
                direction: Direction::Up,
                edge: Some(Edge::Bottom),
                ..
            } if screen != Some(Screen::Control) => self.open_panel(Screen::Control),
            Gesture::Swipe { direction, .. } => match (screen, direction) {
                (Some(Screen::Notices), Direction::Up)
                | (Some(Screen::Control), Direction::Down) => {
                    self.close_panel();
                }
                (Some(Screen::Home), Direction::Left) => self.go(Screen::Apps).await,
                (Some(Screen::Apps), Direction::Left) => self.go(Screen::Running).await,
                (Some(Screen::Apps), Direction::Right) => self.go(Screen::Home).await,
                (Some(Screen::Running), Direction::Right) => self.go(Screen::Apps).await,
                (None, Direction::Right) => {
                    // Back out of the app to the launcher it was opened from.
                    self.services.apps.home();
                    self.go(Screen::Apps).await;
                }
                (Some(Screen::Install | Screen::Info | Screen::Exited), Direction::Right) => {
                    self.cancel().await;
                }
                _ => {}
            },
            Gesture::LongPress { x, y } if screen == Some(Screen::Apps) => {
                // A long press selects what a tap there would open.
                let entry = self
                    .session
                    .resolve_press(x, y)
                    .filter(|call| call.handler == "openApp")
                    .and_then(|call| index(&call))
                    .and_then(|index| self.entries.get(index))
                    .filter(|entry| !entry.offered)
                    .map(|entry| entry.info.clone());
                if let Some(info) = entry {
                    self.subject = Some(info);
                    self.show(Some(Screen::Info));
                }
            }
            Gesture::LongPress { .. } => {}
        }
    }

    /// Acts on a press in the system view.
    pub async fn call(&mut self, call: ViewCall) {
        let selected = index(&call);
        match call.handler.as_str() {
            "openApp" => {
                let Some(entry) = selected.and_then(|index| self.entries.get(index)) else {
                    return;
                };
                let (info, offered) = (entry.info.clone(), entry.offered);
                if offered {
                    self.subject = Some(info);
                    self.show(Some(Screen::Install));
                } else {
                    self.launch(info).await;
                }
            }
            "switchTo" => {
                if let Some(app) = selected.and_then(|index| self.running.get(index)) {
                    let info = app.app.clone();
                    self.launch(info).await;
                }
            }
            "closeApp" => {
                if let Some(app) = selected.and_then(|index| self.running.get(index)) {
                    self.services.apps.terminate(&app.app.id);
                }
                self.running = self.services.apps.running();
                self.render();
            }
            "dismiss" => {
                if let Some(id) = selected.and_then(|id| u32::try_from(id).ok()) {
                    self.services.notices.dismiss(id);
                }
                self.render();
            }
            "clearAll" => {
                self.services.notices.clear();
                self.render();
            }
            "toggle" => {
                match selected.and_then(|id| i32::try_from(id).ok()) {
                    Some(TILE_QUIET) => self.quiet = !self.quiet,
                    Some(TILE_BRIGHTNESS) => {
                        self.brightness = (self.brightness + 1) % BRIGHTNESS.len();
                        self.services
                            .ui
                            .set_brightness(BRIGHTNESS[self.brightness].0);
                    }
                    _ => {}
                }
                self.render();
            }
            "alertTap" => {
                if self.offline() {
                    self.open_panel(Screen::Control);
                } else {
                    self.open_panel(Screen::Notices);
                }
            }
            "confirm" => self.confirm().await,
            "cancel" => self.cancel().await,
            _ => {}
        }
    }

    async fn confirm(&mut self) {
        let Some(subject) = self.subject.clone() else {
            return;
        };
        match self.screen {
            Some(Screen::Install) => {
                if let Err(error) = self.services.apps.install(&subject.id).await {
                    log::warn!("shell: cannot install {}: {error}", subject.id);
                    self.services
                        .notices
                        .post(&format!("无法安装“{}”", subject.name), "应用文件有问题");
                }
                self.go(Screen::Apps).await;
            }
            Some(Screen::Exited) => self.launch(subject).await,
            Some(Screen::Info) if subject.kind == AppKind::Installable => {
                if let Err(error) = self.services.apps.uninstall(&subject.id).await {
                    log::warn!("shell: cannot uninstall {}: {error}", subject.id);
                }
                self.go(Screen::Apps).await;
            }
            _ => {}
        }
    }

    async fn cancel(&mut self) {
        match self.screen {
            Some(Screen::Install) => {
                if let Some(subject) = &self.subject {
                    self.services.apps.decline_install(&subject.id);
                }
                self.go(Screen::Apps).await;
            }
            Some(Screen::Info) => self.go(Screen::Apps).await,
            _ => self.go(Screen::Home).await,
        }
    }

    /// Opens an app; its view replaces the launching screen once it runs.
    async fn launch(&mut self, info: AppInfo) {
        self.subject = Some(info.clone());
        self.show(Some(Screen::Launching));
        match self.services.apps.open(&info.id).await {
            Ok(app) => {
                self.foreground = self.services.apps.foreground().map(|app| app.app.id);
                if app.app.has_view {
                    self.show(None);
                } else {
                    // A headless app runs as a service; the launcher marks it.
                    self.go(Screen::Apps).await;
                }
            }
            Err(error) => {
                log::warn!("shell: cannot open {}: {error}", info.id);
                self.services.notices.post(
                    &format!("无法打开“{}”", info.name),
                    words::open_error(error),
                );
                if error == AppError::NotInstalled {
                    self.reload().await;
                }
                self.go(Screen::Apps).await;
            }
        }
    }

    /// Catches up with the app lifecycle: exits, install requests, and apps
    /// opened or closed by someone else, such as a Workflow.
    pub async fn reconcile(&mut self) {
        let apps = self.services.apps.clone();
        self.running = apps.running();
        if let Some(exit) = apps.last_exit()
            && self.exit_seen != Some(exit.run_id)
        {
            self.exit_seen = Some(exit.run_id);
            if exit.outcome != VmRunOutcome::Success {
                let name = self.name(&exit.id);
                if exit.foreground {
                    self.subject = self.info(&exit.id);
                    self.exit_reason = words::exit(&exit);
                    self.below = None;
                    self.show(Some(Screen::Exited));
                } else {
                    self.services
                        .notices
                        .post(&format!("“{name}”已退出"), words::exit(&exit));
                }
            }
        }
        if let Some(request) = apps.install_request()
            && !(self.screen == Some(Screen::Install)
                && self
                    .subject
                    .as_ref()
                    .is_some_and(|subject| subject.id == request.id))
        {
            self.reload().await;
            self.subject = Some(request);
            self.below = None;
            self.show(Some(Screen::Install));
        }
        let foreground = apps.foreground().map(|app| app.app.id);
        if foreground != self.foreground {
            self.foreground.clone_from(&foreground);
            match (&foreground, self.screen) {
                (
                    Some(_),
                    Some(Screen::Home | Screen::Apps | Screen::Running | Screen::Launching),
                ) => {
                    self.show(None);
                }
                (None, None) => self.show(Some(Screen::Home)),
                _ => {}
            }
        }
        self.render();
    }

    /// Redraws after a notice or the minute changed.
    pub fn refresh(&mut self) {
        self.render();
    }

    fn info(&self, id: &str) -> Option<AppInfo> {
        self.entries
            .iter()
            .map(|entry| &entry.info)
            .chain(self.running.iter().map(|app| &app.app))
            .find(|info| info.id == id)
            .cloned()
    }

    fn name(&self, id: &str) -> String {
        self.info(id)
            .map_or_else(|| String::from(id), |info| info.name)
    }

    fn offline(&self) -> bool {
        !matches!(
            self.services.wifi.status().station(),
            StationState::Connected { .. }
        )
    }

    /// Writes every value of the system view.
    fn render(&self) {
        let Some(screen) = self.screen else {
            return;
        };
        let notices = self.services.notices.list();
        let unread = self.services.notices.unread() && !self.quiet;
        let (alert, alert_detail) = if self.offline() {
            ("未连接 Wi-Fi", "Agent 需要网络")
        } else {
            match notices.first().filter(|_| unread) {
                Some(notice) => (notice.title.as_str(), "新通知"),
                None => ("", ""),
            }
        };
        let clock = self
            .services
            .clock
            .now()
            .map(|now| words::clock(u64::from(now)))
            .unwrap_or_default();
        let subject = self.subject.as_ref();
        let subject_name = subject.map_or("", |info| info.name.as_str());
        let (title, detail, lines, primary, secondary): (String, String, Vec<String>, &str, &str) =
            match screen {
                Screen::Running => (
                    String::from("运行中"),
                    String::from(if self.running.is_empty() {
                        "没有运行中的应用"
                    } else {
                        ""
                    }),
                    Vec::new(),
                    "",
                    "",
                ),
                Screen::Notices => (
                    String::from("通知"),
                    String::from(if notices.is_empty() {
                        "没有新通知"
                    } else {
                        "全部清除"
                    }),
                    Vec::new(),
                    "",
                    "",
                ),
                Screen::Launching => (
                    String::from(subject_name),
                    String::from("正在打开"),
                    Vec::new(),
                    "",
                    "",
                ),
                Screen::Exited => (
                    format!("“{subject_name}”已退出"),
                    String::from(self.exit_reason),
                    Vec::new(),
                    "重新打开",
                    "回到主页",
                ),
                Screen::Install => (
                    format!("安装“{subject_name}”？"),
                    format!(
                        "来自工作区 · 版本 {}",
                        subject.map_or("", |info| info.version.as_str())
                    ),
                    permissions(subject),
                    "安装",
                    "取消",
                ),
                Screen::Info => {
                    let system = subject.is_some_and(|info| info.kind == AppKind::System);
                    (
                        String::from(subject_name),
                        format!(
                            "版本 {} · {}",
                            subject.map_or("", |info| info.version.as_str()),
                            if system { "系统应用" } else { "已安装" }
                        ),
                        if system {
                            Vec::new()
                        } else {
                            permissions(subject)
                        },
                        if system { "" } else { "卸载" },
                        "返回",
                    )
                }
                Screen::Home | Screen::Apps | Screen::Control => {
                    (String::new(), String::new(), Vec::new(), "", "")
                }
            };
        let (connected, network) = match self.services.wifi.status().station() {
            StationState::Connected { ssid } => {
                (true, ssid.clone().unwrap_or_else(|| String::from("已连接")))
            }
            StationState::Connecting => (false, String::from("连接中")),
            StationState::Disconnected => (false, String::from("未连接")),
        };
        let running_ids: Vec<&str> = self.running.iter().map(|app| app.app.id.as_str()).collect();
        let foreground = self.foreground.as_deref();
        let values = [
            ("screen", ViewValue::I32(screen.code())),
            ("clock", ViewValue::Str(clock)),
            ("unread", ViewValue::Bool(unread)),
            ("headline", ViewValue::Str(String::from("待命"))),
            ("hint", ViewValue::Str(String::from("左滑查看应用"))),
            ("alert", ViewValue::Str(String::from(alert))),
            ("alertDetail", ViewValue::Str(String::from(alert_detail))),
            (
                "apps",
                ViewValue::List(
                    self.entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            item([
                                ViewValue::I32(index as i32),
                                ViewValue::Str(entry.info.name.clone()),
                                ViewValue::Str(words::glyph(&entry.info.name)),
                                ViewValue::Bool(
                                    !entry.offered && running_ids.contains(&entry.info.id.as_str()),
                                ),
                                ViewValue::Bool(entry.offered),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "running",
                ViewValue::List(
                    self.running
                        .iter()
                        .enumerate()
                        .map(|(index, app)| {
                            item([
                                ViewValue::I32(index as i32),
                                ViewValue::Str(app.app.name.clone()),
                                ViewValue::Str(words::glyph(&app.app.name)),
                                ViewValue::Str(String::from(words::state(app.state))),
                                ViewValue::Bool(foreground == Some(app.app.id.as_str())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "notices",
                ViewValue::List(
                    notices
                        .iter()
                        .map(|notice| {
                            item([
                                ViewValue::I32(notice.id as i32),
                                ViewValue::Str(notice.title.clone()),
                                ViewValue::Str(notice.body.clone()),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "tiles",
                ViewValue::List(vec![
                    item([
                        ViewValue::I32(TILE_WIFI),
                        ViewValue::Str(String::from("Wi-Fi")),
                        ViewValue::Str(network),
                        ViewValue::Bool(connected),
                    ]),
                    item([
                        ViewValue::I32(TILE_QUIET),
                        ViewValue::Str(String::from("勿扰")),
                        ViewValue::Str(String::from(if self.quiet { "开" } else { "关" })),
                        ViewValue::Bool(self.quiet),
                    ]),
                    item([
                        ViewValue::I32(TILE_BRIGHTNESS),
                        ViewValue::Str(String::from("亮度")),
                        ViewValue::Str(String::from(BRIGHTNESS[self.brightness].1)),
                        ViewValue::Bool(false),
                    ]),
                ]),
            ),
            ("title", ViewValue::Str(title)),
            ("glyph", ViewValue::Str(words::glyph(subject_name))),
            ("detail", ViewValue::Str(detail)),
            (
                "lines",
                ViewValue::List(
                    lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, text)| {
                            item([ViewValue::I32(index as i32), ViewValue::Str(text)])
                        })
                        .collect(),
                ),
            ),
            ("primary", ViewValue::Str(String::from(primary))),
            ("secondary", ViewValue::Str(String::from(secondary))),
        ];
        let channel = self.session.channel();
        for (name, value) in values {
            if let Err(error) = channel.set(name, value) {
                log::warn!("shell: cannot show {name}: {error:?}");
            }
        }
    }
}

/// A struct value: its fields in declaration order.
fn item<const N: usize>(fields: [ViewValue; N]) -> ViewValue {
    ViewValue::List(fields.into())
}

/// The first argument of a press, as an index.
fn index(call: &ViewCall) -> Option<usize> {
    match call.args.first() {
        Some(ViewValue::I32(index)) => usize::try_from(*index).ok(),
        _ => None,
    }
}

/// What an app asks to be allowed, in the person's words.
fn permissions(subject: Option<&AppInfo>) -> Vec<String> {
    let permissions: Vec<String> = subject
        .map(|info| {
            info.permissions
                .iter()
                .map(|package| words::permission(package))
                .collect()
        })
        .unwrap_or_default();
    if permissions.is_empty() {
        vec![String::from("不需要额外权限")]
    } else {
        permissions
    }
}
