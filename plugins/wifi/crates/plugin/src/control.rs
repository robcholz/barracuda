use alloc::{boxed::Box, rc::Rc, string::String, string::ToString, vec::Vec};
use core::{cell::RefCell, future::Future, pin::Pin};

use barracuda_platform::{
    AccessPointConfiguration, AccessPointState, StationConfiguration, StationState, VisibleNetwork,
    WifiCapabilities, WifiDevice,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

type WifiControlFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, WifiControlError>> + 'a>>;

/// Failure returned by the selected Platform's Wi-Fi mechanism.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("Platform Wi-Fi operation failed: {message}")]
pub struct WifiControlError {
    message: String,
}

impl WifiControlError {
    fn platform(error: impl core::error::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

/// Observable AP and station state owned by the Wi-Fi Plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WifiStatus {
    capabilities: WifiCapabilities,
    station: StationState,
    access_point: AccessPointState,
}

impl WifiStatus {
    /// Status reported by Host Platforms whose network is managed externally.
    #[must_use]
    pub const fn platform_managed() -> Self {
        Self {
            capabilities: WifiCapabilities::host_managed(),
            station: StationState::Connected { ssid: None },
            access_point: AccessPointState::Stopped,
        }
    }

    /// Returns the supported operations.
    #[must_use]
    pub const fn capabilities(&self) -> WifiCapabilities {
        self.capabilities
    }

    /// Returns station link state.
    #[must_use]
    pub const fn station(&self) -> &StationState {
        &self.station
    }

    /// Returns access-point state.
    #[must_use]
    pub const fn access_point(&self) -> &AccessPointState {
        &self.access_point
    }
}

trait WifiBackend {
    fn capabilities(&self) -> WifiCapabilities;

    fn status(&self) -> WifiStatus;

    fn scan(&self) -> WifiControlFuture<'_, Vec<VisibleNetwork>>;

    fn start_access_point<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()>;

    fn stop_access_point(&self) -> WifiControlFuture<'_, ()>;

    fn connect_station<'a>(&'a self, ssid: &'a str, password: &'a str)
    -> WifiControlFuture<'a, ()>;

    fn disconnect_station(&self) -> WifiControlFuture<'_, ()>;
}

struct PlatformWifiBackend<Device: WifiDevice> {
    device: Rc<Mutex<NoopRawMutex, Device>>,
    status: Rc<RefCell<WifiStatus>>,
}

impl<Device: WifiDevice> PlatformWifiBackend<Device> {
    fn new(device: Device) -> Self {
        let status = WifiStatus {
            capabilities: device.capabilities(),
            station: device.station_state(),
            access_point: device.access_point_state(),
        };
        Self {
            device: Rc::new(Mutex::new(device)),
            status: Rc::new(RefCell::new(status)),
        }
    }

    fn refresh(&self, device: &Device) {
        self.status.replace(WifiStatus {
            capabilities: device.capabilities(),
            station: device.station_state(),
            access_point: device.access_point_state(),
        });
    }
}

impl<Device: WifiDevice> WifiBackend for PlatformWifiBackend<Device> {
    fn capabilities(&self) -> WifiCapabilities {
        self.status.borrow().capabilities
    }

    fn status(&self) -> WifiStatus {
        self.status.borrow().clone()
    }

    fn scan(&self) -> WifiControlFuture<'_, Vec<VisibleNetwork>> {
        Box::pin(async move {
            self.device
                .lock()
                .await
                .scan()
                .await
                .map_err(WifiControlError::platform)
        })
    }

    fn start_access_point<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()> {
        Box::pin(async move {
            let configuration = AccessPointConfiguration::new(ssid, password);
            let mut device = self.device.lock().await;
            device
                .start_access_point(&configuration)
                .await
                .map_err(WifiControlError::platform)?;
            self.refresh(&device);
            Ok(())
        })
    }

    fn stop_access_point(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            let mut device = self.device.lock().await;
            device
                .stop_access_point()
                .await
                .map_err(WifiControlError::platform)?;
            self.refresh(&device);
            Ok(())
        })
    }

    fn connect_station<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()> {
        Box::pin(async move {
            let configuration = StationConfiguration::new(ssid, password);
            let mut device = self.device.lock().await;
            device
                .connect_station(&configuration)
                .await
                .map_err(WifiControlError::platform)?;
            self.refresh(&device);
            Ok(())
        })
    }

    fn disconnect_station(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            let mut device = self.device.lock().await;
            device
                .disconnect_station()
                .await
                .map_err(WifiControlError::platform)?;
            self.refresh(&device);
            Ok(())
        })
    }
}

/// Typed Plugin capability controlling access-point and station modes.
#[derive(Clone)]
pub struct WifiControl {
    backend: Rc<dyn WifiBackend>,
}

impl WifiControl {
    /// Takes ownership of one Platform Wi-Fi mechanism.
    #[must_use]
    pub fn new<Device: WifiDevice>(device: Device) -> Self {
        Self {
            backend: Rc::new(PlatformWifiBackend::new(device)),
        }
    }

    /// Returns the selected Platform's supported operations.
    #[must_use]
    pub fn capabilities(&self) -> WifiCapabilities {
        self.backend.capabilities()
    }

    /// Returns the latest state observed after construction or a control operation.
    #[must_use]
    pub fn status(&self) -> WifiStatus {
        self.backend.status()
    }

    /// Scans for nearby networks.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's scan failure.
    pub async fn scan(&self) -> Result<Vec<VisibleNetwork>, WifiControlError> {
        self.backend.scan().await
    }

    /// Starts access-point mode.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's AP configuration failure.
    pub async fn start_access_point(
        &self,
        ssid: &str,
        password: &str,
    ) -> Result<(), WifiControlError> {
        self.backend.start_access_point(ssid, password).await
    }

    /// Stops access-point mode.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's AP shutdown failure.
    pub async fn stop_access_point(&self) -> Result<(), WifiControlError> {
        self.backend.stop_access_point().await
    }

    /// Connects station mode to one configured network.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's station connection failure.
    pub async fn connect_station(
        &self,
        ssid: &str,
        password: &str,
    ) -> Result<(), WifiControlError> {
        self.backend.connect_station(ssid, password).await
    }

    /// Disconnects station mode.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's station disconnection failure.
    pub async fn disconnect_station(&self) -> Result<(), WifiControlError> {
        self.backend.disconnect_station().await
    }
}
