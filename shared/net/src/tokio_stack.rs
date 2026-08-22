use core::net::{IpAddr, SocketAddr};

use embedded_io::ErrorType;
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, ConnectedUdp, Dns, TcpConnect, UdpStack, UnconnectedUdp};
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

impl UdpStack for TokioStack {
    type Error = std::io::Error;
    type Connected = TokioConnectedUdp;
    type UniquelyBound = TokioUnconnectedUdp;
    type MultiplyBound = TokioUnconnectedUdp;

    async fn connect_from(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<(SocketAddr, Self::Connected), Self::Error> {
        let socket = tokio::net::UdpSocket::bind(local).await?;
        socket.connect(remote).await?;
        let local = socket.local_addr()?;
        Ok((local, TokioConnectedUdp(socket)))
    }

    async fn bind_single(
        &self,
        local: SocketAddr,
    ) -> Result<(SocketAddr, Self::UniquelyBound), Self::Error> {
        let socket = tokio::net::UdpSocket::bind(local).await?;
        let local = socket.local_addr()?;
        Ok((local, TokioUnconnectedUdp(socket)))
    }

    async fn bind_multiple(&self, local: SocketAddr) -> Result<Self::MultiplyBound, Self::Error> {
        Ok(TokioUnconnectedUdp(
            tokio::net::UdpSocket::bind(local).await?,
        ))
    }
}

/// Connected Tokio UDP socket exposed through `embedded-nal-async`.
pub struct TokioConnectedUdp(tokio::net::UdpSocket);

impl ConnectedUdp for TokioConnectedUdp {
    type Error = std::io::Error;

    async fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        let written = self.0.send(data).await?;
        if written == data.len() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "UDP datagram was only partially sent",
            ))
        }
    }

    async fn receive_into(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.recv(buffer).await
    }
}

/// Unconnected Tokio UDP socket exposed through `embedded-nal-async`.
pub struct TokioUnconnectedUdp(tokio::net::UdpSocket);

impl UnconnectedUdp for TokioUnconnectedUdp {
    type Error = std::io::Error;

    async fn send(
        &mut self,
        _local: SocketAddr,
        remote: SocketAddr,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        let written = self.0.send_to(data, remote).await?;
        if written == data.len() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "UDP datagram was only partially sent",
            ))
        }
    }

    async fn receive_into(
        &mut self,
        buffer: &mut [u8],
    ) -> Result<(usize, SocketAddr, SocketAddr), Self::Error> {
        let (length, remote) = self.0.recv_from(buffer).await?;
        Ok((length, self.0.local_addr()?, remote))
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
