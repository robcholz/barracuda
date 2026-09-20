//! Wi-Fi AP and station lifecycle, persistence, and Portal integration.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;

mod control;
mod endpoint;

use alloc::{boxed::Box, rc::Rc};
use core::{future::Future, net::Ipv4Addr, pin::Pin, time::Duration};

use barracuda_captive_portal_plugin::{AssetsProvider, CaptivePortal, ResourceFiles, WebEntry};
use barracuda_platform::{StationConfiguration, StationState, WifiDevice};
use barracuda_plugin::{
    api::PluginContext,
    manager::{
        Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements,
        PluginResult, PluginStartContext, PluginTaskToken,
    },
};
use barracuda_webserver_plugin::{HttpResponse, WebServer};
use embassy_futures::select::select;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::Timer;
use endpoint::{ScanEndpoint, WIFI_API_PATH, WIFI_SCAN_API_PATH, WifiEndpoint, load_configuration};

pub use control::{WifiControl, WifiStatus};

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
pub struct WifiPlugin<Device: WifiDevice> {
    control: WifiControl<Device>,
    initial_configuration: Option<StationConfiguration>,
    access_point_stack: Stack<'static>,
    access_point_shutdown: Rc<Signal<NoopRawMutex, ()>>,
}

impl<Device: WifiDevice> WifiPlugin<Device> {
    /// Takes ownership of the selected Platform's Wi-Fi mechanism.
    #[must_use]
    pub fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>, device: Device) -> Self {
        let access_point_stack = device.access_point_stack();
        Self {
            control: WifiControl::new(device),
            initial_configuration: None,
            access_point_stack,
            access_point_shutdown: Rc::new(Signal::new()),
        }
    }
}

impl<Device: WifiDevice> Plugin for WifiPlugin<Device> {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        let webserver = context.require::<WebServer>("webserver")?;
        let portal = context.require::<CaptivePortal>("captive-portal")?;
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
        if self.control.capabilities().access_point {
            context.retain(
                webserver
                    .listen_on_stack(self.access_point_stack, CAPTIVE_HTTP_PORT)
                    .map_err(PluginError::registration)?,
            );
        }

        self.initial_configuration =
            embassy_futures::block_on(load_configuration(context.storage()))?;
        context.retain(
            webserver
                .serve_http(
                    WIFI_API_PATH,
                    WifiEndpoint::new(
                        self.control.clone(),
                        Rc::clone(&self.access_point_shutdown),
                        context.storage().clone(),
                    ),
                )
                .map_err(PluginError::registration)?,
        );
        context.retain(
            webserver
                .serve_http(WIFI_SCAN_API_PATH, ScanEndpoint::new(self.control.clone()))
                .map_err(PluginError::registration)?,
        );
        Ok(())
    }

    fn start<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginStartContext<'_, Storage>,
    ) -> PluginResult<()> {
        if !self.control.capabilities().station_configuration {
            if matches!(
                self.control.status().station(),
                StationState::Connected { .. }
            ) {
                log::info!("network is managed by the Platform host");
            } else {
                log::warn!("selected Platform does not provide a Wi-Fi implementation");
            }
            return Ok(());
        }
        let control = self.control.clone();
        let configuration = self.initial_configuration.take();
        let stack = self.access_point_stack;
        let access_point_shutdown = Rc::clone(&self.access_point_shutdown);
        let policy = cancellable(
            context.task_token(),
            Box::pin(run_wifi_policy(
                control,
                configuration,
                access_point_shutdown,
            )),
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

async fn initialize_wifi<Device: WifiDevice>(
    control: WifiControl<Device>,
    configuration: Option<StationConfiguration>,
) {
    if let Some(configuration) = configuration {
        match control
            .connect_station(configuration.ssid(), configuration.password())
            .await
        {
            Ok(()) => {
                log::info!("connected Wi-Fi station to {}", configuration.ssid());
                return;
            }
            Err(error) => {
                log::warn!(
                    "failed to restore Wi-Fi station connection to {}: {error}",
                    configuration.ssid()
                );
            }
        }
    }
    if let Err(error) = control
        .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
        .await
    {
        log::error!("failed to start Wi-Fi provisioning access point: {error}");
    } else {
        log::info!("started Wi-Fi provisioning access point {SETUP_AP_SSID}");
    }
}

async fn run_wifi_policy<Device: WifiDevice>(
    control: WifiControl<Device>,
    configuration: Option<StationConfiguration>,
    access_point_shutdown: Rc<Signal<NoopRawMutex, ()>>,
) {
    initialize_wifi(control.clone(), configuration).await;
    loop {
        access_point_shutdown.wait().await;
        Timer::after_secs(1).await;
        if let Err(error) = control.stop_access_point().await {
            log::warn!("failed to stop Wi-Fi provisioning access point: {error}");
        } else {
            log::info!("stopped Wi-Fi provisioning access point after station configuration");
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
