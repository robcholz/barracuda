//! The RPC messages this host exchanges with the co-processor.
//!
//! Every message is a protobuf `Rpc` (ESP-Hosted-MCU `esp_hosted_rpc.proto`,
//! `rpc_v2.proto` from 3.x): `msg_type` (1), `msg_id` (2), `uid` (3), and a
//! `oneof payload` whose field number equals `msg_id`. A request with ID
//! `0x1xx` is answered by the response `0x2xx` with the same `uid`; events use
//! IDs from `0x301`. The messages and fields used here have the same numbers
//! in every release from 0.0.6 to 3.x; later releases only append fields,
//! which earlier co-processors skip.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::protobuf::{DecodeError, Reader, Value, Writer};

const TYPE_REQUEST: u64 = 1;
const TYPE_RESPONSE: u64 = 2;
const TYPE_EVENT: u64 = 3;

/// `esp_wifi_set_mode` modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiMode {
    /// Station only.
    Station = 1,
    /// Soft access point only.
    AccessPoint = 2,
    /// Station and soft access point.
    AccessPointStation = 3,
}

/// `wifi_interface_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiInterface {
    /// The station interface.
    Station = 0,
    /// The soft access point interface.
    AccessPoint = 1,
}

/// `wifi_auth_mode_t` values this host sets on its access point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    /// No authentication.
    Open = 0,
    /// WPA2-Personal.
    Wpa2Personal = 3,
}

/// One request to the co-processor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `esp_wifi_get_mac(interface)`.
    GetMacAddress(WifiInterface),
    /// `esp_wifi_set_mode`.
    SetMode(WifiMode),
    /// `esp_wifi_init` with the host's `wifi_init_config_t`.
    WifiInit,
    /// `esp_wifi_start`.
    WifiStart,
    /// `esp_wifi_stop`.
    WifiStop,
    /// `esp_wifi_connect`.
    WifiConnect,
    /// `esp_wifi_disconnect`.
    WifiDisconnect,
    /// `esp_wifi_set_config(WIFI_IF_STA, ...)`.
    SetStationConfig {
        /// Network name, at most 32 bytes.
        ssid: Vec<u8>,
        /// Passphrase, at most 64 bytes; empty for an open network.
        password: Vec<u8>,
    },
    /// `esp_wifi_set_config(WIFI_IF_AP, ...)`.
    SetAccessPointConfig {
        /// Network name, at most 32 bytes.
        ssid: Vec<u8>,
        /// Passphrase of 8 to 64 bytes, or empty for an open network.
        password: Vec<u8>,
        /// Primary channel.
        channel: u8,
        /// Stations allowed at once.
        max_connections: u8,
    },
    /// `esp_wifi_scan_start` with the default scan, not blocking.
    ScanStart,
    /// `esp_wifi_scan_get_ap_records` for at most this many records.
    ScanGetApRecords(u16),
    /// `esp_wifi_sta_get_ap_info`.
    StationGetApInfo,
    /// The co-processor firmware version (ESP-Hosted 2.x and later).
    GetCoprocessorFirmwareVersion,
}

/// Request message IDs (`RpcId`).
mod id {
    pub const GET_MAC_ADDRESS: u32 = 0x101;
    pub const SET_WIFI_MODE: u32 = 0x104;
    pub const WIFI_INIT: u32 = 0x116;
    pub const WIFI_START: u32 = 0x118;
    pub const WIFI_STOP: u32 = 0x119;
    pub const WIFI_CONNECT: u32 = 0x11a;
    pub const WIFI_DISCONNECT: u32 = 0x11b;
    pub const WIFI_SET_CONFIG: u32 = 0x11c;
    pub const WIFI_SCAN_START: u32 = 0x11e;
    pub const WIFI_SCAN_GET_AP_RECORDS: u32 = 0x121;
    pub const WIFI_STA_GET_AP_INFO: u32 = 0x126;
    pub const GET_COPROCESSOR_FW_VERSION: u32 = 0x15e;

    pub const EVENT_ESP_INIT: u32 = 0x301;
    pub const EVENT_HEARTBEAT: u32 = 0x302;
    pub const EVENT_AP_STA_CONNECTED: u32 = 0x303;
    pub const EVENT_AP_STA_DISCONNECTED: u32 = 0x304;
    pub const EVENT_WIFI_NO_ARGS: u32 = 0x305;
    pub const EVENT_STA_SCAN_DONE: u32 = 0x306;
    pub const EVENT_STA_CONNECTED: u32 = 0x307;
    pub const EVENT_STA_DISCONNECTED: u32 = 0x308;
}

/// Offset from a request ID to its response ID.
const RESPONSE_OFFSET: u32 = 0x100;

/// `wifi_init_config_t` the host sends with [`Request::WifiInit`]: ESP-IDF's
/// `WIFI_INIT_CONFIG_DEFAULT()` with esp_wifi_remote's Kconfig defaults
/// (`Kconfig.wifi.in`, no PSRAM), except that the co-processor keeps no
/// Wi-Fi settings in its flash (`nvs_enable` 0): the host owns credentials.
/// (field, value) pairs in `wifi_init_config` field order.
const WIFI_INIT_CONFIG: [(u32, i64); 24] = [
    (1, 10),          // static_rx_buf_num
    (2, 32),          // dynamic_rx_buf_num
    (3, 1),           // tx_buf_type: dynamic
    (4, 0),           // static_tx_buf_num: none with dynamic buffers
    (5, 32),          // dynamic_tx_buf_num
    (6, 0),           // cache_tx_buf_num: no PSRAM cache
    (7, 0),           // csi_enable
    (8, 1),           // ampdu_rx_enable
    (9, 1),           // ampdu_tx_enable
    (10, 0),          // amsdu_tx_enable
    (11, 0),          // nvs_enable
    (12, 0),          // nano_enable
    (13, 6),          // rx_ba_win
    (14, 0),          // wifi_task_core_id
    (15, 752),        // beacon_max_len
    (16, 32),         // mgmt_sbuf_num
    (17, 1),          // feature_caps: WPA3-SAE; the co-processor keeps its own
    (18, 1),          // sta_disconnected_pm
    (19, 7),          // espnow_max_encrypt_num
    (20, 0x1F2F3F4F), // magic: WIFI_INIT_CONFIG_MAGIC
    (21, 0),          // rx_mgmt_buf_type: static
    (22, 5),          // rx_mgmt_buf_num
    (23, 3),          // tx_hetb_queue_num
    (24, 0),          // dump_hesigb_enable
];

impl Request {
    /// Returns the request message ID.
    #[must_use]
    pub const fn id(&self) -> u32 {
        match self {
            Self::GetMacAddress(_) => id::GET_MAC_ADDRESS,
            Self::SetMode(_) => id::SET_WIFI_MODE,
            Self::WifiInit => id::WIFI_INIT,
            Self::WifiStart => id::WIFI_START,
            Self::WifiStop => id::WIFI_STOP,
            Self::WifiConnect => id::WIFI_CONNECT,
            Self::WifiDisconnect => id::WIFI_DISCONNECT,
            Self::SetStationConfig { .. } | Self::SetAccessPointConfig { .. } => {
                id::WIFI_SET_CONFIG
            }
            Self::ScanStart => id::WIFI_SCAN_START,
            Self::ScanGetApRecords(_) => id::WIFI_SCAN_GET_AP_RECORDS,
            Self::StationGetApInfo => id::WIFI_STA_GET_AP_INFO,
            Self::GetCoprocessorFirmwareVersion => id::GET_COPROCESSOR_FW_VERSION,
        }
    }

    /// Returns the ID of the response that answers this request.
    #[must_use]
    pub const fn response_id(&self) -> u32 {
        self.id() + RESPONSE_OFFSET
    }

    /// Encodes the request as an `Rpc` message with `uid`.
    #[must_use]
    pub fn encode(&self, uid: u32) -> Vec<u8> {
        let mut rpc = Writer::new();
        rpc.uint(1, TYPE_REQUEST)
            .uint(2, u64::from(self.id()))
            .uint(3, u64::from(uid));
        rpc.message(self.id(), |payload| match self {
            Self::GetMacAddress(interface) => {
                payload.int(1, *interface as i32);
            }
            Self::SetMode(mode) => {
                payload.int(1, *mode as i32);
            }
            Self::WifiInit => {
                payload.message(1, |config| {
                    for (field, value) in WIFI_INIT_CONFIG {
                        config.uint(field, value as u64);
                    }
                });
            }
            Self::SetStationConfig { ssid, password } => {
                payload
                    .int(1, WifiInterface::Station as i32)
                    .message(2, |config| {
                        config.message(2, |station| {
                            station.bytes(1, ssid).bytes(2, password);
                        });
                    });
            }
            Self::SetAccessPointConfig {
                ssid,
                password,
                channel,
                max_connections,
            } => {
                let auth = if password.is_empty() {
                    AuthMode::Open
                } else {
                    AuthMode::Wpa2Personal
                };
                payload
                    .int(1, WifiInterface::AccessPoint as i32)
                    .message(2, |config| {
                        config.message(1, |access_point| {
                            access_point
                                .bytes(1, ssid)
                                .bytes(2, password)
                                .uint(3, ssid.len() as u64)
                                .uint(4, u64::from(*channel))
                                .int(5, auth as i32)
                                .uint(7, u64::from(*max_connections))
                                .uint(8, 100);
                        });
                    });
            }
            Self::ScanStart => {
                // No `config`, `config_set` 0: the co-processor runs
                // `esp_wifi_scan_start(NULL, false)` and reports
                // `Event_StaScanDone`.
            }
            Self::ScanGetApRecords(number) => {
                payload.int(1, i32::from(*number));
            }
            Self::WifiStart
            | Self::WifiStop
            | Self::WifiConnect
            | Self::WifiDisconnect
            | Self::StationGetApInfo
            | Self::GetCoprocessorFirmwareVersion => {}
        });
        rpc.finish()
    }
}

/// One received `Rpc` message, payload still encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message<'a> {
    /// Whether it is a response or an event.
    pub kind: MessageKind,
    /// Its message ID.
    pub id: u32,
    /// The `uid` of the request it answers.
    pub uid: u32,
    /// The encoded payload message.
    pub payload: &'a [u8],
}

/// The `msg_type` of a received message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    /// A request, which a co-processor never sends.
    Request,
    /// A response.
    Response,
    /// An event.
    Event,
}

impl<'a> Message<'a> {
    /// Decodes an `Rpc` message.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed protobuf or an unknown message type.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, RpcError> {
        let mut kind = None;
        let mut id = 0;
        let mut uid = 0;
        let mut payloads = Vec::new();
        for field in Reader::new(bytes) {
            match field? {
                (1, value) => {
                    kind = Some(match value.as_u64() {
                        TYPE_REQUEST => MessageKind::Request,
                        TYPE_RESPONSE => MessageKind::Response,
                        TYPE_EVENT => MessageKind::Event,
                        other => return Err(RpcError::UnknownType(other)),
                    });
                }
                (2, value) => id = value.as_u32(),
                (3, value) => uid = value.as_u32(),
                (field, Value::Bytes(payload)) => payloads.push((field, payload)),
                _ => {}
            }
        }
        let kind = kind.ok_or(RpcError::UnknownType(0))?;
        // The payload is the `oneof` member numbered like the message ID;
        // an empty payload message is legal and leaves no bytes.
        let payload = payloads
            .into_iter()
            .find(|(field, _)| *field == id)
            .map_or(&[][..], |(_, payload)| payload);
        Ok(Self {
            kind,
            id,
            uid,
            payload,
        })
    }

    /// Returns the `esp_err_t` a response or event carries in its `resp`
    /// field: field 2 in the MAC-address response, 1 everywhere else.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed payload.
    pub fn status(&self) -> Result<i32, RpcError> {
        let field = if self.id == id::GET_MAC_ADDRESS + RESPONSE_OFFSET {
            2
        } else {
            1
        };
        let mut status = 0;
        for entry in Reader::new(self.payload) {
            let (number, value) = entry?;
            if number == field {
                status = value.as_i32();
            }
        }
        Ok(status)
    }
}

/// An access point a scan found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPointRecord {
    /// Network name.
    pub ssid: String,
    /// The access point's MAC address.
    pub bssid: [u8; 6],
    /// Primary channel.
    pub channel: u8,
    /// Signal strength in dBm.
    pub rssi: i8,
    /// `wifi_auth_mode_t`; 0 is an open network.
    pub auth_mode: u8,
}

impl AccessPointRecord {
    fn decode(bytes: &[u8]) -> Result<Self, RpcError> {
        let mut record = Self {
            ssid: String::new(),
            bssid: [0; 6],
            channel: 0,
            rssi: 0,
            auth_mode: 0,
        };
        for field in Reader::new(bytes) {
            match field? {
                (1, value) => record.bssid = mac(value.as_bytes()),
                (2, value) => record.ssid = text(value.as_bytes()),
                (3, value) => record.channel = value.as_u32() as u8,
                (5, value) => record.rssi = value.as_i32().clamp(-128, 127) as i8,
                (6, value) => record.auth_mode = value.as_u32() as u8,
                _ => {}
            }
        }
        Ok(record)
    }
}

/// Decodes the MAC address from a `GetMacAddress` response payload.
///
/// # Errors
///
/// Returns an error for a malformed payload or a MAC that is not 6 bytes.
pub fn decode_mac_address(payload: &[u8]) -> Result<[u8; 6], RpcError> {
    for field in Reader::new(payload) {
        if let (1, Value::Bytes(bytes)) = field? {
            return <[u8; 6]>::try_from(bytes).map_err(|_| RpcError::Malformed);
        }
    }
    Err(RpcError::Malformed)
}

/// Decodes the records of a `ScanGetApRecords` response payload.
///
/// # Errors
///
/// Returns an error for a malformed payload.
pub fn decode_ap_records(payload: &[u8]) -> Result<Vec<AccessPointRecord>, RpcError> {
    let mut records = Vec::new();
    for field in Reader::new(payload) {
        if let (3, Value::Bytes(record)) = field? {
            records.push(AccessPointRecord::decode(record)?);
        }
    }
    Ok(records)
}

/// Decodes the record of a `StationGetApInfo` response payload.
///
/// # Errors
///
/// Returns an error for a malformed payload or a response without a record.
pub fn decode_ap_info(payload: &[u8]) -> Result<AccessPointRecord, RpcError> {
    for field in Reader::new(payload) {
        if let (2, Value::Bytes(record)) = field? {
            return AccessPointRecord::decode(record);
        }
    }
    Err(RpcError::Malformed)
}

/// Decodes a `GetCoprocessorFwVersion` response payload into
/// (major, minor, patch).
///
/// # Errors
///
/// Returns an error for a malformed payload.
pub fn decode_firmware_version(payload: &[u8]) -> Result<(u32, u32, u32), RpcError> {
    let mut version = (0, 0, 0);
    for field in Reader::new(payload) {
        match field? {
            (2, value) => version.0 = value.as_u32(),
            (3, value) => version.1 = value.as_u32(),
            (4, value) => version.2 = value.as_u32(),
            _ => {}
        }
    }
    Ok(version)
}

/// `wifi_event_t` values the co-processor forwards in
/// `Event_WifiEventNoArgs`.
pub mod wifi_event {
    /// The station interface started.
    pub const STA_START: i32 = 2;
    /// The station interface stopped.
    pub const STA_STOP: i32 = 3;
    /// The soft access point started.
    pub const AP_START: i32 = 12;
    /// The soft access point stopped.
    pub const AP_STOP: i32 = 13;
}

/// An event the co-processor sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The co-processor's RPC service is ready.
    EspInit,
    /// A periodic heartbeat.
    Heartbeat(i32),
    /// The station associated with an access point.
    StationConnected {
        /// Its network name.
        ssid: String,
        /// Its MAC address.
        bssid: [u8; 6],
        /// Its channel.
        channel: u8,
        /// Its `wifi_auth_mode_t`.
        auth_mode: u8,
    },
    /// The station lost or failed an association.
    StationDisconnected {
        /// The network name.
        ssid: String,
        /// The `wifi_err_reason_t`.
        reason: u16,
        /// Signal strength in dBm at disconnection.
        rssi: i8,
    },
    /// A scan finished.
    ScanDone {
        /// 0 on success.
        status: u32,
        /// Records found.
        number: u16,
    },
    /// A station joined the soft access point.
    AccessPointStationConnected {
        /// Its MAC address.
        mac: [u8; 6],
        /// Its association ID.
        aid: u16,
    },
    /// A station left the soft access point.
    AccessPointStationDisconnected {
        /// Its MAC address.
        mac: [u8; 6],
        /// Its association ID.
        aid: u16,
    },
    /// Another `wifi_event_t` without arguments, such as [`wifi_event::AP_START`].
    Wifi(i32),
    /// An event this host does not decode.
    Other(u32),
}

impl Event {
    /// Decodes an event message.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed payload.
    pub fn decode(message: &Message<'_>) -> Result<Self, RpcError> {
        let nested = |field: u32| -> Result<&[u8], RpcError> {
            for entry in Reader::new(message.payload) {
                if let (number, Value::Bytes(bytes)) = entry?
                    && number == field
                {
                    return Ok(bytes);
                }
            }
            Ok(&[])
        };
        Ok(match message.id {
            id::EVENT_ESP_INIT => Self::EspInit,
            id::EVENT_HEARTBEAT => {
                let mut beat = 0;
                for entry in Reader::new(message.payload) {
                    if let (1, value) = entry? {
                        beat = value.as_i32();
                    }
                }
                Self::Heartbeat(beat)
            }
            id::EVENT_STA_CONNECTED => {
                let mut event = (String::new(), [0; 6], 0, 0);
                for entry in Reader::new(nested(2)?) {
                    match entry? {
                        (1, value) => event.0 = text(value.as_bytes()),
                        (3, value) => event.1 = mac(value.as_bytes()),
                        (4, value) => event.2 = value.as_u32() as u8,
                        (5, value) => event.3 = value.as_u32() as u8,
                        _ => {}
                    }
                }
                Self::StationConnected {
                    ssid: event.0,
                    bssid: event.1,
                    channel: event.2,
                    auth_mode: event.3,
                }
            }
            id::EVENT_STA_DISCONNECTED => {
                let mut event = (String::new(), 0, 0);
                for entry in Reader::new(nested(2)?) {
                    match entry? {
                        (1, value) => event.0 = text(value.as_bytes()),
                        (4, value) => event.1 = value.as_u32() as u16,
                        (5, value) => event.2 = value.as_i32().clamp(-128, 127) as i8,
                        _ => {}
                    }
                }
                Self::StationDisconnected {
                    ssid: event.0,
                    reason: event.1,
                    rssi: event.2,
                }
            }
            id::EVENT_STA_SCAN_DONE => {
                let mut event = (0, 0);
                for entry in Reader::new(nested(2)?) {
                    match entry? {
                        (1, value) => event.0 = value.as_u32(),
                        (2, value) => event.1 = value.as_u32() as u16,
                        _ => {}
                    }
                }
                Self::ScanDone {
                    status: event.0,
                    number: event.1,
                }
            }
            id::EVENT_AP_STA_CONNECTED | id::EVENT_AP_STA_DISCONNECTED => {
                let mut event = ([0; 6], 0);
                for entry in Reader::new(message.payload) {
                    match entry? {
                        (2, value) => event.0 = mac(value.as_bytes()),
                        (3, value) => event.1 = value.as_u32() as u16,
                        _ => {}
                    }
                }
                if message.id == id::EVENT_AP_STA_CONNECTED {
                    Self::AccessPointStationConnected {
                        mac: event.0,
                        aid: event.1,
                    }
                } else {
                    Self::AccessPointStationDisconnected {
                        mac: event.0,
                        aid: event.1,
                    }
                }
            }
            id::EVENT_WIFI_NO_ARGS => {
                let mut event = 0;
                for entry in Reader::new(message.payload) {
                    if let (2, value) = entry? {
                        event = value.as_i32();
                    }
                }
                Self::Wifi(event)
            }
            other => Self::Other(other),
        })
    }
}

/// The SSID bytes up to the first NUL, as ESP-IDF pads its 33-byte arrays.
fn text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).into_owned()
}

fn mac(bytes: &[u8]) -> [u8; 6] {
    <[u8; 6]>::try_from(bytes.get(..6).unwrap_or(&[0; 6])).unwrap_or_default()
}

/// An RPC message could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcError {
    /// The protobuf encoding is malformed.
    Decode(DecodeError),
    /// The `msg_type` is unknown.
    UnknownType(u64),
    /// A field is missing or has the wrong size.
    Malformed,
}

impl From<DecodeError> for RpcError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "RPC message: {error}"),
            Self::UnknownType(kind) => write!(formatter, "RPC message type {kind} is unknown"),
            Self::Malformed => formatter.write_str("RPC message lacks a required field"),
        }
    }
}

impl core::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use alloc::vec;

    use super::*;

    #[test]
    fn a_mac_address_request_selects_the_station_interface() {
        // msg_type Req (08 01), msg_id 257 (10 81 02), uid 1 (18 01), then
        // field 257 (key 2058: 8a 10) holding an empty Rpc_Req_GetMacAddress:
        // mode 0, WIFI_IF_STA, is proto3's default and stays out.
        assert_eq!(
            Request::GetMacAddress(WifiInterface::Station).encode(1),
            [0x08, 0x01, 0x10, 0x81, 0x02, 0x18, 0x01, 0x8a, 0x10, 0x00]
        );
        assert_eq!(
            Request::GetMacAddress(WifiInterface::AccessPoint).encode(2),
            [
                0x08, 0x01, 0x10, 0x81, 0x02, 0x18, 0x02, 0x8a, 0x10, 0x02, 0x08, 0x01
            ]
        );
    }

    #[test]
    fn a_start_request_carries_an_empty_payload_message() {
        // msg_id 280 = 0x118 (10 98 02); field 280 key 2242 (c2 11), length 0.
        assert_eq!(
            Request::WifiStart.encode(7),
            [0x08, 0x01, 0x10, 0x98, 0x02, 0x18, 0x07, 0xc2, 0x11, 0x00]
        );
    }

    #[test]
    fn station_credentials_nest_in_wifi_config_sta() {
        let request = Request::SetStationConfig {
            ssid: b"lab".to_vec(),
            password: b"secret12".to_vec(),
        };
        let mut expected = vec![0x08, 0x01, 0x10, 0x9c, 0x02, 0x18, 0x03];
        // Field 284 (key 2274: e2 11). Rpc_Req_WifiSetConfig: iface 0 left
        // out, cfg (12) = wifi_config { sta (12) = wifi_sta_config {
        // ssid (0a) "lab", password (12) "secret12" } }.
        expected.extend_from_slice(&[0xe2, 0x11, 19, 0x12, 17, 0x12, 15]);
        expected.extend_from_slice(&[0x0a, 3]);
        expected.extend_from_slice(b"lab");
        expected.extend_from_slice(&[0x12, 8]);
        expected.extend_from_slice(b"secret12");
        assert_eq!(request.encode(3), expected);
        assert_eq!(request.response_id(), 0x21c);
    }

    #[test]
    fn an_access_point_config_sets_wpa2_only_with_a_password() {
        let request = Request::SetAccessPointConfig {
            ssid: b"ap".to_vec(),
            password: Vec::new(),
            channel: 1,
            max_connections: 4,
        };
        let encoded = request.encode(1);
        // iface WIFI_IF_AP (08 01), then cfg { ap (0a) { ssid "ap",
        // ssid_len 2, channel 1, max_connection 4, beacon_interval 100 } }:
        // the open authmode 0 stays out.
        let tail = [
            0x08, 0x01, 0x12, 14, 0x0a, 12, 0x0a, 2, b'a', b'p', 0x18, 2, 0x20, 1, 0x38, 4, 0x40,
            100,
        ];
        assert!(encoded.ends_with(&tail), "{encoded:02x?}");

        let secured = Request::SetAccessPointConfig {
            ssid: b"ap".to_vec(),
            password: b"12345678".to_vec(),
            channel: 6,
            max_connections: 4,
        }
        .encode(1);
        // authmode WIFI_AUTH_WPA2_PSK: field 5 (28) = 3.
        assert!(secured.windows(2).any(|pair| pair == [0x28, 3]));
    }

    #[test]
    fn wifi_init_sends_the_esp_idf_defaults_and_magic() {
        let encoded = Request::WifiInit.encode(1);
        // magic 0x1F2F3F4F as field 20 (a0 01) varint cf fe bc f9 01.
        assert!(
            encoded
                .windows(7)
                .any(|window| window == [0xa0, 0x01, 0xcf, 0xfe, 0xbc, 0xf9, 0x01])
        );
        // beacon_max_len 752 as field 15 (78) varint f0 05.
        assert!(
            encoded
                .windows(3)
                .any(|window| window == [0x78, 0xf0, 0x05])
        );
        // nvs_enable (field 11) stays out: the host owns credentials.
        assert!(!encoded.windows(2).any(|window| window == [0x58, 0x01]));
    }

    #[test]
    fn a_mac_address_response_decodes_with_its_status_in_field_two() {
        // Rpc { msg_type Resp, msg_id 513, uid 5, field 513 (key 4106:
        // 8a 20) { mac (0a 06) 60 55 f9 01 02 03, resp 0 } }
        let bytes = [
            0x08, 0x02, 0x10, 0x81, 0x04, 0x18, 0x05, 0x8a, 0x20, 0x08, 0x0a, 0x06, 0x60, 0x55,
            0xf9, 0x01, 0x02, 0x03,
        ];
        let message = Message::decode(&bytes).unwrap();
        assert_eq!(message.kind, MessageKind::Response);
        assert_eq!(message.id, 0x201);
        assert_eq!(message.uid, 5);
        assert_eq!(message.status().unwrap(), 0);
        assert_eq!(
            decode_mac_address(message.payload).unwrap(),
            [0x60, 0x55, 0xf9, 0x01, 0x02, 0x03]
        );
    }

    #[test]
    fn a_failed_response_reports_its_esp_err() {
        // Resp_WifiConnect (538 = 0x21a: 10 9a 04), field 538 (key 4306:
        // d2 21) { resp (08) ESP_ERR_WIFI_CONN 0x3007 (87 60) }.
        let bytes = [
            0x08, 0x02, 0x10, 0x9a, 0x04, 0x18, 0x09, 0xd2, 0x21, 0x03, 0x08, 0x87, 0x60,
        ];
        let message = Message::decode(&bytes).unwrap();
        assert_eq!(message.status().unwrap(), 0x3007);
    }

    #[test]
    fn scan_records_decode_name_signal_and_security() {
        // Two wifi_ap_record entries in field 3, the second open.
        let mut record = vec![0x0a, 6, 1, 2, 3, 4, 5, 6, 0x12, 5];
        record.extend_from_slice(b"home\0");
        record.extend_from_slice(&[0x18, 6, 0x28]);
        record.extend_from_slice(&[0xc4, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
        record.extend_from_slice(&[0x30, 3]);
        let mut payload = vec![0x08, 0x00, 0x10, 2, 0x1a, record.len() as u8];
        payload.extend_from_slice(&record);
        payload.extend_from_slice(&[0x1a, 7, 0x12, 3, b'c', b'a', b'f', 0x18, 11]);
        let records = decode_ap_records(&payload).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].ssid, "home");
        assert_eq!(records[0].rssi, -60);
        assert_eq!(records[0].channel, 6);
        assert_eq!(records[0].auth_mode, 3);
        assert_eq!(records[0].bssid, [1, 2, 3, 4, 5, 6]);
        assert_eq!(records[1].ssid, "caf");
        assert_eq!(records[1].auth_mode, 0);
    }

    #[test]
    fn station_events_decode_from_their_nested_esp_idf_structs() {
        // Event_StaConnected (775 = 0x307: 10 87 06), field 775 (key 6202:
        // ba 30) { resp 0, sta_connected (12) { ssid "lab", ssid_len 3,
        // bssid, channel 11, authmode 3, aid 1 } }.
        let mut inner = vec![
            0x0a, 3, b'l', b'a', b'b', 0x10, 3, 0x1a, 6, 9, 8, 7, 6, 5, 4,
        ];
        inner.extend_from_slice(&[0x20, 11, 0x28, 3, 0x30, 1]);
        let mut payload = vec![0x12, inner.len() as u8];
        payload.extend_from_slice(&inner);
        let mut bytes = vec![
            0x08,
            0x03,
            0x10,
            0x87,
            0x06,
            0xba,
            0x30,
            payload.len() as u8,
        ];
        bytes.extend_from_slice(&payload);
        let message = Message::decode(&bytes).unwrap();
        assert_eq!(message.kind, MessageKind::Event);
        assert_eq!(
            Event::decode(&message).unwrap(),
            Event::StationConnected {
                ssid: "lab".into(),
                bssid: [9, 8, 7, 6, 5, 4],
                channel: 11,
                auth_mode: 3,
            }
        );

        // Event_StaDisconnected (776: 10 88 06), key 6210 (c2 30) {
        // sta_disconnected { ssid "lab", reason 201 (NO_AP_FOUND), rssi -90 } }.
        let mut inner = vec![0x0a, 3, b'l', b'a', b'b', 0x20, 0xc9, 0x01, 0x28];
        inner.extend_from_slice(&[0xa6, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
        let mut payload = vec![0x12, inner.len() as u8];
        payload.extend_from_slice(&inner);
        let mut bytes = vec![
            0x08,
            0x03,
            0x10,
            0x88,
            0x06,
            0xc2,
            0x30,
            payload.len() as u8,
        ];
        bytes.extend_from_slice(&payload);
        assert_eq!(
            Event::decode(&Message::decode(&bytes).unwrap()).unwrap(),
            Event::StationDisconnected {
                ssid: "lab".into(),
                reason: 201,
                rssi: -90,
            }
        );
    }

    #[test]
    fn scan_done_and_bare_wifi_events_decode() {
        // Event_StaScanDone (774: 10 86 06), key 6194 (b2 30) {
        // scan_done (12) { status 0, number 4 } }.
        let bytes = [
            0x08, 0x03, 0x10, 0x86, 0x06, 0xb2, 0x30, 0x04, 0x12, 0x02, 0x10, 0x04,
        ];
        assert_eq!(
            Event::decode(&Message::decode(&bytes).unwrap()).unwrap(),
            Event::ScanDone {
                status: 0,
                number: 4
            }
        );
        // Event_WifiEventNoArgs (773: 10 85 06), key 6186 (aa 30) {
        // event_id 12, WIFI_EVENT_AP_START }.
        let bytes = [0x08, 0x03, 0x10, 0x85, 0x06, 0xaa, 0x30, 0x02, 0x10, 0x0c];
        assert_eq!(
            Event::decode(&Message::decode(&bytes).unwrap()).unwrap(),
            Event::Wifi(wifi_event::AP_START)
        );
        // Event_ESPInit with an empty payload.
        let bytes = [0x08, 0x03, 0x10, 0x81, 0x06];
        assert_eq!(
            Event::decode(&Message::decode(&bytes).unwrap()).unwrap(),
            Event::EspInit
        );
    }
}
