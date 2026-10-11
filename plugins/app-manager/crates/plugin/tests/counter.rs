//! Launches the bundled counter app on a real VM and drives its view.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]

use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_app_manager_plugin::{AppError, AppLocation, AppManager};
use barracuda_ui_plugin::{DEFAULT_VIEWPORT, UiService};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{LuaPackageRegistry, Vm, VmExecutionError, VmRunOutcome};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;

const COUNTER: &[(&str, &[u8])] = &[
    (
        "app.json",
        include_bytes!("../../../filesystem/resources/apps/counter/app.json"),
    ),
    (
        "main.lua",
        include_bytes!("../../../filesystem/resources/apps/counter/main.lua"),
    ),
    (
        "view.bvir",
        include_bytes!("../../../filesystem/resources/apps/counter/view.bvir"),
    ),
    (
        "styles.bin",
        include_bytes!("../../../filesystem/resources/apps/counter/styles.bin"),
    ),
    (
        "font-1.bin",
        include_bytes!("../../../filesystem/resources/apps/counter/font-1.bin"),
    ),
    (
        "font-2.bin",
        include_bytes!("../../../filesystem/resources/apps/counter/font-2.bin"),
    ),
    (
        "font-12.bin",
        include_bytes!("../../../filesystem/resources/apps/counter/font-12.bin"),
    ),
];

async fn filesystem() -> ScopedVfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let filesystem = vfs.scoped("/app-manager").unwrap();
    for (name, bytes) in COUNTER {
        filesystem
            .write(&format!("/resources/apps/counter/{name}"), bytes)
            .await
            .unwrap();
    }
    filesystem
        .write(
            "/media/apps/broken/app.json",
            br#"{"id":"broken","name":"Broken","version":"1"}"#,
        )
        .await
        .unwrap();
    filesystem
        .write(
            "/media/apps/broken/main.lua",
            b"print('starting')\nerror('boom: ' .. APP.id .. ' ' .. tostring(APP.ui))",
        )
        .await
        .unwrap();
    // Not listed: the manifest names another app.
    filesystem
        .write(
            "/media/apps/stray/app.json",
            br#"{"id":"other","name":"","version":""}"#,
        )
        .await
        .unwrap();
    filesystem
        .write("/media/apps/stray/main.lua", b"")
        .await
        .unwrap();
    filesystem
}

/// Polls until `check` holds, giving the VM tasks time to run.
async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        Timer::after_millis(10).await;
    }
    panic!("timed out waiting for {what}");
}

/// Taps a grid over the view until a tap calls `handler`. "Reset" sits right
/// of "+1", so it is searched for from the right.
fn tap(manager: &AppManager, handler: &str) {
    let columns: Vec<i32> = (0..360).step_by(6).collect();
    for y in (0..360).step_by(6) {
        for &x in columns.iter() {
            let x = if handler == "reset" { 354 - x } else { x };
            let called = manager.tap(x as f32, y as f32).unwrap();
            assert!(
                called.is_none() || called.as_deref() == Some(handler),
                "hit {called:?} before {handler}"
            );
            if called.is_some() {
                return;
            }
        }
    }
    panic!("no button calls {handler}");
}

async fn scenario(spawner: Spawner) {
    let packages = LuaPackageRegistry::new();
    let (ui, _registration) = UiService::register(&packages, DEFAULT_VIEWPORT).unwrap();
    let vm = Rc::new(Vm::new(packages).unwrap());
    vm.start(spawner).unwrap();
    let manager = AppManager::new(filesystem().await, vm, Rc::new(ui));
    spawner.spawn(supervise(manager.clone()).unwrap());

    let apps = manager.list().await;
    let summary: Vec<_> = apps
        .iter()
        .map(|app| (app.id.as_str(), app.location, app.has_view))
        .collect();
    assert_eq!(
        summary,
        [
            ("broken", AppLocation::Installed, false),
            ("counter", AppLocation::Bundled, true),
        ]
    );
    assert_eq!(apps[1].name, "计数器");

    assert_eq!(manager.tap(0.0, 0.0), Err(AppError::NoForegroundApp));
    let launched = manager.launch("counter").await.unwrap();
    assert_eq!(manager.foreground(), Some(launched));
    until("the first frame", || {
        manager.texts().unwrap() == ["0", "+1", "Reset"]
    })
    .await;

    // Each tap that finds "+1" is one increment; the list below moves it.
    tap(&manager, "increment");
    until("the first increment", || manager.texts().unwrap()[0] == "1").await;
    tap(&manager, "increment");
    until("the second increment", || {
        manager.texts().unwrap() == ["2", "+1", "Reset", "+1 = 2", "+1 = 1"]
    })
    .await;
    tap(&manager, "reset");
    until("the reset", || {
        manager.texts().unwrap() == ["0", "+1", "Reset"]
    })
    .await;

    // Launching another app replaces the counter and closes its view.
    assert_eq!(
        manager.launch("broken").await.map(|app| app.app.id),
        Ok("broken".into())
    );
    assert_eq!(manager.texts(), Err(AppError::NoView));
    until("the broken app to exit", || manager.foreground().is_none()).await;
    let exit = manager.last_exit().expect("an exit");
    assert_eq!(
        (exit.id.as_str(), exit.outcome),
        ("broken", VmRunOutcome::Error)
    );
    assert_eq!(exit.error, Some(VmExecutionError::LuaRuntime));
    assert!(
        exit.diagnostic
            .as_deref()
            .unwrap_or("")
            .contains("boom: broken nil"),
        "{exit:?}"
    );
    assert_eq!(exit.output, ["starting"]);

    assert_eq!(
        manager.launch("missing").await.err(),
        Some(AppError::NotFound)
    );
    assert_eq!(
        manager.launch("../counter").await.err(),
        Some(AppError::NotFound)
    );
    assert_eq!(
        manager.launch("stray").await.err(),
        Some(AppError::InvalidBundle)
    );

    // Stopping ends the app without recording an exit.
    manager.launch("counter").await.unwrap();
    until("the relaunch", || {
        manager.texts().is_ok_and(|texts| texts[0] == "0")
    })
    .await;
    assert_eq!(manager.stop().map(|app| app.app.id), Some("counter".into()));
    assert_eq!(manager.foreground(), None);
    assert_eq!(
        manager.last_exit().map(|exit| exit.id),
        Some("broken".into())
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
fn counter_app_runs_end_to_end() {
    let (done, finished) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| spawner.spawn(run(spawner, done).unwrap()));
    });
    finished
        .recv_timeout(Duration::from_secs(60))
        .expect("the scenario finished");
}
