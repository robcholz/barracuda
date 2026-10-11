//! The Platform Wi-Fi radio, handed to the Plugin that owns it without its type.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::{fmt, future::Future, pin::Pin};

use barracuda_platform::{
    AccessPointConfiguration, AccessPointState, StationConfiguration, StationState,
    VisibleNetwork, WifiCapabilities, WifiDevice,
};
use embassy_net::Stack;

type DeviceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PlatformWifiError>> + 'a>>;

/// The selected Platform's Wi-Fi radio with its concrete type erased.
///
/// System assigns it to `PluginContext::wifi`, and the one Plugin that owns
/// Wi-Fi policy takes it during construction. Each operation boxes its future,
/// and a failure keeps only its message.
pub struct PlatformWifi {
    device: Box<dyn DynWifiDevice>,
}

impl PlatformWifi {
    /// Erases `device`.
    #[must_use]
    pub fn new<D: WifiDevice>(device: D) -> Self {
        Self {
            device: Box::new(device),
        }
    }
}

impl fmt::Debug for PlatformWifi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("PlatformWifi").finish_non_exhaustive()
    }
}

impl WifiDevice for PlatformWifi {
    type Error = PlatformWifiError;

    fn capabilities(&self) -> WifiCapabilities {
        self.device.capabilities()
    }

    fn access_point_stack(&self) -> Stack<'static> {
        self.device.access_point_stack()
    }

    fn station_state(&self) -> StationState {
        self.device.station_state()
    }

    fn access_point_state(&self) -> AccessPointState {
        self.device.access_point_state()
    }

    async fn scan(&mut self) -> Result<Vec<VisibleNetwork>, Self::Error> {
        self.device.scan().await
    }

    async fn start_access_point(
        &mut self,
        configuration: &AccessPointConfiguration,
    ) -> Result<(), Self::Error> {
        self.device.start_access_point(configuration).await
    }

    async fn stop_access_point(&mut self) -> Result<(), Self::Error> {
        self.device.stop_access_point().await
    }

    async fn connect_station(
        &mut self,
        configuration: &StationConfiguration,
    ) -> Result<(), Self::Error> {
        self.device.connect_station(configuration).await
    }

    async fn disconnect_station(&mut self) -> Result<(), Self::Error> {
        self.device.disconnect_station().await
    }
}

/// A failed Platform Wi-Fi operation, kept as its message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformWifiError {
    message: String,
}

impl PlatformWifiError {
    fn from_device(error: impl core::error::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl fmt::Display for PlatformWifiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl core::error::Error for PlatformWifiError {}

/// The object-safe part of [`WifiDevice`].
trait DynWifiDevice {
    fn capabilities(&self) -> WifiCapabilities;
    fn access_point_stack(&self) -> Stack<'static>;
    fn station_state(&self) -> StationState;
    fn access_point_state(&self) -> AccessPointState;
    fn scan(&mut self) -> DeviceFuture<'_, Vec<VisibleNetwork>>;
    fn start_access_point<'a>(
        &'a mut self,
        configuration: &'a AccessPointConfiguration,
    ) -> DeviceFuture<'a, ()>;
    fn stop_access_point(&mut self) -> DeviceFuture<'_, ()>;
    fn connect_station<'a>(
        &'a mut self,
        configuration: &'a StationConfiguration,
    ) -> DeviceFuture<'a, ()>;
    fn disconnect_station(&mut self) -> DeviceFuture<'_, ()>;
}

impl<D: WifiDevice> DynWifiDevice for D {
    fn capabilities(&self) -> WifiCapabilities {
        WifiDevice::capabilities(self)
    }

    fn access_point_stack(&self) -> Stack<'static> {
        WifiDevice::access_point_stack(self)
    }

    fn station_state(&self) -> StationState {
        WifiDevice::station_state(self)
    }

    fn access_point_state(&self) -> AccessPointState {
        WifiDevice::access_point_state(self)
    }

    fn scan(&mut self) -> DeviceFuture<'_, Vec<VisibleNetwork>> {
        Box::pin(async move {
            WifiDevice::scan(self)
                .await
                .map_err(PlatformWifiError::from_device)
        })
    }

    fn start_access_point<'a>(
        &'a mut self,
        configuration: &'a AccessPointConfiguration,
    ) -> DeviceFuture<'a, ()> {
        Box::pin(async move {
            WifiDevice::start_access_point(self, configuration)
                .await
                .map_err(PlatformWifiError::from_device)
        })
    }

    fn stop_access_point(&mut self) -> DeviceFuture<'_, ()> {
        Box::pin(async move {
            WifiDevice::stop_access_point(self)
                .await
                .map_err(PlatformWifiError::from_device)
        })
    }

    fn connect_station<'a>(
        &'a mut self,
        configuration: &'a StationConfiguration,
    ) -> DeviceFuture<'a, ()> {
        Box::pin(async move {
            WifiDevice::connect_station(self, configuration)
                .await
                .map_err(PlatformWifiError::from_device)
        })
    }

    fn disconnect_station(&mut self) -> DeviceFuture<'_, ()> {
        Box::pin(async move {
            WifiDevice::disconnect_station(self)
                .await
                .map_err(PlatformWifiError::from_device)
        })
    }
}
