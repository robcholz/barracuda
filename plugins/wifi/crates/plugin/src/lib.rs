//! Wi-Fi AP and station lifecycle, persistence, and Portal integration.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;

mod control;
mod endpoint;

use alloc::{boxed::Box, rc::Rc};
use core::{future::Future, net::Ipv4Addr, pin::Pin, time::Duration};

use barracuda_captive_portal_plugin::{AssetsProvider, CaptivePortal, ResourceFiles, WebEntry};
use barracuda_platform::{StationState, WifiDevice};
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
use endpoint::{ScanEndpoint, WIFI_API_PATH, WIFI_SCAN_API_PATH, WifiEndpoint};

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
        let entry = portal
            .register(
                WebEntry {
                    id: "wifi",
                    title: "Wi-Fi",
                    module: "entry.js",
                },
                ResourceFiles::from(context.filesystem()?.clone()),
            )
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
        let result = io::server::run(
            &mut Server::<_, 64>::new_with_et(SETUP_AP_ADDRESS),
            &ServerOptions::new(SETUP_AP_ADDRESS, Some(&mut gateways)),
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
