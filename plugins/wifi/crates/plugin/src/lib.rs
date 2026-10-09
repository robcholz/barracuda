//! Wi-Fi AP and station lifecycle, persistence, and Portal integration.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;

mod control;
mod endpoint;

use alloc::{boxed::Box, rc::Rc};
use core::{future::Future, net::Ipv4Addr, pin::Pin, time::Duration};

use barracuda_captive_portal_plugin::{
    AssetsProvider, CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup,
    WebText,
};
use barracuda_platform::{AccessPointState, StationState, WifiDevice};
use barracuda_plugin::{
    api::PluginContext,
    manager::{
        Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements,
        PluginResult, PluginStartContext, PluginTaskToken,
    },
};
use barracuda_webserver_plugin::{HttpResponse, WebServer};
use control::RadioControl;
use embassy_futures::select::select;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::Timer;
use endpoint::{
    ScanEndpoint, StatusEndpoint, WIFI_API_PATH, WIFI_SCAN_API_PATH, WIFI_STATUS_API_PATH,
    WifiEndpoint,
};

pub use control::{WifiControl, WifiControlError, WifiControlErrorKind, WifiStatus};

const SETUP_AP_SSID: &str = "Barracuda Setup";
const SETUP_AP_PASSWORD: &str = "";
const SETUP_AP_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const CAPTIVE_HTTP_PORT: u16 = 80;
const CAPTIVE_DETECTION_PATHS: [&str; 8] = [
    "/",
    "/generate_204",
    "/gen_204",
    "/hotspot-detect.html",
    "/library/test/success.html",
    "/ncsi.txt",
    "/connecttest.txt",
    "/fwlink",
];

type BoxedWifiRuntime = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Plugin owning Wi-Fi provisioning, access-point fallback, and station policy.
#[barracuda_plugin::macros::plugin]
pub struct WifiPlugin {
    radio: Option<RadioControl>,
    control: Option<Rc<WifiControl>>,
    access_point_stack: Stack<'static>,
    access_point_shutdown: Rc<Signal<NoopRawMutex, u64>>,
}

impl WifiPlugin {
    /// Takes ownership of the selected Platform's Wi-Fi mechanism.
    #[must_use]
    pub fn new<Builtins, Io, Device: WifiDevice>(
        _context: &mut PluginContext<Builtins, Io>,
        device: Device,
    ) -> Self {
        let access_point_stack = device.access_point_stack();
        Self {
            radio: Some(RadioControl::new(device)),
            control: None,
            access_point_stack,
            access_point_shutdown: Rc::new(Signal::new()),
        }
    }
}

impl Plugin for WifiPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        let webserver = context.require::<WebServer>("webserver")?;
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        let radio = self
            .radio
            .take()
            .ok_or_else(|| PluginError::registration(WifiRuntimeUnavailable))?;
        let control = Rc::new(WifiControl::new(
            radio,
            context.storage().clone(),
            Rc::clone(&self.access_point_shutdown),
        ));
        let entry = WebEntry {
            id: "wifi",
            group: WebGroup::Device,
            order: 10,
            title: WebText {
                zh: "Wi-Fi",
                en: "Wi-Fi",
            },
            summary: WebText {
                zh: "扫描、连接或忘记无线网络",
                en: "Scan, join or forget wireless networks",
            },
            icon: Some("icon.svg"),
            figure: Some("figure.js"),
            module: "entry.js",
        };
        let files = ResourceFiles::from(context.filesystem()?.clone());
        let status = Rc::clone(&control);
        let entry = portal
            .register_with_status(entry, files, move || entry_status(&status.status()))
            .map_err(PluginError::registration)?;
        context.retain(entry);
        for path in CAPTIVE_DETECTION_PATHS {
            context.retain(
                webserver
                    .serve(path, PortalLanding(Rc::clone(&portal)))
                    .map_err(PluginError::registration)?,
            );
        }
        if control.capabilities().access_point {
            context.retain(
                webserver
                    .listen_on_stack(self.access_point_stack, CAPTIVE_HTTP_PORT)
                    .map_err(PluginError::registration)?,
            );
        }

        context.retain(
            webserver
                .serve_http(WIFI_API_PATH, WifiEndpoint::new(Rc::clone(&control)))
                .map_err(PluginError::registration)?,
        );
        context.retain(
            webserver
                .serve_http(WIFI_SCAN_API_PATH, ScanEndpoint::new(Rc::clone(&control)))
                .map_err(PluginError::registration)?,
        );
        context.retain(
            webserver
                .serve_http(
                    WIFI_STATUS_API_PATH,
                    StatusEndpoint::new(Rc::clone(&control)),
                )
                .map_err(PluginError::registration)?,
        );
        context.provide(Rc::clone(&control))?;
        self.control = Some(control);
        Ok(())
    }

    fn start<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginStartContext<'_, Storage>,
    ) -> PluginResult<()> {
        let control = self
            .control
            .take()
            .ok_or_else(|| PluginError::registration(WifiRuntimeUnavailable))?;
        if !control.capabilities().station_configuration {
            if matches!(control.status().station(), StationState::Connected { .. }) {
                log::info!("network is managed by the Platform host");
            } else {
                log::warn!("selected Platform does not provide a Wi-Fi implementation");
            }
            return Ok(());
        }
        let stack = self.access_point_stack;
        let access_point_shutdown = Rc::clone(&self.access_point_shutdown);
        let policy = cancellable(
            context.task_token(),
            Box::pin(run_wifi_policy(control, access_point_shutdown)),
        );
        let dhcp = cancellable(context.task_token(), Box::pin(run_dhcp(stack)));
        let dns = cancellable(context.task_token(), Box::pin(run_captive_dns(stack)));
        let tasks = [policy, dhcp, dns]
            .map(run_wifi_runtime)
            .into_iter()
            .collect::<Result<alloc::vec::Vec<_>, _>>()
            .map_err(PluginError::registration)?;
        let spawner = context.task_spawner()?;
        for task in tasks {
            spawner.spawn(task);
        }
        Ok(())
    }
}

/// Maps the last observed radio state to the portal status of the `wifi` entry.
fn entry_status(status: &WifiStatus) -> EntryStatus {
    match (status.station(), status.access_point()) {
        (StationState::Connected { ssid }, _) => {
            let connected = EntryStatus::new(
                EntryState::Ready,
                WebText {
                    zh: "已连接",
                    en: "Connected",
                },
            );
            match ssid {
                Some(ssid) => connected.with_detail(ssid.as_str()),
                None => connected,
            }
        }
        (StationState::Connecting, _) => EntryStatus::new(
            EntryState::Attention,
            WebText {
                zh: "正在连接…",
                en: "Connecting…",
            },
        ),
        (StationState::Disconnected, AccessPointState::Started { ssid }) => {
            EntryStatus::new(EntryState::Attention, SETUP_HOTSPOT).with_detail(ssid.as_str())
        }
        (StationState::Disconnected, AccessPointState::Starting) => {
            EntryStatus::new(EntryState::Attention, SETUP_HOTSPOT)
        }
        (StationState::Disconnected, AccessPointState::Stopped) => EntryStatus::new(
            EntryState::Off,
            WebText {
                zh: "未连接",
                en: "Not connected",
            },
        ),
    }
}

const SETUP_HOTSPOT: WebText = WebText {
    zh: "配置热点",
    en: "Setup hotspot",
};

struct PortalLanding(Rc<CaptivePortal>);

impl AssetsProvider for PortalLanding {
    async fn serve(&self, _path: &str) -> HttpResponse {
        AssetsProvider::serve(self.0.as_ref(), "/portal/").await
    }
}

fn cancellable(cancellation: PluginTaskToken, runtime: BoxedWifiRuntime) -> BoxedWifiRuntime {
    Box::pin(async move {
        let _completed = select(cancellation.cancelled(), runtime).await;
    })
}

async fn run_wifi_policy(
    control: Rc<WifiControl>,
    access_point_shutdown: Rc<Signal<NoopRawMutex, u64>>,
) {
    if let Err(error) = control.initialize().await {
        log::error!("failed to initialize Wi-Fi policy: {error}");
    }
    loop {
        let generation = access_point_shutdown.wait().await;
        Timer::after_secs(1).await;
        match control.stop_access_point_if_current(generation).await {
            Ok(true) => {
                log::info!("stopped Wi-Fi provisioning access point after station configuration");
            }
            Ok(false) => {}
            Err(error) => {
                log::warn!("failed to stop Wi-Fi provisioning access point: {error}");
            }
        }
    }
}

async fn run_dhcp(stack: Stack<'static>) {
    use core::net::{Ipv4Addr, SocketAddrV4};

    use edge_dhcp::{
        io::{self, DEFAULT_SERVER_PORT},
        server::{Server, ServerOptions},
    };
    use edge_nal::UdpBind;
    use edge_nal_embassy::{Udp, UdpBuffers};

    let mut packet = [0_u8; 1500];
    let mut gateways = [Ipv4Addr::UNSPECIFIED];
    let dns_servers = [SETUP_AP_ADDRESS];
    let buffers = UdpBuffers::<1, 1500, 1500, 2>::new();
    let udp = Udp::new(stack, &buffers);
    loop {
        let Ok(mut socket) = udp
            .bind(core::net::SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::UNSPECIFIED,
                DEFAULT_SERVER_PORT,
            )))
            .await
        else {
            log::warn!("failed to bind Wi-Fi DHCP server socket");
            Timer::after_millis(500).await;
            continue;
        };
        let mut options = ServerOptions::new(SETUP_AP_ADDRESS, Some(&mut gateways));
        options.dns = &dns_servers;
        let result = io::server::run(
            &mut Server::<_, 64>::new_with_et(SETUP_AP_ADDRESS),
            &options,
            &mut socket,
            &mut packet,
        )
        .await;
        log::warn!("Wi-Fi DHCP server stopped: {result:?}");
        Timer::after_millis(500).await;
    }
}

async fn run_captive_dns(stack: Stack<'static>) {
    use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    use edge_nal_embassy::{Udp, UdpBuffers};

    let mut tx = [0_u8; 512];
    let mut rx = [0_u8; 512];
    let buffers = UdpBuffers::<1, 512, 512, 2>::new();
    let udp = Udp::new(stack, &buffers);
    let bind = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 53));
    loop {
        let result = edge_captive::io::run(
            &udp,
            bind,
            &mut tx,
            &mut rx,
            SETUP_AP_ADDRESS,
            Duration::from_secs(60),
        )
        .await;
        log::warn!("Wi-Fi captive DNS server stopped: {result:?}");
        Timer::after_millis(500).await;
    }
}

#[embassy_executor::task(pool_size = 3)]
async fn run_wifi_runtime(runtime: BoxedWifiRuntime) {
    runtime.await;
}

#[derive(Debug, thiserror::Error)]
#[error("Wi-Fi runtime was not prepared during Plugin registration")]
struct WifiRuntimeUnavailable;

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use barracuda_platform::{AccessPointState, StationState, WifiCapabilities};
    use serde_json::{Value, json};

    use super::{WifiStatus, entry_status};

    fn status(station: StationState, access_point: AccessPointState) -> Value {
        let status = WifiStatus::from_parts(WifiCapabilities::managed(), station, access_point);
        serde_json::to_value(entry_status(&status)).unwrap_or(Value::Null)
    }

    #[test]
    fn entry_status_follows_station_then_setup_hotspot() {
        let hotspot = || AccessPointState::Started {
            ssid: String::from("Barracuda Setup"),
        };
        assert_eq!(
            status(
                StationState::Connected {
                    ssid: Some(String::from("HomeNet")),
                },
                hotspot(),
            ),
            json!({"state": "ready", "label": {"zh": "已连接", "en": "Connected"}, "detail": "HomeNet"})
        );
        assert_eq!(
            status(
                StationState::Connected { ssid: None },
                AccessPointState::Stopped
            ),
            json!({"state": "ready", "label": {"zh": "已连接", "en": "Connected"}})
        );
        assert_eq!(
            status(StationState::Connecting, hotspot()),
            json!({"state": "attention", "label": {"zh": "正在连接…", "en": "Connecting…"}})
        );
        assert_eq!(
            status(StationState::Disconnected, hotspot()),
            json!({
                "state": "attention",
                "label": {"zh": "配置热点", "en": "Setup hotspot"},
                "detail": "Barracuda Setup"
            })
        );
        assert_eq!(
            status(StationState::Disconnected, AccessPointState::Starting),
            json!({"state": "attention", "label": {"zh": "配置热点", "en": "Setup hotspot"}})
        );
        assert_eq!(
            status(StationState::Disconnected, AccessPointState::Stopped),
            json!({"state": "off", "label": {"zh": "未连接", "en": "Not connected"}})
        );
    }
}
