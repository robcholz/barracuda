//! ESP32-S3 process entry and bootstrap resources.

mod c_abi;

use core::cell::RefCell;
use core::task::Context;

use embassy_net::{Runner, StackResources};
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use static_cell::StaticCell;

use crate::{Esp32S3PlatformBindings, Esp32S3PlatformError};

static EXTERNAL_MEMORY_ALLOCATOR: esp_alloc::ExternalMemory = esp_alloc::ExternalMemory;
static BULK_MEMORY_BACKEND: barracuda_bulk_memory::platform::Backend =
    barracuda_bulk_memory::platform::Backend::new(&EXTERNAL_MEMORY_ALLOCATOR);

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

struct OfflineDriver;

enum NeverToken {}

impl RxToken for NeverToken {
    fn consume<R, F>(self, _f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {}
    }
}

impl TxToken for NeverToken {
    fn consume<R, F>(self, _len: usize, _f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {}
    }
}

impl Driver for OfflineDriver {
    type RxToken<'a> = NeverToken;
    type TxToken<'a> = NeverToken;

    fn receive(&mut self, _cx: &mut Context<'_>) -> Option<(NeverToken, NeverToken)> {
        None
    }

    fn transmit(&mut self, _cx: &mut Context<'_>) -> Option<NeverToken> {
        None
    }

    fn link_state(&mut self, _cx: &mut Context<'_>) -> LinkState {
        LinkState::Down
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.max_transmission_unit = 1514;
        capabilities
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet([0x02, 0, 0, 0, 0, 1])
    }
}

#[embassy_executor::task]
async fn run_offline_network(mut runner: Runner<'static, OfflineDriver>) -> ! {
    runner.run().await
}

/// Installs only internal-memory regions in the global allocator.
///
/// ESP32-S3 atomic instructions cannot safely target PSRAM. Long-lived System
/// futures contain atomics, so PSRAM must remain an explicit-use allocator
/// instead of becoming part of the global heap.
#[doc(hidden)]
pub fn initialize_allocator() {
    esp_alloc::heap_allocator!(size: 64 * 1024);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
}

/// Initializes Board-declared external memory as the explicit-use bulk domain.
#[doc(hidden)]
pub fn initialize_bulk_memory(
    board: &barracuda_board::Board,
    psram: esp_hal::peripherals::PSRAM<'static>,
) {
    barracuda_bulk_memory::platform::install_global();
    let Some(memory) = board.hardware().external_memory() else {
        return;
    };
    let mode = match memory.interface() {
        barracuda_board::ExternalMemoryInterface::QuadSpi => esp_hal::psram::PsramMode::QuadSpi,
        barracuda_board::ExternalMemoryInterface::OctalSpi => esp_hal::psram::PsramMode::OctalSpi,
    };
    let config = esp_hal::psram::PsramConfig {
        mode,
        size: esp_hal::psram::PsramSize::Size(memory.size_bytes()),
        ..Default::default()
    };
    esp_alloc::psram_allocator!(psram, esp_hal::psram, config);
    barracuda_bulk_memory::platform::install(&BULK_MEMORY_BACKEND);
}

/// Creates the process-lifetime mechanisms needed by the selected S3 Platform.
pub fn bindings(
    board: &barracuda_board::Board,
    spawner: embassy_executor::Spawner,
    flash_token: esp_hal::peripherals::FLASH<'static>,
) -> Result<Esp32S3PlatformBindings, Esp32S3PlatformError> {
    static NETWORK: StaticCell<StackResources<16>> = StaticCell::new();
    static FLASH: StaticCell<
        Mutex<CriticalSectionRawMutex, RefCell<crate::Esp32S3Flash<'static>>>,
    > = StaticCell::new();

    let network = NETWORK.init(StackResources::new());
    let (ip_stack, runner) = embassy_net::new(OfflineDriver, Default::default(), network, 0);
    if let Ok(task) = run_offline_network(runner) {
        spawner.spawn(task);
    } else {
        log::error!("failed to spawn ESP32-S3 network runner");
    }
    let flash = FLASH.init(Mutex::new(RefCell::new(crate::flash(flash_token))));
    Esp32S3PlatformBindings::from_initialized_services(board, ip_stack, flash)
}

/// Generates the ESP32-S3 async entry around one selected application.
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
        ) {
            let $platform_bindings = match $crate::application::bindings($board, $spawner, flash) {
                Ok(bindings) => bindings,
                Err(error) => {
                    $crate::application::__println::println!(
                        "failed to bootstrap ESP32-S3 Platform: {}",
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

        #[doc = "ESP32-S3 firmware entry point generated by the selected Platform."]
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
