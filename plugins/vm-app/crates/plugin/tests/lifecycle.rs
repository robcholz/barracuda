//! Runs apps on a real VM through their lifecycle, the way the system UI
//! drives it: open, tap, cover, go home, resume, terminate.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]

use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_app_plugin::{AppError, AppLocation, AppManager, AppState};
use barracuda_ui_plugin::{DEFAULT_VIEWPORT, UiService};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{LuaPackageRegistry, SeedSource, Vm, VmExecutionError, VmRunOutcome};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;

macro_rules! counter_file {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!(
                "../../../filesystem/resources/apps/counter/",
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

/// A headless Workspace app that exercises storage and the timer.
const NOTES: &str = r#"
local app = require("app")
assert(app.view() == nil)
print(app.id, app.name, app.version)
assert(app.fs.write("notes/a.txt", "hello"))
assert(app.fs.append("notes/a.txt", " world"))
print(app.fs.read("notes/a.txt"))
assert(app.fs.mkdir("empty"))
print(table.concat(app.fs.list(), ","), table.concat(app.fs.list("notes"), ","))
print(app.fs.exists("notes/a.txt"), app.fs.exists("missing"))
print(select(2, app.fs.read("../escape")))
print(select(2, app.fs.read("missing")))
assert(app.fs.remove("notes/a.txt"))
print(#app.fs.list("notes"))
assert(app.kv.set("cfg", { n = 1, f = 2.5, tags = { "a", "b" }, nested = { ok = true } }))
local cfg = app.kv.get("cfg")
print(math.type(cfg.n), cfg.f, cfg.tags[2], cfg.nested.ok)
print(select(2, app.kv.set("bad", { 1, x = 2 })))
print(select(2, app.kv.set("bad", print)))
print(select(2, app.kv.set("bad", { f = print })))
assert(app.kv.set("other", "x"))
assert(app.kv.delete("other"))
print(table.concat(app.kv.keys(), ","), app.kv.get("other"))
local ok, message = pcall(app.events, { every = 5 })
print(ok, (message:gsub("^.-:%d+: ", "")))
local ticks = 0
for event in app.events({ every = 100 }) do
  if event == "app:tick" then
    ticks = ticks + 1
    if ticks == 3 then app.exit() end
  end
end
print("ticks", ticks)
"#;

async fn put(fs: &ScopedVfs, path: &str, bytes: &[u8]) {
    fs.write(path, bytes).await.unwrap();
}

/// A Workspace app reusing the counter's view with its own program.
async fn view_app(fs: &ScopedVfs, id: &str, program: &str) {
    for (name, bytes) in COUNTER
        .iter()
        .filter(|(name, _)| name.ends_with(".bin") || *name == "view.bvir")
    {
        put(fs, &format!("/workspace/media/apps/{id}/{name}"), bytes).await;
    }
    put(
        fs,
        &format!("/workspace/media/apps/{id}/app.json"),
        format!(
            r#"{{"format":1,"id":"{id}","name":"{id}","version":"1","entry":"app.lua",
                "view":{{"program":"view.bvir","styles":"styles.bin","fonts":["font-1.bin"]}}}}"#
        )
        .as_bytes(),
    )
    .await;
    put(
        fs,
        &format!("/workspace/media/apps/{id}/app.lua"),
        program.as_bytes(),
    )
    .await;
}

async fn filesystem() -> ScopedVfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app").unwrap();
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
        b"print('starting')\nlocal app = require('app')\nerror('boom: ' .. app.id .. ' ' .. tostring(app.view()))",
    )
    .await;
    put(
        &fs,
        "/workspace/media/apps/notes/app.json",
        br#"{"format":1,"id":"notes","name":"Notes","version":"2"}"#,
    )
    .await;
    put(
        &fs,
        "/workspace/media/apps/notes/main.lua",
        NOTES.as_bytes(),
    )
    .await;

    // Shows 42 and ignores `app:terminate`, so it is cancelled after the grace.
    view_app(
        &fs,
        "agent-counter",
        "local app = require('app') \
         assert(assert(app.view()):set('count', 42)) \
         for event in app.events() do end",
    )
    .await;
    // Busy instead of parking when it leaves the foreground.
    view_app(
        &fs,
        "busy",
        "local app = require('app') \
         for event in app.events() do \
           if event == 'app:background' then while true do end end \
         end",
    )
    .await;
    for id in ["idle-1", "idle-2", "idle-3"] {
        view_app(
            &fs,
            id,
            "local app = require('app') \
             for event in app.events() do if event == 'app:terminate' then break end end",
        )
        .await;
    }

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
    for _ in 0..1000 {
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
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
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

fn state_of(manager: &AppManager, id: &str) -> Option<AppState> {
    manager
        .running()
        .into_iter()
        .find(|app| app.app.id == id)
        .map(|app| app.state)
}

fn texts(manager: &AppManager) -> Vec<String> {
    manager.texts().unwrap_or_default()
}

fn first_text(manager: &AppManager) -> Option<String> {
    texts(manager).into_iter().next()
}

async fn scenario(spawner: Spawner) {
    let packages = LuaPackageRegistry::new();
    let ui = Rc::new(UiService::new(DEFAULT_VIEWPORT));
    let vm = Rc::new(Vm::new(packages.clone(), SeedSource::unavailable()).unwrap());
    vm.start(spawner).unwrap();
    let (manager, _registration) =
        AppManager::register(filesystem().await, vm, Rc::clone(&ui), &packages).unwrap();
    spawner.spawn(supervise(manager.clone()).unwrap());

    // The container list, with broken containers and their reasons.
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
            ("busy", AppLocation::Workspace, true),
            ("counter", AppLocation::Bundled, true),
            ("idle-1", AppLocation::Workspace, true),
            ("idle-2", AppLocation::Workspace, true),
            ("idle-3", AppLocation::Workspace, true),
            ("notes", AppLocation::Workspace, false),
        ]
    );
    let invalid: Vec<_> = list
        .invalid
        .iter()
        .map(|app| (app.dir.as_str(), app.problem.as_str()))
        .collect();
    assert_eq!(invalid.len(), 5, "{invalid:#?}");
    for ((dir, problem), (expected_dir, expected)) in invalid.iter().zip([
        ("Draft", "not an app ID"),
        ("escape", "$.view.program"),
        ("future", "$.format"),
        ("nomain", "main.lua is missing"),
        ("stray", "does not match the directory name"),
    ]) {
        assert_eq!(*dir, expected_dir);
        assert!(problem.contains(expected), "{dir}: {problem}");
    }
    for (id, error) in [
        ("missing", AppError::NotFound),
        ("../counter", AppError::NotFound),
        ("escape", AppError::InvalidBundle),
        ("stray", AppError::InvalidBundle),
    ] {
        assert_eq!(manager.open(id).await.err(), Some(error), "{id}");
    }

    // Open, tap, and cover the counter.
    assert_eq!(manager.tap(0.0, 0.0), Err(AppError::NoForegroundApp));
    assert_eq!(ui.showing(), None);
    let opened = manager.open("counter").await.unwrap();
    assert_eq!(opened.state, AppState::Active);
    // The active app's view is on the display and takes touches.
    assert_eq!(ui.showing(), Some(true));
    until("the first frame", || {
        texts(&manager) == ["0", "+1", "Reset"]
    })
    .await;
    tap(&manager, "increment");
    until("the first increment", || {
        first_text(&manager).as_deref() == Some("1")
    })
    .await;
    tap(&manager, "increment");
    let two = ["2", "+1", "Reset", "+1 = 2", "+1 = 1"];
    until("the second increment", || texts(&manager) == two).await;
    assert_eq!(
        manager.set_interactive(false).unwrap().state,
        AppState::Inactive
    );
    assert_eq!(manager.tap(180.0, 180.0), Err(AppError::NotInteractive));
    assert_eq!(ui.showing(), Some(false));
    assert_eq!(
        manager.set_interactive(true).unwrap().state,
        AppState::Active
    );
    assert_eq!(ui.showing(), Some(true));

    // Home: background, then suspended once it parks; the view is released.
    assert_eq!(
        manager.home().map(|app| app.state),
        Some(AppState::Background)
    );
    assert_eq!(ui.showing(), None);
    assert_eq!(manager.foreground(), None);
    assert_eq!(manager.texts(), Err(AppError::NoForegroundApp));
    until("the counter to suspend", || {
        state_of(&manager, "counter") == Some(AppState::Suspended)
    })
    .await;

    // Reopening rebuilds the view from the values it kept.
    assert_eq!(
        manager.open("counter").await.unwrap().state,
        AppState::Active
    );
    assert_eq!(texts(&manager), two);
    tap(&manager, "increment");
    until("an increment after resuming", || {
        first_text(&manager).as_deref() == Some("3")
    })
    .await;

    // Terminate: it saves and exits; a relaunch restores the saved state.
    assert_eq!(
        manager.terminate("counter").map(|app| app.state),
        Some(AppState::Terminating)
    );
    until("the counter to exit", || {
        state_of(&manager, "counter").is_none()
    })
    .await;
    let exit = manager.last_exit().unwrap();
    assert_eq!(
        (exit.id.as_str(), exit.outcome),
        ("counter", VmRunOutcome::Success),
        "{exit:?}"
    );
    manager.open("counter").await.unwrap();
    until("the restored count", || {
        texts(&manager) == ["3", "+1", "Reset", "+1 = 3", "+1 = 2", "+1 = 1"]
    })
    .await;

    // Opening another view app moves the counter to the background.
    manager.open("agent-counter").await.unwrap();
    assert_eq!(state_of(&manager, "counter"), Some(AppState::Background));
    until("the agent app", || texts(&manager) == ["42", "+1", "Reset"]).await;
    // It ignores `app:terminate`, so it is cancelled after the grace period.
    manager.terminate("agent-counter");
    until("the agent app to be cancelled", || {
        state_of(&manager, "agent-counter").is_none()
    })
    .await;
    let exit = manager.last_exit().unwrap();
    assert_eq!(exit.outcome, VmRunOutcome::Cancelled);
    assert!(
        exit.diagnostic
            .unwrap()
            .contains("termination grace period")
    );
    manager.terminate("counter");
    until("the counter to exit", || {
        state_of(&manager, "counter").is_none()
    })
    .await;

    // A failing headless app reports its error and output.
    manager.open("broken").await.unwrap();
    until("the broken app to exit", || {
        state_of(&manager, "broken").is_none()
    })
    .await;
    let exit = manager.last_exit().unwrap();
    assert_eq!(
        (exit.id.as_str(), exit.outcome),
        ("broken", VmRunOutcome::Error)
    );
    assert_eq!(exit.error, Some(VmExecutionError::LuaRuntime));
    assert!(exit.diagnostic.unwrap().contains("boom: broken nil"));
    assert_eq!(exit.output, ["starting"]);

    // A headless app runs as a service with storage and a timer.
    assert_eq!(
        manager.open("notes").await.unwrap().state,
        AppState::Running
    );
    assert_eq!(manager.foreground(), None);
    until("the notes app to exit", || {
        state_of(&manager, "notes").is_none()
    })
    .await;
    let exit = manager.last_exit().unwrap();
    assert_eq!(exit.outcome, VmRunOutcome::Success, "{exit:?}");
    assert_eq!(
        exit.output,
        [
            "notes\tNotes\t2",
            "hello world",
            "empty/,notes/\ta.txt",
            "true\tfalse",
            "paths stay inside the app: at most 8 names of 1-64 bytes, no . or ..",
            "no such file or directory",
            "0",
            "integer\t2.5\tb\ttrue",
            "a table is either an array or has only string keys",
            "values are booleans, numbers, strings, and tables",
            "tables hold only booleans, numbers, strings, and tables",
            "cfg\tnil",
            "false\tevery is at least 100 milliseconds",
            "ticks\t3",
        ]
    );

    // An app that keeps running in the background is cancelled.
    manager.open("busy").await.unwrap();
    manager.home();
    until("the busy app to be cancelled", || {
        state_of(&manager, "busy").is_none()
    })
    .await;
    assert!(
        manager
            .last_exit()
            .unwrap()
            .diagnostic
            .unwrap()
            .contains("background grace period")
    );

    // Budgets: at most two suspended apps, and a launch takes the place of
    // the least recently used one when every run is in use.
    for id in ["idle-1", "idle-2", "idle-3"] {
        manager.open(id).await.unwrap();
        manager.home();
        until("the app to park", || {
            state_of(&manager, id) != Some(AppState::Background)
        })
        .await;
    }
    let running: Vec<_> = manager
        .running()
        .into_iter()
        .map(|app| (app.app.id, app.state))
        .collect();
    assert_eq!(
        running,
        [
            ("idle-3".into(), AppState::Suspended),
            ("idle-2".into(), AppState::Suspended),
        ]
    );
    manager.open("idle-1").await.unwrap();
    manager.open("counter").await.unwrap();
    let running: Vec<_> = manager
        .running()
        .into_iter()
        .map(|app| (app.app.id, app.state))
        .collect();
    assert_eq!(
        running,
        [
            ("counter".into(), AppState::Active),
            ("idle-1".into(), AppState::Background),
            ("idle-3".into(), AppState::Suspended),
        ]
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
fn apps_run_through_their_lifecycle() {
    let (done, finished) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| spawner.spawn(run(spawner, done).unwrap()));
    });
    finished
        .recv_timeout(Duration::from_secs(120))
        .expect("the scenario finished");
}
