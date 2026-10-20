//! ESP32-P4 process entry and bootstrap resources.

use core::cell::RefCell;

use barracuda_platform::UnavailableNetworkDriver;
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use static_cell::StaticCell;

use crate::{Esp32P4PlatformBindings, Esp32P4PlatformError};

#[doc(hidden)]
pub use barracuda_platform::FIRMWARE_VERSION as __FIRMWARE_VERSION;
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

#[embassy_executor::task]
async fn run_network(mut runner: Runner<'static, UnavailableNetworkDriver>) -> ! {
    runner.run().await
}

/// Installs the internal-memory regions in the global allocator.
#[doc(hidden)]
pub fn initialize_allocator() {
    esp_alloc::heap_allocator!(size: 64 * 1024);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
}

static EXTERNAL_MEMORY_ALLOCATOR: esp_alloc::ExternalMemory = esp_alloc::ExternalMemory;
static BULK_MEMORY_BACKEND: barracuda_bulk_memory::platform::Backend =
    barracuda_bulk_memory::platform::Backend::new(&EXTERNAL_MEMORY_ALLOCATOR);

/// Initializes Board-declared external memory as the explicit-use bulk domain.
///
/// The ESP32-P4's in-package PSRAM runs on the 16-line hex bus. Without
/// external memory, bulk memory is the global heap.
#[doc(hidden)]
pub fn initialize_bulk_memory(
    board: &barracuda_board::Board,
    psram: esp_hal::peripherals::PSRAM<'static>,
) {
    barracuda_bulk_memory::platform::install_global();
    let Some(memory) = board.hardware().external_memory() else {
        return;
    };
    if memory.interface() != barracuda_board::ExternalMemoryInterface::Hex {
        esp_println::println!("ESP32-P4 drives only hex PSRAM; external memory stays unused");
        return;
    }
    let config = esp_hal::psram::PsramConfig {
        mode: esp_hal::psram::PsramMode::Hex,
        size: esp_hal::psram::PsramSize::Size(memory.size_bytes()),
        ..Default::default()
    };
    esp_alloc::psram_allocator!(psram, esp_hal::psram, config);
    barracuda_bulk_memory::platform::install(&BULK_MEMORY_BACKEND);
}

/// Creates the process-lifetime mechanisms needed by the selected ESP32-P4 Platform.
pub fn bindings(
    board: &barracuda_board::Board,
    spawner: embassy_executor::Spawner,
    flash_token: esp_hal::peripherals::FLASH<'static>,
) -> Result<Esp32P4PlatformBindings, Esp32P4PlatformError> {
    static NETWORK: StaticCell<StackResources<4>> = StaticCell::new();
    static FLASH: StaticCell<
        Mutex<CriticalSectionRawMutex, RefCell<crate::Esp32P4Flash<'static>>>,
    > = StaticCell::new();

    // ESP32-P4 has no radio, so the IP stack never has a link.
    let (ip_stack, runner) = embassy_net::new(
        UnavailableNetworkDriver,
        embassy_net::Config::default(),
        NETWORK.init(StackResources::new()),
        0,
    );
    let network = run_network(runner).map_err(|_error| Esp32P4PlatformError::NetworkTask)?;
    spawner.spawn(network);
    let flash = FLASH.init(Mutex::new(RefCell::new(crate::flash(flash_token))));
    Esp32P4PlatformBindings::from_initialized_services(board, ip_stack, flash)
}

/// Generates the ESP32-P4 async entry around one selected application.
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
        ) {
            let $platform_bindings = match $crate::application::bindings($board, $spawner, flash) {
                Ok(bindings) => bindings,
                Err(error) => {
                    $crate::application::__println::println!(
                        "failed to bootstrap ESP32-P4 Platform: {}",
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

        #[doc = "ESP32-P4 firmware entry point generated by the selected Platform."]
        #[$crate::application::__hal::__macro_implementation::__entry]
        fn main() -> ! {
            $crate::application::initialize_allocator();
            let peripherals =
                $crate::application::__hal::init($crate::application::__hal::Config::default());
            $crate::application::initialize_bulk_memory($board, peripherals.PSRAM);
            let $board_bindings_value = $board_bindings!(peripherals);
            let flash = peripherals.FLASH;
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
                        __barracuda_application_task(spawner, $board_bindings_value, flash)
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
