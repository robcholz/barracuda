use alloc::{boxed::Box, string::String, vec::Vec};

use alloc::rc::Rc;
use barracuda_platform::{
    AccessPointState, StationConfiguration, StationState, VisibleNetwork, WifiCapabilities,
    WifiDevice,
};
use barracuda_plugin::manager::{PluginError, PluginResult, PluginStorage};
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use serde::{Deserialize, Serialize};

use crate::{SETUP_AP_PASSWORD, SETUP_AP_SSID, WifiControl};

pub(crate) const WIFI_API_PATH: &str = "/api/wifi";
pub(crate) const WIFI_SCAN_API_PATH: &str = "/api/wifi/scan";
const CONFIGURATION_STORAGE_KEY: &str = "station";
const JSON_CONTENT_TYPE: &str = "application/json";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredConfiguration {
    ssid: String,
    password: String,
}

impl StoredConfiguration {
    fn valid(&self) -> bool {
        let ssid_length = self.ssid.len();
        let password_length = self.password.len();
        (1..=32).contains(&ssid_length)
            && (password_length == 0 || (8..=63).contains(&password_length))
    }
}

impl From<StoredConfiguration> for StationConfiguration {
    fn from(configuration: StoredConfiguration) -> Self {
        Self::new(configuration.ssid, configuration.password)
    }
}

pub(crate) struct WifiEndpoint<Storage, Device: WifiDevice> {
    control: WifiControl<Device>,
    access_point_shutdown: Rc<Signal<NoopRawMutex, ()>>,
    storage: Storage,
}

impl<Storage, Device: WifiDevice> WifiEndpoint<Storage, Device> {
    pub(crate) const fn new(
        control: WifiControl<Device>,
        access_point_shutdown: Rc<Signal<NoopRawMutex, ()>>,
        storage: Storage,
    ) -> Self {
        Self {
            control,
            access_point_shutdown,
            storage,
        }
    }

    fn static_response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }

    fn json_response<T: Serialize>(status: u16, value: &T) -> HttpResponse {
        match serde_json::to_vec(value) {
            Ok(body) => HttpResponse::new(status, JSON_CONTENT_TYPE, body),
            Err(error) => {
                log::error!("failed to encode Wi-Fi response: {error}");
                Self::static_response(500, br#"{"error":"encoding"}"#)
            }
        }
    }
}

impl<Storage: PluginStorage, Device: WifiDevice> HttpEndpoint for WifiEndpoint<Storage, Device> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Get => {
                    Self::json_response(200, &StatusResponse::from(self.control.status()))
                }
                HttpMethod::Put => {
                    if !self.control.capabilities().station_configuration {
                        return Self::static_response(409, br#"{"error":"platform_managed"}"#);
                    }
                    let Ok(configuration) =
                        serde_json::from_slice::<StoredConfiguration>(request.body())
                    else {
                        return Self::static_response(400, br#"{"error":"invalid_request"}"#);
                    };
                    if !configuration.valid() {
                        return Self::static_response(422, br#"{"error":"invalid_configuration"}"#);
                    }
                    if let Err(error) = self
                        .control
                        .connect_station(&configuration.ssid, &configuration.password)
                        .await
                    {
                        log::warn!("Wi-Fi station connection failed: {error}");
                        if let Err(access_point_error) = self
                            .control
                            .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
                            .await
                        {
                            log::error!(
                                "failed to keep the provisioning AP available: {access_point_error}"
                            );
                        }
                        return Self::static_response(422, br#"{"error":"connection_failed"}"#);
                    }
                    let Ok(encoded) = serde_json::to_vec(&configuration) else {
                        return Self::static_response(500, br#"{"error":"encoding"}"#);
                    };
                    if let Err(error) = self
                        .storage
                        .put(CONFIGURATION_STORAGE_KEY, encoded.as_slice())
                        .await
                    {
                        log::error!("failed to persist Wi-Fi station configuration: {error}");
                        return Self::static_response(500, br#"{"error":"storage"}"#);
                    }
                    self.access_point_shutdown.signal(());
                    log::info!("configured Wi-Fi station for {}", configuration.ssid);
                    Self::static_response(204, b"")
                }
                HttpMethod::Delete => {
                    if !self.control.capabilities().station_configuration {
                        return Self::static_response(409, br#"{"error":"platform_managed"}"#);
                    }
                    if let Err(error) = self.storage.delete(CONFIGURATION_STORAGE_KEY).await {
                        log::error!("failed to forget Wi-Fi station configuration: {error}");
                        return Self::static_response(500, br#"{"error":"storage"}"#);
                    }
                    if let Err(error) = self.control.disconnect_station().await {
                        log::warn!("failed to disconnect Wi-Fi station: {error}");
                        return Self::static_response(500, br#"{"error":"wifi"}"#);
                    }
                    if let Err(error) = self
                        .control
                        .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
                        .await
                    {
                        log::warn!(
                            "failed to start provisioning AP after forgetting network: {error}"
                        );
                        return Self::static_response(500, br#"{"error":"wifi"}"#);
                    }
                    Self::static_response(204, b"")
                }
                _ => Self::static_response(405, br#"{"error":"method_not_allowed"}"#),
            }
        })
    }
}

pub(crate) struct ScanEndpoint<Device: WifiDevice> {
    control: WifiControl<Device>,
}

impl<Device: WifiDevice> ScanEndpoint<Device> {
    pub(crate) const fn new(control: WifiControl<Device>) -> Self {
        Self { control }
    }
}

impl<Device: WifiDevice> HttpEndpoint for ScanEndpoint<Device> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Get {
                return WifiEndpoint::<(), Device>::static_response(
                    405,
                    br#"{"error":"method_not_allowed"}"#,
                );
            }
            if !self.control.capabilities().scanning {
                return WifiEndpoint::<(), Device>::static_response(
                    409,
                    br#"{"error":"platform_managed"}"#,
                );
            }
            match self.control.scan().await {
                Ok(networks) => WifiEndpoint::<(), Device>::json_response(
                    200,
                    &networks
                        .into_iter()
                        .map(NetworkResponse::from)
                        .collect::<Vec<_>>(),
                ),
                Err(error) => {
                    log::warn!("Wi-Fi scan failed: {error}");
                    WifiEndpoint::<(), Device>::static_response(500, br#"{"error":"wifi"}"#)
                }
            }
        })
    }
}

#[derive(Serialize)]
struct StatusResponse {
    capabilities: CapabilitiesResponse,
    station: StationResponse,
    access_point: AccessPointResponse,
}

impl From<crate::WifiStatus> for StatusResponse {
    fn from(status: crate::WifiStatus) -> Self {
        Self {
            capabilities: status.capabilities().into(),
            station: status.station().clone().into(),
            access_point: status.access_point().clone().into(),
        }
    }
}

#[derive(Serialize)]
struct CapabilitiesResponse {
    access_point: bool,
    scanning: bool,
    station_configuration: bool,
}

impl From<WifiCapabilities> for CapabilitiesResponse {
    fn from(capabilities: WifiCapabilities) -> Self {
        Self {
            access_point: capabilities.access_point,
            scanning: capabilities.scanning,
            station_configuration: capabilities.station_configuration,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum StationResponse {
    Disconnected,
    Connecting,
    Connected {
        #[serde(skip_serializing_if = "Option::is_none")]
        ssid: Option<String>,
    },
}

impl From<StationState> for StationResponse {
    fn from(state: StationState) -> Self {
        match state {
            StationState::Disconnected => Self::Disconnected,
            StationState::Connecting => Self::Connecting,
            StationState::Connected { ssid } => Self::Connected { ssid },
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AccessPointResponse {
    Stopped,
    Starting,
    Started { ssid: String },
}

impl From<AccessPointState> for AccessPointResponse {
    fn from(state: AccessPointState) -> Self {
        match state {
            AccessPointState::Stopped => Self::Stopped,
            AccessPointState::Starting => Self::Starting,
            AccessPointState::Started { ssid } => Self::Started { ssid },
        }
    }
}

#[derive(Serialize)]
struct NetworkResponse {
    ssid: String,
    signal_dbm: Option<i8>,
    secured: bool,
}

impl From<VisibleNetwork> for NetworkResponse {
    fn from(network: VisibleNetwork) -> Self {
        Self {
            ssid: network.ssid,
            signal_dbm: network.signal_dbm,
            secured: network.secured,
        }
    }
}

pub(crate) async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<StationConfiguration>> {
    storage
        .get_bytes(CONFIGURATION_STORAGE_KEY)
        .await?
        .map(|bytes| serde_json::from_slice::<StoredConfiguration>(&bytes))
        .transpose()
        .map_err(PluginError::registration)
        .and_then(|configuration| match configuration {
            Some(configuration) if !configuration.valid() => {
                Err(PluginError::registration(InvalidStoredConfiguration))
            }
            configuration => Ok(configuration.map(StationConfiguration::from)),
        })
}

#[derive(Debug, thiserror::Error)]
#[error("stored Wi-Fi configuration is invalid")]
struct InvalidStoredConfiguration;
