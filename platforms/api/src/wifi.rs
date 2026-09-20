use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embassy_net::Stack;

/// Wi-Fi operations supported by one Platform implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiCapabilities {
    /// Whether the Platform can create and configure an access point.
    pub access_point: bool,
    /// Whether the Platform can scan for nearby access points.
    pub scanning: bool,
    /// Whether the station connection can be configured by Barracuda.
    pub station_configuration: bool,
}

impl WifiCapabilities {
    /// Capabilities for a Host whose network is managed outside Barracuda.
    #[must_use]
    pub const fn host_managed() -> Self {
        Self {
            access_point: false,
            scanning: false,
            station_configuration: false,
        }
    }

    /// Capabilities for a Platform with controllable AP and station modes.
    #[must_use]
    pub const fn managed() -> Self {
        Self {
            access_point: true,
            scanning: true,
            station_configuration: true,
        }
    }
}

/// Station credentials accepted by a [`WifiDevice`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationConfiguration {
    ssid: String,
    password: String,
}

impl StationConfiguration {
    /// Creates station credentials.
    #[must_use]
    pub fn new(ssid: impl ToString, password: impl ToString) -> Self {
        Self {
            ssid: ssid.to_string(),
            password: password.to_string(),
        }
    }

    /// Returns the configured SSID.
    #[must_use]
    pub fn ssid(&self) -> &str {
        &self.ssid
    }

    /// Returns the configured password.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }
}

/// Access-point configuration accepted by a [`WifiDevice`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessPointConfiguration {
    ssid: String,
    password: String,
}

impl AccessPointConfiguration {
    /// Creates access-point credentials.
    #[must_use]
    pub fn new(ssid: impl ToString, password: impl ToString) -> Self {
        Self {
            ssid: ssid.to_string(),
            password: password.to_string(),
        }
    }

    /// Returns the configured SSID.
    #[must_use]
    pub fn ssid(&self) -> &str {
        &self.ssid
    }

    /// Returns the configured password.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }
}

/// One access point discovered during a station scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibleNetwork {
    /// Network name.
    pub ssid: String,
    /// Received signal strength in dBm when the Platform reports it.
    pub signal_dbm: Option<i8>,
    /// Whether joining the network requires credentials.
    pub secured: bool,
}

/// Current station link state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StationState {
    /// Station mode is not associated with an access point.
    Disconnected,
    /// An association attempt is in progress.
    Connecting,
    /// The station is associated. Host-managed networks have no SSID.
    Connected {
        /// Associated network name, or `None` when managed by the Host.
        ssid: Option<String>,
    },
}

/// Current access-point state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccessPointState {
    /// Access-point mode is stopped.
    Stopped,
    /// Access-point mode is starting.
    Starting,
    /// Access-point mode is accepting stations.
    Started {
        /// Advertised network name.
        ssid: String,
    },
}

/// Platform mechanism used by the Wi-Fi Plugin to control AP and station modes.
///
/// The selected Platform provides one concrete implementation. The Wi-Fi Plugin
/// owns connection policy, persistence, retries, and captive services.
pub trait WifiDevice: 'static {
    /// Platform-specific operation failure.
    type Error: core::error::Error;

    /// Returns operations supported by this implementation.
    fn capabilities(&self) -> WifiCapabilities;

    /// Returns the IP stack serving the access-point side.
    fn access_point_stack(&self) -> Stack<'static>;

    /// Returns the current station state.
    fn station_state(&self) -> StationState;

    /// Returns the current access-point state.
    fn access_point_state(&self) -> AccessPointState;

    /// Scans for nearby access points.
    fn scan(
        &mut self,
    ) -> impl core::future::Future<Output = Result<Vec<VisibleNetwork>, Self::Error>>;

    /// Starts or reconfigures access-point mode.
    fn start_access_point(
        &mut self,
        configuration: &AccessPointConfiguration,
    ) -> impl core::future::Future<Output = Result<(), Self::Error>>;

    /// Stops access-point mode.
    fn stop_access_point(&mut self) -> impl core::future::Future<Output = Result<(), Self::Error>>;

    /// Connects station mode to the configured network.
    fn connect_station(
        &mut self,
        configuration: &StationConfiguration,
    ) -> impl core::future::Future<Output = Result<(), Self::Error>>;

    /// Disconnects station mode.
    fn disconnect_station(&mut self)
        -> impl core::future::Future<Output = Result<(), Self::Error>>;
}

/// Host adapter whose existing IP service is always connected and externally managed.
pub struct HostWifiDevice {
    stack: Stack<'static>,
}

impl HostWifiDevice {
    /// Wraps the Host Platform's already configured IP stack.
    #[must_use]
    pub const fn new(stack: Stack<'static>) -> Self {
        Self { stack }
    }
}

impl WifiDevice for HostWifiDevice {
    type Error = core::convert::Infallible;

    fn capabilities(&self) -> WifiCapabilities {
        WifiCapabilities::host_managed()
    }

    fn access_point_stack(&self) -> Stack<'static> {
        self.stack
    }

    fn station_state(&self) -> StationState {
        StationState::Connected { ssid: None }
    }

    fn access_point_state(&self) -> AccessPointState {
        AccessPointState::Stopped
    }

    async fn scan(&mut self) -> Result<Vec<VisibleNetwork>, Self::Error> {
        Ok(Vec::new())
    }

    async fn start_access_point(
        &mut self,
        _configuration: &AccessPointConfiguration,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn stop_access_point(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn connect_station(
        &mut self,
        _configuration: &StationConfiguration,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn disconnect_station(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Placeholder for a Platform whose Wi-Fi driver has not been implemented.
///
/// Unlike [`HostWifiDevice`], this adapter does not claim that the supplied IP
/// stack is connected. It keeps the cross-Platform resource shape honest while
/// reporting no controllable Wi-Fi capabilities.
pub struct UnavailableWifiDevice {
    stack: Stack<'static>,
}

impl UnavailableWifiDevice {
    /// Wraps the Platform's disconnected placeholder IP stack.
    #[must_use]
    pub const fn new(stack: Stack<'static>) -> Self {
        Self { stack }
    }
}

impl WifiDevice for UnavailableWifiDevice {
    type Error = core::convert::Infallible;

    fn capabilities(&self) -> WifiCapabilities {
        WifiCapabilities::host_managed()
    }

    fn access_point_stack(&self) -> Stack<'static> {
        self.stack
    }

    fn station_state(&self) -> StationState {
        StationState::Disconnected
    }

    fn access_point_state(&self) -> AccessPointState {
        AccessPointState::Stopped
    }

    async fn scan(&mut self) -> Result<Vec<VisibleNetwork>, Self::Error> {
        Ok(Vec::new())
    }

    async fn start_access_point(
        &mut self,
        _configuration: &AccessPointConfiguration,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn stop_access_point(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn connect_station(
        &mut self,
        _configuration: &StationConfiguration,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn disconnect_station(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
