//! Single-file app packages: offered from the Workspace, checked and pinned
//! when an install is asked for, signed or not, and updated only by the
//! signer an app was installed from.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]

use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_appkit::{AppError, AppManager, Device};
use barracuda_uikit::{DEFAULT_VIEWPORT, UiService};
use barracuda_vfs::{MountOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_vm_plugin::{LuaPackageRegistry, SeedSource, Vm};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;
use p256::ecdsa::{Signature, SigningKey, signature::Signer as _};

const APPS: &str = "/workspace/media/apps";

/// A package of `files` as `docs/plugins/app-manager/app-format.mdx` lays it out, signed by
/// `key` when given.
fn pack(files: &[(&str, &[u8])], key: Option<&SigningKey>) -> Vec<u8> {
    let directory_end = 12 + files.len() * 40;
    let content_len = directory_end + files.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
    let mut out = Vec::new();
    out.extend_from_slice(b"BAPP");
    out.push(1);
    out.push(u8::try_from(files.len()).unwrap());
    out.push(u8::from(key.is_some()));
    out.push(0);
    out.extend_from_slice(&u32::try_from(content_len).unwrap().to_le_bytes());
    let mut offset = directory_end;
    for (name, bytes) in files {
        let mut padded = [0; 32];
        padded[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&padded);
        out.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_le_bytes());
        offset += bytes.len();
    }
    for (_, bytes) in files {
        out.extend_from_slice(bytes);
    }
    if let Some(key) = key {
        let signature: Signature = key.sign(&out);
        out.extend_from_slice(b"BSIG");
        out.extend_from_slice(key.verifying_key().to_encoded_point(false).as_bytes());
        out.extend_from_slice(&signature.to_bytes());
    }
    out
}

/// App `id` at `version`, whose program prints `said`.
fn app(id: &str, version: &str, said: &str, key: Option<&SigningKey>) -> Vec<u8> {
    let manifest = format!(r#"{{"format":1,"id":"{id}","name":"{id}","version":"{version}"}}"#);
    let program = format!("print({said:?})");
    pack(
        &[
            ("app.json", manifest.as_bytes()),
            ("main.lua", program.as_bytes()),
        ],
        key,
    )
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32].into()).unwrap()
}

async fn offer(fs: &ScopedVfs, id: &str, bytes: &[u8]) {
    fs.write(&format!("{APPS}/{id}.bapp"), bytes).await.unwrap();
}

/// Runs installed app `id` and returns what it printed.
async fn said(manager: &AppManager, id: &str) -> Vec<String> {
    manager.open(id).await.unwrap();
    for _ in 0..500 {
        if manager.running().is_empty() {
            break;
        }
        Timer::after_millis(10).await;
    }
    manager.last_exit().unwrap().output
}

async fn scenario(spawner: Spawner) {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    let fs = vfs.scoped("/app").unwrap();
    let (alice, mallory) = (key(7), key(9));
    offer(&fs, "signed", &app("signed", "1", "signed 1", Some(&alice))).await;
    offer(&fs, "plain", &app("plain", "1", "plain 1", None)).await;
    let mut tampered = app("tampered", "1", "honest", Some(&alice));
    let at = tampered
        .windows(6)
        .position(|bytes| bytes == b"honest")
        .unwrap();
    tampered[at] = b'H';
    offer(&fs, "tampered", &tampered).await;
    offer(&fs, "renamed", &app("other", "1", "renamed", None)).await;
    offer(&fs, "twice", &app("twice", "1", "twice", None)).await;
    fs.write(
        &format!("{APPS}/twice/app.json"),
        br#"{"format":1,"id":"twice","name":"twice","version":"1"}"#,
    )
    .await
    .unwrap();
    fs.write(&format!("{APPS}/twice/main.lua"), b"")
        .await
        .unwrap();

    let packages = LuaPackageRegistry::new();
    let vm = Rc::new(Vm::new(packages.clone(), SeedSource::unavailable()).unwrap());
    vm.start(spawner).unwrap();
    let ui = Rc::new(UiService::new(DEFAULT_VIEWPORT));
    let (manager, _registration) = AppManager::register(
        fs.clone(),
        vm,
        Some(ui),
        Device::headless("test-board"),
        &packages,
    )
    .unwrap();
    spawner.spawn(supervise(manager.clone()).unwrap());

    // Listing reads only the packages' directories.
    let list = manager.list().await;
    let offered: Vec<_> = list.installable.iter().map(|app| app.id.as_str()).collect();
    assert_eq!(offered, ["plain", "signed", "tampered"]);
    let invalid: Vec<_> = list
        .invalid
        .iter()
        .map(|app| (app.dir.as_str(), app.problem.as_str()))
        .collect();
    assert_eq!(
        invalid,
        [
            (
                "renamed",
                "app.json: id \"other\" does not match the container's name"
            ),
            ("twice", "offered both as a directory and as a package"),
        ]
    );

    // Asking checks the signature and pins the digest.
    assert_eq!(
        manager.request_install("tampered").await.err(),
        Some(AppError::InvalidBundle)
    );
    let requested = manager.request_install("signed").await.unwrap();
    let alice_name = requested.signer.clone().unwrap();
    assert_eq!(alice_name.len(), 16);
    assert_eq!(requested.digest.as_ref().map(String::len), Some(64));
    let plain = manager.install_offer("plain").await.unwrap();
    assert_eq!(plain.signer, None);

    // A package that changed after the person was asked is not installed.
    offer(&fs, "signed", &app("signed", "1", "swapped", Some(&alice))).await;
    assert_eq!(
        manager.install("signed").await.err(),
        Some(AppError::Changed)
    );
    manager.request_install("signed").await.unwrap();
    manager.install("signed").await.unwrap();
    assert_eq!(said(&manager, "signed").await, ["swapped"]);

    // An app installed from a signed package takes updates only from its
    // signer.
    offer(
        &fs,
        "signed",
        &app("signed", "2", "mallory", Some(&mallory)),
    )
    .await;
    let update = manager.install_offer("signed").await.unwrap();
    assert_eq!(
        update.incompatible.as_deref(),
        Some("签名与已安装的版本不同")
    );
    assert_ne!(update.signer, Some(alice_name.clone()));
    assert_eq!(
        manager.request_install("signed").await.err(),
        Some(AppError::Incompatible)
    );
    assert_eq!(
        manager.install("signed").await.err(),
        Some(AppError::Incompatible)
    );
    offer(&fs, "signed", &app("signed", "2", "unsigned", None)).await;
    assert_eq!(
        manager.install("signed").await.err(),
        Some(AppError::Incompatible)
    );
    offer(&fs, "signed", &app("signed", "2", "signed 2", Some(&alice))).await;
    manager.install("signed").await.unwrap();
    assert_eq!(said(&manager, "signed").await, ["signed 2"]);

    // Unsigned apps install, and any update replaces them.
    manager.install("plain").await.unwrap();
    offer(&fs, "plain", &app("plain", "2", "plain 2", Some(&mallory))).await;
    manager.install("plain").await.unwrap();
    assert_eq!(said(&manager, "plain").await, ["plain 2"]);

    // A package sent whole, as the portal sends Live Editor's, is offered in
    // the Workspace and asked about at once; a newer one replaces both.
    let sent = app("sent", "1", "sent 1", Some(&alice));
    let asked = manager
        .offer_package("sent", &mut sent.as_slice(), sent.len() as u64)
        .await
        .unwrap();
    assert_eq!((asked.id.as_str(), asked.version.as_str()), ("sent", "1"));
    assert!(fs.exists(&format!("{APPS}/sent.bapp")).await.unwrap());
    let newer = app("sent", "2", "sent 2", Some(&alice));
    manager
        .offer_package("sent", &mut newer.as_slice(), newer.len() as u64)
        .await
        .unwrap();
    let waiting = manager.install_request().unwrap();
    assert_eq!(
        (waiting.id.as_str(), waiting.version.as_str()),
        ("sent", "2")
    );
    manager.install("sent").await.unwrap();
    assert_eq!(said(&manager, "sent").await, ["sent 2"]);

    // Nothing that is not a whole, valid package stays offered.
    let broken = b"not a package";
    assert_eq!(
        manager
            .offer_package("broken", &mut &broken[..], broken.len() as u64)
            .await
            .err(),
        Some(AppError::InvalidBundle)
    );
    assert_eq!(
        manager
            .offer_package("short", &mut &sent[..10], sent.len() as u64)
            .await
            .err(),
        Some(AppError::Storage)
    );
    let names = fs.list_dir(APPS).await.unwrap();
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("broken") || name.contains("short"))
    );

    // Neither is a bad ID, an empty or oversized body, or an ID the
    // Workspace offers as a directory.
    for (id, length) in [
        ("../escape", sent.len() as u64),
        ("Sent", sent.len() as u64),
        ("empty", 0),
        ("huge", barracuda_appkit::MAX_PACKAGE_BYTES + 1),
        ("twice", sent.len() as u64),
    ] {
        assert_eq!(
            manager
                .offer_package(id, &mut sent.as_slice(), length)
                .await
                .err(),
            Some(AppError::InvalidBundle),
            "{id}"
        );
    }
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
fn packages_are_checked_pinned_and_signed() {
    let (done, finished) = sync_channel(1);
    std::thread::spawn(move || {
        let executor: &'static mut Executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| spawner.spawn(run(spawner, done).unwrap()));
    });
    finished
        .recv_timeout(Duration::from_secs(30))
        .expect("the scenario finished");
}
