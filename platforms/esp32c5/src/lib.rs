//! ESP32-C5 Platform mechanisms backed by an ESP-IDF native partition table.

#![no_std]

/// Runtime access discipline declared by ESP-IDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Esp32C5RegionAccess {
    /// Provisioned bytes cannot be changed at runtime.
    ReadOnly,
    /// Runtime code may erase and program the region.
    ReadWrite,
}

/// One Board-bound region from the ESP-IDF partition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp32C5Region {
    name: &'static str,
    offset: u32,
    size: u32,
    access: Esp32C5RegionAccess,
}

impl Esp32C5Region {
    /// Creates a generated native ESP32 region.
    #[must_use]
    pub const fn new(
        name: &'static str,
        offset: u32,
        size: u32,
        access: Esp32C5RegionAccess,
    ) -> Self {
        Self {
            name,
            offset,
            size,
            access,
        }
    }

    /// Returns the ESP-IDF partition label.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the flash-relative byte offset.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Returns the region size in bytes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Returns the ESP-IDF runtime access flag.
    #[must_use]
    pub const fn access(&self) -> Esp32C5RegionAccess {
        self.access
    }
}

/// Complete native ESP-IDF partition table selected by the Board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp32C5PartitionTable {
    chip: &'static str,
    ota_slot_count: usize,
    regions: &'static [Esp32C5Region],
}

impl Esp32C5PartitionTable {
    /// Creates a build-validated projection of a complete ESP-IDF table.
    #[must_use]
    pub const fn new(
        chip: &'static str,
        ota_slot_count: usize,
        regions: &'static [Esp32C5Region],
    ) -> Self {
        Self {
            chip,
            ota_slot_count,
            regions,
        }
    }

    /// Returns the selected ESP HAL chip.
    #[must_use]
    pub const fn chip(&self) -> &'static str {
        self.chip
    }

    /// Returns the number of OTA application slots in the native table.
    #[must_use]
    pub const fn ota_slot_count(&self) -> usize {
        self.ota_slot_count
    }

    /// Returns every entry from the native table in native order.
    #[must_use]
    pub const fn regions(&self) -> &'static [Esp32C5Region] {
        self.regions
    }

    /// Finds a region by its native ESP-IDF label.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Esp32C5Region> {
        self.regions.iter().find(|region| region.name() == name)
    }
}

include!(concat!(env!("OUT_DIR"), "/esp32c5_layout.rs"));

#[cfg(target_arch = "riscv32")]
mod internal_flash {
    use core::{cell::RefCell, ffi::CStr};

    use barracuda_board::Board;
    use embassy_embedded_hal::flash::partition::BlockingPartition;
    use embassy_executor::Spawner;
    use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
    use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
    use esp_hal::{
        peripherals::{FLASH, FROM_CPU_INTR0, TIMG0, WIFI},
        rng::Rng,
        timer::timg::TimerGroup,
    };
    use esp_radio::wifi::{
        ap::AccessPointConfig, Config as WifiConfig, ControllerConfig, Interface, WifiController,
    };
    use static_cell::StaticCell;

    use barracuda_platform::{
        NamedPartition, PartitionAccess, Partitions, PartitionsInsertError, Platform,
        PlatformInitResult, PlatformResources,
    };

    use crate::{Esp32C5RegionAccess, BOARD_ESP32C5_PARTITION_TABLE};

    // The embedded System currently needs one listener plus a small number of
    // concurrent client sockets. Keep this Platform mechanism bounded so its
    // static pool does not consume RAM needed by portable System Components.
    const NETWORK_SOCKET_COUNT: usize = 6;
    const NETWORK_SEED: u64 = 0x6261_7272_6163_7564;
    const TRUST_ROOT_PEM: &[u8] = concat!(include_str!("../isrg-root-x1.pem"), "\0").as_bytes();

    type SharedFlash = Mutex<CriticalSectionRawMutex, RefCell<Esp32C5Flash<'static>>>;

    static FLASH_STORAGE: StaticCell<SharedFlash> = StaticCell::new();
    static NETWORK_RESOURCES: StaticCell<StackResources<NETWORK_SOCKET_COUNT>> = StaticCell::new();
    static TLS_RNG: StaticCell<Esp32C5CryptoRng> = StaticCell::new();

    /// C5 hardware RNG adapter used only while the radio entropy source is active.
    struct Esp32C5CryptoRng(Rng);

    impl rand_core::TryRng for Esp32C5CryptoRng {
        type Error = core::convert::Infallible;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Ok(self.0.random())
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            let mut bytes = [0; 8];
            self.0.read(&mut bytes);
            Ok(u64::from_le_bytes(bytes))
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
            self.0.read(destination);
            Ok(())
        }
    }

    impl rand_core::TryCryptoRng for Esp32C5CryptoRng {}

    /// ESP HAL synchronous internal flash driver.
    pub type Esp32C5Flash<'d> = esp_storage::FlashStorage<'d>;

    /// One ESP-IDF partition backed by ESP HAL flash.
    pub type Esp32C5Partition =
        BlockingPartition<'static, CriticalSectionRawMutex, Esp32C5Flash<'static>>;

    /// Generic named ESP-IDF partitions available to System.
    pub type Esp32C5Partitions = Partitions<Esp32C5Partition, 16>;

    /// ESP Platform implementation for the selected ESP32-C5 target.
    pub struct Esp32C5Platform;

    /// Board/HAL bindings consumed by [`Esp32C5Platform`].
    pub struct Esp32C5PlatformBindings {
        flash: FLASH<'static>,
        wifi: WIFI<'static>,
        timer_group: TIMG0<'static>,
        software_interrupt: FROM_CPU_INTR0<'static>,
    }

    impl Esp32C5PlatformBindings {
        /// Assigns raw ESP chip resources to the C5 Platform axis.
        ///
        /// # Errors
        ///
        /// Returns an error when the selected Board does not use ESP32-C5.
        pub fn new(
            board: &Board,
            flash: FLASH<'static>,
            wifi: WIFI<'static>,
            timer_group: TIMG0<'static>,
            software_interrupt: FROM_CPU_INTR0<'static>,
        ) -> Result<Self, Esp32C5PlatformError> {
            if board.hardware().chip() != "esp32c5" {
                return Err(Esp32C5PlatformError::IncompatibleChip);
            }
            Ok(Self {
                flash,
                wifi,
                timer_group,
                software_interrupt,
            })
        }
    }

    /// Projects every build-validated native entry from shared ESP flash.
    ///
    /// # Errors
    ///
    /// Returns an error if the Platform collection capacity is smaller than
    /// the selected ESP-IDF table.
    pub fn partitions(
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32C5Flash<'static>>>,
    ) -> Result<Esp32C5Partitions, barracuda_platform::PartitionsInsertError> {
        let mut partitions = Esp32C5Partitions::new();
        for region in BOARD_ESP32C5_PARTITION_TABLE.regions() {
            let access = match region.access() {
                Esp32C5RegionAccess::ReadOnly => PartitionAccess::ReadOnly,
                Esp32C5RegionAccess::ReadWrite => PartitionAccess::ReadWrite,
            };
            partitions.insert(NamedPartition::new(
                region.name(),
                access,
                BlockingPartition::new(flash, region.offset(), region.size()),
            ))?;
        }
        Ok(partitions)
    }

    #[embassy_executor::task]
    async fn run_network(mut runner: Runner<'static, Interface>) {
        runner.run().await
    }

    #[embassy_executor::task]
    async fn own_wifi_controller(controller: WifiController<'static>) {
        loop {
            match controller
                .wait_for_access_point_connected_event_async()
                .await
            {
                Ok(esp_radio::wifi::ap::EventInfo::Connected(info)) => {
                    log::info!("ESP32-C5 access-point station connected: {info:?}");
                }
                Ok(esp_radio::wifi::ap::EventInfo::Disconnected(info)) => {
                    log::info!("ESP32-C5 access-point station disconnected: {info:?}");
                }
                Err(error) => {
                    log::warn!("ESP32-C5 access-point event error: {error:?}");
                }
            }
        }
    }

    fn initialize_network(
        spawner: Spawner,
        wifi: WIFI<'static>,
    ) -> Result<Stack<'static>, Esp32C5PlatformError> {
        let ssid = "Barracuda-C5".try_into()?;
        let wifi_config = WifiConfig::AccessPoint(AccessPointConfig::default().with_ssid(ssid));
        // Barracuda's control-plane AP favors bounded memory over bulk Wi-Fi
        // throughput. These are ESP driver mechanisms, so the policy belongs
        // to this Platform implementation rather than the portable device app.
        let controller_config = ControllerConfig::default()
            .with_rx_queue_size(3)
            .with_tx_queue_size(2)
            .with_static_rx_buf_num(4)
            .with_dynamic_rx_buf_num(8)
            .with_dynamic_tx_buf_num(8)
            .with_ampdu_rx_enable(false)
            .with_ampdu_tx_enable(false)
            .with_rx_ba_win(2)
            .with_initial_config(wifi_config);
        let controller = WifiController::new(wifi, controller_config)?;
        let interface = Interface::access_point();
        let network_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 4, 1), 24),
            gateway: None,
            dns_servers: Default::default(),
        });
        let (stack, runner) = embassy_net::new(
            interface,
            network_config,
            NETWORK_RESOURCES.init(StackResources::new()),
            NETWORK_SEED,
        );
        let network_task =
            run_network(runner).map_err(|_error| Esp32C5PlatformError::SpawnNetwork)?;
        spawner.spawn(network_task);
        let controller_task = own_wifi_controller(controller)
            .map_err(|_error| Esp32C5PlatformError::SpawnWifiController)?;
        spawner.spawn(controller_task);
        Ok(stack)
    }

    fn initialize_tls() -> Result<barracuda_tls::MbedTls, Esp32C5PlatformError> {
        let trust_root = CStr::from_bytes_with_nul(TRUST_ROOT_PEM)
            .map_err(|_error| Esp32C5PlatformError::InvalidTrustRoot)?;
        let rng = Esp32C5CryptoRng(Rng::new());
        barracuda_tls::MbedTls::from_pem(TLS_RNG.init(rng), trust_root).map_err(Into::into)
    }

    impl Platform for Esp32C5Platform {
        type Bindings = Esp32C5PlatformBindings;
        type Tls = barracuda_tls::MbedTls;
        type Partitions = Esp32C5Partitions;
        type Error = Esp32C5PlatformError;

        fn prepare() -> Result<(), Self::Error> {
            esp_println::logger::init_logger(crate::PLATFORM_LOG_LEVEL);
            log::info!("preparing ESP32-C5 Platform");
            Ok(())
        }

        async fn initialize(
            spawner: Spawner,
            bindings: Self::Bindings,
        ) -> PlatformInitResult<Self> {
            log::info!("starting ESP32-C5 Platform runtime");
            let timer_group = TimerGroup::new(bindings.timer_group);
            esp_rtos::start(timer_group.timer0, bindings.software_interrupt);

            log::info!("initializing ESP32-C5 Platform partitions");
            let flash = FLASH_STORAGE.init(Mutex::new(RefCell::new(
                esp_storage::FlashStorage::new(bindings.flash),
            )));
            let partitions = partitions(flash)?;
            log::info!("initializing ESP32-C5 Platform network");
            let ip_stack = initialize_network(spawner, bindings.wifi)?;
            log::info!("initializing ESP32-C5 Platform TLS");
            let tls = initialize_tls()?;
            log::info!("initialized ESP32-C5 Platform");
            Ok(PlatformResources {
                ip_stack,
                tls,
                partitions,
            })
        }
    }

    /// ESP32-C5 Platform initialization failure.
    #[derive(Debug)]
    pub enum Esp32C5PlatformError {
        /// The selected Board targets a different chip family.
        IncompatibleChip,
        /// The native table exceeded or violated the generic collection.
        Partitions(PartitionsInsertError),
        /// Platform TLS initialization failed.
        Tls(barracuda_tls::TlsError),
        /// ESP radio or Wi-Fi initialization failed.
        Wifi(esp_radio::wifi::WifiError),
        /// The Platform could not spawn its permanent Embassy network runner.
        SpawnNetwork,
        /// The Platform could not spawn the task owning the Wi-Fi controller.
        SpawnWifiController,
        /// The embedded trust root was not one NUL-terminated PEM string.
        InvalidTrustRoot,
    }

    impl From<PartitionsInsertError> for Esp32C5PlatformError {
        fn from(error: PartitionsInsertError) -> Self {
            Self::Partitions(error)
        }
    }

    impl From<barracuda_tls::TlsError> for Esp32C5PlatformError {
        fn from(error: barracuda_tls::TlsError) -> Self {
            Self::Tls(error)
        }
    }

    impl From<esp_radio::wifi::WifiError> for Esp32C5PlatformError {
        fn from(error: esp_radio::wifi::WifiError) -> Self {
            Self::Wifi(error)
        }
    }

    impl core::fmt::Display for Esp32C5PlatformError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::IncompatibleChip => formatter.write_str("incompatible ESP32 chip"),
                Self::Partitions(error) => write!(formatter, "invalid ESP32 partitions: {error}"),
                Self::Tls(error) => write!(formatter, "failed to initialize ESP32 TLS: {error}"),
                Self::Wifi(error) => {
                    write!(formatter, "failed to initialize ESP32 Wi-Fi: {error:?}")
                }
                Self::SpawnNetwork => formatter.write_str("failed to spawn ESP32 network runner"),
                Self::SpawnWifiController => {
                    formatter.write_str("failed to spawn ESP32 Wi-Fi controller owner")
                }
                Self::InvalidTrustRoot => formatter.write_str("invalid embedded ESP32 trust root"),
            }
        }
    }

    impl core::error::Error for Esp32C5PlatformError {}
}

#[cfg(target_arch = "riscv32")]
pub use internal_flash::{
    partitions, Esp32C5Flash, Esp32C5Partition, Esp32C5Partitions, Esp32C5Platform,
    Esp32C5PlatformBindings, Esp32C5PlatformError,
};
