use core::net::{IpAddr, SocketAddr};

use embedded_io::ErrorType;
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Host network adapter for reqwless. HTTP behavior remains identical to the
/// device path; only the TCP/DNS HAL is backed by Tokio.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioStack;

impl Dns for TokioStack {
    type Error = std::io::Error;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        let mut addresses = tokio::net::lookup_host((host, 0)).await?;
        addresses
            .find(|address| match addr_type {
                AddrType::IPv4 => address.is_ipv4(),
                AddrType::IPv6 => address.is_ipv6(),
                AddrType::Either => true,
            })
            .map(|address| address.ip())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "DNS returned no matching address",
                )
            })
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "reverse DNS is not supported",
        ))
    }
}

impl TcpConnect for TokioStack {
    type Error = std::io::Error;
    type Connection<'a> = TokioConnection;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        Ok(TokioConnection(
            tokio::net::TcpStream::connect(remote).await?,
        ))
    }
}

pub struct TokioConnection(tokio::net::TcpStream);

impl ErrorType for TokioConnection {
    type Error = std::io::Error;
}

impl Read for TokioConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buffer).await
    }
}

impl Write for TokioConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await
    }
}
