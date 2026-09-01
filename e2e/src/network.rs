//! In-process IP loopback owned by the e2e composition.

use std::boxed::Box;

use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_net_driver_channel::{
    driver::{HardwareAddress, LinkState},
    Device, State as ChannelState,
};

const EMBASSY_MTU: usize = 1500;
const EMBASSY_PACKETS: usize = 4;
const EMBASSY_SOCKETS: usize = 24;
type LoopbackDevice = Device<'static, EMBASSY_MTU>;

pub(crate) struct LoopbackNetwork {
    stack: Stack<'static>,
    network_runner: Runner<'static, LoopbackDevice>,
    channel_runner: embassy_net_driver_channel::Runner<'static, EMBASSY_MTU>,
}

impl LoopbackNetwork {
    pub(crate) const fn stack(&self) -> Stack<'static> {
        self.stack
    }

    pub(crate) async fn run(mut self) {
        embassy_futures::join::join(self.network_runner.run(), async move {
            let mut packet = [0_u8; EMBASSY_MTU];
            loop {
                let outgoing = self.channel_runner.tx_buf().await;
                let length = outgoing.len();
                let Some(packet_frame) = packet.get_mut(..length) else {
                    self.channel_runner.tx_done();
                    continue;
                };
                packet_frame.copy_from_slice(outgoing);
                self.channel_runner.tx_done();
                let incoming = self.channel_runner.rx_buf().await;
                let copied = match (incoming.get_mut(..length), packet.get(..length)) {
                    (Some(incoming_frame), Some(packet_frame)) => {
                        incoming_frame.copy_from_slice(packet_frame);
                        length
                    }
                    _ => 0,
                };
                self.channel_runner.rx_done(copied);
            }
        })
        .await;
    }
}

pub(crate) fn loopback_network_with_dns(dns_server: Option<Ipv4Address>) -> LoopbackNetwork {
    let state = Box::leak(Box::new(ChannelState::<
        EMBASSY_MTU,
        EMBASSY_PACKETS,
        EMBASSY_PACKETS,
    >::new()));
    let (mut channel_runner, device) = embassy_net_driver_channel::new(state, HardwareAddress::Ip);
    channel_runner.set_link_state(LinkState::Up);
    let resources = Box::leak(Box::new(StackResources::<EMBASSY_SOCKETS>::new()));
    let address = Ipv4Address::new(10, 0, 0, 1);
    let mut dns_servers = heapless::Vec::new();
    if let Some(server) = dns_server {
        let _capacity_is_one = dns_servers.push(server);
    }
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(address, 24),
        gateway: None,
        dns_servers,
    });
    let (stack, network_runner) = embassy_net::new(device, config, resources, 1);
    LoopbackNetwork {
        stack,
        network_runner,
        channel_runner,
    }
}
