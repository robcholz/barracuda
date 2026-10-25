//! ESP32-P4 process entry and bootstrap resources.

use core::cell::RefCell;

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

// `NetworkTokens` and `__barracuda_esp32p4_network_tokens!`, projected by
// `build.rs` from the selected Board's network links.
include!(concat!(env!("OUT_DIR"), "/esp32p4_network.rs"));

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

/// Powers a Board's Wi-Fi co-processor through one output of a PI4IOE5V6408
/// I2C expander, driving it high.
///
/// It runs before the Board composition takes the I2C bus, on a borrowed
/// controller and pins, and leaves the expander's other outputs as they are.
/// A failure is printed and left to the co-processor's start-up to report.
#[doc(hidden)]
pub fn power_coprocessor_through_pi4ioe5v6408<'d>(
    i2c: impl esp_hal::i2c::master::Instance + 'd,
    scl: impl esp_hal::gpio::interconnect::PeripheralInput<'d>
        + esp_hal::gpio::interconnect::PeripheralOutput<'d>
        + 'd,
    sda: impl esp_hal::gpio::interconnect::PeripheralInput<'d>
        + esp_hal::gpio::interconnect::PeripheralOutput<'d>
        + 'd,
    address: u8,
    pin: u8,
) {
    let config =
        esp_hal::i2c::master::Config::default().with_frequency(esp_hal::time::Rate::from_khz(100));
    let mut bus = match esp_hal::i2c::master::I2c::new(i2c, config) {
        Ok(bus) => bus.with_scl(scl).with_sda(sda),
        Err(error) => {
            esp_println::println!("Wi-Fi co-processor power switch: I2C {error:?}");
            return;
        }
    };
    let powered = barracuda_driver_pi4ioe5v6408::Pi4ioe5v6408::new(&mut bus, address)
        .and_then(|mut expander| expander.configure_output(pin, true));
    if let Err(error) = powered {
        esp_println::println!("Wi-Fi co-processor power switch at {address:#04x}: {error:?}");
    }
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
    network: NetworkTokens,
    entropy: crate::Esp32P4Entropy,
) -> Result<Esp32P4PlatformBindings, Esp32P4PlatformError> {
    static FLASH: StaticCell<
        Mutex<CriticalSectionRawMutex, RefCell<crate::Esp32P4Flash<'static>>>,
    > = StaticCell::new();

    // The ESP32-P4 has no radio; the IP stack runs over the links the Board
    // declares, or stays down without one.
    let links = crate::network::start(spawner, network)?;
    let flash = FLASH.init(Mutex::new(RefCell::new(crate::flash(flash_token))));
    Esp32P4PlatformBindings::from_initialized_services(board, links, entropy, flash)
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
            network: $crate::application::NetworkTokens,
            entropy: $crate::Esp32P4Entropy,
        ) {
            let $platform_bindings =
                match $crate::application::bindings($board, $spawner, flash, network, entropy) {
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
            #[allow(unused_mut)]
            let mut peripherals =
                $crate::application::__hal::init($crate::application::__hal::Config::default());
            // A battery power latch goes high first, or the Board switches
            // off when its power button is released.
            $crate::__barracuda_power_hold!(peripherals);
            // The noise source starts before any task runs and keeps SAR
            // ADC1 from here on.
            let entropy = $crate::Esp32P4Entropy::start(peripherals.RNG, peripherals.ADC1);
            // A Board whose Wi-Fi co-processor sits behind a power switch on
            // a shared bus turns it on before the Board takes the bus.
            $crate::__barracuda_esp32p4_network_power!(peripherals);
            $crate::application::initialize_bulk_memory($board, peripherals.PSRAM);
            // The Platform owns the one SD host: it keeps slot 1 for a Wi-Fi
            // co-processor link and hands slot 0 to the Board's card.
            #[allow(unused_variables)]
            let sd_host = $crate::sd_host::SdHost::take(peripherals.SDHOST);
            let $board_bindings_value = $board_bindings!(peripherals, sd_host.board);
            let network = $crate::__barracuda_esp32p4_network_tokens!(peripherals, sd_host);
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
                    if let Ok(task) = __barracuda_application_task(
                        spawner,
                        $board_bindings_value,
                        flash,
                        network,
                        entropy,
                    ) {
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
