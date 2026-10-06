// Shared ESP process entry for Platforms with a radio, instantiated inside
// each Platform's `application` module. The including module names its chip
// as `CHIP`, sizes its two internal heaps with `HEAP_BYTES` and
// `RECLAIMED_HEAP_BYTES`, imports `PlatformBindings`, `PlatformError`,
// `Flash`, `WifiDevice` and `WifiError` from its Platform, and defines how
// the entry initializes bulk memory through the `__initialize_bulk_memory!`
// macro.

use core::cell::RefCell;
use core::net::Ipv4Addr;

use embassy_net::{Ipv4Cidr, Runner, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use esp_hal::rng::Rng;
use esp_radio::wifi::{ControllerConfig, Interface, WifiController};
use static_cell::StaticCell;

#[doc(hidden)]
pub use esp_backtrace as __backtrace;
#[doc(hidden)]
pub use esp_bootloader_esp_idf as __bootloader;
#[doc(hidden)]
pub use esp_hal as __hal;
#[doc(hidden)]
pub use esp_println as __println;
#[doc(hidden)]
pub use esp_rtos as __rtos;
#[doc(hidden)]
pub use static_cell::StaticCell as __StaticCell;

// Panic-abort builds still emit references from `.eh_frame` sections. No
// unwinding reaches this symbol.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}

#[embassy_executor::task(pool_size = 2)]
async fn run_wifi_network(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
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
    wifi_token: esp_hal::peripherals::WIFI<'static>,
) -> Result<PlatformBindings, PlatformError> {
    static STATION_NETWORK: StaticCell<StackResources<16>> = StaticCell::new();
    static ACCESS_POINT_NETWORK: StaticCell<StackResources<8>> = StaticCell::new();
    static FLASH: StaticCell<Mutex<CriticalSectionRawMutex, RefCell<Flash<'static>>>> =
        StaticCell::new();

    let station_interface = Interface::station();
    let access_point_interface = Interface::access_point();
    let controller = WifiController::new(wifi_token, ControllerConfig::default())
        .map_err(WifiError::from)?;
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let station_config = embassy_net::Config::dhcpv4(Default::default());
    let access_point_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Addr::new(192, 168, 4, 1), 24),
        gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
        dns_servers: Default::default(),
    });
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
    let wifi = WifiDevice::new(controller, ip_stack, access_point_stack);
    let flash = FLASH.init(Mutex::new(RefCell::new(crate::flash(flash_token))));
    PlatformBindings::from_initialized_services(board, ip_stack, wifi, flash)
}

/// Generates the ESP async entry around one selected application.
#[macro_export]
macro_rules! platform_entry {
    ($board:expr, $board_bindings:ident, $board_bindings_type:path, |$spawner:ident, $platform_bindings:ident, $board_bindings_value:ident| $application:expr) => {
        use $crate::application::__backtrace as _;

        $crate::application::__bootloader::esp_app_desc!();

        #[embassy_executor::task]
        async fn __barracuda_application_task(
            $spawner: embassy_executor::Spawner,
            $board_bindings_value: $board_bindings_type,
            flash: $crate::application::__hal::peripherals::FLASH<'static>,
            wifi: $crate::application::__hal::peripherals::WIFI<'static>,
        ) {
            let $platform_bindings =
                match $crate::application::bindings($board, $spawner, flash, wifi) {
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
            let peripherals =
                $crate::application::__hal::init($crate::application::__hal::Config::default());
            $crate::__initialize_bulk_memory!($board, peripherals);
            let $board_bindings_value = $board_bindings!(peripherals);
            let flash = peripherals.FLASH;
            let wifi = peripherals.WIFI;
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
                        __barracuda_application_task(spawner, $board_bindings_value, flash, wifi)
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
