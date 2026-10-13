//! Drives the system UI the way a person does, with gestures and presses on
//! the system view, over a real VM, App Manager, and Wi-Fi Plugin.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]
#![recursion_limit = "256"]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_app_plugin::{AppManager, AppState};
use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_platform::HostWifiDevice;
use barracuda_platform_test::{memory_partition, never_embassy_stack};
use barracuda_plugin::api::{Hardware, PlatformInfo, PlatformWifi, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_shell_plugin::{Notifications, Screen, Services, Shell};
use barracuda_time_plugin::{SyncSample, TimeConfig, utc_clock};
use barracuda_ui_plugin::{
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
            include_bytes!(concat!(
                "../../../../vm-app/crates/plugin/tests/fixtures/counter/",
                $name
            ))
            .as_slice(),
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
        fonts: [view_file!("font-12.bin"), view_file!("font-2.bin")]
            .iter()
            .map(|font| font.to_vec())
            .collect(),
    }
}

async fn put(fs: &ScopedVfs, path: &str, bytes: &[u8]) {
    fs.write(path, bytes).await.unwrap();
}

async fn app_filesystem() -> ScopedVfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app").unwrap();
    // A system app.
    put(
        &fs,
        "/resources/apps/system/app.json",
        br#"{"format":1,"id":"system","name":"System","version":"2"}"#,
    )
    .await;
    put(&fs, "/resources/apps/system/main.lua", b"").await;
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
    fs
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

fn texts(shell: &Shell) -> String {
    shell.session().texts().join("|")
}

/// Where the launcher shows app `name`: the point a press selects it at, and
/// its index.
fn icon(shell: &Shell, name: &str) -> ((f32, f32), i32) {
    let texts = shell.session().texts();
    for y in (0..360).step_by(4) {
        for x in (0..360).step_by(4) {
            let (x, y) = (x as f32, y as f32);
            if let Some(call) = shell.session().resolve_press(x, y)
                && call.handler == "openApp"
                && let Some(ViewValue::I32(index)) = call.args.first()
            {
                // Icons show the glyph, then the name, in index order.
                let names: Vec<&String> = texts.iter().skip(1).skip(1).step_by(2).collect();
                if names
                    .get(*index as usize)
                    .is_some_and(|shown| *shown == name)
                {
                    return ((x, y), *index);
                }
            }
        }
    }
    panic!("no icon for {name} in {texts:?}")
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
    let (apps, _registration) =
        AppManager::register(app_filesystem().await, vm, Rc::clone(&ui), &packages).unwrap();
    spawner.spawn(supervise(apps.clone()).unwrap());
    let (clock, updater) = utc_clock(TimeConfig::new(1_000, 60_000, 3_600_000));
    // 2026-10-13 10:42:00 UTC.
    updater.synchronize(SyncSample::new(1_791_888_120_000, Instant::now()));
    let notices = Rc::new(Notifications::new());
    let mut shell = Shell::new(
        Services {
            ui: Rc::clone(&ui),
            apps: apps.clone(),
            notices: Rc::clone(&notices),
            clock,
            wifi: wifi().await,
        },
        &system_view(),
    )
    .await
    .unwrap();

    // Home: the system layer is on the display and takes touches.
    assert_eq!(shell.screen(), Some(Screen::Home));
    assert_eq!(ui.showing(), Some(true));
    let home = texts(&shell);
    assert!(home.contains("10:42") && home.contains("待命"), "{home}");

    // Left to the launcher: the system app and the offered apps.
    shell.gesture(swipe(Direction::Left, None)).await;
    assert_eq!(shell.screen(), Some(Screen::Apps));
    let launcher = texts(&shell);
    for name in ["Crash", "System", "计数器", "Lamp"] {
        assert!(launcher.contains(name), "{launcher}");
    }

    // A long press shows a system app's info, which offers no uninstall.
    let (at, _) = icon(&shell, "System");
    shell.gesture(Gesture::LongPress { x: at.0, y: at.1 }).await;
    assert_eq!(shell.screen(), Some(Screen::Info));
    let info = texts(&shell);
    assert!(
        info.contains("版本 2 · 系统应用") && !info.contains("卸载"),
        "{info}"
    );
    shell.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(shell.screen(), Some(Screen::Apps));

    // Tapping an offered app asks to install it, with its permissions.
    let (_, lamp) = icon(&shell, "Lamp");
    shell.call(press("openApp", Some(lamp))).await;
    assert_eq!(shell.screen(), Some(Screen::Install));
    let install = texts(&shell);
    assert!(
        install.contains("安装“Lamp”？")
            && install.contains("控制灯带")
            && install.contains("联网收发数据"),
        "{install}"
    );
    shell.call(press("cancel", None)).await;
    assert_eq!(shell.screen(), Some(Screen::Apps));

    // Installing the counter, then opening it: the app's view shows.
    let (_, counter) = icon(&shell, "计数器");
    shell.call(press("openApp", Some(counter))).await;
    assert!(texts(&shell).contains("不需要额外权限"));
    shell.call(press("confirm", None)).await;
    assert_eq!(shell.screen(), Some(Screen::Apps));
    let (_, counter) = icon(&shell, "计数器");
    shell.call(press("openApp", Some(counter))).await;
    assert_eq!(shell.screen(), None);
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Active));
    until("the counter's view", || {
        apps.texts()
            .is_ok_and(|texts| texts.first().map(String::as_str) == Some("0"))
    })
    .await;

    // Panels cover the app and stop its input until they close.
    shell.gesture(swipe(Direction::Down, Some(Edge::Top))).await;
    assert_eq!(shell.screen(), Some(Screen::Notices));
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Inactive));
    assert!(texts(&shell).contains("没有新通知"));
    shell.gesture(swipe(Direction::Up, None)).await;
    assert_eq!(shell.screen(), None);
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Active));
    shell
        .gesture(swipe(Direction::Up, Some(Edge::Bottom)))
        .await;
    assert_eq!(shell.screen(), Some(Screen::Control));
    shell.call(press("toggle", Some(1))).await;
    let control = texts(&shell);
    assert!(
        control.contains("勿扰|开") && control.contains("Wi-Fi|已连接"),
        "{control}"
    );
    // Do not disturb hides the unread mark; turn it off again.
    notices.post("静音时", "");
    shell.refresh();
    assert!(!texts(&shell).contains("静音时"));
    shell.call(press("toggle", Some(1))).await;
    notices.clear();
    shell.gesture(swipe(Direction::Down, None)).await;
    assert_eq!(shell.screen(), None);

    // Right out of the app: it moves to the background, the launcher shows.
    shell.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(shell.screen(), Some(Screen::Apps));
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Background));

    // The switcher lists it and closes it.
    shell.gesture(swipe(Direction::Left, None)).await;
    assert_eq!(shell.screen(), Some(Screen::Running));
    assert!(texts(&shell).contains("计数器|后台"), "{}", texts(&shell));
    shell.call(press("closeApp", Some(0))).await;
    until("the counter to exit", || {
        state_of(&apps, "counter").is_none()
    })
    .await;
    shell.gesture(swipe(Direction::Right, None)).await;

    // An app failing on the display gets the exited screen.
    let (_, crash) = icon(&shell, "Crash");
    shell.call(press("openApp", Some(crash))).await;
    until("the crash", || state_of(&apps, "crash").is_none()).await;
    shell.reconcile().await;
    assert_eq!(shell.screen(), Some(Screen::Exited));
    assert!(texts(&shell).contains("“Crash”已退出"), "{}", texts(&shell));
    shell.call(press("cancel", None)).await;
    assert_eq!(shell.screen(), Some(Screen::Home));

    // A notice marks home; opening the center reads it.
    let id = notices.post("门铃", "有人在门口");
    shell.refresh();
    assert!(texts(&shell).contains("门铃"));
    shell.gesture(swipe(Direction::Down, Some(Edge::Top))).await;
    assert!(!notices.unread());
    shell
        .call(press("dismiss", Some(i32::try_from(id).unwrap())))
        .await;
    assert!(notices.list().is_empty());
    shell.gesture(swipe(Direction::Up, None)).await;
    assert_eq!(shell.screen(), Some(Screen::Home));

    // Install requests and apps opened by Workflows move the screens too.
    apps.request_install("lamp").await.unwrap();
    shell.reconcile().await;
    assert_eq!(shell.screen(), Some(Screen::Install));
    shell.call(press("cancel", None)).await;
    assert_eq!(apps.install_request(), None);
    apps.open("counter").await.unwrap();
    shell.reconcile().await;
    assert_eq!(shell.screen(), None);
    apps.home();
    shell.reconcile().await;
    assert_eq!(shell.screen(), Some(Screen::Home));

    // Uninstalling an installable app from its info screen.
    shell.gesture(swipe(Direction::Left, None)).await;
    let (at, _) = icon(&shell, "计数器");
    shell.gesture(Gesture::LongPress { x: at.0, y: at.1 }).await;
    assert_eq!(shell.screen(), Some(Screen::Info));
    assert!(texts(&shell).contains("卸载"));
    shell.call(press("confirm", None)).await;
    assert_eq!(shell.screen(), Some(Screen::Apps));
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
