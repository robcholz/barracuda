//! One IP stack over two links: wired Ethernet while its cable is up, the
//! Wi-Fi co-processor's station otherwise.
//!
//! The Platform contract has one IP stack, so a Board with both links cannot
//! run one stack per link. The stack instead follows the preferred live
//! link: frames go out and come in on it alone, and the stack takes its MAC.
//! When the active link changes, the stack sees its link go down for one
//! poll, which restarts DHCP on the new network.

use barracuda_platform::FailoverNetworkDriver;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_net::{Runner, Stack, StackResources};
use static_cell::StaticCell;

use super::coprocessor::NetDevice;
use super::ethernet::EthernetLink;
use crate::Esp32P4PlatformError;

/// The IP stack's link on a Board with Ethernet and a Wi-Fi co-processor.
type BoardLink = FailoverNetworkDriver<EthernetLink, NetDevice>;

#[embassy_executor::task]
async fn run_network(mut runner: Runner<'static, BoardLink>, stack: Stack<'static>) -> ! {
    let (never, ()) = join(runner.run(), super::report_addresses("network", stack)).await;
    never
}

/// Starts the stack over both links and the task that runs a DHCP client
/// over the active one.
///
/// # Errors
///
/// Returns an error when the stack task cannot be allocated.
pub(super) fn start(
    spawner: Spawner,
    wired: EthernetLink,
    station: NetDevice,
) -> Result<Stack<'static>, Esp32P4PlatformError> {
    static RESOURCES: StaticCell<StackResources<16>> = StaticCell::new();

    let (stack, runner) = embassy_net::new(
        FailoverNetworkDriver::new(wired, station),
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        super::stack_seed(),
    );
    spawner.spawn(run_network(runner, stack).map_err(|_error| Esp32P4PlatformError::NetworkTask)?);
    Ok(stack)
}
