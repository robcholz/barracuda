//! Drives the system UI the way a person does, with gestures and presses on
//! the system view, over a real VM and App Manager.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]
#![recursion_limit = "256"]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_app_manager_plugin::{
    AgentState, AgentStatus, HotspotFuture, NetworkSource, NetworkState, Notifications,
    Preferences, Screen, Services, SystemNetwork, SystemUi,
};
use barracuda_app_manager_plugin::{AppManager, AppState, BuiltinApp, Device, WIFI_SETTINGS};
use barracuda_platform_test::{memory_partition, never_embassy_stack};
use barracuda_plugin::api::{Hardware, PlatformInfo, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_power_plugin::{Power, PowerPlugin};
use barracuda_sound_plugin::{SoundPlugin, Volume};
use barracuda_time_plugin::{SyncSample, TimeConfig, utc_clock};
use barracuda_uikit::{
    DEFAULT_VIEWPORT, Direction, DisplayInfo, Edge, Gesture, UiAssets, UiService, ViewCall,
    ViewValue,
};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{
    Lua, LuaPackage, LuaPackageRegistry, Package, PackageInterface, SeedSource, Vm, VmPlugin,
};
use barracuda_workflow_plugin::WorkflowPlugin;
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
            include_bytes!(concat!("../../appkit/tests/fixtures/counter.bapp/", $name)).as_slice(),
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
    // An app, one that asks for capabilities, and one this device lacks a
    // capability for.
    for (name, bytes) in COUNTER {
        put(
            &fs,
            &format!("/workspace/media/apps/counter.bapp/{name}"),
            bytes,
        )
        .await;
    }
    put(
        &fs,
        "/workspace/media/apps/lamp.bapp/app.json",
        br#"{"format":1,"id":"lamp","name":"Lamp","version":"1","requires":["light"],"uses":["http"]}"#,
    )
    .await;
    put(&fs, "/workspace/media/apps/lamp.bapp/main.lua", b"").await;
    put(
        &fs,
        "/workspace/media/apps/photo.bapp/app.json",
        br#"{"format":1,"id":"photo","name":"Photo","version":"1","requires":["camera"]}"#,
    )
    .await;
    put(&fs, "/workspace/media/apps/photo.bapp/main.lua", b"").await;
    // More apps than one launcher page holds.
    for id in ["x1", "x2", "x3"] {
        put(
            &fs,
            &format!("/workspace/media/apps/{id}.bapp/app.json"),
            format!(
                r#"{{"format":1,"id":"{id}","name":"{}","version":"1"}}"#,
                id.to_uppercase()
            )
            .as_bytes(),
        )
        .await;
        put(
            &fs,
            &format!("/workspace/media/apps/{id}.bapp/main.lua"),
            b"",
        )
        .await;
    }
    // An app that fails once it is on the display.
    for (name, bytes) in COUNTER
        .iter()
        .filter(|(name, _)| name.ends_with(".bin") || *name == "view.bvir")
    {
        put(
            &fs,
            &format!("/workspace/media/apps/crash.bapp/{name}"),
            bytes,
        )
        .await;
    }
    put(
        &fs,
        "/workspace/media/apps/crash.bapp/app.json",
        br#"{"format":1,"id":"crash","name":"Crash","version":"1",
            "view":{"program":"view.bvir","styles":"styles.bin","fonts":["font-1.bin"]}}"#,
    )
    .await;
    put(
        &fs,
        "/workspace/media/apps/crash.bapp/main.lua",
        b"local app = require('app') for event in app.events() do \
          if event == 'app:active' then error('crashed') end end",
    )
    .await;
    (fs, builtin)
}

/// The capabilities the system UI takes from other Plugins.
type Taken = (Rc<Power>, Rc<Volume>);

/// A network that is always online, with a setup hotspot that opens and
/// closes.
#[derive(Default)]
struct Online {
    hotspot: std::cell::Cell<bool>,
}

impl NetworkSource for Online {
    fn state(&self) -> NetworkState {
        NetworkState::Connected {
            name: Some(String::from("HomeNet")),
        }
    }

    fn hotspot_open(&self) -> bool {
        self.hotspot.get()
    }

    fn hotspot_hint(&self) -> String {
        String::from("1. Join Wi-Fi \"Setup\"\n2. Open http://192.168.4.1")
    }

    fn open_hotspot(&self) -> HotspotFuture<'_> {
        self.hotspot.set(true);
        Box::pin(async { Ok(()) })
    }

    fn close_hotspot(&self) -> HotspotFuture<'_> {
        self.hotspot.set(false);
        Box::pin(async { Ok(()) })
    }
}

/// Takes the capabilities from the real Plugins on a host Platform.
struct Observer(Rc<RefCell<Option<Taken>>>);

impl PluginDeclaration for Observer {
    const ID: &'static str = "observer";
    const DEPENDS_ON: &'static [&'static str] = &["power", "sound"];
}

impl Plugin for Observer {
    fn register<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        self.0.replace(Some((
            context.require::<Power>("power")?,
            context.require::<Volume>("sound")?,
        )));
        Ok(())
    }
}

async fn taken() -> Taken {
    let stack = never_embassy_stack();
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack);
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
    manager.register(WorkflowPlugin::new(&mut context)).unwrap();
    manager.register(VmPlugin::new(&mut context)).unwrap();
    manager.register(PowerPlugin::new(&mut context)).unwrap();
    manager.register(SoundPlugin::new(&mut context)).unwrap();
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

/// The `light` capability, without a light behind it.
struct Light;

impl Package for Light {
    fn install(&self, lua: &mut Lua) -> barracuda_vm_plugin::Result<()> {
        lua.register_lib("light", |_package| Ok(()))
    }
}

impl LuaPackage for Light {
    fn name(&self) -> &'static str {
        "light"
    }

    fn interface(&self) -> Option<PackageInterface> {
        Some(PackageInterface::Stable {
            version: 1,
            purpose: "Control lights",
        })
    }
}

async fn scenario(spawner: Spawner) {
    let packages = LuaPackageRegistry::new();
    let _light = packages.register(Light).unwrap();
    let ui = Rc::new(UiService::new(DEFAULT_VIEWPORT));
    let vm = Rc::new(Vm::new(packages.clone(), SeedSource::unavailable()).unwrap());
    vm.start(spawner).unwrap();
    let (filesystem, builtin) = app_filesystem().await;
    let (apps, _registration) = AppManager::register(
        filesystem,
        vm,
        Some(Rc::clone(&ui)),
        Device {
            board: "test-board",
            display: Some(DisplayInfo {
                width: 360,
                height: 360,
                round: true,
            }),
            touch: true,
        },
        &packages,
    )
    .unwrap();
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
    let (power, audio) = taken().await;
    let network = Rc::new(SystemNetwork::default());
    // The host Board has no fuel gauge: the control center shows no battery.
    assert_eq!(power.battery(), None);
    let mut system = SystemUi::new(
        Services {
            ui: Rc::clone(&ui),
            apps: apps.clone(),
            notices: Rc::clone(&notices),
            agent: Rc::clone(&agent),
            preferences: Rc::new(Preferences::default()),
            clock,
            network: Rc::clone(&network),
            power,
            audio,
        },
        &system_view(),
    )
    .await
    .unwrap();

    // Home: the system layer is on the display and takes touches.
    assert_eq!(system.screen(), Some(Screen::Home));
    assert_eq!(ui.showing(), Some(true));
    let home = texts(&system);
    assert!(home.contains("10:42") && home.contains("Standby"), "{home}");

    // Left to the launcher: the system app, then the installable apps, and
    // last the one this device cannot run.
    system.gesture(swipe(Direction::Left, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    let launcher = texts(&system);
    for name in ["Crash", "System", "Counter", "Lamp", "X3"] {
        assert!(launcher.contains(name), "{launcher}");
    }
    // The round display's page holds 2-3-2 tiles; the eighth app is on the
    // next page, an upward swipe away.
    assert!(!launcher.contains("Photo"), "{launcher}");
    system.gesture(swipe(Direction::Up, None)).await;
    let second = texts(&system);
    assert!(
        second.contains("Photo") && !second.contains("X3"),
        "{second}"
    );
    let (_, photo) = icon(&system, "Photo");
    assert_eq!(photo, 7);
    // An app that cannot run here says why instead of opening.
    system.call(press("openApp", Some(photo))).await;
    assert_eq!(system.screen(), Some(Screen::Info));
    let info = texts(&system);
    assert!(
        info.contains("Can't run\nThis device has no camera") && info.contains("Uninstall"),
        "{info}"
    );
    system.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    system.gesture(swipe(Direction::Up, None)).await;
    assert!(texts(&system).contains("Photo"), "the last page stays");
    system.gesture(swipe(Direction::Down, None)).await;
    assert!(texts(&system).contains("X3"));

    // A long press opens a system app's menu; its info offers no
    // uninstall.
    let (at, _) = icon(&system, "System");
    system
        .gesture(Gesture::LongPress { x: at.0, y: at.1 })
        .await;
    assert_eq!(system.screen(), Some(Screen::Menu));
    assert!(!texts(&system).contains("Uninstall"), "{}", texts(&system));
    system.call(press("info", None)).await;
    assert_eq!(system.screen(), Some(Screen::Info));
    let info = texts(&system);
    assert!(
        info.contains("Version 2 · System app") && !info.contains("Uninstall"),
        "{info}"
    );
    system.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));

    // An installable app's info lists what it asks for.
    let (at, _) = icon(&system, "Lamp");
    system
        .gesture(Gesture::LongPress { x: at.0, y: at.1 })
        .await;
    system.call(press("info", None)).await;
    assert_eq!(system.screen(), Some(Screen::Info));
    let info = texts(&system);
    assert!(
        info.contains("Version 1 · Installed")
            && info.contains("Control lights")
            && info.contains("Use http|Optional")
            && info.contains("Uninstall"),
        "{info}"
    );
    system.gesture(swipe(Direction::Right, None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));

    // Tapping the counter opens it: the app's view shows.
    let (_, counter) = icon(&system, "Counter");
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
    assert!(texts(&system).contains("No new notifications"));
    system.gesture(swipe(Direction::Up, None)).await;
    assert_eq!(system.screen(), None);
    assert_eq!(state_of(&apps, "counter"), Some(AppState::Active));
    system
        .gesture(swipe(Direction::Up, Some(Edge::Bottom)))
        .await;
    assert_eq!(system.screen(), Some(Screen::Control));
    // A device that shows no network has no Wi-Fi or portal tile.
    let control = texts(&system);
    assert!(
        !control.contains("Wi-Fi") && !control.contains("Portal") && control.contains("Quiet"),
        "{control}"
    );
    network.show(Rc::new(Online::default()));
    system.refresh();
    let control = texts(&system);
    assert!(
        control.contains("Wi-Fi|HomeNet") && control.contains("Portal|Off"),
        "{control}"
    );
    // The portal tile opens the source's hotspot and shows how to reach it.
    system.call(press("toggle", Some(5))).await;
    let control = texts(&system);
    assert!(
        control.contains("1. Join Wi-Fi \"Setup\"\n2. Open http://192.168.4.1"),
        "{control}"
    );
    system.call(press("closeControl", None)).await;
    system
        .gesture(swipe(Direction::Up, Some(Edge::Bottom)))
        .await;
    assert!(texts(&system).contains("Portal|On"), "{}", texts(&system));
    system.call(press("toggle", Some(5))).await;
    assert!(texts(&system).contains("Portal|Off"), "{}", texts(&system));
    system.call(press("toggle", Some(3))).await;
    let control = texts(&system);
    assert!(control.contains("Quiet|On"), "{control}");
    // Quiet hides the unread mark; turn it off again.
    notices.post("While quiet", "", None);
    system.refresh();
    system.call(press("toggle", Some(3))).await;
    assert!(texts(&system).contains("Quiet|Off"));
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
    assert!(
        texts(&system).contains("Counter|Background"),
        "{}",
        texts(&system)
    );
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
        texts(&system).contains("\"Crash\" quit"),
        "{}",
        texts(&system)
    );
    system.call(press("cancel", None)).await;
    assert_eq!(system.screen(), Some(Screen::Home));

    // The Agent's states show on home.
    agent.set(
        AgentState::Answering,
        "When is tomorrow's meeting?",
        "9:30 product review",
    );
    system.refresh();
    let home = texts(&system);
    assert!(
        home.contains("When is tomorrow's meeting?") && home.contains("9:30 product review"),
        "{home}"
    );
    agent.set(AgentState::Standby, "", "");

    // A question shows on home and is answered from its card.
    let asked = notices.ask("Turn on the living room light strip?", "Lin Xia · Telegram");
    system.refresh();
    assert!(
        texts(&system).contains("Agent needs approval"),
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
    let id = notices.post("Doorbell", "Someone is at the door", None);
    system.refresh();
    assert!(texts(&system).contains("Doorbell"));
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

    // Apps opened by Workflows move the screens too.
    apps.open("counter").await.unwrap();
    system.reconcile().await;
    assert_eq!(system.screen(), None);
    apps.home();
    system.reconcile().await;
    assert_eq!(system.screen(), Some(Screen::Home));

    // Uninstalling an installable app from its menu.
    system.gesture(swipe(Direction::Left, None)).await;
    let (at, _) = icon(&system, "Counter");
    system
        .gesture(Gesture::LongPress { x: at.0, y: at.1 })
        .await;
    assert_eq!(system.screen(), Some(Screen::Menu));
    assert!(texts(&system).contains("Uninstall"));
    system.call(press("confirm", None)).await;
    assert_eq!(system.screen(), Some(Screen::Apps));
    until("the counter to end", || {
        state_of(&apps, "counter").is_none()
    })
    .await;
    assert!(!apps.list().await.apps.iter().any(|app| app.id == "counter"));
    assert!(!system.launcher().contains(&"Counter"));
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
