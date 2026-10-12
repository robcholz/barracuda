/// Generates the macOS process entry around one application future.
///
/// The entry takes the host's virtual peripheral singleton exactly once, as a
/// device entry takes its chip singleton, and moves the selected Board's
/// tokens out of it. A missing or invalid `BARRACUDA_VIRTUAL_IO_ADDR` stops
/// the process before the System starts.
#[macro_export]
macro_rules! platform_entry {
    ($board:expr, $board_bindings:ident, $board_bindings_type:path, |$spawner:ident, $platform_bindings:ident, $board_bindings_value:ident| $application:expr) => {
        extern crate std;

        #[unsafe(no_mangle)]
        #[embassy_executor::main]
        async fn main($spawner: embassy_executor::Spawner) {
            let peripherals = match $crate::hal::VirtualPeripherals::take() {
                Ok(peripherals) => peripherals,
                Err(error) => {
                    std::eprintln!("error: {error}");
                    std::process::exit(1);
                }
            };
            // A Board without exposed I/O moves nothing out of the singleton.
            let _ = &peripherals;
            let $board_bindings_value: $board_bindings_type = $board_bindings!(peripherals);
            let $platform_bindings = $board;
            if let Err(error) = $application.await {
                std::eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    };
}
