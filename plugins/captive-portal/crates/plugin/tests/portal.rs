//! Tests of the public portal capability and provider contract.
#![allow(clippy::expect_used)]
#![recursion_limit = "256"]

use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_captive_portal_plugin::{AssetsProvider, CaptivePortal, WebEntry};
use barracuda_platform_test::{memory_partition, never_embassy_stack};
use barracuda_plugin::api::{ClientFactory, PluginContext};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_webserver_plugin::HttpResponse;
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use futures_lite::future::block_on;

#[test]
fn resource_provider_reports_unmounted_volume_and_rejects_escape() {
    use barracuda_captive_portal_plugin::ResourceFiles;
    let vfs = barracuda_vfs::Vfs::new();
    let files = ResourceFiles::from(
        vfs.scoped_mounts([("/resources", "/resources/plugins/wifi")])
            .expect("scope"),
    );
    assert_eq!(block_on(files.serve("entry.js")).status(), 503);
    assert_eq!(block_on(files.serve("../data/private.txt")).status(), 404);
}

#[test]
fn entry_ids_follow_plugin_identity_rules() {
    let portal = CaptivePortal::new(Assets);
    for id in ["-wifi", "wifi-", "wifi--config", "-", "WiFi"] {
        assert!(portal
            .register(
                WebEntry {
                    id,
                    title: "invalid",
                    module: "entry.js"
                },
                Assets
            )
            .is_err());
    }
}
use std::cell::RefCell;
use std::rc::Rc;

struct Observer(Rc<RefCell<Option<Rc<WebServer>>>>);
impl PluginDeclaration for Observer {
    const ID: &'static str = "observer";
    const DEPENDS_ON: &'static [&'static str] = &["webserver"];
}
impl Plugin for Observer {
    fn register<S: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, S>,
    ) -> PluginResult<()> {
        *self.0.borrow_mut() = Some(context.require::<WebServer>(Self::DEPENDS_ON[0])?);
        Ok(())
    }
}

struct Consumer;
impl PluginDeclaration for Consumer {
    const ID: &'static str = "wifi";
    const DEPENDS_ON: &'static [&'static str] = &["captive-portal"];
}
impl Plugin for Consumer {
    const REQUIREMENTS: barracuda_plugin::manager::PluginRequirements =
        barracuda_plugin::manager::PluginRequirements::new()
            .with_filesystem(barracuda_plugin::manager::PluginFilesystem::Private);
    fn register<S: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, S>,
    ) -> PluginResult<()> {
        let portal = context.require::<CaptivePortal>(Self::DEPENDS_ON[0])?;
        let entry = portal
            .register(
                WebEntry {
                    id: "wifi",
                    title: "Wi-Fi \"network\"\n设置",
                    module: "entry.js",
                },
                barracuda_captive_portal_plugin::ResourceFiles::from(context.filesystem()?.clone()),
            )
            .map_err(barracuda_plugin::manager::PluginError::registration)?;
        context.retain(entry);
        Ok(())
    }
}

struct Assets;
impl AssetsProvider for Assets {
    async fn serve(&self, path: &str) -> HttpResponse {
        HttpResponse::new(200, "text/plain", path.as_bytes().to_vec())
    }
}

#[test]
fn entry_registration_controls_asset_visibility() {
    let portal = CaptivePortal::new(Assets);
    let entry = portal
        .register(
            WebEntry {
                id: "wifi",
                title: "Wi-Fi",
                module: "entry.js",
            },
            Assets,
        )
        .expect("register");
    let response = block_on(portal.serve("/portal/assets/wifi/chunks/main.js"));
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), Some(&b"chunks/main.js"[..]));
    drop(entry);
    assert_eq!(
        block_on(portal.serve("/portal/assets/wifi/entry.js")).status(),
        404
    );
}

#[test]
fn rejects_duplicate_ids_and_unsafe_resource_paths() {
    let portal = CaptivePortal::new(Assets);
    let _entry = portal
        .register(
            WebEntry {
                id: "wifi",
                title: "Wi-Fi",
                module: "entry.js",
            },
            Assets,
        )
        .expect("register");
    assert!(portal
        .register(
            WebEntry {
                id: "wifi",
                title: "duplicate",
                module: "entry.js"
            },
            Assets
        )
        .is_err());
    assert!(portal
        .register(
            WebEntry {
                id: "../bad",
                title: "bad",
                module: "entry.js"
            },
            Assets
        )
        .is_err());
    for path in [
        "/portal/assets/wifi/../data/secret",
        "/portal/assets/wifi/%2e%2e/data",
        "/portal/assets/wifi//entry.js",
        "/portal/assets/wifi/entry.js?secret",
        "/portal/assets/wifi-other/entry.js",
        "/portal/assets/wifi/\\data",
    ] {
        assert_eq!(block_on(portal.serve(path)).status(), 404, "{path}");
    }
}

#[test]
fn scaffold_paths_are_separate_from_plugin_assets() {
    let portal = CaptivePortal::new(Assets);
    assert_eq!(
        block_on(portal.serve("/portal/")).body(),
        Some(&b"index.html"[..])
    );
    assert_eq!(
        block_on(portal.serve("/portal/app.js")).body(),
        Some(&b"app.js"[..])
    );
    assert_eq!(
        block_on(portal.serve("/portal/../data/secret")).status(),
        404
    );
    assert_eq!(block_on(portal.serve("/outside")).status(), 404);
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_unload_updates_the_http_manifest_and_assets() {
    let stack = never_embassy_stack();
    let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
    let resources = barracuda_vfs_memfs::MemFs::new();
    resources
        .create_dir_all("/plugins/captive-portal")
        .expect("resources");
    resources
        .write_file(
            "/plugins/captive-portal/index.html",
            b"<!doctype html><title>fixture</title>",
        )
        .expect("scaffold fixture");
    resources
        .create_dir_all("/plugins/wifi")
        .expect("consumer resources");
    resources
        .write_file("/plugins/wifi/entry.js", b"entry.js")
        .expect("consumer module");
    let mut filesystem = barracuda_vfs::Vfs::new();
    filesystem
        .mount(
            "/resources",
            resources.into_backend(),
            barracuda_vfs::MountOptions::read_only(),
        )
        .await
        .expect("read-only resources volume");
    filesystem
        .mount(
            "/data",
            barracuda_vfs_memfs::MemFs::new().into_backend(),
            barracuda_vfs::MountOptions::read_write(),
        )
        .await
        .expect("data volume");
    filesystem
        .create_dir_all("/data/plugins/captive-portal")
        .await
        .expect("data");
    filesystem
        .write(
            "/data/plugins/captive-portal/private.txt",
            b"must not be exposed",
        )
        .await
        .expect("private fixture");
    let partition = memory_partition(64 * 1024).await.expect("partition");
    let mut manager = PluginManager::open(partition).await.expect("manager");
    manager.install_vfs(filesystem);
    manager
        .register(WebServerPlugin::new(&mut context))
        .expect("webserver");
    manager
        .register(CaptivePortalPlugin::new(&mut context))
        .expect("portal");
    manager.register(Consumer).expect("consumer");
    let observed = Rc::new(RefCell::new(None));
    manager
        .register(Observer(observed.clone()))
        .expect("observer");
    let server = observed.borrow().as_ref().expect("capability").clone();

    let response = request(server.clone(), "/portal/").await;
    assert_eq!(body(&response), b"<!doctype html><title>fixture</title>");
    for path in [
        "/portal/missing.js",
        "/portal/../data/private.txt",
        "/portal/%2e%2e/data/private.txt",
    ] {
        assert!(request(server.clone(), path)
            .await
            .starts_with(b"HTTP/1.1 404"));
    }

    let response = request(server.clone(), "/portal/entries.json").await;
    let entries: serde_json::Value = serde_json::from_slice(body(&response)).expect("manifest");
    assert_eq!(entries[0]["id"], "wifi");
    assert_eq!(entries[0]["title"], "Wi-Fi \"network\"\n设置");
    assert_eq!(entries[0]["module"], "/portal/assets/wifi/entry.js");
    let response = request(server.clone(), "/portal/assets/wifi/entry.js").await;
    assert_eq!(body(&response), b"entry.js");
    assert!(std::str::from_utf8(&response)
        .expect("response")
        .to_ascii_lowercase()
        .contains("content-type: application/javascript"));
    for path in [
        "/portal/assets/wifi/index.html",
        "/portal/entry.js",
        "/portal/assets/wifi/../captive-portal/index.html",
    ] {
        assert!(request(server.clone(), path)
            .await
            .starts_with(b"HTTP/1.1 404"));
    }
    manager
        .unload(&"wifi".try_into().expect("id"))
        .await
        .expect("unload consumer");
    let response = request(server.clone(), "/portal/entries.json").await;
    assert_eq!(body(&response), b"[]");
    let response = request(server.clone(), "/portal/assets/wifi/entry.js").await;
    assert!(response.starts_with(b"HTTP/1.1 404"));
    manager.register(Consumer).expect("reload consumer");
    let response = request(server, "/portal/entries.json").await;
    let entries: serde_json::Value =
        serde_json::from_slice(body(&response)).expect("reloaded manifest");
    assert_eq!(entries.as_array().expect("array").len(), 1);
}

#[test]
fn cached_manifest_and_leaf_dispatch_do_not_allocate() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    struct StaticAssets;
    impl AssetsProvider for StaticAssets {
        async fn serve(&self, _: &str) -> HttpResponse {
            HttpResponse::stream(200, "application/javascript", 1, &b"x"[..])
        }
    }
    let portal = CaptivePortal::new(StaticAssets);
    let _entry = portal
        .register(
            WebEntry {
                id: "wifi",
                title: "Wi-Fi",
                module: "entry.js",
            },
            StaticAssets,
        )
        .expect("register");
    let allocations = allocation_counter::measure(|| {
        for path in [
            "/portal/entries.json",
            "/portal/assets/wifi/entry.js",
            "/portal/",
        ] {
            let mut future = std::pin::pin!(portal.serve(path));
            let mut cx = Context::from_waker(Waker::noop());
            let Poll::Ready(response) = future.as_mut().poll(&mut cx) else {
                unreachable!()
            };
            assert_eq!(response.status(), 200);
        }
    });
    assert_eq!(allocations.count_total, 0);
}

fn body(response: &[u8]) -> &[u8] {
    let offset = response
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .expect("HTTP headers")
        + 4;
    &response[offset..]
}

async fn request(server: Rc<WebServer>, path: &str) -> Vec<u8> {
    use barracuda_platform_test::loopback_network;
    use embassy_net::{tcp::TcpSocket, Ipv4Address};
    use embedded_io_async::Write as _;
    let network = loopback_network();
    let stack = network.stack();
    let serve = async {
        let mut rx = [0; 4096];
        let mut tx = [0; 4096];
        let mut http = [0; 4096];
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.accept(8787).await.expect("accept");
        server
            .serve_connection(picoserve::time::EmbassyTimer, &mut http, socket)
            .await
            .expect("serve");
        core::future::pending::<Vec<u8>>().await
    };
    let client = async {
        let mut rx = [0; 4096];
        let mut tx = [0; 4096];
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        loop {
            if socket
                .connect((Ipv4Address::new(10, 0, 0, 1), 8787))
                .await
                .is_ok()
            {
                break;
            }
            socket.abort();
            tokio::task::yield_now().await;
        }
        socket
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("request");
        let mut response = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = socket.read(&mut buffer).await.expect("read");
            if count == 0 {
                break;
            }
            response.extend_from_slice(&buffer[..count]);
            if let Some(offset) = response.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&response[..offset]).expect("headers");
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .expect("length");
                if response.len() >= offset + 4 + length {
                    break;
                }
            }
        }
        response
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::select! {
            () = network.run() => Vec::new(),
            result = serve => result,
            result = client => result,
        }
    })
    .await
    .expect("HTTP timeout")
}
