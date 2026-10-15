//! The system UI: which screen is up, what it shows, and what gestures and
//! presses do.
//!
//! The shell owns one view on the system layer. While an app is in the
//! foreground and no system screen covers it, that view is hidden and the
//! app's view shows; every system screen covers the app and stops it from
//! taking input until the screen closes.

use crate::agent::{AgentState, AgentStatus};
use crate::notices::Notifications;
use crate::words;
use alloc::{format, rc::Rc, string::String, vec, vec::Vec};
use barracuda_app_plugin::{
    AppError, AppInfo, AppKind, AppManager, AppState, RunningApp, VmRunOutcome,
};
use barracuda_platform::{AccessPointState, StationState};
use barracuda_time_plugin::UtcClock;
use barracuda_ui_plugin::icons::{self, IconLayer};
use barracuda_ui_plugin::{
    Direction, Edge, Gesture, Layer, UiAssets, UiError, UiService, UiSession, ViewCall, ViewValue,
};
use barracuda_wifi_plugin::{SETUP_AP_ADDRESS, SETUP_AP_PASSWORD, SETUP_AP_SSID, WifiControl};
use embassy_time::{Duration, Instant};

/// The control center's tiles, by their IDs in the view.
const TILE_WIFI: usize = 0;
const TILE_MIC: usize = 1;
const TILE_MUTE: usize = 2;
const TILE_QUIET: usize = 3;
const TILE_BRIGHTNESS: usize = 4;
const TILE_PORTAL: usize = 5;
/// The control center's views: the tiles, the brightness bar, the portal.
const CONTROL_TILES: i32 = 0;
const CONTROL_BRIGHTNESS: i32 = 1;
const CONTROL_PORTAL: i32 = 2;
/// Brightness when the system starts, in percent.
const DEFAULT_BRIGHTNESS: i32 = 70;
/// The brightness bar's inner height in pixels.
const BRIGHTNESS_BAR: i32 = 158;
/// How long an answered or allowed result stays on screen.
const ANSWER_SHOWN: Duration = Duration::from_secs(9);
const RESULT_SHOWN: Duration = Duration::from_secs(30);
const DONE_SHOWN: Duration = Duration::from_millis(1600);
/// The launcher's grid: tile column width and gap, row height and gap, in
/// pixels, on the 360 px display.
const COLUMN: i32 = 78;
const COLUMN_GAP: i32 = 6;
const ROW: i32 = 86;
const ROW_GAP: i32 = 10;
const VIEWPORT: i32 = 360;

/// A system screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// The home screen: the Agent, and the one thing that most needs
    /// attention.
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
    /// The menu a long press on an app opens.
    Menu,
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
            Screen::Menu => 9,
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
    /// The notification center's notices and questions.
    pub notices: Rc<Notifications>,
    /// The Agent's state on the home screen.
    pub agent: Rc<AgentStatus>,
    /// The time on the status bar.
    pub clock: Rc<UtcClock>,
    /// Network status and the portal's access point.
    pub wifi: Rc<WifiControl>,
}

/// System settings the control center switches.
#[derive(Clone, Copy, Debug)]
struct Switches {
    microphone: bool,
    muted: bool,
    quiet: bool,
    brightness: i32,
}

/// The system UI's state.
pub struct Shell {
    services: Services,
    session: UiSession,
    /// The screen on the display, or `None` while the foreground app shows.
    screen: Option<Screen>,
    /// What closing a panel returns to; `None` is the foreground app.
    below: Option<Screen>,
    /// Which of its views the control center shows.
    control: i32,
    entries: Vec<Entry>,
    running: Vec<RunningApp>,
    /// The app a launching, exited, install, info, or menu screen is about.
    subject: Option<AppInfo>,
    /// Why the subject exited, on the exited screen.
    exit_reason: &'static str,
    /// The last exit already acted on.
    exit_seen: Option<u32>,
    /// The foreground app as last seen.
    foreground: Option<String>,
    switches: Switches,
    /// The latest answered question, shown in the notification center and
    /// on the home screen for a while.
    done: Option<(String, Instant)>,
    /// When the Agent's answer appeared.
    answered: Option<Instant>,
}

impl Shell {
    /// Opens the system view and shows the home screen.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the view is rejected.
    pub async fn new(services: Services, assets: &UiAssets) -> Result<Self, UiError> {
        services.ui.ready().await;
        let session = services.ui.open_on(Layer::System, assets)?;
        services
            .ui
            .set_brightness(percent_to_level(DEFAULT_BRIGHTNESS));
        let mut shell = Self {
            services,
            session,
            screen: None,
            below: None,
            control: CONTROL_TILES,
            entries: Vec::new(),
            running: Vec::new(),
            subject: None,
            exit_reason: "",
            exit_seen: None,
            foreground: None,
            switches: Switches {
                microphone: true,
                muted: false,
                quiet: false,
                brightness: DEFAULT_BRIGHTNESS,
            },
            done: None,
            answered: None,
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

    /// The launcher's app names, in the order `openApp` indexes them.
    #[must_use]
    pub fn launcher(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.info.name.as_str())
            .collect()
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
        self.control = CONTROL_TILES;
        self.show(Some(panel));
    }

    fn close_panel(&mut self) {
        let below = self.below.take();
        self.control = CONTROL_TILES;
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
            } if screen != Some(Screen::Notices) && screen != Some(Screen::Control) => {
                self.open_panel(Screen::Notices);
            }
            Gesture::Swipe {
                direction: Direction::Up,
                edge: Some(Edge::Bottom),
                ..
            } if screen != Some(Screen::Control) && screen != Some(Screen::Notices) => {
                self.open_panel(Screen::Control);
            }
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
                    // One page back inside the app, or out of it to the
                    // launcher it was opened from.
                    if !self.services.apps.back() {
                        self.go(Screen::Apps).await;
                    }
                }
                (
                    Some(Screen::Menu | Screen::Info | Screen::Install | Screen::Exited),
                    Direction::Right,
                ) => {
                    self.cancel().await;
                }
                _ => {}
            },
            Gesture::LongPress { x, y } => {
                let call = self.session.resolve_press(x, y);
                match (screen, call) {
                    (Some(Screen::Apps), Some(call)) if call.handler == "openApp" => {
                        let entry = index(&call)
                            .and_then(|index| self.entries.get(index))
                            .filter(|entry| !entry.offered)
                            .map(|entry| entry.info.clone());
                        if let Some(info) = entry {
                            self.subject = Some(info);
                            self.show(Some(Screen::Menu));
                        }
                    }
                    (Some(Screen::Control), Some(call))
                        if call.handler == "toggle" && index(&call) == Some(TILE_WIFI) =>
                    {
                        // The network list lives in Settings.
                        self.open_system_app("settings").await;
                    }
                    _ => {}
                }
            }
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
                self.subject = Some(info.clone());
                if offered {
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
            "allow" | "deny" => {
                let allowed = call.handler == "allow";
                let notice = selected
                    .and_then(|id| u32::try_from(id).ok())
                    .and_then(|id| {
                        self.services
                            .notices
                            .list()
                            .into_iter()
                            .find(|notice| notice.id == id)
                    });
                if let Some(notice) = notice
                    && self.services.notices.decide(notice.id, allowed)
                {
                    let verdict = if allowed { "已允许" } else { "已拒绝" };
                    self.done = Some((format!("{verdict} · {}", notice.title), Instant::now()));
                }
                self.render();
            }
            "clearAll" => {
                self.services.notices.clear();
                self.done = None;
                self.render();
            }
            "toggle" => self.toggle(selected).await,
            "setBrightness" => {
                if let Some(level) = selected.and_then(|level| i32::try_from(level).ok()) {
                    self.switches.brightness = level.clamp(10, 100);
                    self.services
                        .ui
                        .set_brightness(percent_to_level(self.switches.brightness));
                }
                self.render();
            }
            "closeControl" => {
                self.control = CONTROL_TILES;
                self.render();
            }
            "cardTap" => {
                if self.services.agent.state() == AgentState::Answering {
                    self.services.agent.set(AgentState::Standby, "", "");
                    self.render();
                } else if self.offline() {
                    self.open_panel(Screen::Control);
                } else {
                    self.open_panel(Screen::Notices);
                }
            }
            "info" => self.show(Some(Screen::Info)),
            "confirm" => self.confirm().await,
            "cancel" => self.cancel().await,
            _ => {}
        }
    }

    async fn toggle(&mut self, tile: Option<usize>) {
        match tile {
            Some(TILE_WIFI) => self.open_system_app("settings").await,
            Some(TILE_MIC) => self.switches.microphone = !self.switches.microphone,
            Some(TILE_MUTE) => self.switches.muted = !self.switches.muted,
            Some(TILE_QUIET) => self.switches.quiet = !self.switches.quiet,
            Some(TILE_BRIGHTNESS) => self.control = CONTROL_BRIGHTNESS,
            Some(TILE_PORTAL) => {
                let wifi = &self.services.wifi;
                if self.portal_open() {
                    if let Err(error) = wifi.stop_access_point().await {
                        log::warn!("shell: cannot close the portal's access point: {error}");
                    }
                } else {
                    match wifi
                        .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
                        .await
                    {
                        Ok(()) => self.control = CONTROL_PORTAL,
                        Err(error) => {
                            log::warn!("shell: cannot open the portal's access point: {error}");
                        }
                    }
                }
            }
            _ => {}
        }
        self.render();
    }

    async fn open_system_app(&mut self, id: &str) {
        let info = self
            .entries
            .iter()
            .find(|entry| entry.info.id == id && !entry.offered)
            .map(|entry| entry.info.clone());
        if let Some(info) = info {
            self.below = None;
            self.launch(info).await;
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
                    self.services.notices.post(
                        &format!("无法安装“{}”", subject.name),
                        "应用文件有问题",
                        Some("apps"),
                    );
                }
                self.go(Screen::Apps).await;
            }
            Some(Screen::Exited) => self.launch(subject).await,
            Some(Screen::Info | Screen::Menu) if subject.kind == AppKind::Installable => {
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
            Some(Screen::Info | Screen::Menu) => self.go(Screen::Apps).await,
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
                    info.icon.as_deref(),
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
                    let icon = self.info(&exit.id).and_then(|info| info.icon);
                    self.services.notices.post(
                        &format!("“{name}”已退出"),
                        words::exit(&exit),
                        icon.as_deref(),
                    );
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

    /// Redraws after a notice, the Agent, or the time changed, and lets
    /// shown results expire.
    pub fn refresh(&mut self) {
        let now = Instant::now();
        if self
            .done
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= RESULT_SHOWN)
        {
            self.done = None;
        }
        match self.services.agent.state() {
            AgentState::Answering => {
                let since = *self.answered.get_or_insert(now);
                if now.duration_since(since) >= ANSWER_SHOWN {
                    self.services.agent.set(AgentState::Standby, "", "");
                }
            }
            _ => self.answered = None,
        }
        self.render();
    }

    /// How long until something shown expires, for the caller's timer.
    #[must_use]
    pub fn next_expiry(&self) -> Option<Duration> {
        let now = Instant::now();
        let done = self.done.as_ref().map(|(_, at)| {
            let shown = if now.duration_since(*at) < DONE_SHOWN {
                DONE_SHOWN
            } else {
                RESULT_SHOWN
            };
            (*at + shown).saturating_duration_since(now)
        });
        let answer = self
            .answered
            .map(|at| (at + ANSWER_SHOWN).saturating_duration_since(now));
        match (done, answer) {
            (Some(done), Some(answer)) => Some(done.min(answer)),
            (done, answer) => done.or(answer),
        }
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

    fn portal_open(&self) -> bool {
        matches!(
            self.services.wifi.status().access_point(),
            AccessPointState::Started { .. } | AccessPointState::Starting
        )
    }

    /// Writes every value of the system view.
    fn render(&self) {
        let Some(screen) = self.screen else {
            return;
        };
        let now = Instant::now();
        let notices = self.services.notices.list();
        let quiet = self.switches.quiet;
        let unread = self.services.notices.unread() && !quiet;
        let offline = self.offline();
        let question = notices.iter().find(|notice| notice.question);
        let recent = self
            .done
            .as_ref()
            .filter(|(_, at)| now.duration_since(*at) < RESULT_SHOWN);
        // One thing on the home screen: offline, then a pending question,
        // then what was just done.
        let (card, card_label, card_glyph, card_tone) = if offline {
            ("未连接 Wi-Fi", "Agent 需要网络", glyph("wifi-off"), 2)
        } else if let Some(question) = question {
            (
                question.title.as_str(),
                "Agent 请求确认",
                glyph("shield-alert"),
                1,
            )
        } else if let Some((text, _)) = recent {
            (text.as_str(), "刚刚", glyph("check"), 0)
        } else {
            ("", "", String::new(), 0)
        };
        let done = self
            .done
            .as_ref()
            .filter(|(_, at)| now.duration_since(*at) < DONE_SHOWN)
            .map(|(text, _)| text.clone())
            .unwrap_or_default();
        let clock = self
            .services
            .clock
            .now()
            .map(|now| words::clock(u64::from(now)))
            .unwrap_or_default();
        let subject = self.subject.as_ref();
        let subject_name = subject.map_or("", |info| info.name.as_str());
        let system = subject.is_some_and(|info| info.kind == AppKind::System);
        let (title, detail, lines, primary, secondary): (
            String,
            String,
            Vec<(String, String)>,
            &str,
            &str,
        ) = match screen {
            Screen::Launching | Screen::Menu => (
                String::from(subject_name),
                String::new(),
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
                String::from("来自工作区"),
                permissions(subject),
                "安装",
                "取消",
            ),
            Screen::Info => (
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
            ),
            _ => (String::new(), String::new(), Vec::new(), "", ""),
        };
        let status = self.services.wifi.status();
        let network = match status.station() {
            StationState::Connected { ssid } => {
                ssid.clone().unwrap_or_else(|| String::from("已连接"))
            }
            StationState::Connecting => String::from("连接中"),
            StationState::Disconnected => String::from("未连接"),
        };
        let switch = |on: bool| String::from(if on { "开" } else { "关" });
        let tiles = [
            ("Wi-Fi", network, "wifi", !offline),
            (
                "麦克风",
                switch(self.switches.microphone),
                "mic",
                self.switches.microphone,
            ),
            (
                "静音",
                switch(self.switches.muted),
                "volume-x",
                self.switches.muted,
            ),
            ("勿扰", switch(quiet), "moon", quiet),
            (
                "亮度",
                format!("{}%", self.switches.brightness),
                "sun",
                false,
            ),
            (
                "门户",
                switch(self.portal_open()),
                "globe",
                self.portal_open(),
            ),
        ];
        let running_ids: Vec<&str> = self.running.iter().map(|app| app.app.id.as_str()).collect();
        let foreground = self.foreground.as_deref();
        let agent = self.services.agent.state();
        let positions = grid(self.entries.len());
        let values = [
            ("screen", ViewValue::I32(screen.code())),
            ("clock", ViewValue::Str(clock)),
            ("unread", ViewValue::Bool(unread)),
            ("quiet", ViewValue::Bool(quiet)),
            ("offline", ViewValue::Bool(offline)),
            ("battery", ViewValue::I32(-1)),
            ("batteryFill", ViewValue::I32(0)),
            (
                "agent",
                ViewValue::I32(match agent {
                    AgentState::Standby => 0,
                    AgentState::Listening => 1,
                    AgentState::Thinking => 2,
                    AgentState::Answering => 3,
                }),
            ),
            ("question", ViewValue::Str(self.services.agent.question())),
            ("answer", ViewValue::Str(self.services.agent.answer())),
            ("card", ViewValue::Str(String::from(card))),
            ("cardLabel", ViewValue::Str(String::from(card_label))),
            ("cardGlyph", ViewValue::Str(card_glyph)),
            ("cardTone", ViewValue::I32(card_tone)),
            (
                "apps",
                ViewValue::List(
                    self.entries
                        .iter()
                        .zip(positions)
                        .enumerate()
                        .map(|(index, (entry, (x, y)))| {
                            let (icon, letter) = app_icon(&entry.info);
                            item([
                                ViewValue::I32(index as i32),
                                ViewValue::Str(entry.info.name.clone()),
                                icon,
                                ViewValue::Str(letter),
                                ViewValue::Bool(
                                    !entry.offered && running_ids.contains(&entry.info.id.as_str()),
                                ),
                                ViewValue::Bool(entry.offered),
                                ViewValue::I32(x),
                                ViewValue::I32(y),
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
                            let (icon, letter) = app_icon(&app.app);
                            let live = foreground == Some(app.app.id.as_str())
                                || app.state == AppState::Running;
                            item([
                                ViewValue::I32(index as i32),
                                ViewValue::Str(app.app.name.clone()),
                                icon,
                                ViewValue::Str(letter),
                                ViewValue::Str(String::from(words::state(app.state))),
                                ViewValue::Bool(live),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("anyRunning", ViewValue::Bool(!self.running.is_empty())),
            (
                "notices",
                ViewValue::List(
                    notices
                        .iter()
                        .map(|notice| {
                            let (icon, letter) = match notice.icon.as_deref().and_then(icons::icon)
                            {
                                Some(layers) => (icon_value(layers), String::new()),
                                None => (icon_value(&[]), words::glyph(&notice.title)),
                            };
                            item([
                                ViewValue::I32(notice.id as i32),
                                ViewValue::Str(notice.title.clone()),
                                ViewValue::Str(notice.body.clone()),
                                icon,
                                ViewValue::Str(letter),
                                ViewValue::Bool(notice.question),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("anyNotice", ViewValue::Bool(!notices.is_empty())),
            ("done", ViewValue::Str(done)),
            (
                "tiles",
                ViewValue::List(
                    tiles
                        .into_iter()
                        .enumerate()
                        .map(|(id, (label, detail, icon, on))| {
                            item([
                                ViewValue::I32(id as i32),
                                ViewValue::Str(String::from(label)),
                                ViewValue::Str(detail),
                                ViewValue::Str(glyph(icon)),
                                ViewValue::Bool(on),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("control", ViewValue::I32(self.control)),
            ("brightness", ViewValue::I32(self.switches.brightness)),
            (
                "brightnessFill",
                ViewValue::I32(self.switches.brightness * BRIGHTNESS_BAR / 100),
            ),
            (
                "portal",
                ViewValue::Str(format!(
                    "连接热点“{SETUP_AP_SSID}”，打开 http://{SETUP_AP_ADDRESS}/"
                )),
            ),
            ("title", ViewValue::Str(title)),
            ("detail", ViewValue::Str(detail)),
            (
                "icon",
                subject.map_or_else(|| icon_value(&[]), |info| app_icon(info).0),
            ),
            (
                "letter",
                ViewValue::Str(subject.map(|info| app_icon(info).1).unwrap_or_default()),
            ),
            (
                "lines",
                ViewValue::List(
                    lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, (text, glyph))| {
                            item([
                                ViewValue::I32(index as i32),
                                ViewValue::Str(text),
                                ViewValue::Str(glyph),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("primary", ViewValue::Str(String::from(primary))),
            ("secondary", ViewValue::Str(String::from(secondary))),
            ("removable", ViewValue::Bool(!system)),
        ];
        let channel = self.session.channel();
        for (name, value) in values {
            if let Err(error) = channel.set(name, value) {
                log::warn!("shell: cannot show {name}: {error:?}");
            }
        }
    }
}

/// Positions of `count` launcher tiles in alternating rows of two and
/// three, centered on the display.
fn grid(count: usize) -> Vec<(i32, i32)> {
    let mut rows = Vec::new();
    let mut left = count;
    let mut wide = false;
    while left > 0 {
        let width = if wide { 3 } else { 2 }.min(left);
        rows.push(width);
        left -= width;
        wide = !wide;
    }
    let height = rows.len() as i32 * ROW + (rows.len() as i32 - 1).max(0) * ROW_GAP;
    let top = (VIEWPORT - height) / 2 + 4;
    let mut positions = Vec::with_capacity(count);
    for (row, width) in rows.into_iter().enumerate() {
        let span = width as i32 * COLUMN + (width as i32 - 1) * COLUMN_GAP;
        let start = (VIEWPORT - span) / 2;
        for column in 0..width as i32 {
            positions.push((
                start + column * (COLUMN + COLUMN_GAP),
                top + row as i32 * (ROW + ROW_GAP),
            ));
        }
    }
    positions
}

/// The brightness level for a percentage.
fn percent_to_level(percent: i32) -> u8 {
    u8::try_from(percent.clamp(0, 100) * 255 / 100).unwrap_or(u8::MAX)
}

/// A struct value: its fields in declaration order.
fn item<const N: usize>(fields: [ViewValue; N]) -> ViewValue {
    ViewValue::List(fields.into())
}

/// An icon's layers as the view's `Icon`: base, accent, dim.
fn icon_value(layers: &[IconLayer]) -> ViewValue {
    let layer = |colors: &[&str]| {
        layers
            .iter()
            .find(|layer| colors.contains(&layer.color))
            .map_or_else(String::new, |layer| String::from(layer.glyph))
    };
    item([
        ViewValue::Str(layer(&["#F4F4F5", "#FAFAFA"])),
        ViewValue::Str(layer(&["#CCFF00"])),
        ViewValue::Str(layer(&["#52525B"])),
    ])
}

/// An app's icon, or the first letter of its name when it picked none.
fn app_icon(info: &AppInfo) -> (ViewValue, String) {
    match info.icon.as_deref().and_then(icons::icon) {
        Some(layers) => (icon_value(layers), String::new()),
        None => (icon_value(&[]), words::glyph(&info.name)),
    }
}

/// A one-layer icon's glyph.
fn glyph(name: &str) -> String {
    icons::icon(name)
        .and_then(|layers| layers.last())
        .map_or_else(String::new, |layer| String::from(layer.glyph))
}

/// The first argument of a press, as an index.
fn index(call: &ViewCall) -> Option<usize> {
    match call.args.first() {
        Some(ViewValue::I32(index)) => usize::try_from(*index).ok(),
        _ => None,
    }
}

/// What an app asks to be allowed, in the person's words, with its icons.
fn permissions(subject: Option<&AppInfo>) -> Vec<(String, String)> {
    let permissions: Vec<(String, String)> = subject
        .map(|info| {
            info.permissions
                .iter()
                .map(|package| {
                    (
                        words::permission(package),
                        glyph(words::permission_icon(package)),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if permissions.is_empty() {
        vec![(String::from("不需要额外权限"), glyph("check"))]
    } else {
        permissions
    }
}
