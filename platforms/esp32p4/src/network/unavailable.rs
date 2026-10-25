//! The IP stack of a Board without a link the Platform drives.

use barracuda_platform::UnavailableNetworkDriver;
use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use static_cell::StaticCell;

use crate::Esp32P4PlatformError;

#[embassy_executor::task]
async fn run_network(mut runner: Runner<'static, UnavailableNetworkDriver>) -> ! {
    runner.run().await
}

/// Starts a stack whose link is always down, so the Platform still supplies
/// the IP stack its contract requires.
pub(super) fn start(spawner: Spawner) -> Result<Stack<'static>, Esp32P4PlatformError> {
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

    log::info!("the selected Board declares no network link; the IP stack stays down");
    let (stack, runner) = embassy_net::new(
        UnavailableNetworkDriver,
        embassy_net::Config::default(),
        RESOURCES.init(StackResources::new()),
        0,
    );
    spawner.spawn(run_network(runner).map_err(|_error| Esp32P4PlatformError::NetworkTask)?);
    Ok(stack)
}
