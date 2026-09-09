//! macOS Embassy Net driver backed by the shared WebSocket packet gateway.

use barracuda_platform_macos_network_gateway_protocol::{
    decode, encode, encoded_len, Kind, VERSION,
};
use embassy_executor::{SpawnError, Spawner};
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_net_driver_channel::{
    driver::{HardwareAddress, LinkState},
    Device, RxRunner, State, TxRunner,
};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt as _, StreamExt as _,
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, Message},
    MaybeTlsStream, WebSocketStream,
};

/// IPv4 address owned by each gateway-connected Barracuda guest.
pub const STACK_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
/// IPv4 address of the user-space gateway.
pub const GATEWAY_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
/// Guest-visible DNS address translated by the gateway.
pub const DNS_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

const NETWORK_PREFIX_LENGTH: u8 = 24;
const MTU: usize = 1500;
const DRIVER_RX_PACKETS: usize = 8;
const DRIVER_TX_PACKETS: usize = 8;
const STACK_SOCKET_CAPACITY: usize = 16;

type GatewaySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type GatewaySink = SplitSink<GatewaySocket, Message>;
type GatewayStream = SplitStream<GatewaySocket>;
type MacosDevice = Device<'static, MTU>;

/// Connects to the unprivileged packet gateway and starts the Embassy IP stack.
pub(crate) async fn initialize(
    spawner: Spawner,
    gateway_url: &str,
) -> Result<Stack<'static>, MacosNetworkError> {
    if !gateway_url.starts_with("ws://") {
        return Err(MacosNetworkError::InvalidGatewayUrl);
    }
    let (socket, _response) = connect_async(gateway_url).await?;
    let (mut sink, stream) = socket.split();
    send_hello(&mut sink).await?;

    let channel_state = Box::leak(Box::new(
        State::<MTU, DRIVER_RX_PACKETS, DRIVER_TX_PACKETS>::new(),
    ));
    let (channel_runner, device) =
        embassy_net_driver_channel::new(channel_state, HardwareAddress::Ip);
    let (link, rx_runner, tx_runner) = channel_runner.split();

    let mut dns_servers = heapless::Vec::new();
    dns_servers
        .push(DNS_ADDRESS)
        .map_err(|_full| MacosNetworkError::DnsCapacity)?;
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(STACK_ADDRESS, NETWORK_PREFIX_LENGTH),
        gateway: Some(GATEWAY_ADDRESS),
        dns_servers,
    });
    let stack_resources = Box::leak(Box::new(StackResources::<STACK_SOCKET_CAPACITY>::new()));
    let (stack, network_runner) = embassy_net::new(device, config, stack_resources, rand::random());

    spawner.spawn(network_task(network_runner)?);
    spawner.spawn(receive_task(stream, link, rx_runner)?);
    spawner.spawn(transmit_task(sink, link, tx_runner)?);
    link.set_link_state(LinkState::Up);
    Ok(stack)
}

async fn send_hello(sink: &mut GatewaySink) -> Result<(), MacosNetworkError> {
    let mut hello = [0; 3];
    let length = encode(Kind::Hello, &[VERSION], &mut hello)
        .map_err(|_error| MacosNetworkError::Protocol)?;
    sink.send(Message::Binary(hello[..length].to_vec().into()))
        .await?;
    Ok(())
}

#[embassy_executor::task]
async fn network_task(mut runner: Runner<'static, MacosDevice>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn receive_task(
    mut stream: GatewayStream,
    link: embassy_net_driver_channel::StateRunner<'static>,
    mut packets: RxRunner<'static, MTU>,
) {
    while let Some(message) = stream.next().await {
        let message = match message {
            Ok(Message::Binary(message)) => message,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Close(_) | Message::Text(_) | Message::Frame(_)) | Err(_) => break,
        };
        let Ok(frame) = decode(&message) else {
            break;
        };
        match frame.kind {
            Kind::Packet if frame.payload.len() <= MTU => {
                let target = packets.rx_buf().await;
                target[..frame.payload.len()].copy_from_slice(frame.payload);
                packets.rx_done(frame.payload.len());
            }
            Kind::ForwardUrl => match core::str::from_utf8(frame.payload) {
                Ok(url) => log::info!("forwarded guest TCP service available at {url}"),
                Err(_error) => break,
            },
            Kind::Error => {
                log::error!(
                    "network gateway error: {}",
                    String::from_utf8_lossy(frame.payload)
                );
                break;
            }
            _ => break,
        }
    }
    link.set_link_state(LinkState::Down);
}

#[embassy_executor::task]
async fn transmit_task(
    mut sink: GatewaySink,
    link: embassy_net_driver_channel::StateRunner<'static>,
    mut packets: TxRunner<'static, MTU>,
) {
    loop {
        let packet = packets.tx_buf().await;
        let mut message = vec![0; encoded_len(packet.len())];
        let result = encode(Kind::Packet, packet, &mut message).map(|length| {
            message.truncate(length);
            message
        });
        packets.tx_done();
        let Ok(message) = result else {
            break;
        };
        if sink.send(Message::Binary(message.into())).await.is_err() {
            break;
        }
    }
    link.set_link_state(LinkState::Down);
    let _result = sink.close().await;
}

/// macOS network initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum MacosNetworkError {
    /// The generated endpoint is not an unencrypted local WebSocket URL.
    #[error("macOS network gateway URL must use ws://")]
    InvalidGatewayUrl,
    /// The gateway WebSocket could not be opened or used.
    #[error("network gateway WebSocket failed: {0}")]
    WebSocket(#[from] tungstenite::Error),
    /// A protocol frame could not be constructed.
    #[error("network gateway protocol frame could not be constructed")]
    Protocol,
    /// A permanent Embassy network task could not be spawned.
    #[error("failed to spawn Embassy gateway network task: {0}")]
    Spawn(#[from] SpawnError),
    /// The statically sized DNS server list was unexpectedly full.
    #[error("Embassy gateway network DNS server capacity is zero")]
    DnsCapacity,
}

#[cfg(test)]
mod tests {
    use super::{DNS_ADDRESS, GATEWAY_ADDRESS, STACK_ADDRESS};

    #[test]
    fn uses_the_gateway_virtual_subnet() {
        assert_eq!(STACK_ADDRESS.octets(), [10, 0, 2, 15]);
        assert_eq!(GATEWAY_ADDRESS.octets(), [10, 0, 2, 2]);
        assert_eq!(DNS_ADDRESS.octets(), [10, 0, 2, 3]);
    }
}
