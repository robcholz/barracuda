//! Wi-Fi Plugin registration and Host behavior.

#![allow(clippy::expect_used)]
#![recursion_limit = "256"]

use std::{cell::RefCell, rc::Rc, time::Duration};

use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_platform::{HostWifiDevice, WifiCapabilities};
use barracuda_platform_test::{loopback_network, memory_partition};
use barracuda_plugin::{
    api::{ClientFactory, Hardware, PlatformInfo, PluginContext, TargetIdentity},
    manager::{Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult},
};
use barracuda_vfs::{MountOptions, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use barracuda_wifi_plugin::{WifiControl, WifiPlugin};
use embassy_net::{Ipv4Address, tcp::TcpSocket};
use embedded_io_async::Write as _;

struct Observer {
    webserver: Rc<RefCell<Option<Rc<WebServer>>>>,
    wifi: Rc<RefCell<Option<Rc<WifiControl>>>>,
}

impl PluginDeclaration for Observer {
    const ID: &'static str = "observer";
    const DEPENDS_ON: &'static [&'static str] = &["webserver", "wifi"];
}

impl Plugin for Observer {
    fn register<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        self.webserver
            .replace(Some(context.require::<WebServer>(Self::DEPENDS_ON[0])?));
        self.wifi
            .replace(Some(context.require::<WifiControl>(Self::DEPENDS_ON[1])?));
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn host_registers_portal_entry_and_reports_platform_managed_network() {
    let network = loopback_network();
    let stack = network.stack();
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack, ClientFactory::plaintext(stack));
    let resources = MemFs::new();
    resources
        .create_dir_all("/plugins/captive-portal")
        .expect("portal resources");
    resources
        .create_dir_all("/plugins/wifi")
        .expect("wifi resources");
    resources
        .write_file("/plugins/captive-portal/index.html", b"portal")
        .expect("portal fixture");
    resources
        .write_file("/plugins/wifi/entry.js", b"wifi")
        .expect("wifi fixture");
    let filesystem = Vfs::new();
    filesystem
        .mount(
            "/resources",
            resources.into_backend(),
            MountOptions::read_only(),
        )
        .await
        .expect("resources volume");
    filesystem
        .mount(
            "/data",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .expect("data volume");
    let partition = memory_partition(64 * 1024).await.expect("partition");
    let mut manager = PluginManager::open(partition).await.expect("manager");
    manager.install_vfs(filesystem);
    manager
        .register(WebServerPlugin::new(&mut context))
        .expect("webserver");
    manager
        .register(CaptivePortalPlugin::new(&mut context))
        .expect("portal");
    manager
        .register(WifiPlugin::new(&mut context, HostWifiDevice::new(stack)))
        .expect("wifi");
    let observed = Rc::new(RefCell::new(None));
    let observed_wifi = Rc::new(RefCell::new(None));
    manager
        .register(Observer {
            webserver: Rc::clone(&observed),
            wifi: Rc::clone(&observed_wifi),
        })
        .expect("observer");
    let server = observed.borrow().as_ref().expect("server").clone();
    assert_eq!(
        observed_wifi
            .borrow()
            .as_ref()
            .expect("Wi-Fi capability")
            .capabilities(),
        WifiCapabilities::host_managed()
    );

    let manifest = request(
        Rc::clone(&server),
        network,
        "GET",
        "/portal/entries.json",
        b"",
    )
    .await;
    assert!(
        body(&manifest)
            .windows(11)
            .any(|bytes| bytes == b"\"id\":\"wifi\"")
    );

    let network = loopback_network();
    let response = request(Rc::clone(&server), network, "GET", "/api/wifi", b"").await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(body(&response)).expect("status JSON"),
        serde_json::json!({
            "capabilities": {
                "access_point": false,
                "scanning": false,
                "station_configuration": false
            },
            "station": { "state": "connected" },
            "access_point": { "state": "stopped" }
        })
    );

    let network = loopback_network();
    let response = request(
        server,
        network,
        "PUT",
        "/api/wifi",
        br#"{"ssid":"home","password":"password"}"#,
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 409"));
    assert_eq!(body(&response), br#"{"error":"platform_managed"}"#);
}

fn body(response: &[u8]) -> &[u8] {
    let offset = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("HTTP headers")
        + 4;
    &response[offset..]
}

async fn request(
    server: Rc<WebServer>,
    network: barracuda_platform_test::LoopbackNetwork,
    method: &str,
    path: &str,
    body: &[u8],
) -> Vec<u8> {
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
        let headers = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("request headers");
        socket.write_all(body).await.expect("request body");
        let mut response = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = socket.read(&mut buffer).await.expect("read");
            if count == 0 {
                break;
            }
            response.extend_from_slice(&buffer[..count]);
        }
        response
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            () = network.run() => panic!("network stopped"),
            bytes = async { tokio::select! { bytes = serve => bytes, bytes = client => bytes } } => bytes,
        }
    })
    .await
    .expect("roundtrip timeout")
}
