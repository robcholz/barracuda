//! Linux Embassy Net driver bridge over an open layer-three TUN device.

use std::sync::Arc;

use embassy_executor::{SpawnError, Spawner};
use embassy_net::{
    Config, Ipv4Address, Ipv4Cidr, Runner as NetworkRunner, Stack, StackResources, StaticConfigV4,
};
use embassy_net_driver_channel::{
    driver::{HardwareAddress, LinkState},
    Device, RxRunner, State, TxRunner,
};

/// IPv4 address owned by the simulated Embassy Net stack.
pub const STACK_ADDRESS: Ipv4Address = Ipv4Address::new(10, 42, 0, 2);
/// IPv4 address owned by the operating-system side of the TUN link.
pub const GATEWAY_ADDRESS: Ipv4Address = Ipv4Address::new(10, 42, 0, 1);

const DNS_SERVER_ADDRESS: Ipv4Address = Ipv4Address::new(1, 1, 1, 1);
const NETWORK_PREFIX_LENGTH: u8 = 30;
const MTU: usize = 1500;
const DRIVER_RX_PACKETS: usize = 8;
const DRIVER_TX_PACKETS: usize = 8;
const STACK_SOCKET_CAPACITY: usize = 16;

type TunDevice = Device<'static, MTU>;
type TunNetworkRunner = NetworkRunner<'static, TunDevice>;

/// Creates an Embassy Net stack over an already-created native TUN device.
///
/// The concrete Platform owns opening and provisioning the operating-system
/// interface. This function owns only the packet driver queues and permanent
/// Embassy tasks.
///
/// # Errors
///
/// Returns an error when a permanent Embassy task cannot be spawned or the
/// fixed DNS list cannot be constructed.
pub async fn initialize(
    spawner: Spawner,
    tun: tun::AsyncDevice,
) -> Result<Stack<'static>, TunNetworkError> {
    let tun = Arc::new(tun);
    let channel_state = Box::leak(Box::new(
        State::<MTU, DRIVER_RX_PACKETS, DRIVER_TX_PACKETS>::new(),
    ));
    let (mut channel_runner, device) =
        embassy_net_driver_channel::new(channel_state, HardwareAddress::Ip);
    channel_runner.set_link_state(LinkState::Up);
    let (_state_runner, rx_runner, tx_runner) = channel_runner.split();

    let mut dns_servers = heapless::Vec::new();
    dns_servers
        .push(DNS_SERVER_ADDRESS)
        .map_err(|_full| TunNetworkError::DnsCapacity)?;
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(STACK_ADDRESS, NETWORK_PREFIX_LENGTH),
        gateway: Some(GATEWAY_ADDRESS),
        dns_servers,
    });
    let stack_resources = Box::leak(Box::new(StackResources::<STACK_SOCKET_CAPACITY>::new()));
    let (stack, network_runner) = embassy_net::new(device, config, stack_resources, rand::random());

    spawner.spawn(network_task(network_runner))?;
    spawner.spawn(tun_rx_task(Arc::clone(&tun), rx_runner))?;
    spawner.spawn(tun_tx_task(tun, tx_runner))?;
    stack.wait_link_up().await;
    Ok(stack)
}

#[embassy_executor::task]
async fn network_task(mut runner: TunNetworkRunner) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn tun_rx_task(tun: Arc<tun::AsyncDevice>, mut runner: RxRunner<'static, MTU>) {
    let mut packet = [0_u8; MTU];
    loop {
        let Ok(length) = tun.recv(&mut packet).await else {
            continue;
        };
        let target = runner.rx_buf().await;
        let copied_length = match (target.get_mut(..length), packet.get(..length)) {
            (Some(target), Some(packet)) => {
                target.copy_from_slice(packet);
                length
            }
            _ => 0,
        };
        runner.rx_done(copied_length);
    }
}

#[embassy_executor::task]
async fn tun_tx_task(tun: Arc<tun::AsyncDevice>, mut runner: TxRunner<'static, MTU>) {
    loop {
        let packet = runner.tx_buf().await;
        let _result = tun.send(packet).await;
        runner.tx_done();
    }
}

/// Shared Embassy TUN transport initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum TunNetworkError {
    /// A permanent Embassy network task could not be spawned.
    #[error("failed to spawn Embassy TUN network task: {0}")]
    Spawn(#[from] SpawnError),
    /// The statically sized DNS server list was unexpectedly full.
    #[error("Embassy TUN network DNS server capacity is zero")]
    DnsCapacity,
}
