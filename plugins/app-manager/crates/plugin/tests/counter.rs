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

async fn put(filesystem: &ScopedVfs, path: &str, bytes: &[u8]) {
    filesystem.write(path, bytes).await.unwrap();
}

/// The bundled counter, an installed app, and containers an Agent wrote to
/// the Workspace: one launchable, the rest shadowed or broken.
async fn filesystem() -> ScopedVfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app-manager").unwrap();
    for (name, bytes) in COUNTER {
        put(&fs, &format!("/resources/apps/counter/{name}"), bytes).await;
    }

    put(
        &fs,
        "/media/apps/broken/app.json",
        br#"{"format":1,"id":"broken","name":"Broken","version":"1"}"#,
    )
    .await;
    put(
        &fs,
        "/media/apps/broken/main.lua",
        b"print('starting')\nerror('boom: ' .. APP.id .. ' ' .. tostring(APP.ui))",
    )
    .await;

    // An Agent-made app: the counter's view with its own entry program.
    for (name, bytes) in COUNTER
        .iter()
        .filter(|(name, _)| name.ends_with(".bin") || *name == "view.bvir")
    {
        put(
            &fs,
            &format!("/workspace/media/apps/agent-counter/{name}"),
            bytes,
        )
        .await;
    }
    put(
        &fs,
        "/workspace/media/apps/agent-counter/app.json",
        br#"{"format":1,"id":"agent-counter","name":"Agent","version":"1","entry":"app.lua",
            "view":{"program":"view.bvir","styles":"styles.bin","fonts":["font-1.bin"]}}"#,
    )
    .await;
    put(
        &fs,
        "/workspace/media/apps/agent-counter/app.lua",
        b"local view = assert(require('ui').attach(APP.ui)) \
          assert(view:set('count', 42)) \
          while view:next() do end",
    )
    .await;

    // Workspace containers never replace an installed or bundled app.
    for id in ["broken", "counter"] {
        put(
            &fs,
            &format!("/workspace/media/apps/{id}/app.json"),
            format!(r#"{{"format":1,"id":"{id}","name":"Impostor","version":"1"}}"#).as_bytes(),
        )
        .await;
        put(&fs, &format!("/workspace/media/apps/{id}/main.lua"), b"").await;
    }

    for (dir, manifest) in [
        (
            "Draft",
            r#"{"format":1,"id":"Draft","name":"x","version":"1"}"#,
        ),
        (
            "escape",
            r#"{"format":1,"id":"escape","name":"x","version":"1",
                "view":{"program":"../counter/view.bvir","styles":"styles.bin"}}"#,
        ),
        (
            "future",
            r#"{"format":2,"id":"future","name":"x","version":"1"}"#,
        ),
        (
            "nomain",
            r#"{"format":1,"id":"nomain","name":"x","version":"1"}"#,
        ),
        (
            "stray",
            r#"{"format":1,"id":"other","name":"x","version":"1"}"#,
        ),
    ] {
        put(
            &fs,
            &format!("/workspace/media/apps/{dir}/app.json"),
            manifest.as_bytes(),
        )
        .await;
        if dir != "nomain" {
            put(&fs, &format!("/workspace/media/apps/{dir}/main.lua"), b"").await;
        }
    }
    fs
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

    let list = manager.list().await;
    let apps: Vec<_> = list
        .apps
        .iter()
        .map(|app| (app.id.as_str(), app.location, app.has_view))
        .collect();
    assert_eq!(
        apps,
        [
            ("agent-counter", AppLocation::Workspace, true),
            ("broken", AppLocation::Installed, false),
            ("counter", AppLocation::Bundled, true),
        ]
    );
    assert_eq!(list.apps[2].name, "计数器");
    let invalid: Vec<_> = list
        .invalid
        .iter()
        .map(|app| (app.dir.as_str(), app.location, app.problem.as_str()))
        .collect();
    assert_eq!(invalid.len(), 5, "{invalid:#?}");
    for ((dir, location, problem), (expected_dir, expected)) in invalid.iter().zip([
        ("Draft", "not an app ID"),
        ("escape", "$.view.program"),
        ("future", "$.format"),
        ("nomain", "main.lua is missing"),
        ("stray", "does not match the directory name"),
    ]) {
        assert_eq!((*dir, *location), (expected_dir, AppLocation::Workspace));
        assert!(problem.contains(expected), "{dir}: {problem}");
    }
    assert_eq!(
        manager.launch("escape").await.err(),
        Some(AppError::InvalidBundle)
    );

    // The Agent's app runs with its own entry and declared fonts.
    manager.launch("agent-counter").await.unwrap();
    until("the agent app", || {
        manager
            .texts()
            .is_ok_and(|texts| texts == ["42", "+1", "Reset"])
    })
    .await;

    assert_eq!(
        manager.stop().map(|app| app.app.id),
        Some("agent-counter".into())
    );
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
