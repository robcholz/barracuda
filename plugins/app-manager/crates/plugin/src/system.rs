//! The system UI: which screen is up, what it shows, and what gestures and
//! presses do.
//!
//! The system UI owns one view on the system layer. While an app is in the
//! foreground and no system screen covers it, that view is hidden and the
//! app's view shows; every system screen covers the app and stops it from
//! taking input until the screen closes.

use crate::agent::{AgentState, AgentStatus};
use crate::notices::Notifications;
use crate::preferences::Preferences;
use crate::words;
use alloc::{format, rc::Rc, string::String, vec, vec::Vec};
use barracuda_appkit::{
    AppError, AppInfo, AppKind, AppManager, AppState, CAPABILITIES, RunningApp, VmRunOutcome,
};
use barracuda_audio_plugin::AudioOutput;
use barracuda_platform::{AccessPointState, StationState};
use barracuda_power_monitor_plugin::PowerStatus;
use barracuda_time_plugin::UtcClock;
use barracuda_uikit::icons::{self, IconLayer};
use barracuda_uikit::{
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
/// What the Wi-Fi tile opens a builtin app for.
pub const WIFI_SETTINGS: &str = "wifi";

const CONTROL_TILES: i32 = 0;
const CONTROL_BRIGHTNESS: i32 = 1;
const CONTROL_PORTAL: i32 = 2;
/// Brightness when the system starts, in percent.
/// The battery gauge's inner width in pixels.
const BATTERY_GAUGE: i32 = 19;
/// The brightness bar's inner height in pixels.
const BRIGHTNESS_BAR: i32 = 158;
/// How long an answered or allowed result stays on screen.
const ANSWER_SHOWN: Duration = Duration::from_secs(9);
const RESULT_SHOWN: Duration = Duration::from_secs(30);
const DONE_SHOWN: Duration = Duration::from_millis(1600);
/// The launcher's grid: tile column width and gap, row height and gap, in
/// pixels.
const COLUMN: i32 = 78;
const COLUMN_GAP: i32 = 6;
const ROW: i32 = 86;
const ROW_GAP: i32 = 10;

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
    /// The settings the control center switches.
    pub preferences: Rc<Preferences>,
    /// The time on the status bar.
    pub clock: Rc<UtcClock>,
    /// Network status and the portal's access point.
    pub wifi: Rc<WifiControl>,
    /// The battery's charge on the control center.
    pub power: Rc<PowerStatus>,
    /// The speaker's volume and mute.
    pub audio: Rc<AudioOutput>,
}

/// The system UI's state.
pub struct SystemUi {
    services: Services,
    session: UiSession,
    /// The screen on the display, or `None` while the foreground app shows.
    screen: Option<Screen>,
    /// What closing a panel returns to; `None` is the foreground app.
    below: Option<Screen>,
    /// Which of its views the control center shows.
    control: i32,
    entries: Vec<Entry>,
    /// The launcher page on the display.
    page: usize,
    running: Vec<RunningApp>,
    /// The app a launching, exited, install, info, or menu screen is about.
    subject: Option<AppInfo>,
    /// Why the subject exited, on the exited screen.
    exit_reason: &'static str,
    /// The last exit already acted on.
    exit_seen: Option<u32>,
    /// The foreground app as last seen.
    foreground: Option<String>,
    /// The latest answered question, shown in the notification center and
    /// on the home screen for a while.
    done: Option<(String, Instant)>,
    /// When the Agent's answer appeared.
    answered: Option<Instant>,
}

impl SystemUi {
    /// Opens the system view and shows the home screen.
    ///
    /// # Errors
    ///
    /// Returns [`UiError`] when the view is rejected.
    pub async fn new(services: Services, assets: &UiAssets) -> Result<Self, UiError> {
        services.ui.ready().await;
        let session = services.ui.open_on(Layer::System, assets)?;
        let mut system = Self {
            services,
            session,
            screen: None,
            below: None,
            control: CONTROL_TILES,
            entries: Vec::new(),
            page: 0,
            running: Vec::new(),
            subject: None,
            exit_reason: "",
            exit_seen: None,
            foreground: None,
            done: None,
            answered: None,
        };
        system.apply_preferences();
        system.reload().await;
        system.reconcile().await;
        if system.screen.is_none() && system.foreground.is_none() {
            system.show(Some(Screen::Home));
        }
        Ok(system)
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

    /// Applies changed preferences to the display and redraws.
    pub fn preferences_changed(&mut self) {
        self.apply_preferences();
        self.render();
    }

    /// Puts the brightness and screen timeout on the display, and the
    /// volume and mute on the speaker.
    fn apply_preferences(&self) {
        let values = self.services.preferences.get();
        self.services.audio.set(values.volume, values.muted);
        let ui = &self.services.ui;
        ui.set_brightness(percent_to_level(i32::from(values.brightness)));
        ui.set_screen_timeout(
            (values.screen_off != 0).then(|| Duration::from_secs(u64::from(values.screen_off))),
        );
    }

    /// The launcher's app names, in the order `openApp` indexes them.
    #[must_use]
    pub fn launcher(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.info.name.as_str())
            .collect()
    }

    /// The display's size in view pixels, and whether it is round.
    fn display(&self) -> (i32, i32, bool) {
        let (width, height) = self.services.ui.viewport();
        let round = self
            .services
            .apps
            .device()
            .display
            .is_some_and(|display| display.round);
        (width as i32, height as i32, round)
    }

    /// The launcher's page count; an empty launcher still has one page.
    fn pages(&self) -> usize {
        let (width, height, round) = self.display();
        let page = page_rows(width, height, round).iter().sum::<usize>();
        self.entries.len().div_ceil(page).max(1)
    }

    /// Moves the launcher `by` pages, staying within the first and last.
    fn turn_page(&mut self, by: isize) {
        let page = self.page.saturating_add_signed(by).min(self.pages() - 1);
        if page != self.page {
            self.page = page;
            self.render();
        }
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
        // Builtin apps first, then installed ones, then offers.
        self.entries
            .sort_by_key(|entry| match (entry.info.kind, entry.offered) {
                (AppKind::System, _) => 0,
                (_, false) => 1,
                (_, true) => 2,
            });
        self.page = self.page.min(self.pages() - 1);
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
            if self.screen == Some(Screen::Home) {
                // The launcher opens on its first page from home.
                self.page = 0;
            }
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
                (Some(Screen::Apps), Direction::Up) => self.turn_page(1),
                (Some(Screen::Apps), Direction::Down) => self.turn_page(-1),
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
                        self.open_for(WIFI_SETTINGS).await;
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
                    // Asking checks and pins what the person is shown, so
                    // the install takes exactly that.
                    let apps = &self.services.apps;
                    let subject = match apps.request_install(&info.id).await {
                        Ok(requested) => requested,
                        Err(AppError::Incompatible) => {
                            apps.install_offer(&info.id).await.unwrap_or(info)
                        }
                        Err(error) => AppInfo {
                            incompatible: Some(String::from(words::open_error(error))),
                            ..info
                        },
                    };
                    self.subject = Some(subject);
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
                if let Some(level) = selected.and_then(|level| u8::try_from(level.min(100)).ok()) {
                    self.services
                        .preferences
                        .update(|values| values.brightness = level);
                    self.apply_preferences();
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
            Some(TILE_WIFI) => self.open_for(WIFI_SETTINGS).await,
            Some(TILE_MIC) => self
                .services
                .preferences
                .update(|values| values.microphone = !values.microphone),
            Some(TILE_MUTE) => self
                .services
                .preferences
                .update(|values| values.muted = !values.muted),
            Some(TILE_QUIET) => self
                .services
                .preferences
                .update(|values| values.quiet = !values.quiet),
            Some(TILE_BRIGHTNESS) => self.control = CONTROL_BRIGHTNESS,
            Some(TILE_PORTAL) => {
                let wifi = &self.services.wifi;
                if self.portal_open() {
                    if let Err(error) = wifi.stop_access_point().await {
                        log::warn!("system ui: cannot close the portal's access point: {error}");
                    }
                } else {
                    match wifi
                        .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
                        .await
                    {
                        Ok(()) => self.control = CONTROL_PORTAL,
                        Err(error) => {
                            log::warn!("system ui: cannot open the portal's access point: {error}");
                        }
                    }
                }
            }
            _ => {}
        }
        self.render();
    }

    /// Opens the builtin app that opens for `what`, if one does.
    async fn open_for(&mut self, what: &str) {
        let Some(id) = self.services.apps.opening(what) else {
            return;
        };
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
            // An app that cannot run here is never installed.
            Some(Screen::Install) if subject.incompatible.is_some() => {}
            Some(Screen::Install) => {
                if let Err(error) = self.services.apps.install(&subject.id).await {
                    log::warn!("system ui: cannot install {}: {error}", subject.id);
                    self.services.notices.post(
                        &format!("无法安装“{}”", subject.name),
                        words::open_error(error),
                        Some("apps"),
                    );
                }
                self.go(Screen::Apps).await;
            }
            Some(Screen::Exited) => self.launch(subject).await,
            Some(Screen::Info | Screen::Menu) if subject.kind == AppKind::Installable => {
                if let Err(error) = self.services.apps.uninstall(&subject.id).await {
                    log::warn!("system ui: cannot uninstall {}: {error}", subject.id);
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
                log::warn!("system ui: cannot open {}: {error}", info.id);
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

    /// What an app asks to be allowed, in the words of the Plugins that
    /// provide it, with its icons; what it uses only when the device has it
    /// is marked optional.
    fn permissions(&self, subject: Option<&AppInfo>) -> Vec<(String, String)> {
        let Some(info) = subject else {
            return Vec::new();
        };
        let permissions: Vec<(String, String)> = info
            .requires
            .iter()
            .chain(&info.uses)
            .chain(&info.permissions)
            .map(|package| {
                // A capability this device lacks goes by its name in the
                // app API; any other package by its own.
                let purpose = self.services.apps.purpose(package).map_or_else(
                    || {
                        CAPABILITIES
                            .iter()
                            .find(|(name, _, _)| name == package)
                            .map_or_else(
                                || format!("使用 {package}"),
                                |(_, _, noun)| {
                                    let gap = if noun.is_ascii() { " " } else { "" };
                                    format!("使用{gap}{noun}")
                                },
                            )
                    },
                    String::from,
                );
                let text = if info.uses.contains(package) {
                    format!("{purpose}（可选）")
                } else {
                    purpose
                };
                (text, glyph(words::permission_icon(package)))
            })
            .collect();
        if permissions.is_empty() {
            vec![(String::from("不需要额外权限"), glyph("check"))]
        } else {
            permissions
        }
    }

    /// Writes every value of the system view.
    fn render(&self) {
        let Some(screen) = self.screen else {
            return;
        };
        let now = Instant::now();
        let notices = self.services.notices.list();
        let preferences = self.services.preferences.get();
        let quiet = preferences.quiet;
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
            Screen::Install => match subject.and_then(|info| info.incompatible.as_ref()) {
                Some(reason) => (
                    format!("无法安装“{subject_name}”"),
                    reason.clone(),
                    self.permissions(subject),
                    "安装",
                    "返回",
                ),
                None => (
                    format!("安装“{subject_name}”？"),
                    match subject.and_then(|info| info.signer.as_deref()) {
                        Some(signer) => {
                            format!("来自工作区 · 已签名 {}", signer.get(..8).unwrap_or(signer))
                        }
                        None => String::from("来自工作区 · 未签名"),
                    },
                    self.permissions(subject),
                    "安装",
                    "取消",
                ),
            },
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
                    self.permissions(subject)
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
                switch(preferences.microphone),
                "mic",
                preferences.microphone,
            ),
            (
                "静音",
                switch(preferences.muted),
                "volume-x",
                preferences.muted,
            ),
            ("勿扰", switch(quiet), "moon", quiet),
            ("亮度", format!("{}%", preferences.brightness), "sun", false),
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
        let battery = self.services.power.battery();
        let (width, height, round) = self.display();
        let rows = page_rows(width, height, round);
        let page = rows.iter().sum::<usize>();
        let first = self.page * page;
        let shown = &self.entries[first.min(self.entries.len())..];
        let shown = &shown[..shown.len().min(page)];
        let positions = grid(shown.len(), &rows, width, height);
        let values = [
            ("screen", ViewValue::I32(screen.code())),
            ("clock", ViewValue::Str(clock)),
            ("unread", ViewValue::Bool(unread)),
            ("quiet", ViewValue::Bool(quiet)),
            ("offline", ViewValue::Bool(offline)),
            (
                "battery",
                ViewValue::I32(battery.map_or(-1, |battery| i32::from(battery.percent))),
            ),
            (
                "batteryFill",
                ViewValue::I32(battery.map_or(0, |battery| {
                    i32::from(battery.percent) * BATTERY_GAUGE / 100
                })),
            ),
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
                    shown
                        .iter()
                        .zip(positions)
                        .enumerate()
                        .map(|(index, (entry, (x, y)))| {
                            let (icon, letter) = app_icon(&entry.info);
                            item([
                                // The index into the whole launcher, which
                                // `openApp` answers with.
                                ViewValue::I32((first + index) as i32),
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
                "dots",
                ViewValue::List(if self.pages() > 1 {
                    (0..self.pages())
                        .map(|page| {
                            item([
                                ViewValue::I32(page as i32),
                                ViewValue::Bool(page == self.page),
                            ])
                        })
                        .collect()
                } else {
                    Vec::new()
                }),
            ),
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
            (
                "brightness",
                ViewValue::I32(i32::from(preferences.brightness)),
            ),
            (
                "brightnessFill",
                ViewValue::I32(i32::from(preferences.brightness) * BRIGHTNESS_BAR / 100),
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
            (
                "blocked",
                ViewValue::Bool(
                    screen == Screen::Install
                        && subject.is_some_and(|info| info.incompatible.is_some()),
                ),
            ),
        ];
        let channel = self.session.channel();
        for (name, value) in values {
            if let Err(error) = channel.set(name, value) {
                log::warn!("system ui: cannot show {name}: {error:?}");
            }
        }
    }
}

/// The tiles in each row of a full launcher page on a `width` by `height`
/// display: honeycomb rows that alternate one tile narrower and wider, as
/// many as fit. A round display shows only its inscribed circle, so its
/// rows keep to the middle.
fn page_rows(width: i32, height: i32, round: bool) -> Vec<usize> {
    let (span, tall) = if round {
        (width * 3 / 4, height * 4 / 5)
    } else {
        // Room for the status bar and the screen's edges.
        (width - 16, height - 40)
    };
    let wide = usize::try_from((span + COLUMN_GAP) / (COLUMN + COLUMN_GAP))
        .unwrap_or(0)
        .max(1);
    let narrow = wide.saturating_sub(1).max(1);
    let rows = usize::try_from((tall + ROW_GAP) / (ROW + ROW_GAP))
        .unwrap_or(0)
        .max(1);
    (0..rows)
        .map(|row| if row % 2 == 0 { narrow } else { wide })
        .collect()
}

/// Positions of `count` launcher tiles filling `page`'s rows in order,
/// centered on a `width` by `height` display.
fn grid(count: usize, page: &[usize], width: i32, height: i32) -> Vec<(i32, i32)> {
    let mut rows = Vec::new();
    let mut left = count;
    for &tiles in page {
        if left == 0 {
            break;
        }
        let tiles = tiles.min(left);
        rows.push(tiles);
        left -= tiles;
    }
    let tall = rows.len() as i32 * ROW + (rows.len() as i32 - 1).max(0) * ROW_GAP;
    let top = (height - tall) / 2 + 4;
    let mut positions = Vec::with_capacity(count);
    for (row, tiles) in rows.into_iter().enumerate() {
        let span = tiles as i32 * COLUMN + (tiles as i32 - 1) * COLUMN_GAP;
        let start = (width - span) / 2;
        for column in 0..tiles as i32 {
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

#[cfg(test)]
mod tests {
    use super::{grid, page_rows};

    #[test]
    fn launcher_pages_fit_the_display() {
        // The design's round panel: the 2-3-2 honeycomb.
        assert_eq!(page_rows(360, 360, true), [2, 3, 2]);
        // Smaller and rectangular panels hold fewer, wider rows.
        assert_eq!(page_rows(240, 240, true), [1, 2]);
        assert_eq!(page_rows(320, 240, false), [2, 3]);
        assert_eq!(page_rows(135, 240, false), [1, 1]);
        // Every tile of a full page lies on the display.
        for (width, height, round) in [(360, 360, true), (320, 240, false), (720, 1280, false)] {
            let rows = page_rows(width, height, round);
            let count = rows.iter().sum();
            for (x, y) in grid(count, &rows, width, height) {
                assert!(
                    x >= 0 && x + super::COLUMN <= width,
                    "{width}x{height}: x {x}"
                );
                assert!(
                    y >= 0 && y + super::ROW <= height,
                    "{width}x{height}: y {y}"
                );
            }
        }
    }
}
