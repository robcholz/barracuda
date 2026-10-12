use alloc::{boxed::Box, string::String, vec::Vec};

use alloc::rc::Rc;
use barracuda_platform::{AccessPointState, StationState, VisibleNetwork, WifiCapabilities};
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};

use crate::{WifiControl, WifiControlErrorKind};

pub(crate) const WIFI_API_PATH: &str = "/api/wifi";
pub(crate) const WIFI_SCAN_API_PATH: &str = "/api/wifi/scan";
pub(crate) const WIFI_STATUS_API_PATH: &str = "/api/wifi/status";
const JSON_CONTENT_TYPE: &str = "application/json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StationRequest {
    ssid: String,
    password: String,
}

pub(crate) struct WifiEndpoint {
    control: Rc<WifiControl>,
}

impl WifiEndpoint {
    pub(crate) const fn new(control: Rc<WifiControl>) -> Self {
        Self { control }
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

impl HttpEndpoint for WifiEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Put => {
                    if !self.control.capabilities().station_configuration {
                        return Self::static_response(409, br#"{"error":"platform_managed"}"#);
                    }
                    let Ok(configuration) =
                        serde_json::from_slice::<StationRequest>(request.body())
                    else {
                        return Self::static_response(400, br#"{"error":"invalid_request"}"#);
                    };
                    if let Err(error) = self
                        .control
                        .configure_station(&configuration.ssid, &configuration.password)
                        .await
                    {
                        log::warn!("Wi-Fi station connection failed: {error}");
                        return match error.kind() {
                            WifiControlErrorKind::InvalidConfiguration => {
                                Self::static_response(422, br#"{"error":"invalid_configuration"}"#)
                            }
                            WifiControlErrorKind::Storage => {
                                Self::static_response(500, br#"{"error":"storage"}"#)
                            }
                            WifiControlErrorKind::Device => {
                                Self::static_response(422, br#"{"error":"connection_failed"}"#)
                            }
                        };
                    };
                    Self::static_response(204, b"")
                }
                HttpMethod::Delete => {
                    if !self.control.capabilities().station_configuration {
                        return Self::static_response(409, br#"{"error":"platform_managed"}"#);
                    }
                    if let Err(error) = self.control.forget_station().await {
                        log::warn!("failed to forget Wi-Fi station: {error}");
                        return match error.kind() {
                            WifiControlErrorKind::Storage => {
                                Self::static_response(500, br#"{"error":"storage"}"#)
                            }
                            WifiControlErrorKind::InvalidConfiguration
                            | WifiControlErrorKind::Device => {
                                Self::static_response(500, br#"{"error":"wifi"}"#)
                            }
                        };
                    }
                    Self::static_response(204, b"")
                }
                _ => Self::static_response(405, br#"{"error":"method_not_allowed"}"#),
            }
        })
    }
}

/// `GET` [`WIFI_STATUS_API_PATH`]: the capabilities, station, and access point.
pub(crate) struct StatusEndpoint {
    control: Rc<WifiControl>,
}

impl StatusEndpoint {
    pub(crate) const fn new(control: Rc<WifiControl>) -> Self {
        Self { control }
    }
}

impl HttpEndpoint for StatusEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Get {
                return WifiEndpoint::static_response(405, br#"{"error":"method_not_allowed"}"#);
            }
            WifiEndpoint::json_response(200, &StatusResponse::from(self.control.status()))
        })
    }
}

pub(crate) struct ScanEndpoint {
    control: Rc<WifiControl>,
}

impl ScanEndpoint {
    pub(crate) const fn new(control: Rc<WifiControl>) -> Self {
        Self { control }
    }
}

impl HttpEndpoint for ScanEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Get {
                return WifiEndpoint::static_response(405, br#"{"error":"method_not_allowed"}"#);
            }
            if !self.control.capabilities().scanning {
                return WifiEndpoint::static_response(409, br#"{"error":"platform_managed"}"#);
            }
            match self.control.scan().await {
                Ok(networks) => WifiEndpoint::json_response(
                    200,
                    &networks
                        .into_iter()
                        .map(NetworkResponse::from)
                        .collect::<Vec<_>>(),
                ),
                Err(error) => {
                    log::warn!("Wi-Fi scan failed: {error}");
                    WifiEndpoint::static_response(500, br#"{"error":"wifi"}"#)
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
