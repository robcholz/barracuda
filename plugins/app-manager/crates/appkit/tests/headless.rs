//! Runs apps on a device without a display: headless apps run, and apps
//! with views answer that there is nothing to show them on.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]

use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_appkit::{AppError, AppManager, Device};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{LuaPackageRegistry, SeedSource, Vm};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;

macro_rules! counter_file {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!("fixtures/counter.bapp/", $name)).as_slice(),
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

async fn filesystem() -> ScopedVfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app").unwrap();
    for (name, bytes) in COUNTER {
        fs.write(&format!("/workspace/media/apps/counter.bapp/{name}"), bytes)
            .await
            .unwrap();
    }
    fs.write(
        "/workspace/media/apps/beacon.bapp/app.json",
        br#"{"format":1,"id":"beacon","name":"Beacon","version":"1"}"#,
    )
    .await
    .unwrap();
    fs.write(
        "/workspace/media/apps/beacon.bapp/main.lua",
        b"print('beacon')",
    )
    .await
    .unwrap();
    fs
}

async fn scenario(spawner: Spawner) {
    let packages = LuaPackageRegistry::new();
    let vm = Rc::new(Vm::new(packages.clone(), SeedSource::unavailable()).unwrap());
    vm.start(spawner).unwrap();
    let (manager, _registration) = AppManager::register(
        filesystem().await,
        vm,
        None,
        Device::headless("test-board"),
        &packages,
    )
    .unwrap();
    spawner.spawn(supervise(manager.clone()).unwrap());

    let ids: Vec<String> = manager
        .list()
        .await
        .apps
        .into_iter()
        .map(|app| app.id)
        .collect();
    assert_eq!(ids, ["beacon", "counter"]);
    assert_eq!(
        manager.open("counter").await.err(),
        Some(AppError::NoDisplay)
    );
    assert!(manager.running().is_empty());

    manager.open("beacon").await.unwrap();
    for _ in 0..500 {
        if manager.running().is_empty() {
            break;
        }
        Timer::after_millis(10).await;
    }
    let exit = manager.last_exit().unwrap();
    assert_eq!(exit.output, ["beacon"]);
    assert!(!exit.foreground);
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
fn a_device_without_a_display_runs_headless_apps() {
    let (done, finished) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| spawner.spawn(run(spawner, done).unwrap()));
    });
    finished
        .recv_timeout(Duration::from_secs(60))
        .expect("the scenario finished");
}
