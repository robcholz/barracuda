extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use barracuda_platform::{
    AccessPointConfiguration, AccessPointState, StationConfiguration, StationState, VisibleNetwork,
    WifiCapabilities, WifiDevice,
};
use embassy_net::Stack;
use embassy_time::{Duration, WithTimeout};
use esp_radio::wifi::{
    ap::AccessPointConfig, scan::ScanConfig, sta::StationConfig, AuthenticationMethod,
    AuthenticationMethodConfig, Config, ConnectionError, WifiController, WifiError,
};

const DHCP_TIMEOUT: Duration = Duration::from_secs(20);

/// ESP32-S3 radio mechanism controlled by the portable Wi-Fi Plugin.
pub struct Esp32S3WifiDevice {
    controller: WifiController<'static>,
    station_stack: Stack<'static>,
    access_point_stack: Stack<'static>,
    station_configuration: Option<StationConfig>,
    access_point_configuration: Option<AccessPointConfig>,
    station_state: StationState,
    access_point_state: AccessPointState,
}

impl Esp32S3WifiDevice {
    pub(crate) const fn new(
        controller: WifiController<'static>,
        station_stack: Stack<'static>,
        access_point_stack: Stack<'static>,
    ) -> Self {
        Self {
            controller,
            station_stack,
            access_point_stack,
            station_configuration: None,
            access_point_configuration: None,
            station_state: StationState::Disconnected,
            access_point_state: AccessPointState::Stopped,
        }
    }

    fn station_config(
        configuration: &StationConfiguration,
    ) -> Result<StationConfig, Esp32S3WifiError> {
        let authentication = authentication(configuration.password())?;
        Ok(StationConfig::default()
            .with_ssid(configuration.ssid().try_into()?)
            .with_authentication(authentication))
    }

    fn access_point_config(
        configuration: &AccessPointConfiguration,
    ) -> Result<AccessPointConfig, Esp32S3WifiError> {
        let authentication = authentication(configuration.password())?;
        Ok(AccessPointConfig::default()
            .with_ssid(configuration.ssid().try_into()?)
            .with_authentication(authentication))
    }

    fn active_config(&self) -> Config {
        match (
            self.station_configuration.as_ref(),
            self.access_point_configuration.as_ref(),
        ) {
            (Some(station), Some(access_point)) => {
                Config::AccessPointStation(station.clone(), access_point.clone())
            }
            (Some(station), None) => Config::Station(station.clone()),
            (None, Some(access_point)) => Config::AccessPoint(access_point.clone()),
            (None, None) => Config::Station(StationConfig::default()),
        }
    }
}

fn authentication(password: &str) -> Result<AuthenticationMethodConfig, Esp32S3WifiError> {
    if password.is_empty() {
        Ok(AuthenticationMethodConfig::Open)
    } else {
        Ok(AuthenticationMethodConfig::Wpa2Personal(
            password.try_into()?,
        ))
    }
}

impl WifiDevice for Esp32S3WifiDevice {
    type Error = Esp32S3WifiError;

    fn capabilities(&self) -> WifiCapabilities {
        WifiCapabilities::managed()
    }

    fn access_point_stack(&self) -> Stack<'static> {
        self.access_point_stack
    }

    fn station_state(&self) -> StationState {
        self.station_state.clone()
    }

    fn access_point_state(&self) -> AccessPointState {
        self.access_point_state.clone()
    }

    async fn scan(&mut self) -> Result<Vec<VisibleNetwork>, Self::Error> {
        let restore_access_point = self
            .station_configuration
            .is_none()
            .then(|| self.access_point_configuration.clone())
            .flatten();
        if self.station_configuration.is_none() {
            let scan_config = match self.access_point_configuration.as_ref() {
                Some(access_point) => {
                    Config::AccessPointStation(StationConfig::default(), access_point.clone())
                }
                None => Config::Station(StationConfig::default()),
            };
            self.controller.set_config(&scan_config)?;
        }
        let scanned = self.controller.scan_async(&ScanConfig::default()).await;
        if let Some(access_point) = restore_access_point {
            self.controller
                .set_config(&Config::AccessPoint(access_point))?;
        }
        Ok(scanned?
            .into_iter()
            .map(|network| VisibleNetwork {
                ssid: network.ssid.as_str().to_string(),
                signal_dbm: Some(network.signal_strength),
                secured: network.auth_method != Some(AuthenticationMethod::None),
            })
            .collect())
    }

    async fn start_access_point(
        &mut self,
        configuration: &AccessPointConfiguration,
    ) -> Result<(), Self::Error> {
        self.access_point_state = AccessPointState::Starting;
        let access_point = Self::access_point_config(configuration)?;
        let config = match self.station_configuration.as_ref() {
            Some(station) => Config::AccessPointStation(station.clone(), access_point.clone()),
            None => Config::AccessPoint(access_point.clone()),
        };
        if let Err(error) = self.controller.set_config(&config) {
            self.access_point_state = AccessPointState::Stopped;
            return Err(error.into());
        }
        self.access_point_configuration = Some(access_point);
        self.access_point_state = AccessPointState::Started {
            ssid: configuration.ssid().to_string(),
        };
        Ok(())
    }

    async fn stop_access_point(&mut self) -> Result<(), Self::Error> {
        self.access_point_configuration = None;
        self.controller.set_config(&self.active_config())?;
        self.access_point_state = AccessPointState::Stopped;
        Ok(())
    }

    async fn connect_station(
        &mut self,
        configuration: &StationConfiguration,
    ) -> Result<(), Self::Error> {
        let station = Self::station_config(configuration)?;
        if !matches!(self.station_state, StationState::Disconnected) {
            match self.controller.disconnect_async().await {
                Ok(_) | Err(WifiError::NotConnected) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.station_configuration = None;
        self.station_state = StationState::Connecting;
        let config = match self.access_point_configuration.as_ref() {
            Some(access_point) => Config::AccessPointStation(station.clone(), access_point.clone()),
            None => Config::Station(station.clone()),
        };
        if let Err(error) = self.controller.set_config(&config) {
            self.station_state = StationState::Disconnected;
            return Err(error.into());
        }
        if let Err(error) = self.controller.connect_async().await {
            self.station_state = StationState::Disconnected;
            return Err(error.into());
        }
        if self
            .station_stack
            .wait_config_up()
            .with_timeout(DHCP_TIMEOUT)
            .await
            .is_err()
        {
            self.station_state = StationState::Disconnected;
            return Err(Esp32S3WifiError::DhcpTimeout);
        }
        self.station_configuration = Some(station);
        self.station_state = StationState::Connected {
            ssid: Some(configuration.ssid().to_string()),
        };
        Ok(())
    }

    async fn disconnect_station(&mut self) -> Result<(), Self::Error> {
        if !matches!(self.station_state, StationState::Disconnected) {
            match self.controller.disconnect_async().await {
                Ok(_) | Err(WifiError::NotConnected) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.station_configuration = None;
        self.controller.set_config(&self.active_config())?;
        self.station_state = StationState::Disconnected;
        Ok(())
    }
}

/// ESP32-S3 radio operation failure.
#[derive(Debug)]
pub enum Esp32S3WifiError {
    /// The ESP radio rejected a mode or configuration operation.
    Radio(WifiError),
    /// Station association failed.
    Connection(ConnectionError),
    /// Station association succeeded but DHCP did not produce an address.
    DhcpTimeout,
}

impl From<WifiError> for Esp32S3WifiError {
    fn from(error: WifiError) -> Self {
        Self::Radio(error)
    }
}

impl From<ConnectionError> for Esp32S3WifiError {
    fn from(error: ConnectionError) -> Self {
        Self::Connection(error)
    }
}

impl core::fmt::Display for Esp32S3WifiError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Radio(error) => write!(formatter, "ESP radio error: {error}"),
            Self::Connection(error) => write!(formatter, "ESP station connection error: {error}"),
            Self::DhcpTimeout => formatter.write_str("ESP station DHCP timed out"),
        }
    }
}

impl core::error::Error for Esp32S3WifiError {}
