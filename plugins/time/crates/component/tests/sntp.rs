//! SNTP synchronization over the real Embassy UDP path.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::time::Duration as StdDuration;

use barracuda_platform_test::loopback_network;
use barracuda_time_component::sntp::{SntpConfig, SntpSource};
use barracuda_time_component::{TimeSource, TimeSourceError};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};

const NTP_UNIX_DELTA: u64 = 2_208_988_800;

fn response_for(request: &[u8], unix_seconds: u64) -> [u8; 48] {
    let mut response = [0_u8; 48];
    response[0] = 0x24; // no leap warning, NTPv4, unicast server
    response[1] = 1; // primary server
    response[2] = 4;
    response[3] = (-20_i8) as u8;
    response[24..32].copy_from_slice(&request[40..48]);
    let seconds = u32::try_from(unix_seconds + NTP_UNIX_DELTA)
        .expect("test timestamp fits the first NTP era");
    let timestamp = (u64::from(seconds) << 32) | 0x8000_0000;
    response[16..24].copy_from_slice(&timestamp.to_be_bytes());
    response[32..40].copy_from_slice(&timestamp.to_be_bytes());
    response[40..48].copy_from_slice(&timestamp.to_be_bytes());
    response
}

async fn serve_one(stack: embassy_net::Stack<'static>, unix_seconds: Option<u64>) {
    let mut rx_metadata = [PacketMetadata::EMPTY; 1];
    let mut tx_metadata = [PacketMetadata::EMPTY; 1];
    let mut rx_buffer = [0_u8; 512];
    let mut tx_buffer = [0_u8; 512];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_metadata,
        &mut rx_buffer,
        &mut tx_metadata,
        &mut tx_buffer,
    );
    socket.bind(123).expect("bind in-process SNTP server");
    let mut request = [0_u8; 48];
    let (length, sender) = socket
        .recv_from(&mut request)
        .await
        .expect("receive SNTP request");
    assert_eq!(length, 48);
    let response = unix_seconds.map_or([0_u8; 48], |seconds| response_for(&request, seconds));
    socket
        .send_to(&response, sender.endpoint)
        .await
        .expect("send SNTP response");
    Timer::after(Duration::from_millis(10)).await;
}

async fn run_exchange(
    unix_seconds: Option<u64>,
    minimum: u64,
) -> Result<barracuda_time_component::SyncSample, TimeSourceError> {
    let network = loopback_network();
    let stack = network.stack();
    let mut source = SntpSource::new(
        stack,
        SntpConfig::new("10.0.0.1", minimum).with_timeout_millis(1_000),
    );
    tokio::time::timeout(StdDuration::from_secs(2), async {
        tokio::select! {
            () = network.run() => panic!("network runner stopped"),
            result = async {
                let server = serve_one(stack, unix_seconds);
                let client = source.synchronize();
                let (_, result) = futures_lite::future::zip(server, client).await;
                result
            } => result,
        }
    })
    .await
    .expect("SNTP exchange completes")
}

#[test]
fn sntp_configuration_exposes_server_policy_and_safe_timeout_override() {
    let config = SntpConfig::new("clock.test", 1_700_000_000).with_timeout_millis(250);
    assert_eq!(config.server(), "clock.test");
    assert_eq!(config.port(), 123);
    assert_eq!(config.timeout_millis(), 250);
    assert_eq!(config.minimum_unix_seconds(), 1_700_000_000);
}

#[tokio::test(flavor = "current_thread")]
async fn sntp_source_accepts_a_valid_response_and_updates_its_rtc_anchor() {
    let first = run_exchange(Some(1_800_000_000), 1_700_000_000)
        .await
        .expect("valid SNTP response synchronizes");
    assert!(first.unix_millis() >= 1_800_000_000_500);
}

#[tokio::test(flavor = "current_thread")]
async fn sntp_source_distinguishes_protocol_implausibility_dns_and_timeout() {
    assert_eq!(
        run_exchange(None, 1_700_000_000).await,
        Err(TimeSourceError::Protocol)
    );
    assert_eq!(
        run_exchange(Some(1_700_000_000), 1_750_000_000).await,
        Err(TimeSourceError::Implausible)
    );

    let dns_network = loopback_network();
    let mut dns_source = SntpSource::new(
        dns_network.stack(),
        SntpConfig::new("clock.invalid", 1_700_000_000).with_timeout_millis(10),
    );
    let dns = tokio::time::timeout(StdDuration::from_secs(1), async {
        tokio::select! {
            () = dns_network.run() => panic!("network runner stopped"),
            result = dns_source.synchronize() => result,
        }
    })
    .await
    .expect("DNS failure returns");
    assert_eq!(dns, Err(TimeSourceError::Dns));

    let timeout_network = loopback_network();
    let mut timeout_source = SntpSource::new(
        timeout_network.stack(),
        SntpConfig::new("10.0.0.1", 1_700_000_000).with_timeout_millis(0),
    );
    let timeout = tokio::time::timeout(StdDuration::from_secs(1), async {
        tokio::select! {
            () = timeout_network.run() => panic!("network runner stopped"),
            result = timeout_source.synchronize() => result,
        }
    })
    .await
    .expect("SNTP timeout returns");
    assert_eq!(timeout, Err(TimeSourceError::Timeout));
}
