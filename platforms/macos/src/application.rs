/// Generates the macOS process entry around one application future.
#[macro_export]
macro_rules! platform_entry {
    (|$spawner:ident| $application:expr) => {
        extern crate std;

        #[unsafe(no_mangle)]
        #[embassy_executor::main]
        async fn main($spawner: embassy_executor::Spawner) {
            if let Err(error) = $application.await {
                std::eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    };
}
