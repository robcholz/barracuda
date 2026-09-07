use alloc::{boxed::Box, string::String};
use core::net::SocketAddr;

use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, with_timeout};
use getset::{CopyGetters, Getters};
use sntpc::{NtpContext, NtpTimestampGenerator, NtpUdpSocket};

use crate::{SyncSample, TimeSource, TimeSourceError, TimeSourceFuture};

/// SNTP server and validation policy.
#[derive(Clone, Debug, Eq, PartialEq, Getters, CopyGetters)]
pub struct SntpConfig {
    /// DNS name of the SNTP server.
    #[getset(get = "pub")]
    server: String,
    /// UDP port, normally 123.
    #[getset(get_copy = "pub")]
    port: u16,
    /// Per-attempt request timeout.
    #[getset(get_copy = "pub")]
    timeout_millis: u64,
    /// Lower bound used both for NTP era reconstruction and sanity checking.
    #[getset(get_copy = "pub")]
    minimum_unix_seconds: u64,
}

impl SntpConfig {
    /// Creates an SNTP policy with the standard UDP port.
    #[must_use]
    pub fn new(server: impl Into<String>, minimum_unix_seconds: u64) -> Self {
        Self {
            server: server.into(),
            port: 123,
            timeout_millis: 5_000,
            minimum_unix_seconds,
        }
    }

    /// Overrides the request timeout.
    #[must_use]
    pub const fn with_timeout_millis(mut self, timeout_millis: u64) -> Self {
        self.timeout_millis = timeout_millis;
        self
    }
}

#[derive(Clone, Copy)]
struct RtcTimestampGenerator {
    unix_anchor_millis: u64,
    monotonic_anchor: Instant,
    captured_millis: u64,
}

impl RtcTimestampGenerator {
    fn new(unix_anchor_millis: u64, monotonic_anchor: Instant) -> Self {
        Self {
            unix_anchor_millis,
            monotonic_anchor,
            captured_millis: unix_anchor_millis,
        }
    }
}

impl NtpTimestampGenerator for RtcTimestampGenerator {
    fn init(&mut self) {
        self.captured_millis = self.unix_anchor_millis.saturating_add(
            Instant::now()
                .saturating_duration_since(self.monotonic_anchor)
                .as_millis(),
        );
    }

    fn timestamp_sec(&self) -> u64 {
        self.captured_millis / 1_000
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        let millis = self.captured_millis % 1_000;
        u32::try_from(millis.saturating_mul(1_000)).unwrap_or_default()
    }
}

struct ConnectedSntpSocket<'a> {
    socket: Mutex<NoopRawMutex, UdpSocket<'a>>,
    remote: SocketAddr,
}

impl NtpUdpSocket for ConnectedSntpSocket<'_> {
    async fn send_to(&self, buffer: &[u8], address: SocketAddr) -> sntpc::Result<usize> {
        if address != self.remote {
            return Err(sntpc::Error::Network);
        }
        let SocketAddr::V4(address) = address else {
            return Err(sntpc::Error::Network);
        };
        self.socket
            .lock()
            .await
            .send_to(buffer, address)
            .await
            .map_err(|_error| sntpc::Error::Network)?;
        Ok(buffer.len())
    }

    async fn recv_from(&self, buffer: &mut [u8]) -> sntpc::Result<(usize, SocketAddr)> {
        let (length, metadata) = self
            .socket
            .lock()
            .await
            .recv_from(buffer)
            .await
            .map_err(|_error| sntpc::Error::Network)?;
        Ok((length, metadata.endpoint.into()))
    }
}

/// `no_std` SNTP source over the common Embassy Net stack.
pub struct SntpSource {
    network: Stack<'static>,
    config: SntpConfig,
    unix_anchor_millis: u64,
    monotonic_anchor: Instant,
}

impl SntpSource {
    /// Creates a source whose cold-start era pivot is the configured minimum time.
    #[must_use]
    pub fn new(network: Stack<'static>, config: SntpConfig) -> Self {
        let unix_anchor_millis = config.minimum_unix_seconds.saturating_mul(1_000);
        Self {
            network,
            config,
            unix_anchor_millis,
            monotonic_anchor: Instant::now(),
        }
    }
}

impl TimeSource for SntpSource {
    fn synchronize(&mut self) -> TimeSourceFuture<'_> {
        Box::pin(async move {
            let addresses = self
                .network
                .dns_query(&self.config.server, DnsQueryType::A)
                .await
                .map_err(|_error| TimeSourceError::Dns)?;
            let ip = addresses.first().copied().ok_or(TimeSourceError::Dns)?;
            let remote = SocketAddr::new(ip.into(), self.config.port);
            let mut rx_metadata = [PacketMetadata::EMPTY; 1];
            let mut tx_metadata = [PacketMetadata::EMPTY; 1];
            let mut rx_buffer = [0_u8; 512];
            let mut tx_buffer = [0_u8; 512];
            let mut socket = UdpSocket::new(
                self.network,
                &mut rx_metadata,
                &mut rx_buffer,
                &mut tx_metadata,
                &mut tx_buffer,
            );
            socket.bind(0).map_err(|_error| TimeSourceError::Network)?;
            let socket = ConnectedSntpSocket {
                socket: Mutex::new(socket),
                remote,
            };
            let timestamp =
                RtcTimestampGenerator::new(self.unix_anchor_millis, self.monotonic_anchor);
            let result = with_timeout(
                Duration::from_millis(self.config.timeout_millis.max(1)),
                sntpc::get_time(remote, &socket, NtpContext::new(timestamp)),
            )
            .await
            .map_err(|_timeout| TimeSourceError::Timeout)?
            .map_err(|_error| TimeSourceError::Protocol)?;
            if result.sec() < self.config.minimum_unix_seconds {
                return Err(TimeSourceError::Implausible);
            }

            let received_at = Instant::now();
            let server_millis = result.sec().saturating_mul(1_000).saturating_add(u64::from(
                sntpc::fraction_to_milliseconds(result.sec_fraction()),
            ));
            let half_roundtrip_millis = result.roundtrip().saturating_add(1_999) / 2_000;
            let unix_millis = server_millis.saturating_add(half_roundtrip_millis);
            self.unix_anchor_millis = unix_millis;
            self.monotonic_anchor = received_at;
            Ok(SyncSample::new(unix_millis, received_at))
        })
    }
}
