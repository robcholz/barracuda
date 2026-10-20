//! Wi-Fi operations on the co-processor, in ESP-IDF's `esp_wifi_*` terms.

use alloc::vec::Vec;
use core::fmt;

use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};
use embassy_time::{Duration, Instant, with_timeout};

use crate::rpc::{
    self, AccessPointRecord, Event, Message, Request, RpcError, WifiInterface, WifiMode,
};
use crate::runner::Shared;
use crate::{StartError, Started};

/// How long an ordinary request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a scan may take: ESP-IDF's default all-channel active scan
/// dwells up to 120 ms on each of 13 channels, plus a margin.
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long an access point may take to report it started.
const ACCESS_POINT_START: Duration = Duration::from_secs(3);
/// Longest SSID and passphrase ESP-IDF accepts.
const MAX_SSID: usize = 32;
const MAX_PASSWORD: usize = 64;
const MIN_ACCESS_POINT_PASSWORD: usize = 8;

/// The MAC addresses the co-processor's interfaces use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacAddresses {
    /// The station interface.
    pub station: [u8; 6],
    /// The soft access point.
    pub access_point: [u8; 6],
}

/// Runs Wi-Fi operations through the [`crate::Runner`].
///
/// Operations are serialized: one request is outstanding at a time.
pub struct Control<'a> {
    shared: &'a Shared,
    station: ch::StateRunner<'a>,
    access_point: ch::StateRunner<'a>,
}

impl<'a> Control<'a> {
    pub(crate) const fn new(
        shared: &'a Shared,
        station: ch::StateRunner<'a>,
        access_point: ch::StateRunner<'a>,
    ) -> Self {
        Self {
            shared,
            station,
            access_point,
        }
    }

    /// Waits until the runner started the co-processor or gave up.
    ///
    /// # Errors
    ///
    /// Returns why the co-processor could not be started.
    pub async fn wait_started(&self) -> Result<Started, StartError> {
        self.shared.started().await
    }

    /// Returns the start-up outcome, `None` while it is still running.
    #[must_use]
    pub fn started(&self) -> Option<Result<Started, StartError>> {
        self.shared.started_now()
    }

    /// Returns whether the station is associated with an access point.
    #[must_use]
    pub fn station_connected(&self) -> bool {
        self.shared.station_connected()
    }

    /// Sends one request and returns its response payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the co-processor is not running, does not answer
    /// in `timeout`, or answers with an `esp_err_t`.
    pub async fn call(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Vec<u8>, ControlError> {
        match self.started() {
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(ControlError::NotStarted(error)),
            None => return Err(ControlError::Starting),
        }
        let mut uid = self.shared.rpc.lock().await;
        *uid = uid.wrapping_add(1);
        let id = *uid;
        self.shared.response.reset();
        self.shared.request.signal(request.encode(id));
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let bytes = with_timeout(remaining, self.shared.response.wait())
                .await
                .map_err(|_| ControlError::Timeout {
                    request: request.id(),
                })?;
            let message = Message::decode(&bytes)?;
            if message.uid != id || message.id != request.response_id() {
                // A late answer to an earlier request that timed out.
                continue;
            }
            let status = message.status()?;
            if status != 0 {
                return Err(ControlError::Esp {
                    request: request.id(),
                    code: status,
                });
            }
            return Ok(message.payload.to_vec());
        }
    }

    /// Initializes and starts Wi-Fi in station mode and returns the
    /// interface MAC addresses, which the network devices then use.
    ///
    /// # Errors
    ///
    /// Returns the first request that failed.
    pub async fn start_wifi(&self) -> Result<MacAddresses, ControlError> {
        self.call(&Request::WifiInit, REQUEST_TIMEOUT).await?;
        self.call(&Request::SetMode(WifiMode::Station), REQUEST_TIMEOUT)
            .await?;
        self.call(&Request::WifiStart, REQUEST_TIMEOUT).await?;
        let station = rpc::decode_mac_address(
            &self
                .call(
                    &Request::GetMacAddress(WifiInterface::Station),
                    REQUEST_TIMEOUT,
                )
                .await?,
        )?;
        let access_point = rpc::decode_mac_address(
            &self
                .call(
                    &Request::GetMacAddress(WifiInterface::AccessPoint),
                    REQUEST_TIMEOUT,
                )
                .await?,
        )?;
        self.station
            .set_hardware_address(HardwareAddress::Ethernet(station));
        self.access_point
            .set_hardware_address(HardwareAddress::Ethernet(access_point));
        Ok(MacAddresses {
            station,
            access_point,
        })
    }

    /// Asks the co-processor for its firmware version, which ESP-Hosted
    /// answers from 2.x on.
    ///
    /// # Errors
    ///
    /// Returns an error when the firmware does not know the request.
    pub async fn firmware_version(&self) -> Result<(u32, u32, u32), ControlError> {
        let payload = self
            .call(
                &Request::GetCoprocessorFirmwareVersion,
                Duration::from_secs(2),
            )
            .await?;
        Ok(rpc::decode_firmware_version(&payload)?)
    }

    /// Joins `ssid` and waits until the association succeeds or fails.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid credentials, a failed request, the
    /// co-processor's disconnect reason, or no answer within `timeout`.
    pub async fn connect(
        &self,
        ssid: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<(), ControlError> {
        if ssid.is_empty() || ssid.len() > MAX_SSID || password.len() > MAX_PASSWORD {
            return Err(ControlError::InvalidCredentials);
        }
        if self.station_connected() {
            self.disconnect().await?;
        }
        self.drain_events();
        self.call(
            &Request::SetStationConfig {
                ssid: ssid.as_bytes().to_vec(),
                password: password.as_bytes().to_vec(),
            },
            REQUEST_TIMEOUT,
        )
        .await?;
        self.call(&Request::WifiConnect, REQUEST_TIMEOUT).await?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match with_timeout(remaining, self.shared.events.receive()).await {
                Ok(Event::StationConnected { .. }) => return Ok(()),
                Ok(Event::StationDisconnected { reason, .. }) => {
                    return Err(ControlError::Association { reason });
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = self.call(&Request::WifiDisconnect, REQUEST_TIMEOUT).await;
                    return Err(ControlError::Timeout {
                        request: Request::WifiConnect.id(),
                    });
                }
            }
        }
    }

    /// Leaves the current network.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails for another reason than not
    /// being connected.
    pub async fn disconnect(&self) -> Result<(), ControlError> {
        match self.call(&Request::WifiDisconnect, REQUEST_TIMEOUT).await {
            // ESP_ERR_WIFI_NOT_CONNECT: nothing to leave.
            Ok(_) | Err(ControlError::Esp { code: 0x300f, .. }) => {}
            Err(error) => return Err(error),
        }
        self.station.set_link_state(LinkState::Down);
        Ok(())
    }

    /// Scans all channels and returns up to `max` access points.
    ///
    /// # Errors
    ///
    /// Returns an error when the scan cannot start, fails, or does not
    /// finish in time.
    pub async fn scan(&self, max: u16) -> Result<Vec<AccessPointRecord>, ControlError> {
        self.drain_events();
        self.call(&Request::ScanStart, REQUEST_TIMEOUT).await?;
        let deadline = Instant::now() + SCAN_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match with_timeout(remaining, self.shared.events.receive()).await {
                Ok(Event::ScanDone { status: 0, .. }) => break,
                Ok(Event::ScanDone { status, .. }) => {
                    return Err(ControlError::ScanFailed { status });
                }
                Ok(_) => {}
                Err(_) => {
                    return Err(ControlError::Timeout {
                        request: Request::ScanStart.id(),
                    });
                }
            }
        }
        let payload = self
            .call(&Request::ScanGetApRecords(max), REQUEST_TIMEOUT)
            .await?;
        Ok(rpc::decode_ap_records(&payload)?)
    }

    /// Starts or reconfigures the soft access point beside the station.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid SSID or passphrase, or a failed
    /// request.
    pub async fn start_access_point(
        &self,
        ssid: &str,
        password: &str,
        channel: u8,
    ) -> Result<(), ControlError> {
        if ssid.is_empty()
            || ssid.len() > MAX_SSID
            || password.len() > MAX_PASSWORD
            || (!password.is_empty() && password.len() < MIN_ACCESS_POINT_PASSWORD)
        {
            return Err(ControlError::InvalidCredentials);
        }
        self.drain_events();
        self.call(
            &Request::SetMode(WifiMode::AccessPointStation),
            REQUEST_TIMEOUT,
        )
        .await?;
        self.call(
            &Request::SetAccessPointConfig {
                ssid: ssid.as_bytes().to_vec(),
                password: password.as_bytes().to_vec(),
                channel,
                max_connections: 4,
            },
            REQUEST_TIMEOUT,
        )
        .await?;
        // Switching into AP+STA starts the access point; a running one keeps
        // going with the new settings and reports nothing.
        let deadline = Instant::now() + ACCESS_POINT_START;
        while let Ok(event) = with_timeout(
            deadline.saturating_duration_since(Instant::now()),
            self.shared.events.receive(),
        )
        .await
        {
            if event == Event::Wifi(rpc::wifi_event::AP_START) {
                break;
            }
        }
        self.access_point.set_link_state(LinkState::Up);
        Ok(())
    }

    /// Stops the soft access point and keeps the station.
    ///
    /// # Errors
    ///
    /// Returns an error when the mode change fails.
    pub async fn stop_access_point(&self) -> Result<(), ControlError> {
        self.call(&Request::SetMode(WifiMode::Station), REQUEST_TIMEOUT)
            .await?;
        self.access_point.set_link_state(LinkState::Down);
        Ok(())
    }

    fn drain_events(&self) {
        while self.shared.events.try_receive().is_ok() {}
    }
}

/// A Wi-Fi operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// The co-processor is still starting.
    Starting,
    /// The co-processor could not be started.
    NotStarted(StartError),
    /// No response to request `request` in time.
    Timeout {
        /// The request ID.
        request: u32,
    },
    /// The co-processor answered request `request` with `esp_err_t` `code`.
    Esp {
        /// The request ID.
        request: u32,
        /// The `esp_err_t`.
        code: i32,
    },
    /// The station could not associate (`wifi_err_reason_t`).
    Association {
        /// ESP-IDF's disconnect reason, such as 201 for no AP found or 15
        /// for a handshake timeout after a wrong passphrase.
        reason: u16,
    },
    /// A scan finished with a non-zero status.
    ScanFailed {
        /// The scan status.
        status: u32,
    },
    /// The SSID or passphrase exceeds what ESP-IDF accepts.
    InvalidCredentials,
    /// A response could not be decoded.
    Decode(RpcError),
}

impl From<RpcError> for ControlError {
    fn from(error: RpcError) -> Self {
        Self::Decode(error)
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => formatter.write_str("the Wi-Fi co-processor is still starting"),
            Self::NotStarted(error) => {
                write!(formatter, "the Wi-Fi co-processor is not running: {error}")
            }
            Self::Timeout { request } => {
                write!(
                    formatter,
                    "the Wi-Fi co-processor did not answer request {request:#05x}"
                )
            }
            Self::Esp { request, code } => write!(
                formatter,
                "the Wi-Fi co-processor refused request {request:#05x} with error {code:#x}"
            ),
            Self::Association { reason } => {
                write!(formatter, "the station could not join (reason {reason})")
            }
            Self::ScanFailed { status } => write!(formatter, "the scan failed (status {status})"),
            Self::InvalidCredentials => formatter.write_str("the SSID or passphrase is invalid"),
            Self::Decode(error) => write!(formatter, "{error}"),
        }
    }
}

impl core::error::Error for ControlError {}
