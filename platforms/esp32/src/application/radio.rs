// Shared ESP process entry for Platforms with a radio, instantiated inside
// each Platform's `application` module. The including module names its chip
// as `CHIP`, sizes its two internal heaps with `HEAP_BYTES` and
// `RECLAIMED_HEAP_BYTES`, imports `PlatformBindings`, `PlatformError`,
// `Flash` and `Wifi` from its Platform, and `WifiDevice` and `WifiError` with
// its `wifi` feature, and defines how the entry initializes bulk memory
// through the `__initialize_bulk_memory!` macro.
//
// The radio is built only with the Platform's `wifi` feature, which the
// Plugin selection enables when an enabled Plugin takes the Platform's Wi-Fi.
// Without it nothing could ever join a network over the radio, so the
// Platform supplies `UnavailableWifiDevice`, an IP stack whose link is always
// down, and entropy seeded from SAR ADC noise at start-up.

use core::cell::RefCell;

use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
#[cfg(feature = "wifi")]
use esp_hal::rng::Rng;
#[cfg(feature = "wifi")]
use esp_radio::wifi::{ControllerConfig, Interface, WifiController};
use static_cell::StaticCell;

#[doc(hidden)]
pub use esp_backtrace as __backtrace;
#[doc(hidden)]
pub use esp_bootloader_esp_idf as __bootloader;
#[doc(hidden)]
pub use barracuda_platform::FIRMWARE_VERSION as __FIRMWARE_VERSION;
#[doc(hidden)]
pub use esp_hal as __hal;
#[doc(hidden)]
pub use esp_println as __println;
#[doc(hidden)]
pub use esp_rtos as __rtos;
#[doc(hidden)]
pub use static_cell::StaticCell as __StaticCell;

// `__barracuda_power_hold!`, projected by `build.rs` from the selected
// Board's `hardware.power-hold`.
include!(concat!(env!("OUT_DIR"), "/power_hold.rs"));

/// Drives a Board's battery power latch high for the life of the firmware.
///
/// The latch keeps a battery-powered Board on once its power button is
/// released, so the entry drives it before anything else is built.
#[doc(hidden)]
pub fn hold_power(pin: impl esp_hal::gpio::OutputPin + 'static) {
    // The driver lives as long as the firmware, so nothing reconfigures the
    // pin until the chip resets or powers off.
    static LATCH: StaticCell<esp_hal::gpio::Output<'static>> = StaticCell::new();
    LATCH.init(esp_hal::gpio::Output::new(
        pin,
        esp_hal::gpio::Level::High,
        esp_hal::gpio::OutputConfig::default(),
    ));
}

// Panic-abort builds still emit references from `.eh_frame` sections. No
// unwinding reaches this symbol.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}

#[cfg(feature = "wifi")]
#[embassy_executor::task(pool_size = 2)]
async fn run_wifi_network(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

#[cfg(not(feature = "wifi"))]
#[embassy_executor::task]
async fn run_network(mut runner: Runner<'static, barracuda_platform::UnavailableNetworkDriver>) -> ! {
    runner.run().await
}

/// The chip resource the Platform's Wi-Fi takes: the radio.
#[cfg(feature = "wifi")]
#[doc(hidden)]
pub type Radio = esp_hal::peripherals::WIFI<'static>;

/// The chip resource the Platform's Wi-Fi takes: none, without the radio.
#[cfg(not(feature = "wifi"))]
#[doc(hidden)]
pub type Radio = ();

/// Keeps the radio for the Platform's Wi-Fi, or lets it go unused.
#[doc(hidden)]
#[must_use]
pub fn radio(radio: esp_hal::peripherals::WIFI<'static>) -> Radio {
    #[cfg(feature = "wifi")]
    {
        radio
    }
    #[cfg(not(feature = "wifi"))]
    {
        let _unused = radio;
    }
}

/// Starts the Platform's entropy before the Board takes the ADC.
///
/// With the radio, the radio feeds the random number generator once it runs.
/// Without it, this seeds the entropy generator from SAR ADC noise.
#[doc(hidden)]
pub fn prepare_entropy(rng: esp_hal::peripherals::RNG<'_>, adc: esp_hal::peripherals::ADC1<'_>) {
    #[cfg(feature = "wifi")]
    let _fed_by_the_radio = (rng, adc);
    #[cfg(not(feature = "wifi"))]
    crate::entropy::seed(rng, adc);
}

/// Installs only internal-memory regions in the global allocator.
///
/// ESP atomic instructions cannot safely target PSRAM. Long-lived System
/// futures contain atomics, so PSRAM must remain an explicit-use allocator
/// instead of becoming part of the global heap.
#[doc(hidden)]
pub fn initialize_allocator() {
    esp_alloc::heap_allocator!(size: HEAP_BYTES);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: RECLAIMED_HEAP_BYTES);
}

/// Creates the process-lifetime mechanisms needed by the selected Platform.
pub fn bindings(
    board: &barracuda_board::Board,
    spawner: embassy_executor::Spawner,
    flash_token: esp_hal::peripherals::FLASH<'static>,
    radio: Radio,
) -> Result<PlatformBindings, PlatformError> {
    static FLASH: StaticCell<Mutex<CriticalSectionRawMutex, RefCell<Flash<'static>>>> =
        StaticCell::new();

    let (ip_stack, wifi) = network(spawner, radio)?;
    let flash = FLASH.init(Mutex::new(RefCell::new(crate::flash(flash_token))));
    PlatformBindings::from_initialized_services(board, ip_stack, wifi, flash)
}

/// Starts the radio and the IP stacks over its station and access point.
#[cfg(feature = "wifi")]
fn network(
    spawner: embassy_executor::Spawner,
    wifi_token: Radio,
) -> Result<(embassy_net::Stack<'static>, Wifi), PlatformError> {
    static STATION_NETWORK: StaticCell<StackResources<16>> = StaticCell::new();
    static ACCESS_POINT_NETWORK: StaticCell<StackResources<8>> = StaticCell::new();

    let station_interface = Interface::station();
    let access_point_interface = Interface::access_point();
    let controller = WifiController::new(wifi_token, ControllerConfig::default())
        .map_err(WifiError::from)?;
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let station_config = embassy_net::Config::dhcpv4(Default::default());
    // The access point's address is its consumer's choice, assigned when it
    // starts the access point's services.
    let access_point_config = embassy_net::Config::default();
    let (ip_stack, station_runner) = embassy_net::new(
        station_interface,
        station_config,
        STATION_NETWORK.init(StackResources::new()),
        seed,
    );
    let (access_point_stack, access_point_runner) = embassy_net::new(
        access_point_interface,
        access_point_config,
        ACCESS_POINT_NETWORK.init(StackResources::new()),
        seed,
    );
    let station_task =
        run_wifi_network(station_runner).map_err(|_error| PlatformError::NetworkTask)?;
    let access_point_task =
        run_wifi_network(access_point_runner).map_err(|_error| PlatformError::NetworkTask)?;
    spawner.spawn(station_task);
    spawner.spawn(access_point_task);
    Ok((
        ip_stack,
        WifiDevice::new(controller, ip_stack, access_point_stack),
    ))
}

/// Starts the IP stack without a link: with no radio, no network is ever
/// joined, so Plugins see a stack that stays down, as on a radio-less chip.
#[cfg(not(feature = "wifi"))]
fn network(
    spawner: embassy_executor::Spawner,
    _radio: Radio,
) -> Result<(embassy_net::Stack<'static>, Wifi), PlatformError> {
    static NETWORK: StaticCell<StackResources<16>> = StaticCell::new();

    // The stack never carries traffic, so its sequence numbers need no more
    // than what the entropy generator gives.
    let mut seed = [0_u8; 8];
    let seed = match crate::entropy::fill(&mut seed) {
        Ok(()) => u64::from_le_bytes(seed),
        Err(_unavailable) => 0,
    };
    let (ip_stack, runner) = embassy_net::new(
        barracuda_platform::UnavailableNetworkDriver,
        embassy_net::Config::default(),
        NETWORK.init(StackResources::new()),
        seed,
    );
    let task = run_network(runner).map_err(|_error| PlatformError::NetworkTask)?;
    spawner.spawn(task);
    Ok((ip_stack, barracuda_platform::UnavailableWifiDevice::new(ip_stack)))
}

/// Generates the ESP async entry around one selected application.
#[macro_export]
macro_rules! platform_entry {
    ($board:expr, $board_bindings:ident, $board_bindings_type:path, |$spawner:ident, $platform_bindings:ident, $board_bindings_value:ident| $application:expr) => {
        use $crate::application::__backtrace as _;

        // The app descriptor names the release, so a flasher can read which
        // Barracuda a board already holds.
        $crate::application::__bootloader::esp_app_desc!(
            $crate::application::__FIRMWARE_VERSION,
            "barracuda",
            $crate::application::__bootloader::BUILD_TIME,
            $crate::application::__bootloader::BUILD_DATE,
            $crate::application::__bootloader::ESP_IDF_COMPATIBLE_VERSION,
            $crate::application::__bootloader::MMU_PAGE_SIZE,
            0,
            u16::MAX,
            $crate::application::__bootloader::SECURE_VERSION
        );

        #[embassy_executor::task]
        async fn __barracuda_application_task(
            $spawner: embassy_executor::Spawner,
            $board_bindings_value: $board_bindings_type,
            flash: $crate::application::__hal::peripherals::FLASH<'static>,
            radio: $crate::application::Radio,
        ) {
            let $platform_bindings =
                match $crate::application::bindings($board, $spawner, flash, radio) {
                    Ok(bindings) => bindings,
                    Err(error) => {
                        $crate::application::__println::println!(
                            "failed to bootstrap {} Platform: {}",
                            $crate::application::CHIP,
                            error
                        );
                        loop {
                            core::hint::spin_loop();
                        }
                    }
                };
            if let Err(error) = $application.await {
                $crate::application::__println::println!("application exited: {}", error);
                loop {
                    core::hint::spin_loop();
                }
            }
        }

        #[doc = "ESP firmware entry point generated by the selected Platform."]
        #[$crate::application::__hal::__macro_implementation::__entry]
        fn main() -> ! {
            $crate::application::initialize_allocator();
            let mut peripherals =
                $crate::application::__hal::init($crate::application::__hal::Config::default());
            // A battery power latch goes high first, or the Board switches
            // off when its power button is released.
            $crate::__barracuda_power_hold!(peripherals);
            $crate::__initialize_bulk_memory!($board, peripherals);
            // Entropy may borrow the ADC, so it starts before the Board takes it.
            $crate::application::prepare_entropy(
                peripherals.RNG.reborrow(),
                peripherals.ADC1.reborrow(),
            );
            let $board_bindings_value = $board_bindings!(peripherals);
            let flash = peripherals.FLASH;
            let radio = $crate::application::radio(peripherals.WIFI);
            let timer_group =
                $crate::application::__hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
            $crate::application::__rtos::start(timer_group.timer0, peripherals.FROM_CPU_INTR0);
            static EXECUTOR: $crate::application::__StaticCell<
                $crate::application::__rtos::embassy::Executor,
            > = $crate::application::__StaticCell::new();
            EXECUTOR
                .init($crate::application::__rtos::embassy::Executor::new())
                .run(move |spawner| {
                    if let Ok(task) =
                        __barracuda_application_task(spawner, $board_bindings_value, flash, radio)
                    {
                        spawner.spawn(task);
                    } else {
                        $crate::application::__println::println!(
                            "failed to spawn Barracuda application"
                        );
                    }
                })
        }
    };
}
