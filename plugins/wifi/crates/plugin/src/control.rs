use alloc::{boxed::Box, rc::Rc, string::String, string::ToString, vec::Vec};
use core::{cell::Cell, future::Future, pin::Pin};

use barracuda_platform::{
    AccessPointConfiguration, AccessPointState, StationConfiguration, StationState, VisibleNetwork,
    WifiCapabilities, WifiDevice,
};
use barracuda_plugin::manager::PluginStorage;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use serde::{Deserialize, Serialize};

use crate::{SETUP_AP_PASSWORD, SETUP_AP_SSID};

const CONFIGURATION_STORAGE_KEY: &str = "station";

type WifiControlFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, WifiControlError>> + 'a>>;

/// Stable category for a [`WifiControl`] operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WifiControlErrorKind {
    /// The supplied SSID or password is outside the supported Wi-Fi bounds.
    InvalidConfiguration,
    /// Plugin-scoped persistent storage failed.
    Storage,
    /// The selected Platform's Wi-Fi mechanism failed.
    Device,
}

/// Failure returned by a [`WifiControl`] domain operation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct WifiControlError {
    kind: WifiControlErrorKind,
    message: String,
}

impl WifiControlError {
    fn new(kind: WifiControlErrorKind, message: impl ToString) -> Self {
        Self {
            kind,
            message: message.to_string(),
        }
    }

    fn device(error: impl core::error::Error) -> Self {
        Self::new(
            WifiControlErrorKind::Device,
            alloc::format!("Platform Wi-Fi operation failed: {error}"),
        )
    }

    fn storage(error: impl core::error::Error) -> Self {
        Self::new(
            WifiControlErrorKind::Storage,
            alloc::format!("Wi-Fi configuration storage failed: {error}"),
        )
    }

    fn invalid_configuration() -> Self {
        Self::new(
            WifiControlErrorKind::InvalidConfiguration,
            "Wi-Fi SSID or password is invalid",
        )
    }

    /// Returns the stable machine-readable failure category.
    #[must_use]
    pub const fn kind(&self) -> WifiControlErrorKind {
        self.kind
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

trait RadioBackend {
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

struct PlatformRadioBackend<Device: WifiDevice> {
    device: Mutex<NoopRawMutex, Device>,
    status: core::cell::RefCell<WifiStatus>,
}

impl<Device: WifiDevice> PlatformRadioBackend<Device> {
    fn new(device: Device) -> Self {
        let status = WifiStatus {
            capabilities: device.capabilities(),
            station: device.station_state(),
            access_point: device.access_point_state(),
        };
        Self {
            device: Mutex::new(device),
            status: core::cell::RefCell::new(status),
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

impl<Device: WifiDevice> RadioBackend for PlatformRadioBackend<Device> {
    fn capabilities(&self) -> WifiCapabilities {
        self.status.borrow().capabilities
    }

    fn status(&self) -> WifiStatus {
        self.status.borrow().clone()
    }

    fn scan(&self) -> WifiControlFuture<'_, Vec<VisibleNetwork>> {
        Box::pin(async move {
            let mut device = self.device.lock().await;
            let result = device.scan().await.map_err(WifiControlError::device);
            self.refresh(&device);
            result
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
            let result = device
                .start_access_point(&configuration)
                .await
                .map_err(WifiControlError::device);
            self.refresh(&device);
            result
        })
    }

    fn stop_access_point(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            let mut device = self.device.lock().await;
            let result = device
                .stop_access_point()
                .await
                .map_err(WifiControlError::device);
            self.refresh(&device);
            result
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
            let result = device
                .connect_station(&configuration)
                .await
                .map_err(WifiControlError::device);
            self.refresh(&device);
            result
        })
    }

    fn disconnect_station(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            let mut device = self.device.lock().await;
            let result = device
                .disconnect_station()
                .await
                .map_err(WifiControlError::device);
            self.refresh(&device);
            result
        })
    }
}

#[derive(Clone)]
pub(crate) struct RadioControl {
    backend: Rc<dyn RadioBackend>,
}

impl RadioControl {
    pub(crate) fn new<Device: WifiDevice>(device: Device) -> Self {
        Self {
            backend: Rc::new(PlatformRadioBackend::new(device)),
        }
    }

    fn capabilities(&self) -> WifiCapabilities {
        self.backend.capabilities()
    }

    fn status(&self) -> WifiStatus {
        self.backend.status()
    }

    async fn scan(&self) -> Result<Vec<VisibleNetwork>, WifiControlError> {
        self.backend.scan().await
    }

    async fn start_access_point(&self, ssid: &str, password: &str) -> Result<(), WifiControlError> {
        self.backend.start_access_point(ssid, password).await
    }

    async fn stop_access_point(&self) -> Result<(), WifiControlError> {
        self.backend.stop_access_point().await
    }

    async fn connect_station(&self, ssid: &str, password: &str) -> Result<(), WifiControlError> {
        self.backend.connect_station(ssid, password).await
    }

    async fn disconnect_station(&self) -> Result<(), WifiControlError> {
        self.backend.disconnect_station().await
    }
}

#[derive(Deserialize, Serialize)]
struct PersistedStationConfiguration {
    ssid: String,
    password: String,
}

impl PersistedStationConfiguration {
    fn new(ssid: &str, password: &str) -> Self {
        Self {
            ssid: ssid.to_string(),
            password: password.to_string(),
        }
    }

    fn valid(&self) -> bool {
        valid_configuration(&self.ssid, &self.password)
    }
}

fn valid_configuration(ssid: &str, password: &str) -> bool {
    (1..=32).contains(&ssid.len()) && (password.is_empty() || (8..=63).contains(&password.len()))
}

trait WifiService {
    fn capabilities(&self) -> WifiCapabilities;
    fn status(&self) -> WifiStatus;
    fn initialize(&self) -> WifiControlFuture<'_, ()>;
    fn scan(&self) -> WifiControlFuture<'_, Vec<VisibleNetwork>>;
    fn start_access_point<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()>;
    fn stop_access_point(&self) -> WifiControlFuture<'_, ()>;
    fn configure_station<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()>;
    fn forget_station(&self) -> WifiControlFuture<'_, ()>;
    fn stop_access_point_if_current(&self, generation: u64) -> WifiControlFuture<'_, bool>;
}

struct ScopedWifiService<Storage: PluginStorage> {
    radio: RadioControl,
    storage: Storage,
    operation: Mutex<NoopRawMutex, ()>,
    initialized: Cell<bool>,
    access_point_generation: Cell<u64>,
    access_point_shutdown: Rc<Signal<NoopRawMutex, u64>>,
}

impl<Storage: PluginStorage> ScopedWifiService<Storage> {
    fn new(
        radio: RadioControl,
        storage: Storage,
        access_point_shutdown: Rc<Signal<NoopRawMutex, u64>>,
    ) -> Self {
        Self {
            radio,
            storage,
            operation: Mutex::new(()),
            initialized: Cell::new(false),
            access_point_generation: Cell::new(0),
            access_point_shutdown,
        }
    }

    fn next_access_point_generation(&self) -> u64 {
        let generation = self.access_point_generation.get().wrapping_add(1);
        self.access_point_generation.set(generation);
        generation
    }

    async fn load_configuration(
        &self,
    ) -> Result<Option<PersistedStationConfiguration>, WifiControlError> {
        let Some(bytes) = self
            .storage
            .get_bytes(CONFIGURATION_STORAGE_KEY)
            .await
            .map_err(WifiControlError::storage)?
        else {
            return Ok(None);
        };
        let configuration = serde_json::from_slice::<PersistedStationConfiguration>(&bytes)
            .map_err(WifiControlError::storage)?;
        if !configuration.valid() {
            return Err(WifiControlError::new(
                WifiControlErrorKind::Storage,
                "stored Wi-Fi configuration is invalid",
            ));
        }
        Ok(Some(configuration))
    }

    async fn initialize_locked(&self) -> Result<(), WifiControlError> {
        if self.initialized.get() {
            return Ok(());
        }

        match self.load_configuration().await {
            Ok(Some(configuration)) => {
                match self
                    .radio
                    .connect_station(&configuration.ssid, &configuration.password)
                    .await
                {
                    Ok(()) => {
                        self.initialized.set(true);
                        log::info!("connected Wi-Fi station to {}", configuration.ssid);
                        return Ok(());
                    }
                    Err(error) => {
                        log::warn!(
                            "failed to restore Wi-Fi station connection to {}: {error}",
                            configuration.ssid
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("failed to restore stored Wi-Fi configuration: {error}");
            }
        }

        self.radio
            .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
            .await?;
        self.next_access_point_generation();
        self.initialized.set(true);
        log::info!("started Wi-Fi provisioning access point {SETUP_AP_SSID}");
        Ok(())
    }

    async fn ensure_initialized(&self) -> Result<(), WifiControlError> {
        self.initialize_locked().await
    }

    async fn keep_setup_access_point_available(&self) {
        match self
            .radio
            .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
            .await
        {
            Ok(()) => {
                self.next_access_point_generation();
            }
            Err(error) => log::error!("failed to keep the provisioning AP available: {error}"),
        }
    }
}

impl<Storage: PluginStorage> WifiService for ScopedWifiService<Storage> {
    fn capabilities(&self) -> WifiCapabilities {
        self.radio.capabilities()
    }

    fn status(&self) -> WifiStatus {
        self.radio.status()
    }

    fn initialize(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            if !self.capabilities().station_configuration {
                return Ok(());
            }
            let _operation = self.operation.lock().await;
            self.ensure_initialized().await
        })
    }

    fn scan(&self) -> WifiControlFuture<'_, Vec<VisibleNetwork>> {
        Box::pin(async move {
            if !self.capabilities().scanning {
                return Ok(Vec::new());
            }
            let _operation = self.operation.lock().await;
            self.ensure_initialized().await?;
            self.radio.scan().await
        })
    }

    fn start_access_point<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()> {
        Box::pin(async move {
            if !self.capabilities().access_point {
                return Ok(());
            }
            if !valid_configuration(ssid, password) {
                return Err(WifiControlError::invalid_configuration());
            }
            let _operation = self.operation.lock().await;
            self.ensure_initialized().await?;
            self.radio.start_access_point(ssid, password).await?;
            self.next_access_point_generation();
            Ok(())
        })
    }

    fn stop_access_point(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            if !self.capabilities().access_point {
                return Ok(());
            }
            let _operation = self.operation.lock().await;
            self.ensure_initialized().await?;
            self.radio.stop_access_point().await?;
            self.next_access_point_generation();
            Ok(())
        })
    }

    fn configure_station<'a>(
        &'a self,
        ssid: &'a str,
        password: &'a str,
    ) -> WifiControlFuture<'a, ()> {
        Box::pin(async move {
            if !self.capabilities().station_configuration {
                return Ok(());
            }
            if !valid_configuration(ssid, password) {
                return Err(WifiControlError::invalid_configuration());
            }
            let _operation = self.operation.lock().await;
            self.ensure_initialized().await?;
            if let Err(error) = self.radio.connect_station(ssid, password).await {
                self.keep_setup_access_point_available().await;
                return Err(error);
            }
            let configuration = PersistedStationConfiguration::new(ssid, password);
            let encoded = serde_json::to_vec(&configuration).map_err(WifiControlError::storage)?;
            self.storage
                .put(CONFIGURATION_STORAGE_KEY, encoded.as_slice())
                .await
                .map_err(WifiControlError::storage)?;
            let generation = self.next_access_point_generation();
            self.access_point_shutdown.signal(generation);
            log::info!("configured Wi-Fi station for {ssid}");
            Ok(())
        })
    }

    fn forget_station(&self) -> WifiControlFuture<'_, ()> {
        Box::pin(async move {
            if !self.capabilities().station_configuration {
                return Ok(());
            }
            let _operation = self.operation.lock().await;
            self.ensure_initialized().await?;
            self.storage
                .delete(CONFIGURATION_STORAGE_KEY)
                .await
                .map_err(WifiControlError::storage)?;
            let disconnect = self.radio.disconnect_station().await;
            let access_point = self
                .radio
                .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
                .await;
            if access_point.is_ok() {
                self.next_access_point_generation();
            }
            disconnect?;
            access_point
        })
    }

    fn stop_access_point_if_current(&self, generation: u64) -> WifiControlFuture<'_, bool> {
        Box::pin(async move {
            let _operation = self.operation.lock().await;
            if self.access_point_generation.get() != generation {
                return Ok(false);
            }
            self.radio.stop_access_point().await?;
            self.next_access_point_generation();
            Ok(true)
        })
    }
}

/// Typed Plugin capability owning Wi-Fi policy, persistence, and AP/STA control.
pub struct WifiControl {
    service: Rc<dyn WifiService>,
}

impl WifiControl {
    pub(crate) fn new<Storage: PluginStorage>(
        radio: RadioControl,
        storage: Storage,
        access_point_shutdown: Rc<Signal<NoopRawMutex, u64>>,
    ) -> Self {
        Self {
            service: Rc::new(ScopedWifiService::new(
                radio,
                storage,
                access_point_shutdown,
            )),
        }
    }

    /// Returns the selected Platform's supported operations.
    #[must_use]
    pub fn capabilities(&self) -> WifiCapabilities {
        self.service.capabilities()
    }

    /// Returns the latest state observed after construction or a control operation.
    #[must_use]
    pub fn status(&self) -> WifiStatus {
        self.service.status()
    }

    /// Scans for nearby networks.
    ///
    /// # Errors
    ///
    /// Returns [`WifiControlError`] when initialization or the scan fails.
    pub async fn scan(&self) -> Result<Vec<VisibleNetwork>, WifiControlError> {
        self.service.scan().await
    }

    /// Starts or reconfigures access-point mode.
    ///
    /// # Errors
    ///
    /// Returns [`WifiControlError`] for invalid credentials or a device failure.
    pub async fn start_access_point(
        &self,
        ssid: &str,
        password: &str,
    ) -> Result<(), WifiControlError> {
        self.service.start_access_point(ssid, password).await
    }

    /// Stops access-point mode.
    ///
    /// # Errors
    ///
    /// Returns [`WifiControlError`] when initialization or shutdown fails.
    pub async fn stop_access_point(&self) -> Result<(), WifiControlError> {
        self.service.stop_access_point().await
    }

    /// Connects station mode and persists the configuration on success.
    ///
    /// The provisioning access point remains available after a failed connection
    /// and closes after the successful caller has had time to receive its response.
    ///
    /// # Errors
    ///
    /// Returns [`WifiControlError`] for invalid credentials, storage failure, or
    /// a device failure.
    pub async fn configure_station(
        &self,
        ssid: &str,
        password: &str,
    ) -> Result<(), WifiControlError> {
        self.service.configure_station(ssid, password).await
    }

    /// Deletes the persisted station configuration, disconnects, and opens the setup AP.
    ///
    /// # Errors
    ///
    /// Returns [`WifiControlError`] when storage or a device operation fails.
    pub async fn forget_station(&self) -> Result<(), WifiControlError> {
        self.service.forget_station().await
    }

    pub(crate) async fn initialize(&self) -> Result<(), WifiControlError> {
        self.service.initialize().await
    }

    pub(crate) async fn stop_access_point_if_current(
        &self,
        generation: u64,
    ) -> Result<bool, WifiControlError> {
        self.service.stop_access_point_if_current(generation).await
    }
}
