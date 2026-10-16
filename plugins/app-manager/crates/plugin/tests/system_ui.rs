//! Drives the system UI the way a person does, with gestures and presses on
//! the system view, over a real VM, App Manager, and Wi-Fi Plugin.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]
#![recursion_limit = "256"]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_app_manager_plugin::{
    AgentState, AgentStatus, Notifications, Preferences, Screen, Services, SystemUi,
};
use barracuda_app_manager_plugin::{AppManager, AppState, BuiltinApp, WIFI_SETTINGS};
use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_platform::HostWifiDevice;
use barracuda_platform_test::{memory_partition, never_embassy_stack};
use barracuda_plugin::api::{Hardware, PlatformInfo, PlatformWifi, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_time_plugin::{SyncSample, TimeConfig, utc_clock};
use barracuda_uikit::{
    DEFAULT_VIEWPORT, Direction, Edge, Gesture, UiAssets, UiService, ViewCall, ViewValue,
};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{LuaPackageRegistry, SeedSource, Vm};
use barracuda_webserver_plugin::WebServerPlugin;
use barracuda_wifi_plugin::{WifiControl, WifiPlugin};
use embassy_executor::{Executor, Spawner};
use embassy_time::{Instant, Timer};

macro_rules! view_file {
    ($name:literal) => {
        include_bytes!(concat!("../../../filesystem/resources/view/", $name)).as_slice()
    };
}

macro_rules! counter_file {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!("../../appkit/tests/fixtures/counter/", $name)).as_slice(),
        )
    };
}

const COUNTER: &[(&str, &[u8])] = &[
    counter_file!("app.json"),
    counter_file!("main.lua"),
    counter_file!("view.bvir"),
    counter_file!("styles.bin"),
    counter_file!("font-1.bin"),
    counter_file!("font-2.bin"),
    counter_file!("font-12.bin"),
];

fn system_view() -> UiAssets {
    UiAssets {
        view: view_file!("view.bvir").to_vec(),
        styles: view_file!("styles.bin").to_vec(),
        fonts: Vec::new(),
    }
}

async fn put(fs: &ScopedVfs, path: &str, bytes: &[u8]) {
    fs.write(path, bytes).await.unwrap();
}

/// The app manager's filesystem, and a builtin app Plugin's.
async fn app_filesystem() -> (ScopedVfs, ScopedVfs) {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app").unwrap();
    // A builtin app.
    let builtin = vfs.scoped("/builtin").unwrap();
    put(
        &builtin,
        "/resources/app/app.json",
        br#"{"format":1,"id":"system","name":"System","version":"2"}"#,
    )
    .await;
    put(&builtin, "/resources/app/main.lua", b"").await;
    // An offered app, and one that asks for packages.
    for (name, bytes) in COUNTER {
        put(&fs, &format!("/workspace/media/apps/counter/{name}"), bytes).await;
    }
    put(
        &fs,
        "/workspace/media/apps/lamp/app.json",
        br#"{"format":1,"id":"lamp","name":"Lamp","version":"1","permissions":["led_strip","http"]}"#,
    )
    .await;
    put(&fs, "/workspace/media/apps/lamp/main.lua", b"").await;
    // An installed app that fails once it is on the display.
    for (name, bytes) in COUNTER
        .iter()
        .filter(|(name, _)| name.ends_with(".bin") || *name == "view.bvir")
    {
        put(&fs, &format!("/media/apps/crash/{name}"), bytes).await;
    }
    put(
        &fs,
        "/media/apps/crash/app.json",
        br#"{"format":1,"id":"crash","name":"Crash","version":"1",
            "view":{"program":"view.bvir","styles":"styles.bin","fonts":["font-1.bin"]}}"#,
    )
    .await;
    put(
        &fs,
        "/media/apps/crash/main.lua",
        b"local app = require('app') for event in app.events() do \
          if event == 'app:active' then error('crashed') end end",
    )
    .await;
    (fs, builtin)
}

/// Takes the Wi-Fi capability from the real Plugin on a host Platform.
struct Observer(Rc<RefCell<Option<Rc<WifiControl>>>>);

impl PluginDeclaration for Observer {
    const ID: &'static str = "observer";
    const DEPENDS_ON: &'static [&'static str] = &["wifi"];
}

impl Plugin for Observer {
    fn register<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        self.0
            .replace(Some(context.require::<WifiControl>(Self::DEPENDS_ON[0])?));
        Ok(())
    }
}

async fn wifi() -> Rc<WifiControl> {
    let stack = never_embassy_stack();
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack);
    context.wifi = Some(PlatformWifi::new(HostWifiDevice::new(stack)));
    let filesystem = Vfs::new();
    filesystem
        .mount(
            "/resources",
            MemFs::new().into_backend(),
            MountOptions::read_only(),
        )
        .await
        .unwrap();
    filesystem
        .mount(
            "/data",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .unwrap();
    let partition = memory_partition(64 * 1024).await.unwrap();
    let mut manager = PluginManager::open(partition).await.unwrap();
    manager.install_vfs(filesystem);
    manager
        .register(WebServerPlugin::new(&mut context))
        .unwrap();
    manager
        .register(CaptivePortalPlugin::new(&mut context))
        .unwrap();
    manager.register(WifiPlugin::new(&mut context)).unwrap();
    let observed = Rc::new(RefCell::new(None));
    manager.register(Observer(Rc::clone(&observed))).unwrap();
    // The manager owns the Plugins' registrations; keep it for the test.
    std::mem::forget(manager);
    observed.borrow_mut().take().unwrap()
}

fn swipe(direction: Direction, edge: Option<Edge>) -> Gesture {
    Gesture::Swipe {
        direction,
        edge,
        x: 180.0,
        y: 180.0,
    }
}

fn press(handler: &str, index: Option<i32>) -> ViewCall {
    ViewCall {
        handler: handler.into(),
        args: index.map(ViewValue::I32).into_iter().collect(),
    }
}

fn texts(system: &SystemUi) -> String {
    system.session().texts().join("|")
}

/// Where the launcher shows app `name`: the point a press selects it at, and
/// its index.
fn icon(system: &SystemUi, name: &str) -> ((f32, f32), i32) {
    let index = system
        .launcher()
        .iter()
        .position(|shown| *shown == name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", system.launcher()));
    let index = i32::try_from(index).unwrap();
    // Let the launcher finish sliding in.
    system.session().advance(60);
    for y in (0..360).step_by(4) {
        for x in (0..360).step_by(4) {
            let (x, y) = (x as f32, y as f32);
            if let Some(call) = system.session().resolve_press(x, y)
                && call.handler == "openApp"
                && call.args.first() == Some(&ViewValue::I32(index))
            {
                return ((x, y), index);
            }
        }
    }
    panic!("no icon for {name} on the launcher")
}

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        Timer::after_millis(10).await;
    }
    panic!("timed out waiting for {what}");
}

fn state_of(apps: &AppManager, id: &str) -> Option<AppState> {
    apps.running()
        .into_iter()
        .find(|app| app.app.id == id)
        .map(|app| app.state)
}

async fn scenario(spawner: Spawner) {
    let packages = LuaPackageRegistry::new();
    let ui = Rc::new(UiService::new(DEFAULT_VIEWPORT));
    let vm = Rc::new(Vm::new(packages.clone(), SeedSource::unavailable()).unwrap());
    vm.start(spawner).unwrap();
    let (filesystem, builtin) = app_filesystem().await;
    let (apps, _registration) =
        AppManager::register(filesystem, vm, Some(Rc::clone(&ui)), &packages).unwrap();
    apps.add_builtin(BuiltinApp {
        id: "system",
        filesystem: builtin,
        opens: &[WIFI_SETTINGS],
        package: None,
    })
    .unwrap();
    spawner.spawn(supervise(apps.clone()).unwrap());
    let (clock, updater) = utc_clock(TimeConfig::new(1_000, 60_000, 3_600_000));
    // 2026-10-13 10:42:00 UTC.
    updater.synchronize(SyncSample::new(1_791_888_120_000, Instant::now()));
    let notices = Rc::new(Notifications::new());
    let agent = Rc::new(AgentStatus::new());
    let mut system = SystemUi::new(
        Services {
            ui: Rc::clone(&ui),
            apps: apps.clone(),
            notices: Rc::clone(&notices),
            agent: Rc::clone(&agent),
            preferences: Rc::new(Preferences::default()),
            clock,
            wifi: wifi().await,
        },
        &system_view(),
    )
    .await
    .unwrap();

    // Home: the system layer is on the display and takes touches.
    assert_eq!(system.screen(), Some(Screen::Home));
    assert_eq!(ui.showing(), Some(true));
    let home = texts(&system);
    assert!(home.contains("10:42") && home.contains("待命"), "{home}");

    // Left to the launcher: the system app and the offered apps.
    system.gesture(swipe(Direction::Left, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    let launcher = texts(&system);
    for name in ["Crash", "System", "计数器", "Lamp"] {
        assert!(launcher.contains(name), "{launcher}");
    }

    // A long press opens a system app's menu; its info offers no
    // uninstall.
    let (at, _) = icon(&system, "System");
    system
        .gesture(Gesture::LongPress { x: at.0, y: at.1 })
        .await;
    assert_eq!(system.screen(), Some(Screen::Menu));
    assert!(!texts(&system).contains("卸载"), "{}", texts(&system));
    system.call(press("info", None)).await;
    assert_eq!(system.screen(), Some(Screen::Info));
    let info = texts(&system);
    assert!(
        info.contains("版本 2 · 系统应用") && !info.contains("卸载"),
        "{info}"
    );
    system.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));

    // Tapping an offered app asks to install it, with its permissions.
    let (_, lamp) = icon(&system, "Lamp");
    system.call(press("openApp", Some(lamp))).await;
    assert_eq!(system.screen(), Some(Screen::Install));
    let install = texts(&system);
    assert!(
        install.contains("安装“Lamp”？")
            && install.contains("控制灯带")
            && install.contains("联网收发数据"),
        "{install}"
    );
    system.call(press("cancel", None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));

    // Installing the counter, then opening it: the app's view shows.
    let (_, counter) = icon(&system, "计数器");
    system.call(press("openApp", Some(counter))).await;
    assert!(texts(&system).contains("不需要额外权限"));
    system.call(press("confirm", None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    let (_, counter) = icon(&system, "计数器");
    system.call(press("openApp", Some(counter))).await;
    assert_eq!(system.screen(), None);
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Active));
    until("the counter's view", || {
        apps.texts()
            .is_ok_and(|texts| texts.first().map(String::as_str) == Some("0"))
    })
    .await;

    // Panels cover the app and stop its input until they close.
    system
        .gesture(swipe(Direction::Down, Some(Edge::Top)))
        .await;
    assert_eq!(system.screen(), Some(Screen::Notices));
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Inactive));
    assert!(texts(&system).contains("没有新通知"));
    system.gesture(swipe(Direction::Up, None)).await;
    assert_eq!(system.screen(), None);
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Active));
    system
        .gesture(swipe(Direction::Up, Some(Edge::Bottom)))
        .await;
    assert_eq!(system.screen(), Some(Screen::Control));
    system.call(press("toggle", Some(3))).await;
    let control = texts(&system);
    assert!(control.contains("勿扰|开"), "{control}");
    // Do not disturb hides the unread mark; turn it off again.
    notices.post("静音时", "", None);
    system.refresh();
    system.call(press("toggle", Some(3))).await;
    assert!(texts(&system).contains("勿扰|关"));
    // The brightness tile opens its slider, which sets the panel.
    system.call(press("toggle", Some(4))).await;
    system.call(press("setBrightness", Some(40))).await;
    assert!(texts(&system).contains("40%"), "{}", texts(&system));
    system.call(press("closeControl", None)).await;
    notices.clear();
    system.gesture(swipe(Direction::Down, None)).await;
    assert_eq!(system.screen(), None);

    // Right out of the app: it moves to the background, the launcher shows.
    system.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Background));

    // The switcher lists it and closes it.
    system.gesture(swipe(Direction::Left, None)).await;
    assert_eq!(system.screen(), Some(Screen::Running));
    assert!(texts(&system).contains("计数器|后台"), "{}", texts(&system));
    system.call(press("closeApp", Some(0))).await;
    until("the counter to exit", || {
        state_of(&apps, "counter").is_none()
    })
    .await;
    system.gesture(swipe(Direction::Right, None)).await;

    // An app failing on the display gets the exited screen.
    let (_, crash) = icon(&system, "Crash");
    system.call(press("openApp", Some(crash))).await;
    until("the crash", || state_of(&apps, "crash").is_none()).await;
    system.reconcile().await;
    assert_eq!(system.screen(), Some(Screen::Exited));
    assert!(
        texts(&system).contains("“Crash”已退出"),
        "{}",
        texts(&system)
    );
    system.call(press("cancel", None)).await;
    assert_eq!(system.screen(), Some(Screen::Home));

    // The Agent's states show on home.
    agent.set(AgentState::Answering, "明天几点开会？", "9:30 产品评审");
    system.refresh();
    let home = texts(&system);
    assert!(
        home.contains("明天几点开会？") && home.contains("9:30 产品评审"),
        "{home}"
    );
    agent.set(AgentState::Standby, "", "");

    // A question shows on home and is answered from its card.
    let asked = notices.ask("打开客厅灯带？", "林夏 · Telegram");
    system.refresh();
    assert!(
        texts(&system).contains("Agent 请求确认"),
        "{}",
        texts(&system)
    );
    system
        .gesture(swipe(Direction::Down, Some(Edge::Top)))
        .await;
    system
        .call(press("allow", Some(i32::try_from(asked).unwrap())))
        .await;
    assert_eq!(notices.answer(asked).await, Some(true));
    system.gesture(swipe(Direction::Up, None)).await;

    // A notice marks home; opening the center reads it.
    let id = notices.post("门铃", "有人在门口", None);
    system.refresh();
    assert!(texts(&system).contains("门铃"));
    system
        .gesture(swipe(Direction::Down, Some(Edge::Top)))
        .await;
    assert!(!notices.unread());
    system
        .call(press("dismiss", Some(i32::try_from(id).unwrap())))
        .await;
    assert!(notices.list().is_empty());
    system.gesture(swipe(Direction::Up, None)).await;
    assert_eq!(system.screen(), Some(Screen::Home));

    // Install requests and apps opened by Workflows move the screens too.
    apps.request_install("lamp").await.unwrap();
    system.reconcile().await;
    assert_eq!(system.screen(), Some(Screen::Install));
    system.call(press("cancel", None)).await;
    assert_eq!(apps.install_request(), None);
    apps.open("counter").await.unwrap();
    system.reconcile().await;
    assert_eq!(system.screen(), None);
    apps.home();
    system.reconcile().await;
    assert_eq!(system.screen(), Some(Screen::Home));

    // Uninstalling an installable app from its menu.
    system.gesture(swipe(Direction::Left, None)).await;
    let (at, _) = icon(&system, "计数器");
    system
        .gesture(Gesture::LongPress { x: at.0, y: at.1 })
        .await;
    assert_eq!(system.screen(), Some(Screen::Menu));
    assert!(texts(&system).contains("卸载"));
    system.call(press("confirm", None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    until("the counter to end", || {
        state_of(&apps, "counter").is_none()
    })
    .await;
    assert!(
        apps.list()
            .await
            .installable
            .iter()
            .any(|app| app.id == "counter")
    );
}

#[embassy_executor::task]
async fn supervise(apps: AppManager) {
    apps.supervise().await;
}

#[embassy_executor::task]
async fn run(spawner: Spawner, done: SyncSender<()>) {
    scenario(spawner).await;
    done.send(()).unwrap();
}

#[test]
fn the_system_ui_drives_apps_through_their_lifecycle() {
    let (done, finished) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| spawner.spawn(run(spawner, done).unwrap()));
    });
    finished
        .recv_timeout(Duration::from_secs(120))
        .expect("the scenario finished");
}
