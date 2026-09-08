//! Per-device user-space network session.

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use embassy_net::{
    tcp::TcpSocket as GuestTcpSocket, Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources,
    StaticConfigV4,
};
use embassy_net_driver_channel::{
    driver::{HardwareAddress, LinkState},
    Device, State, TxRunner,
};
use futures_util::{
    future::LocalBoxFuture, stream::FuturesUnordered, FutureExt as _, SinkExt as _, StreamExt as _,
};
use netstack_smoltcp::{StackBuilder, TcpListener as NatTcpListener, UdpSocket as NatUdpSocket};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

const MTU: usize = 1500;
const RX_PACKETS: usize = 32;
const TX_PACKETS: usize = 32;
const SOCKETS: usize = 64;
const TCP_BUFFER_SIZE: usize = 32 * 1024;
const NAT_TCP_FLOW_CAPACITY: usize = 128;
const HOST_FORWARD_CAPACITY: usize = 32;
const UDP_FLOW_CAPACITY: usize = 1024;
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Address assigned to a Barracuda guest on the virtual network.
pub(crate) const GUEST_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
/// Address assigned to the gateway endpoint on the virtual network.
pub(crate) const GATEWAY_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
/// Guest-visible address translated to the configured upstream DNS server.
pub(crate) const DNS_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

/// Packet or control information returned to a connected device.
pub(crate) enum Outbound {
    Packet(Vec<u8>),
    DeviceUrl(String),
}

/// Owns all forwarding tasks for one connected virtual NIC.
pub(crate) struct NetworkSession {
    input: mpsc::Sender<Vec<u8>>,
    cancellation: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NetworkSession {
    /// Starts outbound TCP/UDP forwarding and a host listener for the guest WebServer.
    pub(crate) async fn new(
        responses: mpsc::Sender<Outbound>,
        public_base_url: &str,
        forward_address: std::net::IpAddr,
        device_web_port: u16,
        dns_server: SocketAddr,
    ) -> Result<Self, SessionError> {
        let listener = std::net::TcpListener::bind((forward_address, 0))?;
        let forwarded_port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let device_url = forwarded_url(public_base_url, forwarded_port)?;

        let cancellation = CancellationToken::new();
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(RX_PACKETS);
        let (initialized_tx, initialized_rx) = oneshot::channel();
        let url_responses = responses.clone();
        let thread_cancellation = cancellation.clone();
        let thread = std::thread::Builder::new()
            .name(String::from("barracuda-net-session"))
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(SessionError::Io)
                    .and_then(|runtime| {
                        runtime.block_on(run_network(
                            input_rx,
                            listener,
                            responses,
                            device_web_port,
                            dns_server,
                            thread_cancellation,
                            initialized_tx,
                        ))
                    });
                if let Err(error) = result {
                    tracing::warn!(?error, "device network session stopped");
                }
            })?;

        initialized_rx
            .await
            .map_err(|_closed| SessionError::InitializationChannelClosed)??;
        let session = Self {
            input: input_tx,
            cancellation,
            thread: Some(thread),
        };
        if url_responses
            .send(Outbound::DeviceUrl(device_url))
            .await
            .is_err()
        {
            session.close().await;
            return Err(SessionError::ResponseChannelClosed);
        }
        Ok(session)
    }

    /// Submits one raw IP packet emitted by the guest.
    pub(crate) async fn input(&self, packet: &[u8]) -> Result<(), SessionError> {
        if packet.len() > MTU {
            return Err(SessionError::PacketTooLarge(packet.len()));
        }
        self.input
            .send(packet.to_vec())
            .await
            .map_err(|_closed| SessionError::InputChannelClosed)
    }

    /// Cancels every task owned by this session and joins its dedicated executor thread.
    pub(crate) async fn close(mut self) {
        self.cancellation.cancel();
        if let Some(thread) = self.thread.take() {
            let _result = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_network(
    mut input_rx: mpsc::Receiver<Vec<u8>>,
    listener: std::net::TcpListener,
    responses: mpsc::Sender<Outbound>,
    device_web_port: u16,
    dns_server: SocketAddr,
    cancellation: CancellationToken,
    initialized: oneshot::Sender<Result<(), SessionError>>,
) -> Result<(), SessionError> {
    let listener = TcpListener::from_std(listener)?;

    let (nat, nat_runner, udp, tcp) = StackBuilder::default()
        .enable_tcp(true)
        .enable_udp(true)
        .enable_icmp(true)
        .mtu(MTU)
        .build()?;
    let nat_runner = nat_runner.ok_or(SessionError::MissingNatComponent("runner"))?;
    let udp = udp.ok_or(SessionError::MissingNatComponent("UDP socket"))?;
    let tcp = tcp.ok_or(SessionError::MissingNatComponent("TCP listener"))?;
    let (mut nat_sink, mut nat_stream) = nat.split();

    let mut channel_state = State::<MTU, RX_PACKETS, TX_PACKETS>::new();
    let (channel_runner, device): (_, Device<'_, MTU>) =
        embassy_net_driver_channel::new(&mut channel_state, HardwareAddress::Ip);
    let (link, mut gateway_rx, gateway_tx) = channel_runner.split();
    link.set_link_state(LinkState::Up);

    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(GATEWAY_ADDRESS, 24),
        gateway: None,
        dns_servers: Default::default(),
    });
    let mut resources = StackResources::<SOCKETS>::new();
    let (gateway_stack, gateway_runner) = embassy_net::new(device, config, &mut resources, 1);

    let _result = initialized.send(Ok(()));
    tokio::select! {
        () = cancellation.cancelled() => {}
        () = run_gateway_stack(gateway_runner) => {}
        () = send_gateway_packets(gateway_tx, responses.clone()) => {}
        () = route_guest_packets(&mut input_rx, &mut gateway_rx, &mut nat_sink) => {}
        () = send_nat_packets(&mut nat_stream, responses) => {}
        () = run_nat_tcp(tcp, dns_server, cancellation.clone()) => {}
        () = run_nat_udp(udp, dns_server, cancellation.clone()) => {}
        () = run_host_forward(listener, gateway_stack, device_web_port, cancellation.clone()) => {}
        result = nat_runner => if let Err(error) = result {
            tracing::warn!(?error, "user-space TCP runner stopped");
        }
    }
    Ok(())
}

async fn route_guest_packets(
    input: &mut mpsc::Receiver<Vec<u8>>,
    gateway: &mut embassy_net_driver_channel::RxRunner<'_, MTU>,
    nat: &mut futures_util::stream::SplitSink<netstack_smoltcp::Stack, Vec<u8>>,
) {
    while let Some(packet) = input.recv().await {
        if destination(&packet) == Some(GATEWAY_ADDRESS.octets()) {
            let target = gateway.rx_buf().await;
            target[..packet.len()].copy_from_slice(&packet);
            gateway.rx_done(packet.len());
        } else if nat.send(packet).await.is_err() {
            return;
        }
    }
}

async fn send_nat_packets(
    packets: &mut futures_util::stream::SplitStream<netstack_smoltcp::Stack>,
    responses: mpsc::Sender<Outbound>,
) {
    while let Some(packet) = packets.next().await {
        match packet {
            Ok(packet) => {
                if responses.send(Outbound::Packet(packet)).await.is_err() {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(?error, "user-space NAT stopped producing packets");
                return;
            }
        }
    }
}

fn forwarded_url(base: &str, port: u16) -> Result<String, SessionError> {
    let uri: axum::http::Uri = base.parse()?;
    if uri.scheme_str() != Some("http") || !matches!(uri.path(), "" | "/") || uri.query().is_some()
    {
        return Err(SessionError::InvalidPublicBaseUrl);
    }
    let host = uri.host().ok_or(SessionError::InvalidPublicBaseUrl)?;
    let authority_host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Ok(format!("http://{authority_host}:{port}/"))
}

fn destination(packet: &[u8]) -> Option<[u8; 4]> {
    if packet.first().map(|byte| byte >> 4) != Some(4) {
        return None;
    }
    packet.get(16..20)?.try_into().ok()
}

fn translated_destination(destination: SocketAddr, dns_server: SocketAddr) -> SocketAddr {
    if destination.ip() == std::net::IpAddr::V4(DNS_ADDRESS) && destination.port() == 53 {
        dns_server
    } else {
        destination
    }
}

async fn run_nat_tcp(
    mut listener: NatTcpListener,
    dns_server: SocketAddr,
    cancellation: CancellationToken,
) {
    let mut flows = FuturesUnordered::<LocalBoxFuture<'static, ()>>::new();
    loop {
        let connection = tokio::select! {
            () = cancellation.cancelled() => return,
            connection = listener.next(), if flows.len() < NAT_TCP_FLOW_CAPACITY => connection,
            _result = flows.next(), if !flows.is_empty() => continue,
        };
        let Some((mut guest, source, destination)) = connection else {
            return;
        };
        let destination = translated_destination(destination, dns_server);
        let flow_cancellation = cancellation.clone();
        flows.push(async move {
            tracing::debug!(%source, %destination, "forwarding guest TCP connection");
            let result = async {
                let mut host = TcpStream::connect(destination).await?;
                tokio::io::copy_bidirectional(&mut guest, &mut host).await?;
                Ok::<(), std::io::Error>(())
            };
            tokio::select! {
                () = flow_cancellation.cancelled() => {}
                result = result => if let Err(error) = result {
                    tracing::debug!(%source, %destination, ?error, "guest TCP forwarding ended");
                }
            }
        }.boxed_local());
    }
}

type UdpFlow = (SocketAddr, SocketAddr);
type UdpResponse = (Vec<u8>, SocketAddr, SocketAddr);

async fn run_nat_udp(
    socket: NatUdpSocket,
    dns_server: SocketAddr,
    cancellation: CancellationToken,
) {
    let (mut packets, mut replies) = socket.split();
    let (response_tx, mut response_rx) = mpsc::channel::<UdpResponse>(128);
    let mut flows = HashMap::<UdpFlow, mpsc::Sender<Vec<u8>>>::new();
    let mut flow_tasks = FuturesUnordered::<LocalBoxFuture<'static, ()>>::new();
    loop {
        let packet = tokio::select! {
            () = cancellation.cancelled() => return,
            packet = packets.next() => packet,
            response = response_rx.recv() => match response {
                Some(response) => {
                    if replies.send(response).await.is_err() {
                        return;
                    }
                    continue;
                }
                None => return,
            },
            _result = flow_tasks.next(), if !flow_tasks.is_empty() => continue,
        };
        let Some((mut payload, source, original_destination)) = packet else {
            return;
        };
        let key = (source, original_destination);
        if let Some(flow) = flows.get(&key) {
            match flow.try_send(payload) {
                Ok(()) => continue,
                Err(mpsc::error::TrySendError::Full(_payload)) => {
                    tracing::warn!(%source, %original_destination, "guest UDP flow queue is full; dropping datagram");
                    continue;
                }
                Err(mpsc::error::TrySendError::Closed(returned)) => payload = returned,
            }
        }

        if flows.len() >= UDP_FLOW_CAPACITY {
            flows.retain(|_key, sender| !sender.is_closed());
            if flows.len() >= UDP_FLOW_CAPACITY {
                tracing::warn!("guest UDP flow capacity reached; dropping datagram");
                continue;
            }
        }

        let destination = translated_destination(original_destination, dns_server);
        let (flow_tx, flow_rx) = mpsc::channel(32);
        flows.insert(key, flow_tx.clone());
        if flow_tx.send(payload).await.is_err() {
            continue;
        }
        flow_tasks.push(
            run_udp_flow(
                flow_rx,
                response_tx.clone(),
                source,
                original_destination,
                destination,
                cancellation.clone(),
            )
            .boxed_local(),
        );
    }
}

async fn run_udp_flow(
    mut payloads: mpsc::Receiver<Vec<u8>>,
    responses: mpsc::Sender<UdpResponse>,
    guest: SocketAddr,
    virtual_remote: SocketAddr,
    remote: SocketAddr,
    cancellation: CancellationToken,
) {
    let bind_address = if remote.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let Ok(socket) = UdpSocket::bind(bind_address).await else {
        return;
    };
    if socket.connect(remote).await.is_err() {
        return;
    }
    let mut buffer = vec![0; 65_535];
    loop {
        let activity = tokio::select! {
            () = cancellation.cancelled() => return,
            payload = payloads.recv() => match payload {
                Some(payload) => socket.send(&payload).await.map(|_| None),
                None => return,
            },
            received = socket.recv(&mut buffer) => received.map(Some),
            () = tokio::time::sleep(UDP_IDLE_TIMEOUT) => return,
        };
        match activity {
            Ok(Some(length)) => {
                if responses
                    .send((buffer[..length].to_vec(), virtual_remote, guest))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%guest, %remote, ?error, "guest UDP forwarding ended");
                return;
            }
        }
    }
}

async fn run_gateway_stack<'d>(mut runner: Runner<'d, Device<'d, MTU>>) {
    runner.run().await
}

async fn send_gateway_packets(mut packets: TxRunner<'_, MTU>, responses: mpsc::Sender<Outbound>) {
    loop {
        let packet = packets.tx_buf().await;
        let result = responses.send(Outbound::Packet(packet.to_vec())).await;
        packets.tx_done();
        if result.is_err() {
            return;
        }
    }
}

async fn run_host_forward<'d>(
    listener: TcpListener,
    stack: Stack<'d>,
    guest_port: u16,
    cancellation: CancellationToken,
) {
    let mut flows = FuturesUnordered::<LocalBoxFuture<'d, ()>>::new();
    loop {
        let accepted = tokio::select! {
            () = cancellation.cancelled() => return,
            accepted = listener.accept(), if flows.len() < HOST_FORWARD_CAPACITY => accepted,
            _result = flows.next(), if !flows.is_empty() => continue,
        };
        let (host, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(?error, "device port-forward listener failed");
                return;
            }
        };
        let flow_cancellation = cancellation.clone();
        flows.push(
            async move {
                if let Err(error) =
                    forward_host_connection(host, stack, guest_port, flow_cancellation).await
                {
                    tracing::debug!(%peer, ?error, "device port forwarding ended");
                }
            }
            .boxed_local(),
        );
    }
}

async fn forward_host_connection<'d>(
    host: TcpStream,
    stack: Stack<'d>,
    guest_port: u16,
    cancellation: CancellationToken,
) -> Result<(), SessionError> {
    let mut receive_buffer = vec![0; TCP_BUFFER_SIZE];
    let mut transmit_buffer = vec![0; TCP_BUFFER_SIZE];
    let mut guest = GuestTcpSocket::new(stack, &mut receive_buffer, &mut transmit_buffer);
    guest
        .connect((GUEST_ADDRESS, guest_port))
        .await
        .map_err(SessionError::GuestConnect)?;

    let (mut host_reader, mut host_writer) = host.into_split();
    let (mut guest_reader, mut guest_writer) = guest.split();
    let host_to_guest = async {
        let mut buffer = [0; 4096];
        loop {
            let length = host_reader.read(&mut buffer).await?;
            if length == 0 {
                return Ok::<(), SessionError>(());
            }
            let mut written = 0;
            while written < length {
                written += guest_writer
                    .write(&buffer[written..length])
                    .await
                    .map_err(SessionError::GuestIo)?;
            }
        }
    };
    let guest_to_host = async {
        let mut buffer = [0; 4096];
        loop {
            let length = guest_reader
                .read(&mut buffer)
                .await
                .map_err(SessionError::GuestIo)?;
            if length == 0 {
                return Ok::<(), SessionError>(());
            }
            host_writer.write_all(&buffer[..length]).await?;
        }
    };
    tokio::select! {
        () = cancellation.cancelled() => Ok(()),
        result = async { tokio::try_join!(host_to_guest, guest_to_host).map(|_| ()) } => result,
    }
}

/// User-space network session initialization or forwarding failure.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    PublicUrl(#[from] axum::http::uri::InvalidUri),
    #[error("public base URL must be an absolute http URL")]
    InvalidPublicBaseUrl,
    #[error("user-space NAT did not create its {0}")]
    MissingNatComponent(&'static str),
    #[error("device response channel closed during initialization")]
    ResponseChannelClosed,
    #[error("device network session thread stopped during initialization")]
    InitializationChannelClosed,
    #[error("device packet channel closed")]
    InputChannelClosed,
    #[error("device packet has {0} bytes, exceeding the virtual NIC MTU")]
    PacketTooLarge(usize),
    #[error("could not connect to the guest WebServer: {0}")]
    GuestConnect(embassy_net::tcp::ConnectError),
    #[error("guest WebServer connection failed: {0}")]
    GuestIo(embassy_net::tcp::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use embassy_net::{
        tcp::TcpSocket,
        udp::{PacketMetadata, UdpSocket as GuestUdpSocket},
        Config, Ipv4Cidr, StackResources, StaticConfigV4,
    };
    use embassy_net_driver_channel::{
        driver::{HardwareAddress, LinkState},
        Device, State,
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;

    use super::{
        destination, forwarded_url, translated_destination, NetworkSession, Outbound, SessionError,
        DNS_ADDRESS, GATEWAY_ADDRESS, GUEST_ADDRESS, MTU,
    };

    #[test]
    fn public_url_uses_the_allocated_forward_port_and_root_path() {
        assert!(forwarded_url("http://127.0.0.1", 49152)
            .is_ok_and(|url| url == "http://127.0.0.1:49152/"));
        assert!(forwarded_url("https://127.0.0.1", 49152).is_err());
        assert!(forwarded_url("http://127.0.0.1/device", 49152).is_err());
    }

    #[test]
    fn extracts_an_ipv4_destination_without_accepting_other_frames() {
        let mut packet = [0_u8; 20];
        packet[0] = 0x45;
        packet[16..20].copy_from_slice(&[10, 0, 2, 2]);
        assert_eq!(destination(&packet), Some([10, 0, 2, 2]));
        packet[0] = 0x60;
        assert_eq!(destination(&packet), None);
        assert_eq!(destination(&[]), None);
    }

    #[test]
    fn virtual_dns_address_maps_to_the_configured_resolver() {
        let resolver = SocketAddr::from(([9, 9, 9, 9], 53));
        let virtual_dns = SocketAddr::new(IpAddr::V4(DNS_ADDRESS), 53);
        assert_eq!(translated_destination(virtual_dns, resolver), resolver);

        let other = SocketAddr::from((Ipv4Addr::LOCALHOST, 80));
        assert_eq!(translated_destination(other, resolver), other);
    }

    #[tokio::test]
    async fn session_starts_without_native_network_dependencies() {
        let (responses, mut events) = tokio::sync::mpsc::channel(4);
        let session = NetworkSession::new(
            responses,
            "http://127.0.0.1",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            80,
            SocketAddr::from(([1, 1, 1, 1], 53)),
        )
        .await;
        assert!(session.is_ok());
        assert!(matches!(events.recv().await, Some(Outbound::DeviceUrl(_))));
        if let Ok(session) = session {
            assert!(matches!(
                session.input(&[0; MTU + 1]).await,
                Err(SessionError::PacketTooLarge(_))
            ));
            session.close().await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guest_tcp_and_host_forwarding_work_in_both_directions() {
        let host_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind host echo listener");
        let host_address = host_listener.local_addr().expect("echo listener address");
        let echo = tokio::spawn(async move {
            let (mut stream, _peer) = host_listener.accept().await.expect("accept guest flow");
            let mut request = [0; 4];
            stream
                .read_exact(&mut request)
                .await
                .expect("read guest data");
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").await.expect("write guest data");
        });
        let dns_listener = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind DNS echo listener");
        let dns_address = dns_listener.local_addr().expect("DNS listener address");
        let dns_echo = tokio::spawn(async move {
            let mut request = [0; 16];
            let (length, peer) = dns_listener
                .recv_from(&mut request)
                .await
                .expect("receive translated DNS datagram");
            assert_eq!(&request[..length], b"dns");
            dns_listener
                .send_to(b"answer", peer)
                .await
                .expect("send translated DNS response");
        });

        let (responses, mut events) = tokio::sync::mpsc::channel(64);
        let session = NetworkSession::new(
            responses,
            "http://127.0.0.1",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            80,
            dns_address,
        )
        .await
        .expect("start network session");
        let device_url = match events.recv().await {
            Some(Outbound::DeviceUrl(url)) => url,
            _ => String::new(),
        };
        let forward_address = device_url
            .strip_prefix("http://")
            .and_then(|authority| authority.strip_suffix('/'))
            .and_then(|authority| authority.parse::<SocketAddr>().ok())
            .expect("forwarded device address");

        let driver_state = Box::leak(Box::new(State::<MTU, 8, 8>::new()));
        let (driver, device): (_, Device<'static, MTU>) =
            embassy_net_driver_channel::new(driver_state, HardwareAddress::Ip);
        let (link, mut receive, mut transmit) = driver.split();
        link.set_link_state(LinkState::Up);
        let config = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(GUEST_ADDRESS, 24),
            gateway: Some(GATEWAY_ADDRESS),
            dns_servers: Default::default(),
        });
        let resources = Box::leak(Box::new(StackResources::<4>::new()));
        let (stack, mut runner) = embassy_net::new(device, config, resources, 7);
        let input = session.input.clone();

        let local = tokio::task::LocalSet::new();
        let result = local.run_until(async move {
            tokio::task::spawn_local(async move { runner.run().await });
            tokio::task::spawn_local(async move {
                loop {
                    let packet = transmit.tx_buf().await.to_vec();
                    transmit.tx_done();
                    if input.send(packet).await.is_err() {
                        return;
                    }
                }
            });
            tokio::task::spawn_local(async move {
                while let Some(event) = events.recv().await {
                    let Outbound::Packet(packet) = event else {
                        continue;
                    };
                    let target = receive.rx_buf().await;
                    if packet.len() <= target.len() {
                        target[..packet.len()].copy_from_slice(&packet);
                        receive.rx_done(packet.len());
                    } else {
                        receive.rx_done(0);
                    }
                }
            });

            {
                let mut receive_buffer = [0; 4096];
                let mut transmit_buffer = [0; 4096];
                let mut socket = TcpSocket::new(stack, &mut receive_buffer, &mut transmit_buffer);
                socket
                    .connect((Ipv4Addr::LOCALHOST, host_address.port()))
                    .await
                    .expect("connect through user-space NAT");
                assert_eq!(socket.write(b"ping").await, Ok(4));
                let mut response = [0; 4];
                assert_eq!(socket.read(&mut response).await, Ok(4));
                assert_eq!(&response, b"pong");
            }

            {
                let mut receive_metadata = [PacketMetadata::EMPTY; 2];
                let mut receive_buffer = [0; 128];
                let mut transmit_metadata = [PacketMetadata::EMPTY; 2];
                let mut transmit_buffer = [0; 128];
                let mut socket = GuestUdpSocket::new(
                    stack,
                    &mut receive_metadata,
                    &mut receive_buffer,
                    &mut transmit_metadata,
                    &mut transmit_buffer,
                );
                socket.bind(53_000).expect("bind guest UDP socket");
                socket
                    .send_to(b"dns", (DNS_ADDRESS, 53))
                    .await
                    .expect("send virtual DNS datagram");
                let mut response = [0; 16];
                let (length, metadata) = socket
                    .recv_from(&mut response)
                    .await
                    .expect("receive virtual DNS response");
                assert_eq!(&response[..length], b"answer");
                assert_eq!(metadata.endpoint.addr, DNS_ADDRESS.into());
                assert_eq!(metadata.endpoint.port, 53);
            }

            let guest_server = async {
                let mut receive_buffer = [0; 4096];
                let mut transmit_buffer = [0; 4096];
                let mut socket = TcpSocket::new(stack, &mut receive_buffer, &mut transmit_buffer);
                socket.accept(80).await.expect("accept forwarded host flow");
                let mut request = [0; 4];
                assert_eq!(socket.read(&mut request).await, Ok(4));
                assert_eq!(&request, b"host");
                assert_eq!(socket.write(b"guest").await, Ok(5));
                assert_eq!(socket.flush().await, Ok(()));
            };
            let host_client = async {
                let mut socket = TcpStream::connect(forward_address)
                    .await
                    .expect("connect to assigned device URL");
                socket.write_all(b"host").await.expect("write host request");
                let mut response = [0; 5];
                socket
                    .read_exact(&mut response)
                    .await
                    .expect("read guest response");
                assert_eq!(&response, b"guest");
            };
            tokio::join!(guest_server, host_client);
        });
        tokio::time::timeout(Duration::from_secs(5), result)
            .await
            .expect("guest network round trips timed out");
        echo.await.expect("host echo task");
        dns_echo.await.expect("DNS echo task");
        session.close().await;
    }
}
