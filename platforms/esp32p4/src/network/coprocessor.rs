//! Wi-Fi on an ESP-Hosted co-processor wired to the SD host's slot 1.
//!
//! The co-processor runs Espressif's ESP-Hosted-MCU slave firmware; the host
//! side of its protocol lives in `barracuda-driver-esp-hosted`. The Platform
//! owns the SD host and takes slot 1 when the entry starts (`crate::sd_host`);
//! a Board's microSD card may hold slot 0 of the same host, and the two share
//! its transfer engine. This module wires slot 1 to the Board's pins, spawns
//! the driver's runner,
//! runs one IP stack over the station and one over the soft access point,
//! and adapts the driver's Wi-Fi operations to the Platform's `WifiDevice`.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_driver_esp_hosted::{
    Config, Control, ControlError, MacAddresses, Runner, StartError, State,
};
use barracuda_platform::{
    AccessPointConfiguration, AccessPointState, StationConfiguration, StationState, VisibleNetwork,
    WifiCapabilities, WifiDevice,
};
use embassy_executor::Spawner;
use embassy_net::{Stack, StackResources};
use embassy_time::{with_timeout, Delay, Duration};
use esp_hal::{
    gpio::{AnyPin, Level, Output, OutputConfig},
    sdmmc::Slot,
    Async,
};
use static_cell::{ConstStaticCell, StaticCell};

use crate::Esp32P4PlatformError;

/// The co-processor's network devices as embassy-net drives them.
pub(crate) type NetDevice = barracuda_driver_esp_hosted::NetDevice<'static>;

/// The SD host slot the co-processor is wired to.
type CoprocessorSlot = Slot<'static, 1, Async>;

/// The driver task's type.
type CoprocessorRunner = Runner<'static, CoprocessorSlot, Delay, Output<'static>>;

/// How long the Platform waits for the co-processor to boot and answer:
/// its reset and boot take about 1.6 s, card set-up retries up to 1 s more.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a station may take to associate.
const ASSOCIATION_TIMEOUT: Duration = Duration::from_secs(20);
/// How long DHCP may take once associated.
const DHCP_TIMEOUT: Duration = Duration::from_secs(20);
/// Access points one scan reports at most.
const SCAN_RECORDS: u16 = 20;
/// Channel of the soft access point; it follows the station's channel while
/// the station is associated, as ESP-IDF requires.
const ACCESS_POINT_CHANNEL: u8 = 1;

/// Chip resources the Board's Wi-Fi co-processor consumes, built by the
/// generated network bindings from `hardware.wifi-coprocessor`.
pub struct CoprocessorTokens {
    /// Slot 1 of the SD host the Platform owns.
    pub(crate) sdio: crate::sd_host::CoprocessorSlot,
    /// The SDIO clock pin.
    pub(crate) clk: AnyPin<'static>,
    /// The SDIO command pin.
    pub(crate) cmd: AnyPin<'static>,
    /// The four SDIO data pins, D0 first.
    pub(crate) data: [AnyPin<'static>; 4],
    /// The pin wired to the co-processor's active-low enable.
    pub(crate) reset: AnyPin<'static>,
    /// The SDIO clock once the card is set up.
    pub(crate) frequency_hz: u32,
    /// The ESP-Hosted chip ID the co-processor must report.
    pub(crate) chip: u8,
}

#[embassy_executor::task]
async fn run_coprocessor(runner: CoprocessorRunner) -> ! {
    runner.run().await
}

#[cfg(not(barracuda_esp32p4_ethernet))]
#[embassy_executor::task]
async fn run_station(
    mut runner: embassy_net::Runner<'static, NetDevice>,
    stack: Stack<'static>,
) -> ! {
    let (never, ()) =
        embassy_futures::join::join(runner.run(), super::report_addresses("Wi-Fi", stack)).await;
    never
}

#[embassy_executor::task]
async fn run_access_point(mut runner: embassy_net::Runner<'static, NetDevice>) -> ! {
    runner.run().await
}

/// The co-processor's station device and the rest of its radio.
pub(super) struct Coprocessor {
    /// The station's frames, for the IP stack.
    pub(super) station: NetDevice,
    /// The access point and the Wi-Fi operations.
    pub(super) radio: Radio,
}

/// The co-processor's access point and its Wi-Fi operations.
pub(super) struct Radio {
    access_point: NetDevice,
    control: Control<'static>,
}

/// Wires the SDIO slot and spawns the driver's runner, which resets and
/// starts the co-processor in the background.
///
/// # Errors
///
/// Returns an error when the SD host refused the slot or the runner task
/// cannot be allocated.
pub(super) fn start(
    spawner: Spawner,
    tokens: CoprocessorTokens,
) -> Result<Coprocessor, Esp32P4PlatformError> {
    static STATE: ConstStaticCell<State> = ConstStaticCell::new(State::new());

    let [d0, d1, d2, d3] = tokens.data;
    let slot = tokens
        .sdio
        .map_err(Esp32P4PlatformError::Sdio)?
        .with_clk(tokens.clk)
        .with_cmd(tokens.cmd)
        .with_data0(d0)
        .with_data1(d1)
        .with_data2(d2)
        .with_data3(d3);
    // Hold the co-processor in reset until the runner starts it.
    let reset = Output::new(tokens.reset, Level::Low, OutputConfig::default());
    let (devices, control, runner) = barracuda_driver_esp_hosted::new(
        STATE.take(),
        slot,
        Delay,
        reset,
        Config {
            frequency_hz: tokens.frequency_hz,
            expected_chip: tokens.chip,
        },
    );
    spawner.spawn(run_coprocessor(runner).map_err(|_error| Esp32P4PlatformError::NetworkTask)?);
    Ok(Coprocessor {
        station: devices.station,
        radio: Radio {
            access_point: devices.access_point,
            control,
        },
    })
}

/// Starts the IP stack over the station alone and the task that runs a DHCP
/// client over it.
///
/// # Errors
///
/// Returns an error when the stack task cannot be allocated.
#[cfg(not(barracuda_esp32p4_ethernet))]
pub(super) fn station_stack(
    spawner: Spawner,
    station: NetDevice,
) -> Result<Stack<'static>, Esp32P4PlatformError> {
    static RESOURCES: StaticCell<StackResources<16>> = StaticCell::new();

    let (stack, runner) = embassy_net::new(
        station,
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        super::stack_seed(),
    );
    spawner.spawn(run_station(runner, stack).map_err(|_error| Esp32P4PlatformError::NetworkTask)?);
    Ok(stack)
}

impl Radio {
    /// Starts the access point's IP stack and returns the Wi-Fi device over
    /// `station_stack`.
    ///
    /// # Errors
    ///
    /// Returns an error when the stack task cannot be allocated.
    pub(super) fn into_wifi(
        self,
        spawner: Spawner,
        station_stack: Stack<'static>,
    ) -> Result<CoprocessorWifi, Esp32P4PlatformError> {
        static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

        // The access point's address is its consumer's choice, assigned when
        // it starts the access point's services.
        let (access_point_stack, runner) = embassy_net::new(
            self.access_point,
            embassy_net::Config::default(),
            RESOURCES.init(StackResources::new()),
            super::stack_seed().rotate_left(17),
        );
        spawner
            .spawn(run_access_point(runner).map_err(|_error| Esp32P4PlatformError::NetworkTask)?);
        Ok(CoprocessorWifi {
            control: self.control,
            station_stack,
            access_point_stack,
            status: Status::Starting,
            station: None,
            access_point: AccessPointState::Stopped,
        })
    }
}

/// Where the co-processor is in its start-up.
#[derive(Clone, Copy, Debug)]
enum Status {
    /// Not yet started; [`CoprocessorWifi::start`] has not finished.
    Starting,
    /// Wi-Fi runs on the co-processor.
    Ready(MacAddresses),
    /// The co-processor could not be started.
    Failed(CoprocessorFailure),
}

/// Why the co-processor's Wi-Fi is not running.
#[derive(Clone, Copy, Debug)]
pub enum CoprocessorFailure {
    /// The co-processor did not finish starting in time.
    Timeout,
    /// The co-processor could not be started, such as when it runs firmware
    /// this Platform cannot drive.
    Start(StartError),
    /// The co-processor runs, but refused to start Wi-Fi.
    Wifi(ControlError),
}

impl core::fmt::Display for CoprocessorFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Timeout => formatter.write_str("the Wi-Fi co-processor did not start in time"),
            Self::Start(error) => write!(formatter, "{error}"),
            Self::Wifi(error) => write!(
                formatter,
                "Wi-Fi did not start on the co-processor: {error}"
            ),
        }
    }
}

/// The ESP32-P4's Wi-Fi: a radio on the Board's co-processor.
///
/// Its capabilities are the co-processor's: station, scanning, and access
/// point once the co-processor runs Wi-Fi, none when it could not be started.
pub struct CoprocessorWifi {
    control: Control<'static>,
    station_stack: Stack<'static>,
    access_point_stack: Stack<'static>,
    status: Status,
    station: Option<String>,
    access_point: AccessPointState,
}

impl CoprocessorWifi {
    /// Waits for the co-processor to start and starts Wi-Fi on it, so the
    /// capabilities the Wi-Fi Plugin reads are the co-processor's own.
    pub(crate) async fn start(&mut self) {
        self.status = match with_timeout(START_TIMEOUT, self.control.wait_started()).await {
            Err(_) => Status::Failed(CoprocessorFailure::Timeout),
            Ok(Err(error)) => Status::Failed(CoprocessorFailure::Start(error)),
            Ok(Ok(started)) => {
                if started.firmware.is_none() {
                    // ESP-Hosted 2.x answers this request; earlier firmware
                    // does not know it.
                    match self.control.firmware_version().await {
                        Ok((major, minor, patch)) => {
                            log::info!("ESP-Hosted co-processor firmware {major}.{minor}.{patch}")
                        }
                        Err(_) => log::info!(
                            "ESP-Hosted co-processor firmware predates 2.x and reports no version"
                        ),
                    }
                }
                match self.control.start_wifi().await {
                    Ok(addresses) => {
                        let mac = addresses.station;
                        log::info!(
                            "Wi-Fi co-processor started, station MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                            mac[0],
                            mac[1],
                            mac[2],
                            mac[3],
                            mac[4],
                            mac[5]
                        );
                        Status::Ready(addresses)
                    }
                    Err(error) => Status::Failed(CoprocessorFailure::Wifi(error)),
                }
            }
        };
        if let Status::Failed(failure) = self.status {
            log::error!("Wi-Fi is unavailable: {failure}");
        }
    }

    fn ready(&self) -> Result<(), CoprocessorWifiError> {
        match self.status {
            Status::Ready(_) => Ok(()),
            Status::Starting => Err(CoprocessorWifiError::Starting),
            Status::Failed(failure) => Err(CoprocessorWifiError::Unavailable(failure)),
        }
    }
}

impl WifiDevice for CoprocessorWifi {
    type Error = CoprocessorWifiError;

    fn capabilities(&self) -> WifiCapabilities {
        match self.status {
            Status::Ready(_) => WifiCapabilities::managed(),
            Status::Starting | Status::Failed(_) => WifiCapabilities::host_managed(),
        }
    }

    fn access_point_stack(&self) -> Stack<'static> {
        self.access_point_stack
    }

    fn station_mac_address(&self) -> Option<[u8; 6]> {
        match self.status {
            Status::Ready(addresses) => Some(addresses.station),
            Status::Starting | Status::Failed(_) => None,
        }
    }

    fn station_state(&self) -> StationState {
        match &self.station {
            Some(ssid) if self.control.station_connected() => StationState::Connected {
                ssid: Some(ssid.clone()),
            },
            // The co-processor reports a lost association; ESP-IDF does not
            // rejoin by itself, so the station is down until its owner
            // connects again.
            _ => StationState::Disconnected,
        }
    }

    fn access_point_state(&self) -> AccessPointState {
        self.access_point.clone()
    }

    async fn scan(&mut self) -> Result<Vec<VisibleNetwork>, Self::Error> {
        self.ready()?;
        let records = self.control.scan(SCAN_RECORDS).await?;
        Ok(records
            .into_iter()
            .filter(|record| !record.ssid.is_empty())
            .map(|record| VisibleNetwork {
                ssid: record.ssid,
                signal_dbm: Some(record.rssi),
                secured: record.auth_mode != 0,
            })
            .collect())
    }

    async fn start_access_point(
        &mut self,
        configuration: &AccessPointConfiguration,
    ) -> Result<(), Self::Error> {
        self.ready()?;
        self.access_point = AccessPointState::Starting;
        if let Err(error) = self
            .control
            .start_access_point(
                configuration.ssid(),
                configuration.password(),
                ACCESS_POINT_CHANNEL,
            )
            .await
        {
            self.access_point = AccessPointState::Stopped;
            return Err(error.into());
        }
        self.access_point = AccessPointState::Started {
            ssid: configuration.ssid().to_string(),
        };
        Ok(())
    }

    async fn stop_access_point(&mut self) -> Result<(), Self::Error> {
        if self.ready().is_ok() {
            self.control.stop_access_point().await?;
        }
        self.access_point = AccessPointState::Stopped;
        Ok(())
    }

    async fn connect_station(
        &mut self,
        configuration: &StationConfiguration,
    ) -> Result<(), Self::Error> {
        self.ready()?;
        self.station = None;
        self.control
            .connect(
                configuration.ssid(),
                configuration.password(),
                ASSOCIATION_TIMEOUT,
            )
            .await?;
        if with_timeout(DHCP_TIMEOUT, self.station_stack.wait_config_up())
            .await
            .is_err()
        {
            let _ = self.control.disconnect().await;
            return Err(CoprocessorWifiError::DhcpTimeout);
        }
        self.station = Some(configuration.ssid().to_string());
        Ok(())
    }

    async fn disconnect_station(&mut self) -> Result<(), Self::Error> {
        if self.ready().is_ok() {
            self.control.disconnect().await?;
        }
        self.station = None;
        Ok(())
    }
}

/// A Wi-Fi operation on the co-processor failed.
#[derive(Debug)]
pub enum CoprocessorWifiError {
    /// The co-processor is still starting.
    Starting,
    /// The co-processor's Wi-Fi is not running.
    Unavailable(CoprocessorFailure),
    /// The co-processor refused or failed the operation.
    Control(ControlError),
    /// The station associated but DHCP produced no address.
    DhcpTimeout,
}

impl From<ControlError> for CoprocessorWifiError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

impl core::fmt::Display for CoprocessorWifiError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Starting => formatter.write_str("the Wi-Fi co-processor is still starting"),
            Self::Unavailable(failure) => write!(formatter, "Wi-Fi is unavailable: {failure}"),
            Self::Control(error) => write!(formatter, "{error}"),
            Self::DhcpTimeout => formatter.write_str("Wi-Fi station DHCP timed out"),
        }
    }
}

impl core::error::Error for CoprocessorWifiError {}
