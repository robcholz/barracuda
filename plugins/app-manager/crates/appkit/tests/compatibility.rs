//! Checks installable apps against the device: the `app` package's version,
//! the Board, the display, and the packages they ask for, as the registered
//! packages describe themselves; then runs one with exactly the packages it
//! was granted.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]

use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_appkit::{AppError, AppManager, Device};
use barracuda_uikit::{DEFAULT_VIEWPORT, DisplayInfo, UiService};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{
    Lua, LuaPackage, LuaPackageRegistry, Package, PackageInterface, SeedSource, Vm,
};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;

/// A package that promises what it is given.
struct Fake(&'static str, Option<PackageInterface>);

impl Package for Fake {
    fn install(&self, lua: &mut Lua) -> barracuda_vm_plugin::Result<()> {
        lua.register_lib(self.0, |_package| Ok(()))
    }
}

impl LuaPackage for Fake {
    fn name(&self) -> &'static str {
        self.0
    }

    fn interface(&self) -> Option<PackageInterface> {
        self.1
    }
}

const BOARD: &str = "test-board";

/// Prints what the program may require, then what `app.device()` says.
const PROBE: &str = r#"
local app = require("app")
local function can(name) return tostring((pcall(require, name))) end
print(can("time"), can("light"), can("camera"), can("pins"), can("secret"))
local device = app.device()
local capabilities = {}
for name in pairs(device.capabilities) do capabilities[#capabilities + 1] = name end
table.sort(capabilities)
print(device.api, device.board, device.display.width, device.display.height,
  device.display.round, device.touch, table.concat(capabilities, ","))
"#;

async fn put(fs: &ScopedVfs, path: &str, bytes: &[u8]) {
    fs.write(path, bytes).await.unwrap();
}

/// An installable app with `fields` added to its manifest; one with a
/// `view` gets placeholder view files.
async fn offer(fs: &ScopedVfs, id: &str, fields: &str) {
    let dir = format!("/workspace/media/apps/{id}.bapp");
    put(
        fs,
        &format!("{dir}/app.json"),
        format!(r#"{{"format":1,"id":"{id}","name":"{id}","version":"1"{fields}}}"#).as_bytes(),
    )
    .await;
    put(fs, &format!("{dir}/main.lua"), PROBE.as_bytes()).await;
    put(fs, &format!("{dir}/view.bvir"), b"").await;
    put(fs, &format!("{dir}/styles.bin"), b"").await;
}

const VIEW: &str = r#""program":"view.bvir","styles":"styles.bin""#;

async fn filesystem() -> ScopedVfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app").unwrap();
    // Fits: a stable package it requires, one it uses, and a Board's
    // hardware on its Board.
    offer(
        &fs,
        "lamp",
        r#","requires":["light"],"uses":["camera"],"permissions":["pins"],"boards":["test-board"]"#,
    )
    .await;
    offer(&fs, "plain", "").await;
    offer(
        &fs,
        "round",
        &format!(r#","view":{{{VIEW},"size":[360,360],"shape":"round"}}"#),
    )
    .await;
    // A smaller view lays out at the display's size.
    offer(
        &fs,
        "small",
        &format!(r#","view":{{{VIEW},"size":[240,240],"shape":"round"}}"#),
    )
    .await;
    // Does not fit, and says why.
    offer(&fs, "later", r#","api":2,"requires":["teleport"]"#).await;
    offer(&fs, "dim", r#","requires":["light@2"]"#).await;
    offer(
        &fs,
        "elsewhere",
        r#","boards":["other-board","third-board"]"#,
    )
    .await;
    offer(
        &fs,
        "square",
        &format!(r#","view":{{{VIEW},"size":[240,240],"shape":"rect"}}"#),
    )
    .await;
    offer(
        &fs,
        "large",
        &format!(r#","view":{{{VIEW},"size":[480,480],"shape":"round"}}"#),
    )
    .await;
    offer(&fs, "photo", r#","requires":["camera"]"#).await;
    offer(&fs, "serial", r#","permissions":["serial"]"#).await;
    // Breaks the app format.
    offer(&fs, "loose", r#","permissions":["pins"]"#).await;
    offer(&fs, "mislabeled", r#","permissions":["light"]"#).await;
    offer(
        &fs,
        "pinned",
        r#","requires":["pins"],"boards":["test-board"]"#,
    )
    .await;
    offer(&fs, "versioned", r#","requires":["light@soon"]"#).await;
    // Names a package this device lacks, which only a device that has it
    // can check.
    offer(&fs, "unknown", r#","requires":["teleport"]"#).await;
    offer(&fs, "core", r#","uses":["time"]"#).await;
    offer(&fs, "twice", r#","requires":["light"],"uses":["light"]"#).await;
    fs
}

async fn scenario(spawner: Spawner) {
    let packages = LuaPackageRegistry::new();
    let _registrations = [
        Fake("time", Some(PackageInterface::Ambient { version: 1 })),
        Fake(
            "light",
            Some(PackageInterface::Stable {
                version: 1,
                purpose: "控制灯光",
            }),
        ),
        Fake(
            "pins",
            Some(PackageInterface::BoardSpecific {
                purpose: "控制引脚",
            }),
        ),
        Fake("secret", None),
    ]
    .map(|package| packages.register(package).unwrap());
    let ui = Rc::new(UiService::new(DEFAULT_VIEWPORT));
    let vm = Rc::new(Vm::new(packages.clone(), SeedSource::unavailable()).unwrap());
    vm.start(spawner).unwrap();
    let device = Device {
        board: BOARD,
        display: Some(DisplayInfo {
            width: 360,
            height: 360,
            round: true,
        }),
        touch: true,
    };
    let (manager, _registration) =
        AppManager::register(filesystem().await, vm, Some(ui), device, &packages).unwrap();
    spawner.spawn(supervise(manager.clone()).unwrap());

    assert_eq!(manager.purpose("light"), Some("控制灯光"));
    assert_eq!(manager.purpose("pins"), Some("控制引脚"));
    assert_eq!(manager.purpose("secret"), None);
    assert_eq!(manager.purpose("camera"), None);

    let list = manager.list().await;
    let offered: Vec<_> = list
        .apps
        .iter()
        .map(|app| (app.id.as_str(), app.incompatible.as_deref()))
        .collect();
    assert_eq!(
        offered,
        [
            (
                "dim",
                Some("需要更新系统（应用需要 light 第 2 版，本机是第 1 版）")
            ),
            ("elsewhere", Some("只适用于 other-board、third-board")),
            ("lamp", None),
            ("large", Some("屏幕太小（应用需要 480×480）")),
            ("later", Some("需要更新系统（应用需要 API 2，本机是 1）")),
            ("photo", Some("此设备没有 camera")),
            ("plain", None),
            ("round", None),
            ("serial", Some("此设备不支持 serial")),
            ("small", None),
            ("square", Some("应用需要方屏")),
            ("unknown", Some("此设备没有 teleport")),
        ]
    );
    let invalid: Vec<_> = list
        .invalid
        .iter()
        .map(|app| (app.dir.as_str(), app.problem.as_str()))
        .collect();
    assert_eq!(invalid.len(), 6, "{invalid:#?}");
    for ((dir, problem), (expected_dir, expected)) in invalid.iter().zip([
        ("core.bapp", "\"time\" is a package every app has"),
        ("loose.bapp", "\"pins\" talks to a Board's hardware"),
        ("mislabeled.bapp", "\"light\" is a stable package"),
        ("pinned.bapp", "\"pins\" is not a stable package"),
        ("twice.bapp", "\"light\" is listed twice"),
        ("versioned.bapp", "\"light@soon\" is not a package name"),
    ]) {
        assert_eq!(*dir, expected_dir);
        assert!(problem.contains(expected), "{dir}: {problem}");
    }

    // Incompatible apps do not open.
    for id in ["later", "dim", "elsewhere", "square", "photo", "serial"] {
        assert_eq!(
            manager.open(id).await.err(),
            Some(AppError::Incompatible),
            "{id}"
        );
    }

    // A run may require the ambient packages and what its manifest asked for.
    manager.open("lamp").await.unwrap();
    for _ in 0..500 {
        if manager.running().is_empty() {
            break;
        }
        Timer::after_millis(10).await;
    }
    let exit = manager.last_exit().unwrap();
    assert_eq!(
        exit.output,
        [
            "true\ttrue\tfalse\ttrue\tfalse",
            "1\ttest-board\t360\t360\ttrue\ttrue\tlight",
        ],
        "{exit:?}"
    );
}

#[embassy_executor::task]
async fn supervise(manager: AppManager) {
    manager.supervise().await;
}

#[embassy_executor::task]
async fn run(spawner: Spawner, done: SyncSender<()>) {
    scenario(spawner).await;
    done.send(()).unwrap();
}

#[test]
fn apps_are_checked_against_the_device() {
    let (done, finished) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| spawner.spawn(run(spawner, done).unwrap()));
    });
    finished
        .recv_timeout(Duration::from_secs(60))
        .expect("the scenario finished");
}
