use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_platform::{
    AccessPointConfiguration, AccessPointState, StationConfiguration, StationState, VisibleNetwork,
    WifiCapabilities, WifiDevice,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

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

/// Serialized control of the selected Platform's AP and station mechanisms.
pub struct WifiControl<Device: WifiDevice> {
    device: Rc<Mutex<NoopRawMutex, Device>>,
    status: Rc<RefCell<WifiStatus>>,
}

impl<Device: WifiDevice> Clone for WifiControl<Device> {
    fn clone(&self) -> Self {
        Self {
            device: Rc::clone(&self.device),
            status: Rc::clone(&self.status),
        }
    }
}

impl<Device: WifiDevice> WifiControl<Device> {
    /// Takes ownership of one Platform Wi-Fi mechanism.
    #[must_use]
    pub fn new(device: Device) -> Self {
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

    /// Returns the selected Platform's supported operations.
    #[must_use]
    pub fn capabilities(&self) -> WifiCapabilities {
        self.status.borrow().capabilities
    }

    /// Returns the latest state observed after construction or a control operation.
    #[must_use]
    pub fn status(&self) -> WifiStatus {
        self.status.borrow().clone()
    }

    /// Scans for nearby networks.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's scan failure.
    pub async fn scan(&self) -> Result<Vec<VisibleNetwork>, Device::Error> {
        self.device.lock().await.scan().await
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
    ) -> Result<(), Device::Error> {
        let configuration = AccessPointConfiguration::new(ssid, password);
        let mut device = self.device.lock().await;
        device.start_access_point(&configuration).await?;
        self.refresh(&device);
        Ok(())
    }

    /// Stops access-point mode.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's AP shutdown failure.
    pub async fn stop_access_point(&self) -> Result<(), Device::Error> {
        let mut device = self.device.lock().await;
        device.stop_access_point().await?;
        self.refresh(&device);
        Ok(())
    }

    /// Connects station mode to one configured network.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's station connection failure.
    pub async fn connect_station(&self, ssid: &str, password: &str) -> Result<(), Device::Error> {
        let configuration = StationConfiguration::new(ssid, password);
        let mut device = self.device.lock().await;
        device.connect_station(&configuration).await?;
        self.refresh(&device);
        Ok(())
    }

    /// Disconnects station mode.
    ///
    /// # Errors
    ///
    /// Returns the selected Platform's station disconnection failure.
    pub async fn disconnect_station(&self) -> Result<(), Device::Error> {
        let mut device = self.device.lock().await;
        device.disconnect_station().await?;
        self.refresh(&device);
        Ok(())
    }

    fn refresh(&self, device: &Device) {
        self.status.replace(WifiStatus {
            capabilities: device.capabilities(),
            station: device.station_state(),
            access_point: device.access_point_state(),
        });
    }
}
