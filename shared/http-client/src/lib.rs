//! Thin Platform-to-reqwless construction boundary.
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc};

use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    Stack,
};

use static_cell::ConstStaticCell;

pub use embedded_nal_async;
pub use reqwless;
#[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
pub use reqwless::client::TlsConfig;

const TCP_CONNECTIONS: usize = 4;
const TCP_TX_BYTES: usize = 4 * 1024;
const TCP_RX_BYTES: usize = 4 * 1024;

pub type Tcp = TcpClient<'static, TCP_CONNECTIONS, TCP_TX_BYTES, TCP_RX_BYTES>;
type TcpState = TcpClientState<TCP_CONNECTIONS, TCP_TX_BYTES, TCP_RX_BYTES>;

/// The System's TCP connection pool, socket buffers included, placed in static
/// memory at link time instead of being allocated from the heap at startup.
static TCP_STATE: ConstStaticCell<TcpState> = ConstStaticCell::new(TcpState::new());
pub type Resolver = DnsSocket<'static>;

enum TlsMode {
    Plaintext,
    #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
    Config(Rc<dyn Fn() -> Option<TlsConfig<'static>>>),
}

/// Platform-owned resources from which consumers create reqwless clients.
pub struct ClientFactory<'net, T = Tcp, D = Resolver> {
    tcp: &'net T,
    resolver: &'net D,
    tls: Rc<TlsMode>,
}

impl<T, D> Clone for ClientFactory<'_, T, D> {
    fn clone(&self) -> Self {
        Self {
            tcp: self.tcp,
            resolver: self.resolver,
            tls: Rc::clone(&self.tls),
        }
    }
}

impl ClientFactory<'static> {
    /// Creates explicitly plaintext HTTP resources.
    #[must_use]
    pub fn plaintext(stack: Stack<'static>) -> Self {
        Self::from_stack(stack, TlsMode::Plaintext)
    }

    /// Creates HTTP resources backed by a Platform TLS configuration source.
    #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
    #[must_use]
    pub fn new(
        stack: Stack<'static>,
        tls: impl Fn() -> Option<TlsConfig<'static>> + 'static,
    ) -> Self {
        Self::from_stack(stack, TlsMode::Config(Rc::new(tls)))
    }

    fn from_stack(stack: Stack<'static>, tls: TlsMode) -> Self {
        // The System builds one factory. Further factories in one process
        // (host tests) fall back to a leaked heap pool.
        let state: &'static TcpState = match TCP_STATE.try_take() {
            Some(state) => state,
            None => Box::leak(Box::new(TcpState::new())),
        };
        let tcp = Box::leak(Box::new(TcpClient::new(stack, state)));
        let resolver = Box::leak(Box::new(DnsSocket::new(stack)));
        Self {
            tcp,
            resolver,
            tls: Rc::new(tls),
        }
    }
}

impl<'net, T, D> ClientFactory<'net, T, D>
where
    T: embedded_nal_async::TcpConnect + 'net,
    D: embedded_nal_async::Dns + 'net,
{
    /// Borrows an existing TCP connector and DNS resolver for plaintext HTTP.
    #[must_use]
    pub fn from_network(tcp: &'net T, resolver: &'net D) -> Self {
        Self {
            tcp,
            resolver,
            tls: Rc::new(TlsMode::Plaintext),
        }
    }

    /// Creates one reqwless client and reports whether it has TLS configured.
    #[must_use]
    pub fn create(&self) -> (reqwless::client::HttpClient<'net, T, D>, bool) {
        match self.tls.as_ref() {
            TlsMode::Plaintext => (
                reqwless::client::HttpClient::new(self.tcp, self.resolver),
                false,
            ),
            #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
            TlsMode::Config(tls) => match tls() {
                Some(config) => (
                    reqwless::client::HttpClient::new_with_tls(self.tcp, self.resolver, config),
                    true,
                ),
                None => (
                    reqwless::client::HttpClient::new(self.tcp, self.resolver),
                    false,
                ),
            },
        }
    }
}
