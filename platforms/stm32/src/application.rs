//! STM32 process entry and bootstrap resources.

use core::cell::RefCell;

use barracuda_platform::UnavailableNetworkDriver;
use embassy_net::{Runner, StackResources};
use embassy_stm32::flash::Flash;
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use panic_halt as _;
use static_cell::StaticCell;

use crate::{Stm32Flash, Stm32PlatformBindings, Stm32PlatformError, Stm32Rng};

embassy_stm32::bind_interrupts!(struct Irqs {
    RNG => embassy_stm32::rng::InterruptHandler<embassy_stm32::peripherals::RNG>;
});

#[doc(hidden)]
pub use cortex_m_rt as __rt;
#[doc(hidden)]
pub use embassy_stm32 as __hal;
#[doc(hidden)]
pub use log as __log;
#[doc(hidden)]
pub use static_cell::StaticCell as __StaticCell;

/// Bytes of internal RAM given to the global allocator.
const HEAP_SIZE: usize = 512 * 1024;

#[global_allocator]
static HEAP: embedded_alloc::TlsfHeap = embedded_alloc::TlsfHeap::empty();

#[embassy_executor::task]
async fn run_network(mut runner: Runner<'static, UnavailableNetworkDriver>) -> ! {
    runner.run().await
}

/// Runs the system clock at 160 MHz from PLL1 fed by the 16 MHz HSI.
#[doc(hidden)]
#[must_use]
pub fn clock_config() -> embassy_stm32::Config {
    use embassy_stm32::rcc::{
        AHBPrescaler, APBPrescaler, Pll, PllDiv, PllMul, PllPreDiv, PllSource, Sysclk, VoltageScale,
    };

    let mut config = embassy_stm32::Config::default();
    config.rcc.hsi = true;
    config.rcc.pll1 = Some(Pll {
        source: PllSource::HSI,
        prediv: PllPreDiv::DIV1,
        mul: PllMul::MUL20,
        divp: None,
        divq: None,
        divr: Some(PllDiv::DIV2),
    });
    config.rcc.sys = Sysclk::PLL1_R;
    config.rcc.ahb_pre = AHBPrescaler::DIV1;
    config.rcc.apb1_pre = APBPrescaler::DIV1;
    config.rcc.apb2_pre = APBPrescaler::DIV1;
    config.rcc.apb3_pre = APBPrescaler::DIV1;
    config.rcc.voltage_range = VoltageScale::RANGE1;
    config
}

/// Installs a static internal-RAM region as the global heap.
#[doc(hidden)]
pub fn initialize_allocator() {
    // SAFETY: the entry calls this once, before anything allocates.
    #[allow(unsafe_code)]
    unsafe {
        embedded_alloc::init!(HEAP, HEAP_SIZE);
    }
}

/// Creates the process-lifetime mechanisms needed by the selected STM32 Platform.
pub fn bindings(
    board: &barracuda_board::Board,
    spawner: embassy_executor::Spawner,
    flash_token: embassy_stm32::Peri<'static, embassy_stm32::peripherals::FLASH>,
    rng_token: embassy_stm32::Peri<'static, embassy_stm32::peripherals::RNG>,
) -> Result<Stm32PlatformBindings, Stm32PlatformError> {
    static NETWORK: StaticCell<StackResources<4>> = StaticCell::new();
    static FLASH: StaticCell<Mutex<CriticalSectionRawMutex, RefCell<Stm32Flash>>> =
        StaticCell::new();
    static RNG: StaticCell<Mutex<CriticalSectionRawMutex, RefCell<Stm32Rng>>> = StaticCell::new();

    // The STM32U5A5 has no Ethernet MAC and no Board network interface is
    // wired yet, so the IP stack never has a link.
    let (ip_stack, runner) = embassy_net::new(
        UnavailableNetworkDriver,
        embassy_net::Config::default(),
        NETWORK.init(StackResources::new()),
        0,
    );
    let network = run_network(runner).map_err(|_error| Stm32PlatformError::NetworkTask)?;
    spawner.spawn(network);
    let flash = FLASH.init(Mutex::new(RefCell::new(Flash::new_blocking(flash_token))));
    // The RNG runs from HSI48, which the default clock configuration enables.
    let rng = RNG.init(Mutex::new(RefCell::new(embassy_stm32::rng::Rng::new(
        rng_token, Irqs,
    ))));
    Stm32PlatformBindings::from_initialized_services(board, ip_stack, flash, rng)
}

/// Generates the STM32 async entry around one selected application.
#[macro_export]
macro_rules! platform_entry {
    ($board:expr, $board_bindings:ident, $board_bindings_type:path, |$spawner:ident, $platform_bindings:ident, $board_bindings_value:ident| $application:expr) => {
        #[embassy_executor::task]
        async fn __barracuda_application_task(
            $spawner: embassy_executor::Spawner,
            $board_bindings_value: $board_bindings_type,
            flash: $crate::application::__hal::Peri<
                'static,
                $crate::application::__hal::peripherals::FLASH,
            >,
            rng: $crate::application::__hal::Peri<
                'static,
                $crate::application::__hal::peripherals::RNG,
            >,
        ) {
            let $platform_bindings =
                match $crate::application::bindings($board, $spawner, flash, rng) {
                    Ok(bindings) => bindings,
                    Err(error) => {
                        $crate::application::__log::error!(
                            "failed to bootstrap STM32 Platform: {}",
                            error
                        );
                        loop {
                            core::hint::spin_loop();
                        }
                    }
                };
            if let Err(error) = $application.await {
                $crate::application::__log::error!("application exited: {}", error);
                loop {
                    core::hint::spin_loop();
                }
            }
        }

        #[doc = "STM32 firmware entry point generated by the selected Platform."]
        #[$crate::application::__rt::entry]
        fn main() -> ! {
            $crate::application::initialize_allocator();
            let peripherals = $crate::application::__hal::init($crate::application::clock_config());
            let $board_bindings_value = $board_bindings!(peripherals);
            let flash = peripherals.FLASH;
            let rng = peripherals.RNG;
            static EXECUTOR: $crate::application::__StaticCell<embassy_executor::Executor> =
                $crate::application::__StaticCell::new();
            EXECUTOR
                .init(embassy_executor::Executor::new())
                .run(move |spawner| {
                    if let Ok(task) =
                        __barracuda_application_task(spawner, $board_bindings_value, flash, rng)
                    {
                        spawner.spawn(task);
                    }
                })
        }
    };
}
